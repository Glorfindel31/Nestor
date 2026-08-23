//! Headless nesting: import, nest, report, optionally export.
//!
//! **What this is for.** Nest *quality* has never had a regression harness.
//! `cargo test` proves the engine is correct; the benchmark examples measure
//! one scenario each with their inputs compiled in. Neither answers "did this
//! release get worse on the user's own job", because the user's job is files
//! on disk. This runs the real pipeline - the same `commands` functions the
//! window calls - over any files, and prints a fixed set of numbers that a
//! script can diff between builds.
//!
//! It is also just useful: a batch of parts nested and exported without
//! opening a window.
//!
//! Arguments are parsed by hand. `clap` would be a dependency, a derive macro
//! and a build-time cost for about thirty lines of `match`, on a tool whose
//! entire interface is a dozen flags.

use std::time::Instant;

use rustynesting::commands;
use rustynesting::dto::{ExportRequest, NestConfigDto, PartDto, PlacementTypeDto, PointDto, PolygonDto, RunNestRequest};

const USAGE: &str = "\
nest - headless nesting

USAGE:
    nest [OPTIONS] <file.dxf|file.svg>...

    Every shape in every file is a part. The stock sheet is given by --sheet.

OPTIONS:
    --sheet WxH        stock size in mm (default 2440x1220)
    --sheets N         how many sheets are available (default 100)
    --qty N            copies of each part (default 1). Applies to the files
                       named after it, so quantities can be mixed:
                           nest --qty 250 a.dxf --qty 50 b.dxf
    --margin MM        clearance to the sheet edge (default 0)
    --spacing MM       clearance between parts (default 0)
    --kerf MM          cut width; adds to both of the above (default 0)
    --rotations N      angles tried per part (default 4)
    --generations N    GA generations (default 5)
    --population N     GA population (default 10)
    --runs N           escalating attempts (default 1)
    --seed N           RNG seed (default 0)
    --placement TYPE   gravity|box|convexhull|tightfit|gravitytightfit|gravitycorrective
                       (default tightfit)
    --tolerance MM     curve tessellation tolerance on import (default 0.1)
    --svg-unit UNIT    mm|cm|m|px - what one SVG user unit means
    --out FILE         write the result; format from the extension (.dxf/.svg)
    --unplaced         include never-placed parts in the export
    --json             print the summary as JSON instead of text
    -h, --help         this
";

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("nest: {message}");
            std::process::ExitCode::FAILURE
        }
    }
}

struct Options {
    /// Each file with the `--qty` in force when it was named, if any, so one
    /// run can mix quantities (a job of 250 brackets and 50 lids).
    files: Vec<(String, Option<usize>)>,
    sheet: (f64, f64),
    sheets: usize,
    qty: usize,
    out: Option<String>,
    include_unplaced: bool,
    json: bool,
    svg_unit: Option<String>,
    config: NestConfigDto,
}

impl Default for Options {
    fn default() -> Self {
        // Deliberately the app's own defaults, read from `ConfigForm` where
        // it has one, so a CLI run and a GUI run of the same job are the same
        // job. Two sets of defaults that drift apart would make this useless
        // as a harness for what users actually get.
        Self {
            files: Vec::new(),
            sheet: (2440.0, 1220.0),
            sheets: 100,
            qty: 1,
            out: None,
            include_unplaced: false,
            json: false,
            svg_unit: None,
            config: NestConfigDto {
                placement_type: PlacementTypeDto::TightFit,
                rotations: 4,
                population_size: 10,
                mutation_rate: 10.0,
                dominant_part_area_threshold: nesting::placement::DEFAULT_DOMINANT_PART_AREA_THRESHOLD,
                curve_tolerance: 0.1,
                generations: 5,
                margin: 0.0,
                spacing: 0.0,
                kerf: 0.0,
                max_threads: 0,
                seed: 0,
                runs: 1,
                cleanup_threshold_percent: None,
                mirror: false,
            },
        }
    }
}

