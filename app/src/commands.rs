//! Tauri command layer - a redesign, not a port, of the original Electron
//! app's IPC surface. The original dispatches one `background-start` IPC
//! message per GA individual to a pool of separate worker `BrowserWindow`
//! processes, collecting `background-response` messages back asynchronously;
//! this collapses to a single command per nest run, since `nesting::dispatch`
//! already parallelizes a generation in-process via rayon - there's no
//! separate worker process to message. Deliberately not wired to the legacy
//! `frontend/deepnest.js`/`ui/**` Ractive UI (kept in the tree as reference
//! only, unreferenced) - that code assumes a Node-integrated Electron
//! renderer (`require("electron")`/`ipcRenderer`, etc.) that doesn't exist in
//! Tauri's webview.
//!
//! Every command is a thin wrapper around a plain function (`import_dxf`/
//! `run_nest` below) that takes no Tauri types and returns a plain
//! `Result` - testable directly, without spinning up a Tauri runtime.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dxf::Drawing;
use geometry::clearance::{prepare_part, prepare_sheet};
use geometry::dxf_export::{PlacedShape, SheetLayout};
use geometry::dxf_import::{rotate_layered_polygon, LayeredPolygon};
use nesting::cache::NfpCache;
use nesting::consolidation::{recompute_totals, refine_consolidation};
use nesting::dispatch;
use nesting::ga::{is_better_nest, GaConfig, GeneticAlgorithm};
use nesting::placement::{PlaceResult, PlacementConfig, PlacementType};
use nesting::repack;

use crate::dto::{
    expand_parts, BestResultDto, ExpandedParts, PartRuleDto, ReportRequest, ValidatePlacementRequest, ValidatePlacementResponse, ExportRequest, NestConfigDto, NestRunCompleteDto, NestRunStartDto, NestSnapshotDto,
    PlacedPartDto, PolygonDto, RepackSheetRequest, RepackSheetResponse, RunNestRequest, RunNestResponse, SheetPlacementDto,
};

/// Shared per-process nest-run state, managed Tauri state
/// (`app.manage(NestCancelFlag::default())` in `main.rs`). Both fields are
/// `Arc`s so `run_nest_command` can clone them into the `spawn_blocking`
/// closure that actually runs the GA loop, while `cancel_nest_command` sets
/// `cancel` through the same `State` handle from a separate, concurrent IPC
/// call.
///
/// `running` makes "only one nest at a time" a backend-enforced guarantee
/// instead of trusting the frontend to keep the RUN button disabled: without
/// it, two overlapping `run_nest_command` calls would share one `cancel`
/// flag with no way to tell them apart, and the second call's start-of-run
/// reset could silently swallow a stop meant for the first.
#[derive(Default)]
pub struct NestCancelFlag {
    cancel: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
}

/// Requests that the in-flight nest run (if any) stop after its current
/// generation instead of running all `config.generations` - there's no
/// "which run" to target since only one can ever be in flight at a time
/// (`begin_run` rejects a second outright, see `NestCancelFlag::running`).
/// A cancel with nothing running is a harmless no-op.
impl NestCancelFlag {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Backend-enforced single-flight: claims the run slot, or returns
    /// `Err` if one is already in progress. `swap` is the check-and-set in
    /// one atomic step - two callers racing here can't both observe `false`.
    ///
    /// Deliberately `swap` first, `cancel.store(false)` second, not the
    /// reverse: resetting first would run unconditionally, even on the
    /// reject path - a rejected duplicate call would then clobber the cancel
    /// flag of whichever run is *actually* in progress, silently undoing a
    /// real pending Stop request for it. The current order has its own, much
    /// narrower gap (a `cancel()` for this brand-new run landing in the
    /// few-instruction window between the swap and the reset, getting
    /// clobbered by it) - but that's two back-to-back atomic ops with
    /// nothing between them, versus the reject-path regression being real
    /// and always reachable. Kept as-is on purpose.
    pub fn begin_run(&self) -> Result<(), String> {
        if self.running.swap(true, Ordering::AcqRel) {
            return Err("a nest is already running".to_string());
        }
        self.cancel.store(false, Ordering::Relaxed);
        Ok(())
    }

    pub fn end_run(&self) {
        self.running.store(false, Ordering::Release);
    }

    /// The `should_cancel` hook `run_nest_with_progress` polls - handed out
    /// as an owned `Arc` clone so it can cross into the worker thread (and
    /// from there into rayon's, which is why the closure must be `Sync`).
    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }
}

/// Appends one line to a log file that survives across app restarts
/// (`paths::log_file`) - the UI's own console panel calls this for every
/// line it prints, so import/run/export/error/cancel history from a
/// previous session is still readable afterwards, not just while the window
/// is open. Delegates the actual write to
/// `nesting::benchmark_log::append_benchmark_line` rather than hand-rolling
/// another `OpenOptions`/`writeln!` pair - that helper already rotates the
/// file to `.old` past 5MB, which a hand-rolled version here would
/// otherwise have to duplicate (or, as a first pass of this once did,
/// simply lack, leaving the log to grow unbounded).
pub fn append_log(line: &str) -> Result<(), String> {
    nesting::benchmark_log::append_benchmark_line(&crate::paths::log_file()?, line);
    Ok(())
}

/// Persists the last-used nest config so a new session can start from
/// wherever the last one left off, instead of always resetting to the
/// hardcoded defaults. The UI calls this right before every nest run.
pub fn save_config(config: &NestConfigDto) -> Result<(), String> {
    let json = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(crate::paths::config_file()?, json).map_err(|e| e.to_string())
}

/// Loads whatever `save_config` last wrote, if anything - `Ok(None)` (not an
/// error) the first time the app ever runs, before any config has been saved.
pub fn load_config() -> Result<Option<NestConfigDto>, String> {
    let path = crate::paths::config_file()?;
    if !path.exists() {
        return Ok(None);
    }
    let json = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&json).map(Some).map_err(|e| e.to_string())
}

/// Matches `nesting::ga::is_better_nest`'s ordering exactly (fewer unplaced
/// first, then fewer sheets, then higher utilisation) - kept as its own tiny
/// copy here rather than reusing that function directly, since this compares
/// primitives extracted from a `BestResultDto`/`RunNestResponse` pair, not
/// two `nesting::placement::PlaceResult`s.
fn is_better_result(a_unplaced: usize, a_sheets: usize, a_util: f64, b_unplaced: usize, b_sheets: usize, b_util: f64) -> bool {
    if a_unplaced != b_unplaced {
        return a_unplaced < b_unplaced;
    }
    if a_sheets != b_sheets {
        return a_sheets < b_sheets;
    }
    a_util > b_util
}

/// Loads the best nest result saved across every run this app has ever
/// completed (see `save_best_result_if_better`) - the UI calls this once on
/// startup to offer "recover last session's best, or start fresh".
/// `Ok(None)` (not an error) if nothing's been saved yet.
pub fn load_best_result() -> Result<Option<BestResultDto>, String> {
    let path = crate::paths::best_result_file()?;
    if !path.exists() {
        return Ok(None);
    }
    let json = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&json).map(Some).map_err(|e| e.to_string())
}

/// Erases the saved best-result file - "start fresh" on the recover-prompt
/// `load_best_result` triggers. A no-op (not an error) if nothing was ever
/// saved.
pub fn clear_best_result() -> Result<(), String> {
    match std::fs::remove_file(crate::paths::best_result_file()?) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Writes `candidate` over the saved best result, but only if it actually
/// beats what's already there (`is_better_result`). Called on the worker
/// thread after a successful run; the caller deliberately treats a failure
/// here as non-fatal - the UI already has the response regardless - but logs
/// rather than silently swallowing it.
pub fn save_best_result_if_better(candidate: &BestResultDto) -> Result<(), String> {
    let path = crate::paths::best_result_file()?;
    let existing: Option<BestResultDto> = std::fs::read_to_string(&path).ok().and_then(|json| serde_json::from_str(&json).ok());
    let should_write = match &existing {
        None => true,
        Some(prev) => is_better_result(
            candidate.unplaced_count,
            candidate.placements.len(),
            candidate.utilisation,
            prev.unplaced_count,
            prev.placements.len(),
            prev.utilisation,
        ),
    };
    if should_write {
        let json = serde_json::to_string_pretty(candidate).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Reads a DXF file from disk and returns its closed profiles as a
/// parent/hole tree (`geometry::dxf_import::build_polygon_tree`) - the
/// frontend is expected to turn these into `PartDto`s (assigning quantities)
/// for a later `run_nest` call, or into sheets directly.
///
/// `TEXT`/`MTEXT` entities (part labels, engraved numbers, etc.) have no
/// closed boundary of their own, so they don't become tree nodes - they're
/// attached to whichever profile contains them (`attach_texts`) and ride
/// along in that node's own `texts`, surviving rotation/placement/export the
/// same way a hole does. See `geometry::dxf_import`'s module doc comment.
pub fn import_dxf(path: &str, curve_tolerance: f64) -> Result<Vec<PolygonDto>, String> {
    let drawing = Drawing::load_file(path).map_err(|e| format!("couldn't parse {path} as DXF: {e}"))?;

    // `expand_inserts` first: a block reference carries only a block *name*,
    // and block bodies aren't in `drawing.entities()` at all, so anything
    // drawn inside a block is invisible to every pass below until it's been
    // expanded into real model-space geometry. A drawing with no blocks comes
    // back from this unchanged.
    let entities = geometry::dxf_import::expand_inserts(&drawing, curve_tolerance);
    // `_chained`, not `entities_to_polygons`: a profile drawn as loose
    // LINE/ARC segments (or several open polylines meeting end to end) is only
    // a closed shape once those pieces are walked in order - see that
    // function's doc comment.
    let flat = geometry::dxf_import::entities_to_polygons_chained(entities.iter(), curve_tolerance);
    let texts = geometry::dxf_import::entities_to_texts(entities.iter());
    let mut tree = geometry::dxf_import::build_polygon_tree(flat);
    geometry::dxf_import::attach_texts(&mut tree, texts);

    Ok(tree.iter().map(PolygonDto::from).collect())
}

/// Reads an SVG file from disk and returns its closed profiles as a
/// parent/hole tree, in exactly the same `PolygonDto` shape `import_dxf`
/// returns - the frontend treats the two import paths identically past this
/// point. DXF stays the primary/first-class import path (raw file units, no
/// conversion at all); SVG import additionally resolves the file's
/// coordinate system into millimeters and rejects imperial units outright -
/// see `geometry::svg_import`'s module doc for exactly which units convert
/// and which don't.
///
/// `unit_override` (`"mm"`/`"cm"`/`"m"`/`"px"`) is what the frontend's
/// per-import unit-picker dialog sends - a real SVG's `width`/`height`
/// often isn't a trustworthy physical size (many design tools export
/// unitless/arbitrary user units), so the UI asks the user to confirm or
/// override it on every SVG import rather than trusting auto-detection
/// silently. `None` falls back to `geometry::svg_import`'s own
/// `viewBox`/`width`/`height` auto-detection. No `TEXT`/`MTEXT`-equivalent
/// attachment (SVG import doesn't support text elements yet, see that
/// module's doc for scope).
pub fn import_svg(path: &str, curve_tolerance: f64, unit_override: Option<&str>) -> Result<Vec<PolygonDto>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("couldn't read {path}: {e}"))?;
    let flat = geometry::svg_import::parse_svg(&text, curve_tolerance, unit_override)?;
    let tree = geometry::dxf_import::build_polygon_tree(flat);
    Ok(tree.iter().map(PolygonDto::from).collect())
}

/// Builds the `Vec<SheetLayout>` both `export_dxf`/`export_svg` write out -
/// shared so the "resolve placements against parts_by_id" logic (and the
/// id-integrity guarantees below) exist in exactly one place, not
/// duplicated per format.
///
/// Deliberately takes `parts_by_id` straight from `RunNestResponse`, not a
/// `parts`/quantity list to re-run `expand_parts` on: that id assignment is
/// a plain sequential counter over caller-supplied input order, so re-
/// deriving it from a second, client-resent copy is only ever correct if
/// that copy exactly matches what actually produced `placements`' ids - and
/// nothing enforces that. A mismatch there wouldn't error; `parts_by_id.get(&p.id)`
/// would still resolve to *some* entry, silently writing the wrong part's
/// outline at a placement's coordinates.
///
/// `request.include_unplaced`: `parts_by_id` covers every part copy from
/// the run, placed or not (see `ExportRequest::parts_by_id`'s own doc
/// comment); the loop below removes each id `placements` actually
/// references, so whatever's left in the map afterward *is* the unplaced
/// set, with no separate `unplaced_ids` field needed on the request at all.
/// When requested, that leftover set gets packed (`geometry::dxf_export::
/// pack_unplaced_parts`) and appended as one more `SheetLayout`, which both
/// exporters' own left-to-right multi-sheet layout then places automatically
/// after the last real sheet - no format-specific handling needed for it.
fn build_export_layouts(request: ExportRequest) -> Result<Vec<SheetLayout>, String> {
    if request.sheet_spacing < 0.0 {
        return Err("sheet spacing must be >= 0".into());
    }

    let true_sheets: Vec<LayeredPolygon> = request.sheets.into_iter().map(Into::into).collect();
    let mut parts_by_id: HashMap<usize, LayeredPolygon> = request.parts_by_id.into_iter().map(|(id, dto)| (id, dto.into())).collect();

    let mut layouts: Vec<SheetLayout> = request
        .placements
        .into_iter()
        .map(|sp| {
            let sheet = true_sheets.get(sp.sheet_index).cloned().ok_or_else(|| format!("placement references sheet_index {} out of range", sp.sheet_index))?;
            let parts = sp
                .parts
                .into_iter()
                .map(|p| {
                    // `.remove`, not `.get().cloned()`: every real id
                    // appears in exactly one placement, so taking ownership
                    // here is free (no clone) and, as a bonus, turns an
                    // accidental duplicate placement id into a hard
                    // "unknown part id" error (the second occurrence finds
                    // it already removed) instead of silently succeeding
                    // twice. It's also what makes the leftover-map trick
                    // above work: only ids never referenced by any
                    // placement survive this loop.
                    let shape = parts_by_id.remove(&p.id).ok_or_else(|| format!("placement references unknown part id {}", p.id))?;
                    Ok(PlacedShape { shape, x: p.x, y: p.y, rotation: p.rotation })
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(SheetLayout { sheet, parts })
        })
        .collect::<Result<Vec<_>, String>>()?;

    if request.include_unplaced && !parts_by_id.is_empty() {
        // Sorted by id for deterministic, reproducible output - a
        // `HashMap`'s iteration order isn't stable across runs, and nothing
        // about "which never-placed part ends up in which packed row"
        // should vary between two exports of the same result.
        let mut remaining: Vec<(usize, LayeredPolygon)> = parts_by_id.into_iter().collect();
        remaining.sort_by_key(|(id, _)| *id);
        let shapes: Vec<LayeredPolygon> = remaining.into_iter().map(|(_, shape)| shape).collect();
        layouts.push(geometry::dxf_export::pack_unplaced_parts(&shapes, request.sheet_spacing));
    }

    Ok(layouts)
}

/// Writes the given nest result back out to a DXF file at `path` - new
/// scope, not a port (the original app never wrote DXF locally at all).
/// Takes exactly what the frontend already has after a `run_nest_command`
/// call (`request.sheets` for the *true*, unpadded geometry - export never
/// uses the internally padded shapes `run_nest` builds - `response.
/// parts_by_id`, and that same call's `response.placements`) rather than
/// re-deriving anything server-side. See `build_export_layouts` for the
/// shared placement-resolution/unplaced-packing logic.
pub fn export_dxf(path: &str, request: ExportRequest) -> Result<(), String> {
    let sheet_spacing = request.sheet_spacing;
    let include_sheet_outline = request.include_sheet_outline;
    let layouts = build_export_layouts(request)?;

    let drawing = geometry::dxf_export::export_dxf(&layouts, sheet_spacing, include_sheet_outline);
    drawing.save_file(path).map_err(|e| format!("couldn't write {path}: {e}"))
}

/// Writes the given nest result back out to an SVG file at `path` - the SVG
/// counterpart to `export_dxf`, same input shape (`ExportRequest`), same
/// `build_export_layouts` call, only the on-disk format differs
/// (`geometry::svg_export::export_svg`). See that module's doc comment for
/// what's scoped out of SVG export (no text, circles round-trip as
/// polygons) relative to DXF export.
pub fn export_svg(path: &str, request: ExportRequest) -> Result<(), String> {
    let sheet_spacing = request.sheet_spacing;
    let include_sheet_outline = request.include_sheet_outline;
    let layouts = build_export_layouts(request)?;

    let svg = geometry::svg_export::export_svg(&layouts, sheet_spacing, include_sheet_outline);
    std::fs::write(path, svg).map_err(|e| format!("couldn't write {path}: {e}"))
}

/// The manual, click-a-sheet counterpart to `run_nest_with_progress`'s
/// automatic `cleanup_threshold_percent` pass - both backed by the same
/// `nesting::repack::repack_sheet`. Takes just one sheet's worth of state
/// (not a full `RunNestRequest`) since that's all a single-sheet repack
/// needs; `request.config` is reused verbatim rather than a separate
/// "repack settings" struct, matching the "same rights/techniques as the
/// first nest" requirement this feature was built around.
pub fn repack_sheet(request: RepackSheetRequest) -> Result<RepackSheetResponse, String> {
    if request.placement.parts.is_empty() {
        return Err("sheet has no parts to repack".into());
    }
    validate_nest_config(&request.config)?;
    let margin = request.config.margin;
    let spacing = request.config.spacing;

    let true_sheet: LayeredPolygon = request.sheet.into();
    let sheet_points = prepare_sheet(&true_sheet.points, margin, spacing).ok_or("margin/spacing leaves the sheet with no usable area")?;
    let sheet = LayeredPolygon { points: sheet_points, real_boundary: None, ..true_sheet };

    let parts_by_id: HashMap<usize, LayeredPolygon> = request
        .parts_by_id
        .into_iter()
        .map(|(id, dto)| {
            let poly: LayeredPolygon = dto.into();
            let points = prepare_part(&poly.points, spacing).ok_or("spacing leaves a part with no usable outline")?;
            Ok((id, LayeredPolygon { points, real_boundary: None, ..poly }))
        })
        .collect::<Result<_, &str>>()?;

    // A one-off manual repack has no run-wide source_id grouping to reuse -
    // each id stands for itself (fine: repack_sheet gets a fresh NfpCache
    // per call regardless, so there's no cross-run cache benefit being left
    // on the table by not threading the original run's shape_ids through).
    let shape_ids: HashMap<usize, usize> = request.placement.parts.iter().map(|p| (p.id, p.id)).collect();

    // `sheet` above is always a *local*, single-sheet slice from here on
    // (`std::slice::from_ref(&sheet)`) - `recompute_totals` indexes its
    // `sheets` argument by `entry.sheet_index`, so `current`/`repacked` must
    // carry index 0 for every call below, not the real (possibly large)
    // sheet index the frontend sent. The real index is restored onto the
    // response's `placement` right before returning - it's response
    // metadata, never an index into anything in this function.
    let real_sheet_index = request.placement.sheet_index;
    let current = nesting::placement::SheetPlacement {
        sheet_index: 0,
        parts: request
            .placement
            .parts
            .iter()
            .map(|p| nesting::placement::PlacedPart { id: p.id, placement: nesting::placement::Placement { x: p.x, y: p.y }, rotation: p.rotation })
            .collect(),
    };

    let original_totals = recompute_totals(std::slice::from_ref(&current), &parts_by_id, std::slice::from_ref(&sheet));

    // GravityTightFit, not whatever placement_type the main run used, and
    // not plain Gravity either: a repack's whole point is tightening up a
    // single sheet. Plain Gravity only scores the *overall bounding
    // envelope* of everything placed so far (width*5+height) - it has no
    // notion of "touching a neighbor" at all, so multiple candidate
    // positions that happen to produce the same envelope score exactly the
    // same, and the search picks among them with no preference for closing
    // gaps. That's visibly bad for plain rectangles specifically, which tie
    // on envelope score constantly. GravityTightFit keeps the same
    // gravity-driven envelope scoring to decide the general area/side
    // (`find_best_hybrid_candidate`'s champion pick), but breaks ties
    // between equally-good envelope candidates by real contact area
    // instead of arbitrarily - so among positions that tie on compactness,
    // it always prefers the one that actually hugs its neighbors. Every
    // other config value (rotations, dominant area, tolerance, GA params)
    // still comes from the user's real config, only the scoring strategy
    // changes.
    let part_rules: nesting::placement::PartRules =
        std::sync::Arc::new(request.part_rules.iter().map(|(&id, rule)| (id, rule.clone().into())).collect());
    let repack_placement_config =
        PlacementConfig { placement_type: PlacementType::GravityTightFit, part_rules: part_rules.clone(), ..request.config.placement_config() };

    // Same "0 means uncapped, otherwise a scoped pool for this call" pattern
    // run_nest_with_progress uses for the main escalation loop - without
    // this, request.config.max_threads was silently ignored here (this
    // command never read it at all), since repack::repack_sheet's own
    // dispatch::run dispatches its GA generations via rayon::par_iter,
    // which runs on rayon's uncapped global pool absent an explicit scope.
    let pool = if request.config.max_threads > 0 {
        Some(
            rayon::ThreadPoolBuilder::new()
                .num_threads(request.config.max_threads)
                .build()
                .map_err(|e| format!("couldn't build a {}-thread pool: {e}", request.config.max_threads))?,
        )
    } else {
        None
    };
    // Pinned parts, straight off the placement the frontend sent - lock state
    // rides on the placement model itself rather than as a separate list, so
    // it can't drift from the geometry it describes.
    let locked: Vec<usize> = request.placement.parts.iter().filter(|p| p.locked).map(|p| p.id).collect();
    let run_repack = || {
        repack::repack_sheet(
            &sheet,
            &current,
            &parts_by_id,
            &shape_ids,
            &GaConfig { part_rules: part_rules.clone(), ..request.config.ga_config() },
            &repack_placement_config,
            request.config.generations,
            request.config.seed,
            &locked,
            &|| false,
        )
    };

    match match &pool {
        Some(p) => p.install(run_repack),
        None => run_repack(),
    } {
        Some(mut repacked) => {
            let totals = recompute_totals(std::slice::from_ref(&repacked), &parts_by_id, std::slice::from_ref(&sheet));
            repacked.sheet_index = real_sheet_index;
            let mut placement = to_placements_dto(vec![repacked]).remove(0);
            // `nesting`'s own `PlacedPart` has no lock concept - locking is a
            // UI-level intent, not something the engine reasons about - so
            // the flags are re-applied here rather than threaded through the
            // engine and back.
            for part in &mut placement.parts {
                part.locked = locked.contains(&part.id);
            }
            Ok(RepackSheetResponse { placement, improved: true, utilisation: totals.utilisation })
        }
        None => Ok(RepackSheetResponse { placement: request.placement, improved: false, utilisation: original_totals.utilisation }),
    }
}

/// Runs `request.config.generations` GA generations against
/// `request.sheets`/`request.parts` and returns the best result found
/// (`nesting::ga::is_better_nest`, not raw fitness - see its doc comment for
/// why those can rank differently). Every part-shape/quantity pair is
/// expanded into individually-id'd physical copies first
/// (`dto::expand_parts`), same as the original's `launchWorkers` building
/// its GA seed population.
// Only the tests below call this directly (the real `run_nest_command`
// uses `run_nest_with_progress` to get per-generation events) - gated to
// test builds instead of carrying an unused production entry point.
#[cfg(test)]
pub fn run_nest(request: RunNestRequest) -> Result<RunNestResponse, String> {
    run_nest_with_progress(request, |_, _, _| {}, || false, |_, _, _| {}, |_| {}, |_| {})
}

/// Everything `run_nest_with_progress` and `run_nest_live_preview` both need
/// before they diverge: validated, padded sheets/parts and the placement
/// config to run against. Kept as its own struct/function (not inlined
/// twice) so the ~15 validation checks below and the sheet/part padding
/// logic have exactly one place they can go stale, not two.
struct PreparedNestInputs {
    sheets: Vec<LayeredPolygon>,
    /// Padded (via `geometry::clearance::prepare_part`) - what the engine
    /// actually places against.
    parts_by_id: HashMap<usize, LayeredPolygon>,
    /// True, unpadded geometry - what `RunNestResponse::parts_by_id` reports
    /// back to the caller.
    parts_by_id_dto: HashMap<usize, PolygonDto>,
    shape_ids: HashMap<usize, usize>,
    adam: Vec<usize>,
    placement_config: nesting::placement::PlacementConfig,
    /// Per-part orientation constraints, also assigned onto
    /// `placement_config` above - carried separately too because the GA
    /// config is built by the caller, not here.
    part_rules: nesting::placement::PartRules,
}

/// Checks shared by every entry point that builds a `GaConfig`/
/// `PlacementConfig` from a `NestConfigDto` and feeds padded geometry to
/// `geometry::clearance` - both the main escalating run
/// (`prepare_nest_inputs`) and a single-sheet repack (`repack_sheet`).
/// `rotations`/`population_size`/`generations` guard real panic paths
/// (`GeneticAlgorithm::new`'s `random_angles`/first-`generation()` call);
/// `margin`/`spacing`/`mutation_rate`/`curve_tolerance`/
/// `dominant_part_area_threshold` don't panic but silently produce nonsense
/// GA/placement behavior (or, for `margin`/`spacing`/`curve_tolerance`, feed
/// an unvalidated negative value straight through the Clipper2 FFI
/// boundary) with no feedback at all otherwise. `runs`/
/// `cleanup_threshold_percent` are validated separately, only in
/// `prepare_nest_inputs` - `repack_sheet` never reads either field. Upper
/// bounds are generous, deliberately-round sanity ceilings (not tuned
/// limits) - just enough to stop a fat-fingered config from pinning a CPU
/// core on an effectively-unkillable job before the user notices there's a
/// Stop button to press.
fn validate_nest_config(config: &NestConfigDto) -> Result<(), String> {
    if config.rotations == 0 || config.rotations > 360 {
        return Err("rotations must be between 1 and 360".into());
    }
    if !(2..=1000).contains(&config.population_size) {
        return Err("population_size must be between 2 and 1000".into());
    }
    if config.generations == 0 || config.generations > 10_000 {
        return Err("generations must be between 1 and 10000".into());
    }
    if config.max_threads > 256 {
        return Err("max_threads must be 256 or less (0 means uncapped)".into());
    }
    if config.margin < 0.0 {
        return Err("margin must be >= 0".into());
    }
    if config.spacing < 0.0 {
        return Err("spacing must be >= 0".into());
    }
    // Bounds match what `index.html`'s own inputs already constrain
    // client-side (`min`/`max` on `cfg-mutation`/`import-tolerance`/`cfg-dominant`).
    if !(0.0..=100.0).contains(&config.mutation_rate) {
        return Err("mutation_rate must be between 0 and 100".into());
    }
    if config.curve_tolerance <= 0.0 {
        return Err("curve_tolerance must be > 0".into());
    }
    if !(config.dominant_part_area_threshold > 0.0 && config.dominant_part_area_threshold <= 1.0) {
        return Err("dominant_part_area_threshold must be between 0 (exclusive) and 1".into());
    }
    Ok(())
}

/// Answers "may this part sit here?" for the result view's drag-a-part
/// interaction. Same `sheet`/`parts_by_id`/`config` shape a repack takes,
/// plus which part moved and where to.
///
/// Deliberately a round trip to the engine rather than a geometry check in
/// the frontend: overlap has to be judged against the *padded* geometry the
/// nest itself uses (margin/spacing), on the real polygon tree including
/// holes, by the same `has_material_overlap`/`has_material_outside_sheet`
/// pair that accepts or rejects an engine-placed candidate. A JS
/// approximation would disagree with the engine at exactly the interesting
/// moments, and would have to be rewritten again when the frontend becomes
/// Rust.
pub fn validate_placement(request: ValidatePlacementRequest) -> Result<ValidatePlacementResponse, String> {
    validate_nest_config(&request.config)?;
    let margin = request.config.margin;
    let spacing = request.config.spacing;

    let true_sheet: LayeredPolygon = request.sheet.into();
    let sheet_points = prepare_sheet(&true_sheet.points, margin, spacing).ok_or("margin/spacing leaves the sheet with no usable area")?;
    let sheet = LayeredPolygon { points: sheet_points, real_boundary: None, ..true_sheet };

    let pad = |dto: PolygonDto| -> Result<LayeredPolygon, String> {
        let poly: LayeredPolygon = dto.into();
        let points = prepare_part(&poly.points, spacing).ok_or_else(|| "spacing leaves a part with no usable outline".to_string())?;
        Ok(LayeredPolygon { points, real_boundary: None, ..poly })
    };

    let mut parts_by_id: HashMap<usize, LayeredPolygon> = HashMap::new();
    for (id, dto) in request.parts_by_id {
        parts_by_id.insert(id, pad(dto)?);
    }

    let moved = parts_by_id.get(&request.moved_id).ok_or_else(|| format!("unknown part id {}", request.moved_id))?;
    let moved = rotate_layered_polygon(moved, request.rotation);

    // Everything else on the sheet, at its own current position - the part
    // being dragged is not an obstacle to itself.
    let others: Vec<nesting::placement::PlacedObstacle> = request
        .placement
        .parts
        .iter()
        .filter(|p| p.id != request.moved_id)
        .filter_map(|p| {
            parts_by_id.get(&p.id).map(|geometry| nesting::placement::PlacedObstacle {
                polygon: rotate_layered_polygon(geometry, p.rotation),
                id: p.id,
                source_id: p.id,
                rotation: p.rotation,
                placement: nesting::placement::Placement { x: p.x, y: p.y },
            })
        })
        .collect();

    let valid = nesting::placement::placement_is_valid(&sheet, &moved, nesting::placement::Placement { x: request.x, y: request.y }, &others);
    Ok(ValidatePlacementResponse { valid })
}

/// Loads the saved parts library and remnant shelf.
///
/// A corrupt or unreadable store returns `Err`, and the caller is expected to
/// carry on with an empty library *and say so* rather than treating it as an
/// empty one. Silently starting blank is how someone loses a library they
/// spent months building without ever noticing it happened.
pub fn load_shape_store() -> Result<crate::dto::ShapeStore, String> {
    let path = crate::paths::shape_store_file()?;
    if !path.exists() {
        return Ok(crate::dto::ShapeStore::default());
    }
    let json = std::fs::read_to_string(&path).map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
    serde_json::from_str(&json).map_err(|e| format!("{} is not a readable shape store: {e}", path.display()))
}

/// Writes the store, atomically.
///
/// Temp file plus rename, unlike `save_config`'s plain write: a config lost
/// to a half-written file costs the user a few settings they can retype, but
/// this file *is* their saved work. A crash mid-write must leave the previous
/// version intact rather than a truncated one.
pub fn save_shape_store(store: &crate::dto::ShapeStore) -> Result<(), String> {
    let path = crate::paths::shape_store_file()?;
    let temp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    std::fs::write(&temp, json).map_err(|e| format!("couldn't write {}: {e}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|e| format!("couldn't replace {}: {e}", path.display()))
}

/// Computes the reusable offcuts of a finished nest.
///
/// Everything geometric is `geometry::remnant`; this resolves ids to shapes
/// and applies the same `spacing` the nest ran with, so an offcut can never
/// include material that has to stay attached to a part for clearance.
///
/// Deliberately computed for *every* sheet, not only the last one. A job that
/// leaves four sheets half-empty has four offcuts, and only reporting the
/// final one would quietly throw the other three away - which is the waste
/// this whole feature exists to stop.
pub fn compute_remnants(request: crate::dto::RemnantRequest) -> Result<Vec<crate::dto::RemnantDto>, String> {
    validate_nest_config(&request.config)?;
    let spacing = request.config.spacing;

    let parts: HashMap<usize, LayeredPolygon> = request.parts_by_id.into_iter().map(|(id, dto)| (id, dto.into())).collect();

    let mut out = Vec::new();
    for placement in &request.placements {
        let Some(sheet_dto) = request.sheets.get(placement.sheet_index) else { continue };
        let sheet: LayeredPolygon = sheet_dto.clone().into();

        // Outer outlines only - a part's holes are not free material. A
        // drilled hole belongs to a piece someone is going to pick up, and
        // treating it as reclaimable would produce offcuts that physically
        // fall out of the sheet.
        let placed: Vec<Vec<geometry::point::Point>> = placement
            .parts
            .iter()
            .filter_map(|p| {
                parts.get(&p.id).map(|poly| {
                    let rotated = rotate_layered_polygon(poly, p.rotation);
                    rotated.points.iter().map(|pt| geometry::point::Point::new(pt.x + p.x, pt.y + p.y)).collect()
                })
            })
            .collect();

        for remnant in geometry::remnant::sheet_remnants(&sheet.points, &placed, spacing) {
            let polygon = LayeredPolygon {
                points: remnant.outline,
                layer: sheet.layer.clone(),
                is_circle: None,
                children: Vec::new(),
                texts: Vec::new(),
                real_boundary: None,
            };
            out.push(crate::dto::RemnantDto {
                sheet_index: placement.sheet_index,
                polygon: (&polygon).into(),
                area: remnant.area,
                usable_width: remnant.usable.width,
                usable_height: remnant.usable.height,
            });
        }
    }
    // Biggest first: the useful ones should be at the top of any list this
    // ends up in.
    out.sort_by(|a, b| b.area.total_cmp(&a.area));
    Ok(out)
}

/// Checks a whole nest result for manufacturability: overlapping parts,
/// parts off the sheet, and clearances below what was asked for.
///
/// The engine validates each part as it places it, but two things can change
/// a result afterwards - `repack_sheet` and the UI's drag - and neither
/// re-checks the sheet as a whole. So without this, the arrangement a user
/// exports is not one anything has ever validated end to end.
///
/// Everything geometric lives in `nesting::audit`; this is only the boundary
/// work - resolve ids to shapes, and build both the true and the padded
/// outline of each part through the *same* `prepare_sheet`/`prepare_part`
/// calls `run_nest` used. That last point is what stops the audit from being
/// a second opinion that can disagree with the nest on a technicality: it
/// checks the clearances the nest was actually generated under.
pub fn audit_nest(request: crate::dto::AuditRequest) -> Result<crate::dto::AuditReportDto, String> {
    use nesting::audit::{audit, AuditPart, AuditSheet};

    validate_nest_config(&request.config)?;
    let (margin, spacing) = (request.config.margin, request.config.spacing);

    // Each part resolved once, into the pair the audit needs. A part id can
    // appear on several sheets (different copies of the same shape), so doing
    // this per placement would redo the Clipper offset for every copy.
    let mut resolved: HashMap<usize, (LayeredPolygon, LayeredPolygon)> = HashMap::new();
    for (id, dto) in request.parts_by_id {
        let poly: LayeredPolygon = dto.into();
        let padded_points = prepare_part(&poly.points, spacing).ok_or_else(|| format!("spacing leaves part {id} with no usable outline"))?;
        let padded = LayeredPolygon { points: padded_points, real_boundary: None, ..poly.clone() };
        resolved.insert(id, (poly, padded));
    }

    let mut sheets = Vec::with_capacity(request.placements.len());
    for placement in &request.placements {
        let dto = request.sheets.get(placement.sheet_index).ok_or_else(|| format!("placement references sheet {} which wasn't supplied", placement.sheet_index))?;
        let outline: LayeredPolygon = dto.clone().into();
        let usable_points = prepare_sheet(&outline.points, margin, spacing).ok_or("margin/spacing leaves the sheet with no usable area")?;
        let usable = LayeredPolygon { points: usable_points, real_boundary: None, ..outline.clone() };

        // A placement naming an id we weren't given is skipped rather than
        // failing the whole audit: the alternative is that one stale id makes
        // the check unavailable exactly when someone wants reassurance.
        let parts = placement
            .parts
            .iter()
            .filter_map(|p| {
                resolved.get(&p.id).map(|(outline, padded)| {
                    let rotated = rotate_layered_polygon(outline, p.rotation);
                    let rotated_padded = rotate_layered_polygon(padded, p.rotation);
                    AuditPart::placed(p.id, &rotated, &rotated_padded, p.x, p.y)
                })
            })
            .collect();

        sheets.push(AuditSheet { outline, usable, parts });
    }

    Ok((&audit(&sheets)).into())
}

/// Turns an audit result into the block the PDF prints.
///
/// An audit that failed to *run* prints as an explicit "could not be checked"
/// rather than being omitted: a missing section reads as "nothing to report",
/// which is exactly the wrong thing to infer.
///
/// The issue list is capped - a badly broken nest can produce hundreds, and a
/// summary page that becomes a fault listing stops being a summary.
fn report_audit(result: Result<crate::dto::AuditReportDto, String>) -> Option<geometry::pdf_export::ReportAudit> {
    const MAX_LISTED: usize = 12;
    let report = match result {
        Ok(report) => report,
        Err(e) => {
            return Some(geometry::pdf_export::ReportAudit {
                passed: false,
                headline: format!("NOT CHECKED - the manufacturability check could not run ({e})"),
                issues: Vec::new(),
            })
        }
    };

    let headline = if !report.passed {
        format!("FAILED - {} fatal issue(s), {} warning(s). DO NOT CUT.", report.fatal_count, report.warning_count)
    } else if report.warning_count > 0 {
        format!("PASSED with {} warning(s) - cuttable, but not exactly as configured.", report.warning_count)
    } else {
        "PASSED - no overlaps, every piece on its sheet, all clearances met.".to_string()
    };

    let issues = report
        .issues
        .iter()
        .take(MAX_LISTED)
        .map(|i| {
            let kind = match i.kind.as_str() {
                "overlap" => "OVERLAP",
                "outside_sheet" => "OFF THE SHEET",
                "below_spacing" => "TOO CLOSE",
                "outside_margin" => "INSIDE MARGIN",
                other => other,
            };
            let ids = i.part_ids.iter().map(|id| format!("#{id}")).collect::<Vec<_>>().join(" + ");
            format!("{kind} - sheet {}, {ids}", i.sheet_index + 1)
        })
        .chain((report.issues.len() > MAX_LISTED).then(|| format!("...and {} more", report.issues.len() - MAX_LISTED)))
        .collect();

    Some(geometry::pdf_export::ReportAudit { passed: report.passed, headline, issues })
}

/// Writes the PDF job report: a summary page plus one to-scale page per
/// sheet. Reuses `build_export_layouts` verbatim, so the report draws
/// exactly what a DXF/SVG export of the same result would contain.
///
/// `include_unplaced` is forced off - never-placed parts belong in the
/// summary's "not placed" count, not packed into a fake extra sheet page.
pub fn export_report(path: &str, request: ReportRequest) -> Result<(), String> {
    let mut export = request.export;
    export.include_unplaced = false;

    // Run the audit here, from the same inputs, rather than accepting a
    // verdict from the caller: a passed-in result could describe an
    // arrangement edited since it was computed, and a report that certifies
    // the wrong nest is worse than one that certifies nothing. Same reasoning
    // the pdf module already applies to its derived numbers - "the printed
    // numbers can never disagree with the printed picture".
    let audit = audit_nest(crate::dto::AuditRequest {
        sheets: export.sheets.clone(),
        placements: export.placements.clone(),
        parts_by_id: export.parts_by_id.clone(),
        config: request.config.clone(),
    });

    let layouts = build_export_layouts(export)?;
    let config = &request.config;
    let meta = geometry::pdf_export::ReportMeta {
        title: request.title.unwrap_or_else(|| "Nesting job report".to_string()),
        parts: request.parts.iter().map(|p| geometry::pdf_export::ReportPart { name: p.name.clone(), quantity: p.quantity }).collect(),
        settings: vec![
            ("Margin".to_string(), format!("{} mm", config.margin)),
            ("Spacing".to_string(), format!("{} mm", config.spacing)),
            ("Runs".to_string(), config.runs.to_string()),
            ("Starting rotations".to_string(), config.rotations.to_string()),
            ("Mirroring".to_string(), if config.mirror { "allowed" } else { "off" }.to_string()),
            ("Curve tolerance".to_string(), format!("{} mm", config.curve_tolerance)),
            ("Seed".to_string(), config.seed.to_string()),
        ],
        audit: report_audit(audit),
    };

    let bytes = geometry::pdf_export::export_report(&layouts, &meta);
    std::fs::write(path, bytes).map_err(|e| format!("couldn't write {path}: {e}"))
}

/// Validates `request` and builds the padded sheets/parts both nest-running
/// paths place against - see `PreparedNestInputs`'s own doc comment for why
/// this is shared rather than duplicated. A pure extraction of what used to
/// be `run_nest_with_progress`'s own opening ~80 lines; behavior unchanged.
fn prepare_nest_inputs(request: RunNestRequest) -> Result<PreparedNestInputs, String> {
    if request.sheets.is_empty() {
        return Err("at least one sheet is required".into());
    }
    if request.parts.is_empty() {
        return Err("at least one part is required".into());
    }
    validate_nest_config(&request.config)?;
    if request.config.runs == 0 || request.config.runs > 50 {
        return Err("runs must be between 1 and 50".into());
    }
    if let Some(t) = request.config.cleanup_threshold_percent {
        if !(0.0..=100.0).contains(&t) {
            return Err("cleanup_threshold_percent must be between 0 and 100".into());
        }
    }
    let margin = request.config.margin;
    let spacing = request.config.spacing;

    // Padding is applied here, internally, purely to shape the placement
    // decisions the engine makes - see geometry::clearance's module doc for
    // the full derivation. The response only ever reports (id, x, y,
    // rotation), computed against this padded geometry but geometrically
    // valid for the caller's original (true, unpadded) shapes too, since
    // padding doesn't recenter a polygon - nothing padded is ever returned.
    let true_sheets: Vec<LayeredPolygon> = request.sheets.into_iter().map(Into::into).collect();
    let sheets: Vec<LayeredPolygon> = true_sheets
        .iter()
        .map(|sheet| {
            let points = prepare_sheet(&sheet.points, margin, spacing).ok_or("margin/spacing leaves a sheet with no usable area")?;
            Ok(LayeredPolygon { points, layer: sheet.layer.clone(), is_circle: sheet.is_circle, children: sheet.children.clone(), texts: sheet.texts.clone(), real_boundary: None })
        })
        .collect::<Result<_, &str>>()?;

    let ExpandedParts { adam, parts_by_id: true_parts_by_id, shape_ids, part_rules } = expand_parts(request.parts, request.config.mirror);
    if adam.is_empty() {
        return Err("every part had quantity 0".into());
    }
    // This is the authoritative id -> shape mapping `RunNestResponse::
    // parts_by_id` carries out, so a later `export_dxf_command` call never
    // has to re-derive it from a second, client-resent `parts` list (see
    // that DTO field's own doc comment).
    let parts_by_id_dto: HashMap<usize, PolygonDto> = true_parts_by_id.iter().map(|(&id, part)| (id, PolygonDto::from(part))).collect();
    let parts_by_id: HashMap<usize, LayeredPolygon> = true_parts_by_id
        .iter()
        .map(|(&id, part)| {
            let points = prepare_part(&part.points, spacing).ok_or("spacing leaves a part with no usable outline")?;
            Ok((id, LayeredPolygon { points, layer: part.layer.clone(), is_circle: part.is_circle, children: part.children.clone(), texts: part.texts.clone(), real_boundary: None }))
        })
        .collect::<Result<_, &str>>()?;

    // `part_rules` reaches the engine on the two configs that are already
    // threaded everywhere (placement and GA), rather than as a new parameter
    // on six functions - see `nesting::placement::PartRule`.
    let mut placement_config = request.config.placement_config();
    placement_config.part_rules = part_rules.clone();

    Ok(PreparedNestInputs { sheets, parts_by_id, parts_by_id_dto, shape_ids, adam, placement_config, part_rules })
}

/// Shared by `run_nest_with_progress` and `run_nest_live_preview` - both
/// end up with a `Vec<nesting::placement::SheetPlacement>` to hand back to
/// the frontend in the same `SheetPlacementDto` shape.
fn to_placements_dto(placements: Vec<nesting::placement::SheetPlacement>) -> Vec<SheetPlacementDto> {
    placements
        .into_iter()
        .map(|sp| SheetPlacementDto {
            sheet_index: sp.sheet_index,
            parts: sp.parts.into_iter().map(|p| PlacedPartDto { id: p.id, x: p.placement.x, y: p.placement.y, rotation: p.rotation, locked: false }).collect(),
        })
        .collect()
}

/// Auto-escalation step sizes for the "Runs" loop (see `NestConfigDto::runs`'s
/// own doc comment for the user-facing framing): each successive run tries
/// one more rotation angle than the last, plus a proportionally larger
/// population/generation budget so it can actually search that wider grid,
/// not just try more angles once with the same shallow search. Plain linear
/// growth, not anything self-tuning - simple and predictable beats clever
/// here; revisit with real multi-job benchmark data if it proves too
/// aggressive/conservative in practice.
const RUN_POPULATION_STEP: usize = 4;
const RUN_GENERATIONS_STEP: usize = 5;

/// This run's rotations/population_size/generations, escalated from
/// `request.config`'s own values (this escalation's *starting* point,
/// 0-indexed `run_index` away) per `RUN_POPULATION_STEP`/`RUN_GENERATIONS_STEP`
/// above.
fn escalated_run_config(base_ga_config: &GaConfig, base_generations: usize, run_index: usize, total_runs: usize) -> (GaConfig, usize) {
    let rotations = base_ga_config.rotations + run_index as u32;
    let ga_config = GaConfig {
        population_size: base_ga_config.population_size + run_index * RUN_POPULATION_STEP,
        mutation_rate: base_ga_config.mutation_rate,
        rotations,
        // With `mirror` on, run 1 is deliberately still run *without* it, so
        // the escalation always measures a real un-flipped baseline and
        // `is_better_nest` picks between the two. Mirroring only ever widens
        // the search space (every un-flipped arrangement is still reachable
        // in a mirrored run - half the rotation grid is the un-flipped half),
        // but a wider space searched on the same budget can genuinely land
        // worse, so "flip allowed" must not mean "flip-free never tried".
        // Unless there's only one run, where skipping it would mean the
        // setting silently does nothing.
        mirror: base_ga_config.mirror && (run_index > 0 || total_runs == 1),
        part_rules: base_ga_config.part_rules.clone(),
    };
    let generations = base_generations + run_index * RUN_GENERATIONS_STEP;
    (ga_config, generations)
}

/// Same as `run_nest`, but calls `on_progress(generation, total_generations,
/// best_so_far)` after every completed generation - the hook the
/// `run_nest_command` Tauri wrapper uses to `emit` a live "nest-progress"
/// event per generation, so the UI can show what's happening instead of
/// blocking silently until the whole run finishes. Plain `run_nest` (used by
/// every test below and any caller that doesn't care) is just this with
/// no-op hooks and a `should_cancel` that never fires.
///
/// Runs `request.config.runs` escalating attempts (see
/// `NestConfigDto::runs`'s own doc comment and `escalated_run_config` above),
/// keeping whichever one actually nests best across the whole sequence
/// (`nesting::ga::is_better_nest`, the same comparison a single run's own
/// generations already use) - not just the last one tried. `on_run_start`/
/// `on_run_complete` fire once per attempt (before/after its own generation
/// loop) so the UI can narrate the escalation instead of only ever seeing
/// per-generation detail with no sense of which attempt produced it.
/// `generation`/`history` numbering is a running counter across the *whole*
/// escalation, not reset to 1 each run - so `RunNestResponse::history`'s
/// entries stay uniquely labeled instead of colliding across runs.
///
/// `should_cancel` is checked once per generation and once between runs
/// (`run_nest_command` wires it to `NestCancelFlag`, set by
/// `cancel_nest_command`); when it returns true the whole escalation stops
/// after whatever generation just finished and the response reports
/// `cancelled: true` with the best result found so far across every attempt
/// up to that point, rather than erroring - a user-requested stop is a
/// normal outcome, not a failure.
///
/// `on_individual_placed(generation, done, total)` forwards
/// `nesting::dispatch::run_generation`'s own per-individual progress hook
/// (see its doc comment) - called once up front with `done: 0` before a
/// generation's individuals start, then again after each one finishes
/// placing. A single individual's placement can be real, tens-of-seconds
/// work against non-trivial geometry; without this, `on_progress` above is
/// the *only* signal, and it only fires once an entire generation
/// completes - which for a slow generation is indistinguishable from the
/// run having stalled.
///
/// Inlines the generation loop `nesting::dispatch::run` would otherwise do,
/// rather than adding a callback parameter to that function - `dispatch`'s
/// own doc comment already calls progress plumbing out as "left to whatever
/// wraps this loop", so this is that wrapper, not a fork of engine logic.
#[allow(clippy::too_many_arguments)]
pub fn run_nest_with_progress(
    request: RunNestRequest,
    mut on_progress: impl FnMut(usize, usize, &PlaceResult) + Send,
    should_cancel: impl Fn() -> bool + Sync + Send,
    on_individual_placed: impl Fn(usize, usize, usize) + Sync + Send,
    mut on_run_start: impl FnMut(&NestRunStartDto) + Send,
    mut on_run_complete: impl FnMut(&NestRunCompleteDto) + Send,
) -> Result<RunNestResponse, String> {
    // Read before `prepare_nest_inputs` consumes `request` - none of these
    // are needed by the shared validation/padding logic, only by the runs/GA
    // loop below.
    let max_threads = request.config.max_threads;
    let mut base_ga_config = request.config.ga_config();
    let base_generations = request.config.generations;
    let seed = request.config.seed;
    let total_runs = request.config.runs;
    let cleanup_threshold = request.config.cleanup_threshold_percent;

    let PreparedNestInputs { sheets, parts_by_id, parts_by_id_dto, shape_ids, adam, placement_config, part_rules } = prepare_nest_inputs(request)?;
    // Same map on both configs: a rotation *gene* and the *placement* it
    // produces must agree about what each part is allowed to do, or a
    // grain-locked part gets a legal gene and an illegal placement.
    base_ga_config.part_rules = part_rules;

    // One cache for the *whole* escalation - every run, every individual,
    // every generation - not a fresh one per run/generation/individual.
    // Different runs use different (and, for `rotations`, overlapping)
    // angle grids, so the same (part id, part id, rotation, rotation) NFP
    // recurring across runs is still a cache hit instead of a recompute;
    // see `nesting::placement::place_parts`'s own doc comment.
    let cache = NfpCache::new();
    let sheets_ref = &sheets;
    let parts_by_id_ref = &parts_by_id;
    let shape_ids_ref = &shape_ids;
    let cache_ref = &cache;

    // 0 (the default) means "no cap" - just use rayon's own global pool. A
    // cap builds one scoped pool for the *whole* escalation (not one per
    // run - rayon's global pool can only be configured once per process via
    // `build_global()`, which is exactly why this can't just be threads=0's
    // shared pool, but a fresh `ThreadPoolBuilder` still only needs building
    // once here, reused by every run's `pool.install` below).
    let pool = if max_threads > 0 {
        Some(rayon::ThreadPoolBuilder::new().num_threads(max_threads).build().map_err(|e| format!("couldn't build a {max_threads}-thread pool: {e}"))?)
    } else {
        None
    };

    // Port of `widenRotationsIfStalled`: if a single run's best hasn't
    // improved in a while, the search is more likely stuck on a rotation
    // grid too coarse to find a better fit than it is to benefit from trying
    // more of the same angles again - widen it. Doubling (not resizing to an
    // arbitrary count) is what keeps this safe alongside the shared
    // `NfpCache`: {0,90,180,270} is an exact subset of {0,45,90,...,315}, so
    // widening never invalidates NFPs already cached for the coarser
    // angles, only adds new ones to compute. Independent of (and reset
    // every) run - the outer runs loop already escalates rotations between
    // attempts; this only rescues one attempt that's stalled internally.
    //
    // `ROTATION_STAGNATION_LIMIT` no longer matches the original's constant
    // (was 10): a real benchmark session (24-combination grid sweep against
    // the `FLAT.dxf`/`FLAT-struck.dxf` fixtures, see `docs/PORT_STATUS.md`)
    // found `rotations=8` and up is a *strict downgrade* vs `rotations=4`
    // for this job's mostly-rectangular parts (102-103 sheets vs 100-101,
    // every combination tried) - consistent with the already-documented
    // rotation-angle-grid quirk. A live 300-generation run confirmed this
    // in practice: it landed at 102 sheets, worse than a plain
    // never-widened `rotations=4` run's 100, because stagnation-triggered
    // widening fired and pushed past the angle grid that's actually best
    // for this part mix. Raised from 10 to 60 so the mechanism still
    // rescues a genuinely stuck job given enough generations, but won't
    // trigger within a normal run on a job shaped like this one.
    const ROTATION_STAGNATION_LIMIT: usize = 60;
    const ROTATION_CAP: u32 = 32;

    let mut overall_best: Option<PlaceResult> = None;
    let mut overall_history: Vec<(usize, PlaceResult)> = Vec::new();
    let mut overall_cancelled = false;
    let mut final_placement_config = placement_config.clone();
    // Cumulative *generations actually run* across the whole escalation, not
    // `overall_history.len()` (a real bug this replaced: that was the count
    // of recorded *improvements*, which undercounts as soon as any run goes
    // more than one generation without a new best - the normal case once a
    // GA starts converging - producing colliding, non-monotonic labels).
    let mut generations_elapsed: usize = 0;

    'runs: for run_index in 0..total_runs {
        if should_cancel() {
            overall_cancelled = true;
            break;
        }
        let (run_ga_config, generations_for_run) = escalated_run_config(&base_ga_config, base_generations, run_index, total_runs);
        let mut run_placement_config = placement_config.clone();
        run_placement_config.rotations = run_ga_config.rotations;

        on_run_start(&NestRunStartDto {
            run: run_index + 1,
            total_runs,
            rotations: run_ga_config.rotations,
            population_size: run_ga_config.population_size,
            generations: generations_for_run,
        });

        let mut ga = GeneticAlgorithm::new(adam.clone(), run_ga_config.clone(), Vec::new(), seed);

        // Deliberately not a `move` closure: `on_progress`/`on_individual_placed`
        // (mutable/shared borrows of the outer function's own parameters) need
        // to be reusable across every run's closure, not consumed by the
        // first one - Rust's per-capture inference already picks the right
        // mode for each variable individually (`ga`/`run_placement_config` by
        // reference/move as their own usage below requires), `move` would
        // just force everything into an owned copy unnecessarily.
        let mut run_once = || {
            let mut placement_config = run_placement_config.clone();
            let mut best: Option<PlaceResult> = None;
            let mut history: Vec<(usize, PlaceResult)> = Vec::new();
            let mut cancelled = false;
            let mut generations_since_improvement: usize = 0;
            for generation_in_run in 1..=generations_for_run {
                if should_cancel() {
                    cancelled = true;
                    break;
                }
                // `should_cancel` is also passed down into `run_generation`
                // itself (not just checked here, between generations) - a
                // generation is a parallel per-individual placement pass
                // that can take a long time on its own, and without an
                // interior check a stop request would only ever take effect
                // at the boundary between whole generations.
                let results = dispatch::run_generation(&mut ga, sheets_ref, parts_by_id_ref, shape_ids_ref, &placement_config, &should_cancel, &|done, total| {
                    on_individual_placed(generation_in_run, done, total)
                }, cache_ref);
                let mut improved_this_generation = false;
                for evaluated in results {
                    if best.as_ref().is_none_or(|b| is_better_nest(&evaluated.result, b)) {
                        best = Some(evaluated.result.clone());
                        history.push((generation_in_run, evaluated.result));
                        improved_this_generation = true;
                    }
                }
                // Live per-generation progress, relative to *this* run
                // (resets each run) - simple and immediate, same shape the
                // single-run version always had. `on_run_start`/
                // `on_run_complete` (fired around this closure, not inside
                // it) are what tell the console which attempt this progress
                // belongs to.
                if let Some(so_far) = &best {
                    on_progress(generation_in_run, generations_for_run, so_far);
                }
                // Re-checked after the generation too: `run_generation` may
                // have been cut short mid-population by the same flag, in
                // which case this loop must stop here rather than starting
                // another generation on a population `run_generation`
                // deliberately left half-evaluated (see its own doc
                // comment).
                if should_cancel() {
                    cancelled = true;
                    break;
                }

                if improved_this_generation {
                    generations_since_improvement = 0;
                } else {
                    generations_since_improvement += 1;
                    if generations_since_improvement >= ROTATION_STAGNATION_LIMIT && placement_config.rotations < ROTATION_CAP {
                        placement_config.rotations = (placement_config.rotations * 2).min(ROTATION_CAP);
                        ga.set_rotations(placement_config.rotations);
                        generations_since_improvement = 0;
                    }
                }
            }
            (best, history, cancelled, placement_config)
        };

        let (run_best, run_history, run_cancelled, run_final_placement_config) = match &pool {
            Some(p) => p.install(run_once),
            None => run_once(),
        };

        // Whether *this run's own best* ends up beating every run before it -
        // computed against a snapshot of `overall_best` from before this
        // run's history is folded in, not re-derived from loop side effects
        // below (simpler to get right: `run_best`, if any, is always
        // `run_history`'s last/best entry by construction, so this is the
        // one comparison that matters for the "did this attempt pay off"
        // question `on_run_complete` reports).
        let improved = match (&run_best, &overall_best) {
            (Some(rb), Some(prev)) => is_better_nest(rb, prev),
            (Some(_), None) => true,
            (None, _) => false,
        };

        // History labels are a running count across the *whole* escalation
        // (not reset to 1 each run), so `RunNestResponse::history`'s entries
        // stay uniquely identified in the "VIEW ATTEMPT" dropdown instead of
        // colliding with an earlier run's same-numbered generation. Offset by
        // generations *elapsed*, not `overall_history.len()` - a run's own
        // `generation_in_run` numbering already runs 1..=generations_for_run
        // regardless of how many of those generations actually improved on
        // the running best, so the offset for the next run has to match that
        // same full count, not just how many entries got recorded.
        // Only entries that actually beat the *overall* best get pushed into
        // `overall_history` - a real bug this replaced: `run_history`'s own
        // entries are each other's local best (`run_once`'s `best` starts
        // fresh at `None` every run), which is not the same thing as
        // beating what an *earlier* run already achieved. Pushing every
        // local-history entry unconditionally meant a later run's first
        // individual - genuinely worse than an earlier run's result, but
        // still "an improvement" relative to that run's own fresh-starting
        // `None` baseline - showed up in `RunNestResponse::history` (the
        // "VIEW ATTEMPT" dropdown) looking like a legitimate later attempt,
        // even though it never should have counted as one.
        let generation_offset = generations_elapsed;
        for (generation_in_run, result) in run_history {
            if overall_best.as_ref().is_none_or(|b| is_better_nest(&result, b)) {
                overall_best = Some(result.clone());
                final_placement_config = run_final_placement_config.clone();
                overall_history.push((generation_offset + generation_in_run, result));
            }
        }
        generations_elapsed += generations_for_run;

        if let Some(run_best) = &run_best {
            on_run_complete(&NestRunCompleteDto {
                run: run_index + 1,
                total_runs,
                rotations: run_ga_config.rotations,
                population_size: run_ga_config.population_size,
                generations: generations_for_run,
                sheets_used: run_best.placements.len(),
                unplaced_count: run_best.unplaced_count,
                utilisation: run_best.utilisation,
                improved,
            });
        }

        if run_cancelled {
            overall_cancelled = true;
            break 'runs;
        }
    }

    let placement_config = final_placement_config;
    let history = overall_history;
    let cancelled = overall_cancelled;
    // `overall_best` is only ever `None` if no individual was ever placed in
    // any run - either every run's own `generations` was 0 (each loop body
    // never ran) or a cancel that landed before the very first individual
    // finished. The latter is a normal outcome (see this function's own doc
    // comment: "a user-requested stop is a normal outcome, not a failure"),
    // not an error - report it as a zero result (nothing placed, everything
    // still unplaced) rather than failing the whole call.
    let best = match overall_best {
        Some(b) => b,
        None if cancelled => {
            // `adam`, not `parts_by_id_dto`'s keys: with `config.mirror` on
            // the latter also holds every part's mirrored alternate, which
            // is a variant of a part, not a second part to report missing.
            let mut unplaced_ids: Vec<usize> = adam.clone();
            unplaced_ids.sort_unstable();
            PlaceResult {
                placements: Vec::new(),
                fitness: 0.0,
                area: 0.0,
                total_area: 0.0,
                utilisation: 0.0,
                unplaced_count: unplaced_ids.len(),
                unplaced_ids,
            }
        }
        None => return Err("ran zero generations".to_string()),
    };

    // `place_parts` opens sheets once and never revisits them - a classic
    // cause of excess sheet usage in single-pass bin-packing (a sheet closed
    // early off one big part can sit mostly empty while a part that would
    // fit its leftover space ends up opening a whole new sheet instead).
    // `refine_consolidation` fixes this up on the already-computed winner,
    // relocating parts between already-open sheets and dropping any sheet
    // that ends up fully drained - budget-capped so it stays cheap relative
    // to the GA run that already ran ahead of it. Skipped when there's
    // nothing to relocate (cancelled-with-zero-parts).
    let best = if best.placements.is_empty() {
        best
    } else {
        let deadline = Instant::now() + Duration::from_secs(2);
        let refined = refine_consolidation(best.placements, &parts_by_id, &shape_ids, &sheets, &placement_config, deadline, &cache);
        if refined.changed {
            let totals = recompute_totals(&refined.allplacements, &parts_by_id, &sheets);
            PlaceResult {
                placements: refined.allplacements,
                fitness: best.fitness,
                area: totals.total_placed_area,
                total_area: totals.total_usable_sheet_area,
                utilisation: totals.utilisation,
                unplaced_count: best.unplaced_count,
                unplaced_ids: best.unplaced_ids,
            }
        } else {
            PlaceResult { placements: refined.allplacements, ..best }
        }
    };

    // Post-nest cleaning pass: any sheet under `cleanup_threshold` gets
    // repacked in place (nesting::repack::repack_sheet - same technique/
    // config as the main run, that sheet's own parts only). Runs after
    // refine_consolidation, on top of the already-defragmented layout.
    // Never changes unplaced_count/unplaced_ids or which parts ended up on
    // which sheet - repack_sheet only ever keeps or replaces an already-
    // fully-placed sheet's arrangement, it never un-places anything.
    let mut best = best;
    if let Some(threshold) = cleanup_threshold {
        // Same GravityTightFit override as the manual REPACK command
        // (commands::repack_sheet) - both call nesting::repack::repack_sheet
        // for the same "tighten up this one sheet" job, so both should get
        // the same gravity-driven-envelope-plus-contact-tiebreak scoring
        // instead of reusing the main run's placement_type verbatim. See
        // that command's own comment for why plain Gravity alone isn't
        // enough (no tie-break at all between equally-compact envelope
        // candidates, visibly bad for plain rectangles).
        let repack_placement_config = PlacementConfig { placement_type: PlacementType::GravityTightFit, ..placement_config.clone() };
        // Matches repack_placement_config's rotations (the *winning* run's,
        // possibly escalated past the request's original value), not
        // base_ga_config's pre-escalation one - GaConfig::rotations bounds
        // which angles a gene can mutate/randomize to (ga.rs's
        // random_angles), so reusing the narrower base value here would
        // under-search relative to the wider grid the layout being cleaned
        // up was actually placed with.
        let repack_ga_config = GaConfig { rotations: repack_placement_config.rotations, ..base_ga_config.clone() };
        // Same scoped pool as the main escalation loop above, not rayon's
        // uncapped global one - without this, config.max_threads is
        // silently ignored for every repack_sheet call this pass makes
        // (each one dispatches its own GA generations via rayon::par_iter).
        let mut run_cleanup = || {
            for sheet_placement in &mut best.placements {
                if should_cancel() {
                    break;
                }
                let sheet_totals = recompute_totals(std::slice::from_ref(sheet_placement), &parts_by_id, &sheets);
                if sheet_totals.utilisation >= threshold {
                    continue;
                }
                if let Some(repacked) = repack::repack_sheet(
                    &sheets[sheet_placement.sheet_index],
                    sheet_placement,
                    &parts_by_id,
                    &shape_ids,
                    &repack_ga_config,
                    &repack_placement_config,
                    base_generations,
                    seed,
                    &[],
                    &should_cancel,
                ) {
                    *sheet_placement = repacked;
                }
            }
        };
        match &pool {
            Some(p) => p.install(run_cleanup),
            None => run_cleanup(),
        }
        let totals = recompute_totals(&best.placements, &parts_by_id, &sheets);
        best.area = totals.total_placed_area;
        best.total_area = totals.total_usable_sheet_area;
        best.utilisation = totals.utilisation;
    }

    Ok(RunNestResponse {
        history: history
            .into_iter()
            .map(|(generation, r)| NestSnapshotDto {
                generation,
                placements: to_placements_dto(r.placements),
                fitness: r.fitness,
                utilisation: r.utilisation,
                unplaced_count: r.unplaced_count,
                unplaced_ids: r.unplaced_ids,
            })
            .collect(),
        placements: to_placements_dto(best.placements),
        fitness: best.fitness,
        utilisation: best.utilisation,
        unplaced_count: best.unplaced_count,
        unplaced_ids: best.unplaced_ids,
        cancelled,
        part_rules: placement_config.part_rules.iter().map(|(&id, rule)| (id, PartRuleDto::from(rule))).collect(),
        parts_by_id: parts_by_id_dto,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{NestConfigDto, PartDto, PlacementTypeDto, PointDto, ReportPartDto, TextDto};

    fn square_dto(size: f64) -> PolygonDto {
        PolygonDto {
            points: vec![
                PointDto { x: 0.0, y: 0.0 },
                PointDto { x: size, y: 0.0 },
                PointDto { x: size, y: size },
                PointDto { x: 0.0, y: size },
            ],
            layer: "0".into(),
            is_circle: None,
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        }
    }

    /// Every test part is unconstrained unless it says otherwise - see
    /// `PartDto::allowed_rotations`/`mirror`.
    fn part(polygon: PolygonDto, quantity: usize) -> PartDto {
        PartDto { polygon, quantity, allowed_rotations: None, mirror: None }
    }

    fn rect_dto(w: f64, h: f64) -> PolygonDto {
        PolygonDto {
            points: vec![
                PointDto { x: 0.0, y: 0.0 },
                PointDto { x: w, y: 0.0 },
                PointDto { x: w, y: h },
                PointDto { x: 0.0, y: h },
            ],
            layer: "0".into(),
            is_circle: None,
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        }
    }

    fn config(generations: usize) -> NestConfigDto {
        NestConfigDto {
            placement_type: PlacementTypeDto::Gravity,
            rotations: 1,
            population_size: 6,
            mutation_rate: 15.0,
            dominant_part_area_threshold: nesting::placement::DEFAULT_DOMINANT_PART_AREA_THRESHOLD,
            curve_tolerance: 0.3,
            generations,
            margin: 0.0,
            spacing: 0.0,
            max_threads: 0,
            seed: 0,
            runs: 1,
            cleanup_threshold_percent: None,
            mirror: false,
        }
    }

    /// The engine's own output, with a real margin and spacing, must audit
    /// completely clean - no warnings either. A nest the engine itself
    /// produced and considers valid cannot be something the audit complains
    /// about, or the audit is measuring itself rather than the nest.
    #[test]
    fn a_real_nest_with_margin_and_spacing_produces_no_warnings_at_all() {
        let sheets = vec![square_dto(400.0)];
        let cfg = NestConfigDto { margin: 5.0, spacing: 5.0, ..config(2) };
        let response = run_nest(RunNestRequest { sheets: sheets.clone(), parts: vec![part(square_dto(50.0), 12)], config: cfg.clone() }).expect("should nest");
        assert_eq!(response.unplaced_count, 0, "fixture must place, or this proves nothing");

        let report = audit_nest(crate::dto::AuditRequest {
            sheets,
            placements: response.placements.clone(),
            parts_by_id: response.parts_by_id.clone(),
            config: cfg,
        })
        .expect("audit should run");

        assert_eq!(report.fatal_count, 0, "engine output must not be fatal: {:?}", report.issues);
        assert_eq!(report.warning_count, 0, "engine output must not warn either: {:?}", report.issues);
    }

    /// The configuration a user actually runs: TightFit on real irregular
    /// geometry. TightFit deliberately maximises *contact* between padded
    /// outlines, which is exactly the condition an exact
    /// "do these share any area at all" test is most likely to misread.
    #[test]
    fn a_tight_fit_nest_on_real_geometry_produces_no_spurious_findings() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/two.dxf");
        let shapes = import_dxf(path, 0.3).expect("fixture should parse");
        let shape = shapes.into_iter().next().expect("fixture has parts");

        let sheets = vec![rect_dto(2440.0, 1220.0)];
        let cfg = NestConfigDto { margin: 5.0, spacing: 5.0, placement_type: PlacementTypeDto::TightFit, rotations: 4, ..config(2) };
        let response = run_nest(RunNestRequest { sheets: sheets.clone(), parts: vec![part(shape, 8)], config: cfg.clone() }).expect("should nest");

        let report = audit_nest(crate::dto::AuditRequest {
            sheets,
            placements: response.placements.clone(),
            parts_by_id: response.parts_by_id.clone(),
            config: cfg,
        })
        .expect("audit should run");

        assert_eq!(report.fatal_count, 0, "engine output must not be fatal: {:?}", report.issues);
        assert_eq!(report.warning_count, 0, "engine output must not warn either: {:?}", report.issues);
    }

    /// The audit's reason for existing, end to end through the real engine:
    /// a nest the engine produced must pass, and the same nest with one part
    /// dragged on top of another must fail. A drag is exactly how a user
    /// creates the second state, and nothing else in the app re-checks it.
    #[test]
    fn the_audit_passes_a_real_nest_and_catches_a_part_dragged_onto_another() {
        let sheets = vec![square_dto(100.0)];
        let cfg = config(2);
        let response = run_nest(RunNestRequest { sheets: sheets.clone(), parts: vec![part(square_dto(10.0), 3)], config: cfg.clone() }).expect("should nest");
        assert_eq!(response.unplaced_count, 0, "the fixture must actually place, or this proves nothing");

        let request = |placements: Vec<SheetPlacementDto>| crate::dto::AuditRequest {
            sheets: sheets.clone(),
            placements,
            parts_by_id: response.parts_by_id.clone(),
            config: cfg.clone(),
        };

        let clean = audit_nest(request(response.placements.clone())).expect("audit should run");
        assert!(clean.passed, "the engine's own output must audit clean: {:?}", clean.issues);
        assert_eq!(clean.fatal_count, 0);

        // Drag the second part exactly on top of the first.
        let mut broken = response.placements.clone();
        let (target_x, target_y) = (broken[0].parts[0].x, broken[0].parts[0].y);
        let moved_id = broken[0].parts[1].id;
        broken[0].parts[1].x = target_x;
        broken[0].parts[1].y = target_y;

        let report = audit_nest(request(broken)).expect("audit should run");
        assert!(!report.passed, "two parts at the same position must fail the audit");
        assert!(
            report.issues.iter().any(|i| i.kind == "overlap" && i.part_ids.contains(&moved_id)),
            "the overlap must name the part that moved: {:?}",
            report.issues
        );
    }

    /// Clearance shortfalls are advisory, not fatal - reporting them in the
    /// same voice as destroyed parts is how an audit gets ignored. Nest with
    /// spacing, then slide two parts to within less than that.
    #[test]
    fn the_audit_separates_a_clearance_shortfall_from_a_real_overlap() {
        let sheets = vec![square_dto(100.0)];
        let cfg = NestConfigDto { spacing: 6.0, ..config(2) };
        let response = run_nest(RunNestRequest { sheets: sheets.clone(), parts: vec![part(square_dto(10.0), 2)], config: cfg.clone() }).expect("should nest");
        assert_eq!(response.unplaced_count, 0);

        // Butt the two parts up 1mm apart - clear of each other, but well
        // inside the 6mm that was configured.
        let mut placements = response.placements.clone();
        let first = placements[0].parts[0];
        placements[0].parts[1].x = first.x + 11.0;
        placements[0].parts[1].y = first.y;
        placements[0].parts[1].rotation = first.rotation;

        let report = audit_nest(crate::dto::AuditRequest { sheets, placements, parts_by_id: response.parts_by_id, config: cfg }).expect("audit should run");
        assert!(report.passed, "a clearance shortfall must not fail the audit: {:?}", report.issues);
        assert!(report.warning_count > 0, "...but it must still be reported: {:?}", report.issues);
        assert!(report.issues.iter().all(|i| i.kind != "overlap"), "parts 1mm apart do not overlap: {:?}", report.issues);
    }

    #[test]
    fn run_nest_places_a_simple_part_end_to_end() {
        let request = RunNestRequest {
            sheets: vec![square_dto(100.0)],
            parts: vec![part(square_dto(10.0), 3)],
            config: config(2),
        };

        let response = run_nest(request).expect("should nest successfully");

        assert_eq!(response.unplaced_count, 0);
        assert_eq!(response.placements.len(), 1);
        assert_eq!(response.placements[0].parts.len(), 3);
        assert!(response.utilisation > 0.0);
    }

    #[test]
    fn run_nest_consolidates_a_sparse_sheet_the_dominant_area_shortcut_leaves_behind() {
        // Same shape of scenario as `nesting::consolidation`'s own
        // `drains_a_sparse_sheet_into_another_when_relocation_fits` test, but
        // exercised end to end through `run_nest` - this is the regression
        // test for `refine_consolidation` actually being wired into the
        // command, not just built and unit-tested in isolation. Two 1000x1000
        // sheets; a 950x950 part is 90.25% of a sheet - past the default 90%
        // dominant-area threshold, so `place_parts`'s greedy pass closes that
        // sheet immediately without ever trying the second, much smaller
        // part on it, even though its leftover margin has real room. Without
        // consolidation this nests onto 2 sheets; with it, the small part
        // should get relocated onto sheet 0's margin and the second sheet
        // dropped entirely.
        let request = RunNestRequest {
            sheets: vec![square_dto(1000.0), square_dto(1000.0)],
            parts: vec![part(square_dto(950.0), 1), part(square_dto(20.0), 1)],
            config: config(1),
        };

        let response = run_nest(request).expect("should nest successfully");

        assert_eq!(response.unplaced_count, 0);
        assert_eq!(response.placements.len(), 1, "consolidation should have drained the second sheet, leaving both parts on one");
        assert_eq!(response.placements[0].parts.len(), 2);
    }

    #[test]
    fn run_nest_with_cleanup_threshold_never_loses_parts_or_regresses_utilisation() {
        // `cleanup_threshold_percent: Some(100.0)` forces every sheet through
        // the post-nest repack pass (nothing can ever be >=100% "used" for a
        // job with real slack), so this exercises the pass being wired into
        // `run_nest_with_progress` at all, not just built in isolation
        // (`nesting::repack`'s own unit tests already cover the repack
        // mechanism itself finding a real improvement). Utilisation is
        // provably invariant to how a *fixed* set of parts is arranged on a
        // *fixed* sheet (same total part area either way - see
        // `nesting::repack`'s own module doc comment), so a request run
        // twice, once with cleanup off and once forced on, must report
        // identical unplaced_count/sheet count/utilisation - the only thing
        // cleanup is allowed to change is the parts' x/y/rotation.
        let mut request = RunNestRequest {
            sheets: vec![square_dto(300.0), square_dto(300.0)],
            parts: vec![
                part(rect_dto(120.0, 40.0), 1),
                part(rect_dto(90.0, 70.0), 1),
                part(rect_dto(50.0, 50.0), 1),
                part(rect_dto(30.0, 90.0), 1),
            ],
            config: config(3),
        };

        let baseline = run_nest(request.clone()).expect("baseline run should nest successfully");
        request.config.cleanup_threshold_percent = Some(100.0);
        let cleaned = run_nest(request).expect("cleanup-forced run should nest successfully");

        assert_eq!(cleaned.unplaced_count, 0);
        assert_eq!(cleaned.unplaced_count, baseline.unplaced_count);
        assert_eq!(cleaned.placements.len(), baseline.placements.len(), "cleanup must never open or close a sheet");
        let total_parts = |r: &RunNestResponse| r.placements.iter().map(|p| p.parts.len()).sum::<usize>();
        assert_eq!(total_parts(&cleaned), total_parts(&baseline), "cleanup must never drop or duplicate a part");
        assert!((cleaned.utilisation - baseline.utilisation).abs() < 1e-9, "utilisation must be unchanged: {} vs {}", cleaned.utilisation, baseline.utilisation);
    }

    #[test]
    fn run_nest_history_ends_with_the_same_result_as_the_top_level_fields() {
        let request = RunNestRequest {
            sheets: vec![square_dto(100.0)],
            parts: vec![part(square_dto(10.0), 3)],
            config: config(5),
        };

        let response = run_nest(request).expect("should nest successfully");

        assert!(!response.history.is_empty(), "at least the first placed individual should count as an improvement");
        let last = response.history.last().unwrap();
        assert_eq!(last.fitness, response.fitness, "history's last entry should be the same result reported at the top level");
        assert_eq!(last.unplaced_count, response.unplaced_count);
        assert_eq!(last.placements.len(), response.placements.len());
        // generations should be non-decreasing across history (each entry
        // found no earlier than the one before it)
        for pair in response.history.windows(2) {
            assert!(pair[0].generation <= pair[1].generation);
        }
    }

    #[test]
    fn run_nest_fits_a_full_sheet_size_part_with_zero_margin_regardless_of_spacing() {
        // The exact scenario margin/spacing was built for: a part exactly
        // the sheet's size must be placeable with zero waste as long as
        // margin is 0, no matter what spacing is set to (spacing is a
        // part-to-part concern, unrelated to a single part's fit against
        // the sheet edge).
        let mut cfg = config(1);
        cfg.margin = 0.0;
        cfg.spacing = 6.5;
        let request = RunNestRequest { sheets: vec![square_dto(100.0)], parts: vec![part(square_dto(100.0), 1)], config: cfg };

        let response = run_nest(request).expect("full-sheet-size part should nest with zero margin");

        assert_eq!(response.unplaced_count, 0);
        assert_eq!(response.placements[0].parts.len(), 1);
    }

    #[test]
    fn run_nest_rejects_a_part_that_only_fits_without_margin() {
        // Same part/sheet as above, but with a real margin this time - the
        // same part must now correctly fail to place, proving margin is
        // actually enforced and not silently ignored.
        let mut cfg = config(1);
        cfg.margin = 5.0;
        cfg.spacing = 0.0;
        let request = RunNestRequest { sheets: vec![square_dto(100.0)], parts: vec![part(square_dto(100.0), 1)], config: cfg };

        let response = run_nest(request).expect("run_nest itself should still succeed, just leave the part unplaced");

        assert_eq!(response.unplaced_count, 1);
        assert!(response.placements.is_empty());
        assert_eq!(response.unplaced_ids, vec![0], "the single part (id 0, expand_parts's first id) should be reported unplaced by id, not just by count");
    }

    #[test]
    fn run_nest_respects_a_max_threads_cap() {
        let mut cfg = config(2);
        cfg.max_threads = 1;
        let request = RunNestRequest {
            sheets: vec![square_dto(100.0)],
            parts: vec![part(square_dto(10.0), 3)],
            config: cfg,
        };

        let response = run_nest(request).expect("a max_threads cap should still nest successfully, just on fewer threads");

        assert_eq!(response.unplaced_count, 0);
    }

    #[test]
    fn run_nest_rejects_a_zero_thread_count_gracefully() {
        // max_threads: 0 means "no cap" (the default), not "a pool of zero
        // threads" - make sure that sentinel doesn't accidentally reach
        // ThreadPoolBuilder::num_threads(0), which would build a pool that
        // can never run anything.
        let mut cfg = config(1);
        cfg.max_threads = 0;
        let request = RunNestRequest { sheets: vec![square_dto(100.0)], parts: vec![part(square_dto(10.0), 1)], config: cfg };
        let response = run_nest(request).expect("max_threads: 0 must mean uncapped, not a zero-thread pool");
        assert_eq!(response.unplaced_count, 0);
    }

    #[test]
    fn run_nest_enforces_spacing_between_two_placed_parts() {
        // Two parts that would just barely both fit side by side with zero
        // gap must NOT both place once spacing requires more room than the
        // sheet has for both.
        let mut cfg = config(1);
        cfg.margin = 0.0;
        cfg.spacing = 50.0; // larger than the sheet has slack for two 40-wide parts
        let request = RunNestRequest {
            sheets: vec![square_dto(100.0)],
            parts: vec![part(square_dto(40.0), 2)],
            config: cfg,
        };

        let response = run_nest(request).expect("should still run, just not fit both");

        assert_eq!(response.unplaced_count, 1, "spacing=50 between two 40-wide parts on a 100-wide sheet must leave one unplaced");
    }

    #[test]
    fn run_nest_rejects_negative_margin_or_spacing() {
        for (margin, spacing) in [(-1.0, 0.0), (0.0, -1.0)] {
            let mut cfg = config(1);
            cfg.margin = margin;
            cfg.spacing = spacing;
            let request =
                RunNestRequest { sheets: vec![square_dto(100.0)], parts: vec![part(square_dto(10.0), 1)], config: cfg };
            assert!(run_nest(request).is_err(), "margin={margin} spacing={spacing} should be rejected");
        }
    }

    /// Regression test: `repack_sheet` used to only check
    /// `rotations`/`population_size`/`generations`, silently skipping the
    /// same margin/spacing/mutation_rate/curve_tolerance/dominant_threshold
    /// checks `run_nest` already applied - an unvalidated negative
    /// margin/spacing/curve_tolerance from this specific IPC entry point
    /// could reach `geometry::clearance`/Clipper2 unchecked. Covers one bad
    /// value per field now shared via `validate_nest_config`.
    #[test]
    fn repack_sheet_rejects_the_same_bad_config_values_run_nest_does() {
        let base_request = |cfg: NestConfigDto| RepackSheetRequest {
            sheet: square_dto(100.0),
            placement: SheetPlacementDto { sheet_index: 0, parts: vec![PlacedPartDto { id: 0, x: 0.0, y: 0.0, rotation: 0.0, locked: false }] },
            parts_by_id: HashMap::from([(0, square_dto(10.0))]),
            config: cfg,
            part_rules: HashMap::new(),
        };
        for bad_cfg in [
            { let mut c = config(1); c.margin = -1.0; c },
            { let mut c = config(1); c.spacing = -1.0; c },
            { let mut c = config(1); c.mutation_rate = -1.0; c },
            { let mut c = config(1); c.curve_tolerance = 0.0; c },
            { let mut c = config(1); c.dominant_part_area_threshold = 0.0; c },
            { let mut c = config(1); c.rotations = 0; c },
            { let mut c = config(1); c.population_size = 1; c },
            config(0),
        ] {
            assert!(repack_sheet(base_request(bad_cfg)).is_err());
        }
    }

    /// Regression test: `mutation_rate`/`curve_tolerance`/
    /// `dominant_part_area_threshold` used to be the only three fields on
    /// `NestConfigDto` with no validation at all - no panic risk behind
    /// them, but a negative `curve_tolerance` or an out-of-range
    /// `dominant_part_area_threshold` would silently produce nonsense GA
    /// behavior with zero feedback to the caller.
    #[test]
    fn run_nest_rejects_out_of_range_mutation_rate_curve_tolerance_and_dominant_threshold() {
        for (mutation_rate, curve_tolerance, dominant) in [
            (-1.0, 0.3, 0.9),   // mutation_rate below 0
            (101.0, 0.3, 0.9),  // mutation_rate above 100
            (15.0, 0.0, 0.9),   // curve_tolerance not > 0
            (15.0, -0.1, 0.9),  // curve_tolerance negative
            (15.0, 0.3, 0.0),   // dominant_part_area_threshold not > 0
            (15.0, 0.3, 1.5),   // dominant_part_area_threshold above 1
        ] {
            let mut cfg = config(1);
            cfg.mutation_rate = mutation_rate;
            cfg.curve_tolerance = curve_tolerance;
            cfg.dominant_part_area_threshold = dominant;
            let request =
                RunNestRequest { sheets: vec![square_dto(100.0)], parts: vec![part(square_dto(10.0), 1)], config: cfg };
            assert!(
                run_nest(request).is_err(),
                "mutation_rate={mutation_rate} curve_tolerance={curve_tolerance} dominant_part_area_threshold={dominant} should be rejected"
            );
        }
    }

    #[test]
    fn run_nest_rejects_empty_sheets() {
        let request = RunNestRequest { sheets: Vec::new(), parts: vec![part(square_dto(10.0), 1)], config: config(1) };
        assert!(run_nest(request).is_err());
    }

    #[test]
    fn run_nest_rejects_empty_parts() {
        let request = RunNestRequest { sheets: vec![square_dto(100.0)], parts: Vec::new(), config: config(1) };
        assert!(run_nest(request).is_err());
    }

    #[test]
    fn run_nest_excludes_zero_quantity_parts() {
        // A part explicitly given quantity 0 contributes zero copies -
        // matches the original's plain `for (j=0; j<quantity; j++)` loop
        // for parts (no fallback-to-1; that convention only exists for
        // *sheet* quantity, a different code path with different
        // semantics). If every part is quantity 0, nothing to nest at all.
        let request =
            RunNestRequest { sheets: vec![square_dto(100.0)], parts: vec![part(square_dto(10.0), 0)], config: config(1) };
        assert!(run_nest(request).is_err());
    }

    #[test]
    fn run_nest_nests_only_the_non_zero_quantity_parts_in_a_mix() {
        let request = RunNestRequest {
            sheets: vec![square_dto(100.0)],
            parts: vec![
                part(square_dto(10.0), 2),
                part(square_dto(20.0), 0),
            ],
            config: config(2),
        };

        let response = run_nest(request).expect("should nest the non-zero-quantity part");

        assert_eq!(response.unplaced_count, 0);
        assert_eq!(response.placements[0].parts.len(), 2, "only the 2 copies of the quantity=2 part should be nested");
    }

    #[test]
    fn run_nest_rejects_zero_rotations() {
        let mut cfg = config(1);
        cfg.rotations = 0;
        let request = RunNestRequest { sheets: vec![square_dto(100.0)], parts: vec![part(square_dto(10.0), 1)], config: cfg };
        assert!(run_nest(request).is_err());
    }

    #[test]
    fn run_nest_rejects_population_size_under_two() {
        for bad_size in [0, 1] {
            let mut cfg = config(1);
            cfg.population_size = bad_size;
            let request =
                RunNestRequest { sheets: vec![square_dto(100.0)], parts: vec![part(square_dto(10.0), 1)], config: cfg };
            assert!(run_nest(request).is_err(), "population_size {bad_size} should be rejected");
        }
    }

    #[test]
    fn run_nest_with_progress_calls_the_hook_once_per_generation() {
        let request = RunNestRequest {
            sheets: vec![square_dto(100.0)],
            parts: vec![part(square_dto(10.0), 3)],
            config: config(4),
        };

        let mut seen_generations = Vec::new();
        let response = run_nest_with_progress(
            request,
            |generation, generations, best_so_far| {
                assert_eq!(generations, 4);
                assert!(best_so_far.fitness.is_finite());
                seen_generations.push(generation);
            },
            || false,
            |_, _, _| {},
            |_| {},
            |_| {},
        )
        .expect("should nest successfully");

        assert_eq!(seen_generations, vec![1, 2, 3, 4]);
        assert_eq!(response.unplaced_count, 0);
        assert!(!response.cancelled);
    }

    #[test]
    fn run_nest_with_progress_escalates_rotations_population_and_generations_across_runs() {
        let mut cfg = config(8);
        cfg.runs = 3;
        cfg.rotations = 2;
        cfg.population_size = 2;
        cfg.mutation_rate = 90.0;
        // Rectangles (not squares - a square's rotation genes are inert,
        // since every rotation produces the identical shape, and identical-
        // size parts make ordering genes inert too, since every arrangement
        // packs identically regardless of which gene produced it) of mixed,
        // asymmetric sizes, totaling enough area (~16,000mm2) to need
        // multiple 100x100 (10,000mm2) sheets. Both properties matter here:
        // a discrete "fewer sheets used" signal is a much more reliable way
        // to get the GA to keep improving across several generations of a
        // run than hoping a same-sheet-count utilisation nudge happens to
        // occur, and genuinely rotation/order-sensitive geometry is what
        // makes that improvement possible at all - an earlier version of
        // this test used identical squares and was flaky (every run
        // recording exactly one improvement, at generation 1, regardless of
        // the extra generations configured), for exactly this reason. That
        // distinction matters here: `generation_offset` undercounting
        // relative to generations *actually elapsed* only produces an
        // observably wrong (colliding or non-monotonic) label once some run
        // records more than one improving generation - see the assertions
        // below.
        let request = RunNestRequest {
            sheets: (0..4).map(|_| square_dto(100.0)).collect(),
            parts: vec![
                part(rect_dto(35.0, 12.0), 10),
                part(rect_dto(18.0, 27.0), 8),
                part(rect_dto(9.0, 41.0), 6),
            ],
            config: cfg,
        };

        let starts = std::sync::Mutex::new(Vec::new());
        let completes = std::sync::Mutex::new(Vec::new());
        let response = run_nest_with_progress(
            request,
            |_, _, _| {},
            || false,
            |_, _, _| {},
            |start| starts.lock().unwrap().push(*start),
            |complete| completes.lock().unwrap().push(*complete),
        )
        .expect("should nest successfully");

        let starts = starts.into_inner().unwrap();
        let completes = completes.into_inner().unwrap();

        // 3 runs configured: rotations 2,3,4 / population 2,6,10 /
        // generations 8,13,18 - each escalating by RUN_POPULATION_STEP/
        // RUN_GENERATIONS_STEP per run, matching `escalated_run_config`.
        assert_eq!(starts.len(), 3);
        assert_eq!(completes.len(), 3);
        for (i, start) in starts.iter().enumerate() {
            assert_eq!(start.run, i + 1);
            assert_eq!(start.total_runs, 3);
            assert_eq!(start.rotations, 2 + i as u32);
            assert_eq!(start.population_size, 2 + i * 4);
            assert_eq!(start.generations, 8 + i * 5);
        }
        for (i, complete) in completes.iter().enumerate() {
            assert_eq!(complete.run, i + 1);
            assert_eq!(complete.rotations, 2 + i as u32);
        }

        assert_eq!(response.unplaced_count, 0, "40 small squares should all fit within the 4 available 100x100 sheets regardless of which run placed them");
        // history spans every run, not just the last one, with labels that
        // are not just unique but strictly increasing across the whole
        // escalation - regression coverage for a real bug this test caught:
        // `generation_offset` was computed from `overall_history.len()`
        // (the count of *recorded improvements* so far) instead of
        // generations actually elapsed, which only produces an observably
        // wrong (colliding or non-monotonic) label once some run records
        // more than one improving generation - a harder job (mixed
        // rectangles across multiple sheets, vs. 3 trivially-placed
        // squares) makes that the likely case instead of an unlikely one.
        // Only entries that are a genuine *overall* improvement land in
        // `history` at all now (see `run_nest_with_progress`'s own comment
        // on why an earlier version of this bundled a second real bug -
        // unconditionally pushing every run-local entry regardless of
        // whether it beat prior runs), so this no longer asserts a raw
        // count - just that whatever's there is honestly ordered.
        assert!(!response.history.is_empty(), "at least the first placed individual, in some run, should count as an improvement");
        let generations_seen: Vec<usize> = response.history.iter().map(|h| h.generation).collect();
        for pair in generations_seen.windows(2) {
            assert!(pair[0] < pair[1], "history generation labels must be strictly increasing across the whole escalation, got {:?}", generations_seen);
        }
    }

    /// Regression test for a real bug: `overall_history` used to push every
    /// run-local history entry unconditionally, including entries that were
    /// only "an improvement" relative to that *run's own* fresh-starting
    /// `None` baseline, not the actual best found across every run so far.
    /// A later run's early, genuinely-worse-than-an-earlier-run individual
    /// could then show up in `RunNestResponse::history` (the frontend's
    /// "VIEW ATTEMPT" dropdown) looking like a legitimate later attempt.
    /// Forces the scenario directly: run 1 gets a generous budget (likely to
    /// find a good arrangement), run 2 gets a single, tiny generation/
    /// population budget (likely to do *worse* than run 1) - if the bug
    /// were reintroduced, `history`'s last entry would be run 2's inferior
    /// result instead of matching the top-level (genuinely best) fields.
    #[test]
    fn history_never_contains_an_entry_worse_than_an_earlier_run_already_achieved() {
        let mut cfg = config(10);
        cfg.runs = 2;
        cfg.rotations = 2;
        cfg.population_size = 10;
        cfg.mutation_rate = 50.0;
        let request = RunNestRequest {
            sheets: (0..4).map(|_| square_dto(100.0)).collect(),
            parts: vec![
                part(rect_dto(35.0, 12.0), 10),
                part(rect_dto(18.0, 27.0), 8),
                part(rect_dto(9.0, 41.0), 6),
            ],
            config: cfg,
        };

        let response = run_nest(request).expect("should nest successfully");

        assert!(!response.history.is_empty(), "at least the first placed individual should count as an improvement");
        let last = response.history.last().unwrap();
        assert_eq!(last.fitness, response.fitness, "history's last entry must be the same result reported at the top level, even across multiple escalating runs");
        assert_eq!(last.unplaced_count, response.unplaced_count);
        assert_eq!(last.placements.len(), response.placements.len());
        // Every entry must be a genuine improvement over every entry before
        // it, not just over its own run's local starting point - the exact
        // property the unconditional-push bug violated.
        for pair in response.history.windows(2) {
            let (earlier, later) = (&pair[0], &pair[1]);
            assert!(
                later.unplaced_count < earlier.unplaced_count
                    || (later.unplaced_count == earlier.unplaced_count && later.placements.len() < earlier.placements.len())
                    || (later.unplaced_count == earlier.unplaced_count && later.placements.len() == earlier.placements.len() && later.utilisation > earlier.utilisation),
                "history entry at generation {} is not actually better than the one before it at generation {} (unplaced {} vs {}, sheets {} vs {}, util {} vs {})",
                later.generation,
                earlier.generation,
                later.unplaced_count,
                earlier.unplaced_count,
                later.placements.len(),
                earlier.placements.len(),
                later.utilisation,
                earlier.utilisation
            );
        }
    }

    #[test]
    fn run_nest_with_progress_stops_early_when_cancelled() {
        let request = RunNestRequest {
            sheets: vec![square_dto(100.0)],
            parts: vec![part(square_dto(10.0), 3)],
            config: config(20),
        };

        // should_cancel is now `Fn + Sync` (called from multiple rayon
        // threads inside dispatch::run_generation, not just once per
        // generation), so this needs a thread-safe counter, not a plain
        // captured `let mut`.
        let checks = std::sync::atomic::AtomicUsize::new(0);
        let response = run_nest_with_progress(request, |_, _, _| {}, || checks.fetch_add(1, Ordering::Relaxed) >= 2, |_, _, _| {}, |_| {}, |_| {})
            .expect("should still return the best result found so far");

        assert!(response.cancelled);
    }

    #[test]
    fn run_nest_with_progress_reports_per_individual_ticks_within_a_generation() {
        let request = RunNestRequest {
            sheets: vec![square_dto(100.0)],
            parts: vec![part(square_dto(10.0), 3)],
            config: config(2),
        };

        let ticks = std::sync::Mutex::new(Vec::new());
        let response = run_nest_with_progress(request, |_, _, _| {}, || false, |generation, done, total| {
            ticks.lock().unwrap().push((generation, done, total));
        }, |_| {}, |_| {})
        .expect("should nest successfully");

        let ticks = ticks.into_inner().unwrap();
        assert!(!ticks.is_empty(), "should see at least one tick per generation");
        // every tick's generation is within range, and the upfront (done: 0)
        // tick appears before any individual actually finishes for that
        // generation
        for &(generation, _, _) in &ticks {
            assert!((1..=2).contains(&generation));
        }
        assert!(ticks.iter().any(|&(_, done, _)| done == 0), "the upfront tick (0, total) should appear");
        assert_eq!(response.unplaced_count, 0);
    }

    #[test]
    fn run_nest_with_progress_reports_a_graceful_cancelled_result_when_stopped_before_any_placement() {
        // Cancelling immediately (before generation 1 ever gets a result)
        // used to return an Err ("cancelled before any nest was found"),
        // contradicting this function's own doc comment that a
        // user-requested stop is a normal outcome, not a failure. It must
        // now succeed with cancelled: true and every part reported unplaced.
        let request = RunNestRequest {
            sheets: vec![square_dto(100.0)],
            parts: vec![part(square_dto(10.0), 3)],
            config: config(20),
        };

        let response = run_nest_with_progress(request, |_, _, _| {}, || true, |_, _, _| {}, |_| {}, |_| {})
            .expect("an immediate cancel must still succeed gracefully");

        assert!(response.cancelled);
        assert_eq!(response.placements.len(), 0);
        assert_eq!(response.unplaced_count, 3);
        assert_eq!(response.unplaced_ids, vec![0, 1, 2]);
        assert!(response.history.is_empty());
    }

    #[test]
    fn export_dxf_round_trips_a_real_nest_result() {
        let sheets = vec![square_dto(100.0)];
        let parts = vec![part(square_dto(10.0), 3)];
        let request = RunNestRequest { sheets: sheets.clone(), parts, config: config(2) };
        let response = run_nest(request).expect("should nest successfully");

        let out_path = std::env::temp_dir().join("rustynesting_export_dxf_test.dxf");
        let export_request = ExportRequest {
            sheets,
            parts_by_id: response.parts_by_id,
            placements: response.placements,
            sheet_spacing: 20.0,
            include_sheet_outline: true,
            include_unplaced: false,
        };
        export_dxf(out_path.to_str().unwrap(), export_request).expect("export should succeed");

        let drawing = Drawing::load_file(&out_path).expect("exported file should be a readable DXF");
        let polyline_count = drawing.entities().filter(|e| matches!(e.specific, dxf::entities::EntityType::LwPolyline(_))).count();
        // 1 sheet outline + 3 placed parts
        assert_eq!(polyline_count, 4);

        let _ = std::fs::remove_file(&out_path);
    }

    #[test]
    fn export_dxf_omits_the_sheet_outline_when_not_requested() {
        let sheets = vec![square_dto(100.0)];
        let parts = vec![part(square_dto(10.0), 2)];
        let request = RunNestRequest { sheets: sheets.clone(), parts, config: config(2) };
        let response = run_nest(request).expect("should nest successfully");

        let out_path = std::env::temp_dir().join("rustynesting_export_dxf_no_outline_test.dxf");
        let export_request = ExportRequest {
            sheets,
            parts_by_id: response.parts_by_id,
            placements: response.placements,
            sheet_spacing: 10.0,
            include_sheet_outline: false,
            include_unplaced: false,
        };
        export_dxf(out_path.to_str().unwrap(), export_request).expect("export should succeed");

        let drawing = Drawing::load_file(&out_path).expect("exported file should be a readable DXF");
        let polyline_count = drawing.entities().filter(|e| matches!(e.specific, dxf::entities::EntityType::LwPolyline(_))).count();
        assert_eq!(polyline_count, 2, "only the 2 placed parts, no sheet outline");

        let _ = std::fs::remove_file(&out_path);
    }

    #[test]
    fn export_dxf_rejects_negative_sheet_spacing() {
        let sheets = vec![square_dto(100.0)];
        let parts = vec![part(square_dto(10.0), 1)];
        let request = RunNestRequest { sheets: sheets.clone(), parts, config: config(1) };
        let response = run_nest(request).expect("should nest successfully");

        let export_request = ExportRequest {
            sheets,
            parts_by_id: response.parts_by_id,
            placements: response.placements,
            sheet_spacing: -5.0,
            include_sheet_outline: false,
            include_unplaced: false,
        };
        assert!(export_dxf("unused.dxf", export_request).is_err());
    }

    /// Regression test for the export-uses-resent-input bug: `export_dxf`
    /// used to re-run `expand_parts` on a client-resent `parts`/quantity
    /// list to rebuild its own id->shape mapping, which only happened to be
    /// correct if that resent list exactly matched what actually produced
    /// the ids in `placements` - nothing enforced that, and a mismatch
    /// wouldn't error, it would just silently write the wrong part's
    /// outline at a placement's coordinates. Now that `ExportRequest`
    /// takes `parts_by_id` directly (no re-derivation possible - the field
    /// doesn't exist to re-derive from), this proves export genuinely uses
    /// exactly the mapping it's given: two distinguishably-sized parts at
    /// fixed ids, checked by reading back the actual exported geometry's
    /// size, not just a polyline count.
    #[test]
    fn export_dxf_writes_each_placement_using_its_own_ids_mapped_shape() {
        let sheets = vec![square_dto(100.0)];
        let parts_by_id = HashMap::from([(0, square_dto(10.0)), (1, square_dto(30.0))]);
        let placements = vec![SheetPlacementDto {
            sheet_index: 0,
            parts: vec![
                PlacedPartDto { id: 0, x: 0.0, y: 0.0, rotation: 0.0, locked: false },
                PlacedPartDto { id: 1, x: 50.0, y: 50.0, rotation: 0.0, locked: false },
            ],
        }];

        let out_path = std::env::temp_dir().join("rustynesting_export_dxf_id_mapping_test.dxf");
        let export_request = ExportRequest { sheets, parts_by_id, placements, sheet_spacing: 20.0, include_sheet_outline: false, include_unplaced: false };
        export_dxf(out_path.to_str().unwrap(), export_request).expect("export should succeed");

        let drawing = Drawing::load_file(&out_path).expect("exported file should be a readable DXF");
        let mut widths: Vec<f64> = drawing
            .entities()
            .filter_map(|e| match &e.specific {
                dxf::entities::EntityType::LwPolyline(p) => {
                    let xs: Vec<f64> = p.vertices.iter().map(|v| v.x).collect();
                    let (min, max) = xs.iter().fold((f64::MAX, f64::MIN), |(min, max), &x| (min.min(x), max.max(x)));
                    Some(max - min)
                }
                _ => None,
            })
            .collect();
        widths.sort_by(f64::total_cmp);

        assert_eq!(widths.len(), 2);
        assert!((widths[0] - 10.0).abs() < 1e-6, "id 0's 10x10 part should export at its own size, got {widths:?}");
        assert!((widths[1] - 30.0).abs() < 1e-6, "id 1's 30x30 part should export at its own size, got {widths:?}");

        let _ = std::fs::remove_file(&out_path);
    }

    /// Regression test: a part's `texts` (carried through `PolygonDto` since
    /// import) must still be there after going through `export_dxf`'s own
    /// `PolygonDto -> LayeredPolygon` conversion and placement transform -
    /// not just at the lower `geometry::dxf_export` level (already covered
    /// there), but through this command's actual DTO boundary.
    #[test]
    fn export_dxf_command_carries_a_parts_texts_through_the_dto_boundary() {
        let mut part = square_dto(10.0);
        part.texts.push(TextDto { position: PointDto { x: 1.0, y: 1.0 }, rotation_deg: 0.0, height: 1.5, value: "LABEL".into(), is_multiline: false });

        let sheets = vec![square_dto(100.0)];
        let parts_by_id = HashMap::from([(0, part)]);
        let placements = vec![SheetPlacementDto { sheet_index: 0, parts: vec![PlacedPartDto { id: 0, x: 20.0, y: 0.0, rotation: 0.0, locked: false }] }];

        let out_path = std::env::temp_dir().join("rustynesting_export_dxf_text_dto_test.dxf");
        let export_request = ExportRequest { sheets, parts_by_id, placements, sheet_spacing: 20.0, include_sheet_outline: false, include_unplaced: false };
        export_dxf(out_path.to_str().unwrap(), export_request).expect("export should succeed");

        let drawing = Drawing::load_file(&out_path).expect("exported file should be a readable DXF");
        let texts: Vec<&dxf::entities::Text> =
            drawing.entities().filter_map(|e| if let dxf::entities::EntityType::Text(t) = &e.specific { Some(t) } else { None }).collect();
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].value, "LABEL");
        // local (1,1) shifted by the part's placement (20,0)
        assert!((texts[0].location.x - 21.0).abs() < 1e-9, "x was {}", texts[0].location.x);
        assert!((texts[0].location.y - 1.0).abs() < 1e-9, "y was {}", texts[0].location.y);

        let _ = std::fs::remove_file(&out_path);
    }

    #[test]
    fn export_dxf_omits_never_placed_parts_by_default() {
        // A single small sheet that can only fit one 10x10 part - the other
        // two (quantity 3 requested) can never be placed anywhere, since
        // there's only one sheet in the whole job.
        let sheets = vec![square_dto(12.0)];
        let parts = vec![part(square_dto(10.0), 3)];
        let request = RunNestRequest { sheets: sheets.clone(), parts, config: config(2) };
        let response = run_nest(request).expect("should nest successfully");
        let placed_count: usize = response.placements.iter().map(|sp| sp.parts.len()).sum();
        assert!(placed_count < 3, "expected fewer than 3 of 3 parts placed on a single 12x12 sheet, got {placed_count}");

        let out_path = std::env::temp_dir().join("rustynesting_export_dxf_no_unplaced_test.dxf");
        let export_request = ExportRequest {
            sheets,
            parts_by_id: response.parts_by_id,
            placements: response.placements,
            sheet_spacing: 20.0,
            include_sheet_outline: false,
            include_unplaced: false,
        };
        export_dxf(out_path.to_str().unwrap(), export_request).expect("export should succeed");

        let drawing = Drawing::load_file(&out_path).expect("exported file should be a readable DXF");
        let polyline_count = drawing.entities().filter(|e| matches!(e.specific, dxf::entities::EntityType::LwPolyline(_))).count();
        assert_eq!(polyline_count, placed_count, "only placed parts should be written when include_unplaced is false (the default)");

        let _ = std::fs::remove_file(&out_path);
    }

    #[test]
    fn export_dxf_can_include_never_placed_parts_when_requested() {
        let sheets = vec![square_dto(12.0)];
        let parts = vec![part(square_dto(10.0), 3)];
        let request = RunNestRequest { sheets: sheets.clone(), parts, config: config(2) };
        let response = run_nest(request).expect("should nest successfully");
        assert!(response.unplaced_count > 0, "expected at least one unplaceable part on a single 12x12 sheet with three 10x10 parts");

        let out_path = std::env::temp_dir().join("rustynesting_export_dxf_with_unplaced_test.dxf");
        let export_request = ExportRequest {
            sheets,
            parts_by_id: response.parts_by_id,
            placements: response.placements,
            sheet_spacing: 20.0,
            include_sheet_outline: false,
            include_unplaced: true,
        };
        export_dxf(out_path.to_str().unwrap(), export_request).expect("export should succeed");

        let drawing = Drawing::load_file(&out_path).expect("exported file should be a readable DXF");
        let polyline_count = drawing.entities().filter(|e| matches!(e.specific, dxf::entities::EntityType::LwPolyline(_))).count();
        assert_eq!(polyline_count, 3, "all 3 parts (placed + packed-unplaced) should be written when include_unplaced is set");

        let _ = std::fs::remove_file(&out_path);
    }

    #[test]
    fn export_svg_command_writes_a_readable_svg_file() {
        let sheets = vec![square_dto(100.0)];
        let parts = vec![part(square_dto(10.0), 2)];
        let request = RunNestRequest { sheets: sheets.clone(), parts, config: config(2) };
        let response = run_nest(request).expect("should nest successfully");

        let out_path = std::env::temp_dir().join("rustynesting_export_svg_test.svg");
        let export_request = ExportRequest {
            sheets,
            parts_by_id: response.parts_by_id,
            placements: response.placements,
            sheet_spacing: 20.0,
            include_sheet_outline: true,
            include_unplaced: false,
        };
        export_svg(out_path.to_str().unwrap(), export_request).expect("export should succeed");

        let contents = std::fs::read_to_string(&out_path).expect("exported file should be readable");
        assert!(contents.starts_with("<?xml"), "should be a well-formed XML document: {contents}");
        assert_eq!(contents.matches("<path").count(), 3, "1 sheet outline + 2 placed parts");

        let _ = std::fs::remove_file(&out_path);
    }

    /// End-to-end through the real importer, on the same fixture
    /// `geometry`'s own `dxf_fixtures.rs` validates: layer identity and the
    /// hole tree have to survive the DTO boundary too, not just the geometry
    /// crate's internal types.
    #[test]
    fn import_dxf_reads_a_real_fixture_with_its_holes_and_layers_intact() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/two.dxf");
        let polygons = import_dxf(path, 0.3).expect("fixture should parse");
        assert_eq!(polygons.len(), 4, "expected 4 outer parts, got {}", polygons.len());
        let holes: usize = polygons.iter().map(|p| p.children.len()).sum();
        assert_eq!(holes, 12, "the drilled circles must arrive as children, not as parts");
        assert!(polygons.iter().flat_map(|p| &p.children).any(|c| c.layer == "VISIBLE"), "layer identity must survive import");
    }

    /// Regression test for a real low-density job clustering in an
    /// arbitrary sheet corner instead of the origin - see
    /// `nesting::placement`'s `FIRST_PART_CONTACT_TOLERANCE` doc comment for
    /// the root cause (the sheet's first part, under a TightFit-family
    /// placement type, used to pick whichever rotation/corner had the
    /// single highest raw border-contact score, with no origin preference
    /// unless two candidates tied exactly). 20 real, irregular parts on a
    /// 500x500 sheet - before the fix, this fixture's whole cluster landed
    /// at x=[328,500]/y=[304,500], nowhere near the origin.
    ///
    /// This fixture draws the sheet AND all 20 parts already positioned
    /// inside the sheet's own outline (a reference layout for comparison,
    /// not a "here are 21 separate shapes, assign roles yourself" import) -
    /// import_dxf's containment-based tree-building (build_polygon_tree)
    /// would treat every part as a *hole* of the sheet polygon since each
    /// one is geometrically inside it, collapsing "1 sheet + 20 parts" down
    /// to a single polygon with 20 children. Bypassed here by reading the
    /// flat, pre-tree entity list directly instead - this test cares about
    /// placement quality, not import behavior.
    #[test]
    // ponytail: `supernesting 20part 500x500.dxf` was never committed to this
    // repo (only 17MM/FLAT*/hat-monotile* ever were) and isn't recoverable from
    // git history, so this test cannot run on a clean checkout. Left in place,
    // ignored, rather than deleted - drop the file into `tests/fixtures/` and
    // remove the attribute to get it back.
    #[ignore = "needs tests/fixtures/supernesting 20part 500x500.dxf, which is not in the repo"]
    fn run_nest_anchors_a_low_density_job_near_the_sheet_origin() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/supernesting 20part 500x500.dxf");
        let drawing = dxf::Drawing::load_file(path).expect("fixture should parse");
        let flat = geometry::dxf_import::entities_to_polygons(drawing.entities(), 0.3);

        let area = |pts: &[geometry::point::Point]| -> f64 {
            let mut a = 0.0;
            for j in 0..pts.len() {
                let k = (j + 1) % pts.len();
                a += pts[j].x * pts[k].y - pts[k].x * pts[j].y;
            }
            a.abs() / 2.0
        };
        let (sheet_idx, _) = flat.iter().enumerate().max_by(|(_, a), (_, b)| area(&a.points).total_cmp(&area(&b.points))).unwrap();
        let sheet = PolygonDto::from(&flat[sheet_idx]);
        let parts: Vec<PartDto> = flat
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != sheet_idx)
            .map(|(_, p)| part(PolygonDto::from(p), 1))
            .collect();

        let mut cfg = config(5);
        cfg.population_size = 10;
        cfg.rotations = 4;
        cfg.seed = 1;
        cfg.placement_type = PlacementTypeDto::GravityCorrective; // the GUI's actual default
        let request = RunNestRequest { sheets: vec![sheet], parts, config: cfg };

        let response = run_nest(request).expect("should nest");
        assert_eq!(response.unplaced_count, 0);

        // id `k` in placements maps back to flat[k] (parts was built by
        // enumerating flat, skipping sheet_idx, quantity 1 each - so
        // expand_parts's sequential id assignment lines up 1:1 with flat's
        // own index order).
        let min_x = response.placements[0]
            .parts
            .iter()
            .flat_map(|p| {
                let rad = p.rotation.to_radians();
                let (cos, sin) = (rad.cos(), rad.sin());
                flat[p.id].points.iter().map(move |pt| pt.x * cos - pt.y * sin + p.x)
            })
            .fold(f64::MAX, f64::min);
        let min_y = response.placements[0]
            .parts
            .iter()
            .flat_map(|p| {
                let rad = p.rotation.to_radians();
                let (cos, sin) = (rad.cos(), rad.sin());
                flat[p.id].points.iter().map(move |pt| pt.x * sin + pt.y * cos + p.y)
            })
            .fold(f64::MAX, f64::min);
        assert!(min_x < 10.0, "pack should start near the sheet's left edge, min_x was {min_x:.1}");
        assert!(min_y < 10.0, "pack should start near the sheet's top edge, min_y was {min_y:.1}");
    }

    /// Not a test - a one-off generator, run manually (`cargo test -p
    /// rustynesting --bin rustynesting generate_importable_supernesting_fixture
    /// -- --ignored --nocapture`), for a version of "supernesting 20part
    /// 500x500.dxf" that actually imports as 21 separate shapes instead of
    /// 1 shape with 20 holes - see `debug_real_import_of_supernesting_fixture`
    /// below for why the original doesn't: it draws every part already
    /// positioned *inside* the sheet's own outline, which the importer's
    /// (correct, for real drilled-hole parts) containment-based tree-
    /// building treats as holes of the sheet. Moves the same 20 part
    /// shapes into a grid well clear of the sheet instead, so BROWSE...
    /// produces a normal "assign SHEET/PART roles yourself" import.
    #[test]
    #[ignore = "needs tests/fixtures/supernesting 20part 500x500.dxf, which is not in the repo"]
    fn generate_importable_supernesting_fixture() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/supernesting 20part 500x500.dxf");
        let drawing = dxf::Drawing::load_file(path).expect("fixture should parse");
        let flat = geometry::dxf_import::entities_to_polygons(drawing.entities(), 0.3);

        let area = |pts: &[geometry::point::Point]| -> f64 {
            let mut a = 0.0;
            for j in 0..pts.len() {
                let k = (j + 1) % pts.len();
                a += pts[j].x * pts[k].y - pts[k].x * pts[j].y;
            }
            a.abs() / 2.0
        };
        let (sheet_idx, _) = flat.iter().enumerate().max_by(|(_, a), (_, b)| area(&a.points).total_cmp(&area(&b.points))).unwrap();

        let mut out = dxf::Drawing::new();
        out.header.version = dxf::enums::AcadVersion::R2000;

        let add_polyline = |out: &mut dxf::Drawing, layer: &str, points: &[(f64, f64)]| {
            let mut poly = dxf::entities::LwPolyline {
                vertices: points.iter().map(|&(x, y)| dxf::LwPolylineVertex { x, y, bulge: 0.0, ..Default::default() }).collect(),
                ..Default::default()
            };
            poly.set_is_closed(true);
            out.add_entity(dxf::entities::Entity {
                common: dxf::entities::EntityCommon { layer: layer.to_string(), ..Default::default() },
                specific: dxf::entities::EntityType::LwPolyline(poly),
            });
        };

        // The sheet, untouched.
        let sheet_points: Vec<(f64, f64)> = flat[sheet_idx].points.iter().map(|p| (p.x, p.y)).collect();
        add_polyline(&mut out, &flat[sheet_idx].layer, &sheet_points);

        // Every part, translated into a grid starting well clear of the
        // sheet's own [0,500]x[0,500] footprint (each part's own local
        // bounding box is roughly 33x45, so an 80x80 grid cell leaves
        // generous clearance).
        const COLS: usize = 5;
        const CELL: f64 = 80.0;
        const START_X: f64 = 600.0;
        let mut col = 0usize;
        let mut row = 0usize;
        for (i, p) in flat.iter().enumerate() {
            if i == sheet_idx {
                continue;
            }
            let bounds = geometry::polygon::get_polygon_bounds(&p.points).expect("part always has points");
            let dx = START_X + (col as f64) * CELL - bounds.x;
            let dy = (row as f64) * CELL - bounds.y;
            let points: Vec<(f64, f64)> = p.points.iter().map(|pt| (pt.x + dx, pt.y + dy)).collect();
            add_polyline(&mut out, &p.layer, &points);
            col += 1;
            if col >= COLS {
                col = 0;
                row += 1;
            }
        }

        let out_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/supernesting 20part 500x500 - importable.dxf");
        out.save_file(out_path).expect("should write fixture");
        eprintln!("wrote {out_path}");

        // Round-trip check: this must import as 21 separate top-level
        // shapes with no children, unlike the original.
        let reimported = import_dxf(out_path, 0.3).expect("generated fixture should parse");
        eprintln!("re-imported as {} top-level shape(s)", reimported.len());
        assert_eq!(reimported.len(), 21, "should be 1 sheet + 20 parts, all separate");
        assert!(reimported.iter().all(|p| p.children.is_empty()), "none of these should have been swallowed as holes");
    }

    /// Documents real, correct-but-surprising behavior: "supernesting
    /// 20part 500x500.dxf" (a reference/comparison layout, parts drawn
    /// already positioned *inside* the sheet's own outline) imports as a
    /// *single* shape with 20 children, not 21 separate shapes -
    /// `build_polygon_tree`'s containment-based hole detection is exactly
    /// what real drilled-hole parts need, and can't distinguish "this
    /// contained shape is a manufacturing hole" from "this contained shape
    /// is actually a separate part that happens to be drawn overlapping the
    /// sheet." See `generate_importable_supernesting_fixture` above for a
    /// version of this same geometry that imports as 21 separate shapes.
    #[test]
    // ponytail: `supernesting 20part 500x500.dxf` was never committed to this
    // repo (only 17MM/FLAT*/hat-monotile* ever were) and isn't recoverable from
    // git history, so this test cannot run on a clean checkout. Left in place,
    // ignored, rather than deleted - drop the file into `tests/fixtures/` and
    // remove the attribute to get it back.
    #[ignore = "needs tests/fixtures/supernesting 20part 500x500.dxf, which is not in the repo"]
    fn import_dxf_treats_parts_drawn_inside_the_sheet_outline_as_its_holes() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/supernesting 20part 500x500.dxf");
        let polygons = import_dxf(path, 0.3).expect("fixture should parse");
        assert_eq!(polygons.len(), 1, "the 20 parts should have collapsed into the sheet's own children, not stayed separate");
        assert_eq!(polygons[0].children.len(), 20);
    }

    #[test]
    fn import_dxf_reports_a_missing_file_as_an_error_not_a_panic() {
        assert!(import_dxf("does-not-exist.dxf", 0.3).is_err());
    }

    /// End-to-end regression test for the "text is silently removed" bug:
    /// a real DXF file with a closed profile plus a `TEXT` entity inside it
    /// must come back from `import_dxf` with that text attached to the
    /// profile's `PolygonDto`, not dropped on the floor.
    #[test]
    fn import_dxf_attaches_a_text_entity_to_its_containing_profile() {
        use dxf::entities::{Entity, EntityCommon, EntityType, LwPolyline, Text};
        use dxf::{Drawing as DxfDrawing, LwPolylineVertex, Point as DxfPoint};

        let mut drawing = DxfDrawing::new();
        drawing.header.version = dxf::enums::AcadVersion::R2000;

        let mut poly = LwPolyline {
            vertices: vec![
                LwPolylineVertex { x: 0.0, y: 0.0, bulge: 0.0, ..Default::default() },
                LwPolylineVertex { x: 20.0, y: 0.0, bulge: 0.0, ..Default::default() },
                LwPolylineVertex { x: 20.0, y: 20.0, bulge: 0.0, ..Default::default() },
                LwPolylineVertex { x: 0.0, y: 20.0, bulge: 0.0, ..Default::default() },
            ],
            ..Default::default()
        };
        poly.set_is_closed(true);
        drawing.add_entity(Entity {
            common: EntityCommon { layer: "CUT".to_string(), ..Default::default() },
            specific: EntityType::LwPolyline(poly),
        });
        drawing.add_entity(Entity {
            common: EntityCommon { layer: "CUT".to_string(), ..Default::default() },
            specific: EntityType::Text(Text {
                location: DxfPoint::new(5.0, 5.0, 0.0),
                value: "PART-001".to_string(),
                text_height: 2.0,
                ..Default::default()
            }),
        });

        let out_path = std::env::temp_dir().join("rustynesting_import_dxf_text_test.dxf");
        drawing.save_file(out_path.to_str().unwrap()).expect("should write test fixture");

        let polygons = import_dxf(out_path.to_str().unwrap(), 0.3).expect("fixture should parse");
        let _ = std::fs::remove_file(&out_path);

        assert_eq!(polygons.len(), 1);
        assert_eq!(polygons[0].texts.len(), 1, "the TEXT entity inside the profile should be attached to it");
        assert_eq!(polygons[0].texts[0].value, "PART-001");
        assert_eq!(polygons[0].texts[0].position.x, 5.0);
        assert_eq!(polygons[0].texts[0].position.y, 5.0);
    }

    #[test]
    fn is_better_result_prefers_fewer_unplaced_parts_above_all_else() {
        assert!(is_better_result(0, 10, 50.0, 1, 3, 99.0));
        assert!(!is_better_result(1, 3, 99.0, 0, 10, 50.0));
    }

    #[test]
    fn is_better_result_then_prefers_fewer_sheets() {
        assert!(is_better_result(0, 3, 50.0, 0, 5, 99.0));
        assert!(!is_better_result(0, 5, 99.0, 0, 3, 50.0));
    }

    #[test]
    fn is_better_result_finally_prefers_higher_utilisation() {
        assert!(is_better_result(0, 3, 91.0, 0, 3, 90.0));
        assert!(!is_better_result(0, 3, 90.0, 0, 3, 90.0));
    }

    /// `config.mirror`'s plumbing end to end: mirrored variants have to be
    /// reachable by the search (some placement lands on a `MIRROR_ID_BIT`
    /// id), have to come with geometry the caller can export/render, and
    /// must not turn one part into two. Off, no id may carry the bit.
    /// (That a mirrored *shape* is geometrically right is
    /// `geometry::dxf_import`'s own `mirroring_preserves_arcs_and_winding`.)
    #[test]
    fn mirror_variants_are_reachable_without_duplicating_parts() {
        let l_shape = PolygonDto {
            points: [(0.0, 0.0), (30.0, 0.0), (30.0, 10.0), (10.0, 10.0), (10.0, 25.0), (0.0, 25.0)]
                .iter()
                .map(|&(x, y)| PointDto { x, y })
                .collect(),
            layer: "0".into(),
            is_circle: None,
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        };
        let build = |mirror: bool| {
            let mut cfg = config(6);
            cfg.rotations = 2;
            cfg.mirror = mirror;
            RunNestRequest { sheets: vec![square_dto(200.0)], parts: vec![part(l_shape.clone(), 6)], config: cfg }
        };

        let off = run_nest(build(false)).expect("nest without mirror");
        let placed_off: Vec<usize> = off.placements.iter().flat_map(|s| s.parts.iter().map(|p| p.id)).collect();
        use nesting::dispatch::MIRROR_ID_BIT;
        assert!(placed_off.iter().all(|id| id & MIRROR_ID_BIT == 0), "mirror off must never place a flipped variant");

        let on = run_nest(build(true)).expect("nest with mirror");
        let placed_on: Vec<usize> = on.placements.iter().flat_map(|s| s.parts.iter().map(|p| p.id)).collect();
        assert_eq!(placed_on.len() + on.unplaced_ids.len(), 6, "mirroring must not turn one part into two");
        assert!(placed_on.iter().any(|id| id & MIRROR_ID_BIT != 0), "mirror on must actually reach the flipped variants");
        for id in &placed_on {
            assert!(on.parts_by_id.contains_key(id), "every placed id (flipped or not) needs geometry for export/render");
        }
    }

    /// "Flip allowed" must not mean "flip-free never tried": the first of
    /// several runs stays un-mirrored so the escalation has a real baseline
    /// to compare against - unless there is only one run to give.
    #[test]
    fn mirror_leaves_the_first_run_of_several_un_mirrored() {
        let base = GaConfig { population_size: 6, mutation_rate: 10.0, rotations: 2, mirror: true, part_rules: Default::default() };
        let mirrors: Vec<bool> = (0..3).map(|i| escalated_run_config(&base, 5, i, 3).0.mirror).collect();
        assert_eq!(mirrors, vec![false, true, true]);
        assert!(escalated_run_config(&base, 5, 0, 1).0.mirror, "a single-run job must still honour the setting");

        let off = GaConfig { mirror: false, ..base };
        assert!((0..3).all(|i| !escalated_run_config(&off, 5, i, 3).0.mirror));
    }

    /// Per-part mirror override, both directions, in one job - the whole
    /// point of the feature: a grain-critical part must not be flipped even
    /// when the job allows flipping, and vice versa.
    #[test]
    fn a_parts_own_mirror_setting_overrides_the_job_wide_switch() {
        use nesting::dispatch::MIRROR_ID_BIT;

        let l_shape = |scale: f64| PolygonDto {
            points: [(0.0, 0.0), (30.0, 0.0), (30.0, 10.0), (10.0, 10.0), (10.0, 25.0), (0.0, 25.0)]
                .iter()
                .map(|&(x, y)| PointDto { x: x * scale, y: y * scale })
                .collect(),
            layer: "0".into(),
            is_circle: None,
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        };

        // Job-wide mirroring ON, but part 0 opts out.
        let mut cfg = config(6);
        cfg.rotations = 2;
        cfg.mirror = true;
        let request = RunNestRequest {
            sheets: vec![square_dto(200.0)],
            parts: vec![
                PartDto { polygon: l_shape(1.0), quantity: 4, allowed_rotations: None, mirror: Some(false) },
                PartDto { polygon: l_shape(0.8), quantity: 4, allowed_rotations: None, mirror: None },
            ],
            config: cfg,
        };
        let response = run_nest(request).expect("nests");

        // Ids 0..3 are the opted-out part (expand_parts assigns sequentially
        // in definition order), 4..7 the free one.
        let placed: Vec<usize> = response.placements.iter().flat_map(|s| s.parts.iter().map(|p| p.id)).collect();
        assert!(!placed.is_empty());
        for id in &placed {
            if id & !MIRROR_ID_BIT < 4 {
                assert_eq!(id & MIRROR_ID_BIT, 0, "part {id} opted out of mirroring but was placed flipped");
            }
        }
        // The opted-out part has no mirrored geometry registered at all -
        // there is nothing for a stray gene to reach.
        assert!(
            response.parts_by_id.keys().all(|id| !(id & MIRROR_ID_BIT != 0 && id & !MIRROR_ID_BIT < 4)),
            "no mirrored variant should exist for the opted-out part"
        );
        // ...while the free part does have them.
        assert!(response.parts_by_id.keys().any(|id| id & MIRROR_ID_BIT != 0), "the un-opted-out part should still get mirrored variants");
    }

    #[test]
    fn a_part_can_opt_into_mirroring_when_the_job_wide_switch_is_off() {
        use nesting::dispatch::MIRROR_ID_BIT;
        let mut cfg = config(3);
        cfg.mirror = false;
        let request = RunNestRequest {
            sheets: vec![square_dto(200.0)],
            parts: vec![
                PartDto { polygon: square_dto(20.0), quantity: 2, allowed_rotations: None, mirror: Some(true) },
                part(square_dto(15.0), 2),
            ],
            config: cfg,
        };
        let response = run_nest(request).expect("nests");
        let mirrored: Vec<usize> = response.parts_by_id.keys().copied().filter(|id| id & MIRROR_ID_BIT != 0).collect();
        assert!(!mirrored.is_empty(), "the opted-in part should have mirrored variants");
        assert!(mirrored.iter().all(|id| id & !MIRROR_ID_BIT < 2), "only the opted-in part, got {mirrored:?}");
    }

    /// Grain direction end to end: whatever the search does, the constrained
    /// part may only come to rest at an angle it was allowed.
    #[test]
    fn allowed_rotations_are_honoured_end_to_end_and_reported_back() {
        let mut cfg = config(6);
        cfg.rotations = 8;
        let request = RunNestRequest {
            sheets: vec![square_dto(300.0)],
            parts: vec![
                PartDto { polygon: rect_dto(80.0, 20.0), quantity: 3, allowed_rotations: Some(vec![0.0, 180.0]), mirror: None },
                part(rect_dto(40.0, 40.0), 2),
            ],
            config: cfg,
        };
        let response = run_nest(request).expect("nests");

        for sheet in &response.placements {
            for placed in &sheet.parts {
                if placed.id < 3 {
                    assert!(placed.rotation == 0.0 || placed.rotation == 180.0, "grain-locked part {} placed at {}", placed.id, placed.rotation);
                }
            }
        }

        // The rules come back with the result so a later repack is held to
        // the same constraints - see RepackSheetRequest::part_rules.
        assert_eq!(response.part_rules.len(), 3, "one entry per constrained copy");
        for id in 0..3 {
            assert_eq!(response.part_rules[&id].angles, vec![0.0, 180.0]);
        }
        assert!(!response.part_rules.contains_key(&3), "the unconstrained part needs no entry");
    }

    #[test]
    fn authored_angles_are_normalised_and_a_degenerate_list_means_unconstrained() {
        // Out of order, out of range, duplicated, and a stray non-finite.
        let parts = vec![PartDto {
            polygon: square_dto(10.0),
            quantity: 1,
            allowed_rotations: Some(vec![540.0, -90.0, 180.0, 180.0, f64::NAN]),
            mirror: None,
        }];
        let expanded = expand_parts(parts, false);
        assert_eq!(expanded.part_rules[&0].angles, vec![180.0, 270.0], "540 -> 180 (deduped), -90 -> 270, NaN dropped");

        // An explicitly empty list is not "this part may never be placed".
        let parts = vec![PartDto { polygon: square_dto(10.0), quantity: 1, allowed_rotations: Some(Vec::new()), mirror: None }];
        assert!(expand_parts(parts, false).part_rules.is_empty(), "an empty allow-list means unconstrained, not unplaceable");
    }

    // --- drag / lock / re-nest ------------------------------------------

    fn validate_request(spacing: f64, x: f64, y: f64) -> ValidatePlacementRequest {
        let mut cfg = config(1);
        cfg.spacing = spacing;
        ValidatePlacementRequest {
            sheet: square_dto(100.0),
            placement: SheetPlacementDto {
                sheet_index: 0,
                parts: vec![
                    PlacedPartDto { id: 0, x: 0.0, y: 0.0, rotation: 0.0, locked: false },
                    PlacedPartDto { id: 1, x: 50.0, y: 0.0, rotation: 0.0, locked: false },
                ],
            },
            parts_by_id: HashMap::from([(0, square_dto(20.0)), (1, square_dto(20.0))]),
            moved_id: 1,
            x,
            y,
            rotation: 0.0,
            config: cfg,
        }
    }

    #[test]
    fn a_hand_dragged_part_is_judged_by_the_same_rules_the_engine_uses() {
        // Clear of its neighbour and inside the sheet: fine.
        assert!(validate_placement(validate_request(0.0, 50.0, 50.0)).unwrap().valid);

        // Dropped straight on top of part 0: rejected.
        assert!(!validate_placement(validate_request(0.0, 0.0, 0.0)).unwrap().valid, "an overlapping drop must be rejected");

        // Partly off the sheet: rejected.
        assert!(!validate_placement(validate_request(0.0, 95.0, 50.0)).unwrap().valid, "a part hanging off the sheet edge must be rejected");
    }

    #[test]
    fn drag_validation_respects_the_runs_own_spacing() {
        // Part 0 occupies 0..20. Sitting at x=25 is a 5mm gap: legal with no
        // spacing configured, illegal once the job demands 10mm between
        // parts. Same padded geometry the nest itself places against.
        assert!(validate_placement(validate_request(0.0, 25.0, 0.0)).unwrap().valid);
        assert!(!validate_placement(validate_request(10.0, 25.0, 0.0)).unwrap().valid, "a gap under the configured spacing must be rejected");
    }

    #[test]
    fn a_part_is_never_an_obstacle_to_itself() {
        // Dropping part 1 exactly where it already is must stay legal.
        let request = validate_request(0.0, 50.0, 0.0);
        assert!(validate_placement(request).unwrap().valid);
    }

    #[test]
    fn drag_validation_rejects_an_unknown_part_rather_than_panicking() {
        let mut request = validate_request(0.0, 10.0, 10.0);
        request.moved_id = 99;
        assert!(validate_placement(request).is_err());
    }

    #[test]
    fn repacking_a_sheet_leaves_every_locked_part_untouched_and_reports_them_back() {
        let request = RepackSheetRequest {
            sheet: square_dto(120.0),
            placement: SheetPlacementDto {
                sheet_index: 4,
                parts: vec![
                    PlacedPartDto { id: 0, x: 33.5, y: 61.25, rotation: 0.0, locked: true },
                    PlacedPartDto { id: 1, x: 0.0, y: 0.0, rotation: 0.0, locked: false },
                    PlacedPartDto { id: 2, x: 0.0, y: 40.0, rotation: 0.0, locked: false },
                ],
            },
            parts_by_id: HashMap::from([(0, rect_dto(70.0, 25.0)), (1, rect_dto(50.0, 35.0)), (2, rect_dto(30.0, 30.0))]),
            config: config(6),
            part_rules: HashMap::new(),
        };
        let response = repack_sheet(request).expect("repacks around the pinned part");

        assert_eq!(response.placement.sheet_index, 4, "the real sheet index is restored");
        assert_eq!(response.placement.parts.len(), 3, "nothing may be dropped");
        let locked = response.placement.parts.iter().find(|p| p.id == 0).unwrap();
        assert_eq!((locked.x, locked.y), (33.5, 61.25), "a pinned part must not move");
        assert!(locked.locked, "and must come back still pinned, so the next repack sees it too");
        assert!(response.placement.parts.iter().filter(|p| p.id != 0).all(|p| !p.locked));
    }

    // --- PDF job report --------------------------------------------------

    #[test]
    fn export_report_writes_a_pdf_whose_numbers_match_the_result_it_drew() {
        let dir = std::env::temp_dir().join("rustynesting-report-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report.pdf");

        // Two 10mm parts on a 100mm sheet: 200 of 10000 = 2%.
        let request = ReportRequest {
            export: ExportRequest {
                sheets: vec![square_dto(100.0)],
                parts_by_id: HashMap::from([(0, square_dto(10.0)), (1, square_dto(10.0))]),
                placements: vec![SheetPlacementDto {
                    sheet_index: 0,
                    parts: vec![
                        PlacedPartDto { id: 0, x: 0.0, y: 0.0, rotation: 0.0, locked: false },
                        PlacedPartDto { id: 1, x: 20.0, y: 0.0, rotation: 0.0, locked: false },
                    ],
                }],
                sheet_spacing: 10.0,
                include_sheet_outline: true,
                include_unplaced: true, // must be forced off by the command
            },
            config: config(1),
            parts: vec![ReportPartDto { name: "bracket".into(), quantity: 2 }],
            title: Some("Job 42".into()),
        };
        export_report(path.to_str().unwrap(), request).expect("writes a report");

        let text = std::fs::read_to_string(&path).expect("the report is ASCII-only by design");
        assert!(text.starts_with("%PDF-"));
        assert!(text.trim_end().ends_with("%%EOF"));
        assert!(text.contains("Job 42"), "the caller's title is the heading");
        assert!(text.contains("Utilisation: 2.0%"), "utilisation is measured off the drawn geometry");
        assert!(text.contains("Pieces placed: 2 of 2"));
        assert!(text.contains("bracket"), "the piece table is printed");
        assert!(text.contains("Spacing"), "the settings the run used are printed");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn export_report_rejects_the_same_bad_input_the_other_exporters_do() {
        let dir = std::env::temp_dir();
        let request = ReportRequest {
            export: ExportRequest {
                sheets: vec![square_dto(100.0)],
                parts_by_id: HashMap::new(),
                placements: vec![SheetPlacementDto { sheet_index: 0, parts: vec![PlacedPartDto { id: 9, x: 0.0, y: 0.0, rotation: 0.0, locked: false }] }],
                sheet_spacing: 0.0,
                include_sheet_outline: true,
                include_unplaced: false,
            },
            config: config(1),
            parts: Vec::new(),
            title: None,
        };
        // Same "placement references unknown part id" guard build_export_layouts
        // already enforces for DXF/SVG - the report reuses it verbatim.
        assert!(export_report(dir.join("never-written.pdf").to_str().unwrap(), request).is_err());
    }
}