fn parse_args() -> Result<Option<Options>, String> {
    let mut opts = Options::default();
    // A file named *after* a `--qty` takes that quantity; one named before any
    // `--qty` inherits whatever the flag finally settles on, so the plain
    // `nest file.dxf --qty 250` ordering keeps working.
    let mut qty_seen = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        // Every value-taking flag needs the same "and there is a next
        // argument" check, and the same "and it parses" check; doing it once
        // here is what keeps the match arms one line each.
        let mut value = |name: &str| -> Result<String, String> { args.next().ok_or_else(|| format!("{name} needs a value")) };
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(None);
            }
            "--sheet" => opts.sheet = parse_size(&value("--sheet")?)?,
            "--sheets" => opts.sheets = number(&value("--sheets")?, "--sheets")?,
            "--qty" => {
                opts.qty = number(&value("--qty")?, "--qty")?;
                qty_seen = true;
            }
            "--margin" => opts.config.margin = number(&value("--margin")?, "--margin")?,
            "--spacing" => opts.config.spacing = number(&value("--spacing")?, "--spacing")?,
            "--kerf" => opts.config.kerf = number(&value("--kerf")?, "--kerf")?,
            "--rotations" => opts.config.rotations = number(&value("--rotations")?, "--rotations")?,
            "--generations" => opts.config.generations = number(&value("--generations")?, "--generations")?,
            "--population" => opts.config.population_size = number(&value("--population")?, "--population")?,
            "--runs" => opts.config.runs = number(&value("--runs")?, "--runs")?,
            "--seed" => opts.config.seed = number(&value("--seed")?, "--seed")?,
            "--tolerance" => opts.config.curve_tolerance = number(&value("--tolerance")?, "--tolerance")?,
            "--placement" => opts.config.placement_type = placement_type(&value("--placement")?)?,
            "--svg-unit" => opts.svg_unit = Some(value("--svg-unit")?),
            "--out" => opts.out = Some(value("--out")?),
            "--unplaced" => opts.include_unplaced = true,
            "--json" => opts.json = true,
            other if other.starts_with('-') => return Err(format!("unknown option '{other}'\n\n{USAGE}")),
            file => opts.files.push((file.to_string(), qty_seen.then_some(opts.qty))),
        }
    }
    if opts.files.is_empty() {
        return Err(format!("no input files\n\n{USAGE}"));
    }
    Ok(Some(opts))
}

fn number<T: std::str::FromStr>(raw: &str, name: &str) -> Result<T, String> {
    raw.parse().map_err(|_| format!("{name}: '{raw}' is not a number"))
}

fn parse_size(raw: &str) -> Result<(f64, f64), String> {
    let (w, h) = raw.split_once(['x', 'X']).ok_or_else(|| format!("--sheet: expected WxH, got '{raw}'"))?;
    Ok((number(w, "--sheet")?, number(h, "--sheet")?))
}

fn placement_type(raw: &str) -> Result<PlacementTypeDto, String> {
    match raw.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
        "gravity" => Ok(PlacementTypeDto::Gravity),
        "box" => Ok(PlacementTypeDto::Box),
        "convexhull" => Ok(PlacementTypeDto::ConvexHull),
        "tightfit" => Ok(PlacementTypeDto::TightFit),
        "gravitytightfit" => Ok(PlacementTypeDto::GravityTightFit),
        "gravitycorrective" => Ok(PlacementTypeDto::GravityCorrective),
        other => Err(format!("--placement: unknown type '{other}'")),
    }
}

fn rect(w: f64, h: f64) -> PolygonDto {
    PolygonDto {
        points: vec![PointDto { x: 0.0, y: 0.0 }, PointDto { x: w, y: 0.0 }, PointDto { x: w, y: h }, PointDto { x: 0.0, y: h }],
        layer: "sheet".to_string(),
        is_circle: None,
        children: Vec::new(),
        texts: Vec::new(),
        real_boundary: None,
    }
}

fn run() -> Result<(), String> {
    let Some(opts) = parse_args()? else { return Ok(()) };

    let mut parts: Vec<PartDto> = Vec::new();
    for (file, quantity) in &opts.files {
        let quantity = quantity.unwrap_or(opts.qty);
        let is_svg = std::path::Path::new(file).extension().is_some_and(|e| e.eq_ignore_ascii_case("svg"));
        let shapes = if is_svg {
            let (shapes, size_guessed) = commands::import_svg(file, opts.config.curve_tolerance, opts.svg_unit.as_deref())?;
            if size_guessed {
                // The one import that succeeds cleanly at the wrong size. The
                // window says so in its status line; here it goes to stderr,
                // so it survives `--json` being piped somewhere.
                eprintln!("nest: warning: {file} does not say how big it is (viewBox but no width/height) - assuming 96dpi. Use --svg-unit.");
            }
            shapes
        } else {
            commands::import_dxf(file, opts.config.curve_tolerance)?
        };
        if shapes.is_empty() {
            return Err(format!("{file}: no closed profiles found"));
        }
        parts.extend(shapes.into_iter().map(|polygon| PartDto { polygon, quantity, allowed_rotations: None, mirror: None }));
    }

    let sheets = vec![rect(opts.sheet.0, opts.sheet.1); opts.sheets];
    let started = Instant::now();
    // The full-fat entry point with every callback stubbed out - the plain
    // `run_nest` beside it is `#[cfg(test)]` only, deliberately, so that the
    // production path has exactly one shape. A progress bar on a tool whose
    // whole output is one summary would be noise.
    let response = commands::run_nest_with_progress(
        RunNestRequest { sheets: sheets.clone(), parts, config: opts.config.clone() },
        |_, _, _| {},
        || false,
        |_, _, _| {},
        |_| {},
        |_| {},
    )?;
    let elapsed = started.elapsed();

    // The same audit the window runs after every nest. A harness that reports
    // only utilisation would happily record a "better" result that is not
    // cuttable.
    let audit = commands::audit_nest(rustynesting::dto::AuditRequest {
        sheets: sheets.clone(),
        placements: response.placements.clone(),
        parts_by_id: response.parts_by_id.clone(),
        config: opts.config.clone(),
    })?;

    if let Some(out) = &opts.out {
        let request = ExportRequest {
            sheets: sheets.clone(),
            parts_by_id: response.parts_by_id.clone(),
            placements: response.placements.clone(),
            sheet_spacing: 10.0,
            include_sheet_outline: true,
            include_unplaced: opts.include_unplaced,
        };
        match std::path::Path::new(out).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
            Some("svg") => commands::export_svg(out, request)?,
            Some("dxf") => commands::export_dxf(out, request)?,
            other => return Err(format!("--out: don't know how to write '{}'", other.unwrap_or("a file with no extension"))),
        }
        eprintln!("nest: wrote {out}");
    }

    report(&opts, &response, &audit, elapsed);
    Ok(())
}

/// One word for the whole audit, so a harness can compare runs on a string
/// rather than on three counters.
fn verdict(audit: &rustynesting::dto::AuditReportDto) -> &'static str {
    if audit.fatal_count > 0 {
        "FAILED"
    } else if audit.warning_count > 0 {
        "WARNED"
    } else {
        "PASSED"
    }
}

/// Per-sheet utilisation, using the *true* part areas the run reports.
fn per_sheet(response: &rustynesting::dto::RunNestResponse, sheet_area: f64) -> Vec<f64> {
    response
        .placements
        .iter()
        .map(|placement| {
            let used: f64 = placement.parts.iter().filter_map(|p| response.parts_by_id.get(&p.id)).map(material_area_of).sum();
            if sheet_area > 0.0 {
                used / sheet_area * 100.0
            } else {
                0.0
            }
        })
        .collect()
}

/// Outline area minus its holes - the same rule `geometry::polygon_material_area`
/// uses, and the same one a commercial job report's Util column uses. Counting
/// a drilled hole as material overstates every sheet holding a holed part, so
/// the two reports cannot be read side by side.
fn material_area_of(poly: &rustynesting::dto::PolygonDto) -> f64 {
    let outer = area_of(&poly.points);
    let holes: f64 = poly.children.iter().map(|c| area_of(&c.points)).sum();
    (outer - holes).max(0.0)
}

fn area_of(points: &[PointDto]) -> f64 {
    let mut sum = 0.0;
    for (i, p) in points.iter().enumerate() {
        let q = &points[(i + 1) % points.len()];
        sum += p.x * q.y - q.x * p.y;
    }
    (sum / 2.0).abs()
}

fn report(opts: &Options, response: &rustynesting::dto::RunNestResponse, audit: &rustynesting::dto::AuditReportDto, elapsed: std::time::Duration) {
    let sheet_area = opts.sheet.0 * opts.sheet.1;
    let sheets = per_sheet(response, sheet_area);
    let best = sheets.iter().copied().fold(0.0_f64, f64::max);
    let mean = if sheets.is_empty() { 0.0 } else { sheets.iter().sum::<f64>() / sheets.len() as f64 };
    let placed: usize = response.placements.iter().map(|p| p.parts.len()).sum();

    if opts.json {
        // Hand-written rather than through serde: it is eight scalars and an
        // array, and the point of it is being diffable by a script, so the
        // key order being fixed and obvious matters more than the machinery.
        let per = sheets.iter().map(|u| format!("{u:.4}")).collect::<Vec<_>>().join(",");
        // Parts per sheet, not just utilisation: a commercial nester's job
        // report lists a per-sheet quantity, and without ours the two can
        // only be lined up by inferring counts from ratios of percentages.
        let per_parts = response.placements.iter().map(|p| p.parts.len().to_string()).collect::<Vec<_>>().join(",");
        println!(
            "{{\"sheets\":{},\"placed\":{placed},\"unplaced\":{},\"utilisation\":{:.4},\"best_sheet\":{best:.4},\"mean_sheet\":{mean:.4},\"fitness\":{:.4},\"audit\":\"{}\",\"seconds\":{:.3},\"per_sheet\":[{per}],\"per_sheet_parts\":[{per_parts}]}}",
            response.placements.len(),
            response.unplaced_count,
            response.utilisation,
            response.fitness,
            verdict(audit),
            elapsed.as_secs_f64(),
        );
    } else {
        println!("sheets used   {}", response.placements.len());
        println!("parts placed  {placed}");
        println!("unplaced      {}", response.unplaced_count);
        println!("utilisation   {:.2}%", response.utilisation);
        println!("best sheet    {best:.2}%");
        println!("mean sheet    {mean:.2}%");
        println!("audit         {}", verdict(audit));
        for issue in &audit.issues {
            println!("  {} {} on sheet {}: {:?}", if issue.fatal { "FATAL" } else { "warn " }, issue.kind, issue.sheet_index + 1, issue.part_ids);
        }
        println!("elapsed       {:.2?}", elapsed);
    }
}
