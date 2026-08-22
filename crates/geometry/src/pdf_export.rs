//! Writes a nest result out as a printable PDF job report: a summary page
//! (totals, per-sheet table, the settings the run used) followed by one
//! to-scale page per sheet showing the actual layout with part labels.
//!
//! **Hand-rolled, no PDF crate.** The obvious candidate (`printpdf`) resolves
//! to a 30-plus-crate tree - font shaping, image codecs, a vector-graphics
//! engine, an RNG - for a page that strokes polylines and writes a few lines
//! of Helvetica. What this module actually needs is a subset of PDF 1.4 that
//! has not changed since 2001: a content stream of `m`/`l`/`h`/`S` path
//! operators, `BT`/`Tf`/`Td`/`Tj`/`ET` for text, and one of the 14 standard
//! fonts, which need no embedding. That is the ~200 lines below.
//!
//! ponytail: WinAnsi + the built-in Helvetica means **the report is
//! English-only** - a Vietnamese UI string or a non-ASCII layer name is
//! transliterated to `?` rather than rendered (see `pdf_string`). Embedding a
//! TrueType font with a real Unicode CMap is the upgrade path, and roughly
//! triples this file; not worth it until someone actually needs it.
//!
//! Parts are labelled by their position on the sheet (`#1`, `#2`, ...), which
//! is what someone matching a printed page against a pile of cut parts needs;
//! see `sheet_page` for why neither the layer name nor the run's internal
//! part id is used.
//!
//! Geometry comes in as the same `SheetLayout`/`PlacedShape` values
//! `dxf_export`/`svg_export` take, straight from the caller's
//! `build_export_layouts`, so the drawn page can never disagree with the
//! exported DXF.

use std::fmt::Write as _;

use crate::dxf_export::{PlacedShape, SheetLayout};
use crate::dxf_import::{polygon_material_area, rotate_layered_polygon, shift_layered_polygon, LayeredPolygon};
use crate::point::Point;
use crate::polygon::{get_polygon_bounds, polygon_area};

/// A4 landscape in PostScript points (1/72 inch), the unit PDF's default
/// coordinate system uses.
const PAGE_W: f64 = 841.89;
const PAGE_H: f64 = 595.28;
const MARGIN: f64 = 40.0;

/// One line of the report's part table.
///
/// Only the three things this module cannot know: what the piece is called,
/// how many were ordered, and how many the result placed. `nested` has to be
/// told because a `PlacedShape` carries no part identity by the time the
/// report draws it; the caller knows, because it handed out the ids. Size,
/// area, contour count and cut length are all measured off `shape` here.
#[derive(Clone, Debug)]
pub struct ReportPart {
    pub name: String,
    /// How many were ordered.
    pub quantity: usize,
    /// How many of them the result actually placed.
    pub nested: usize,
    /// The piece itself, holes included, at its original orientation.
    pub shape: LayeredPolygon,
}

/// Everything the report says that isn't derivable from the drawn geometry.
/// Anything that *is* derivable is computed from the layouts themselves (see
/// `export_report`), so the printed numbers can never disagree with the
/// printed picture.
#[derive(Clone, Debug)]
pub struct ReportMeta {
    pub title: String,
    /// When the report was produced, already formatted. `geometry` owns no
    /// clock and has no business knowing the user's timezone.
    pub generated: String,
    /// The job's inter-part clearance, in mm. Needed to work out the offcuts:
    /// an offcut is only real material if it is at least a cut's width away
    /// from every piece around it.
    pub spacing: f64,
    pub parts: Vec<ReportPart>,
    /// `(label, value)` pairs, printed verbatim - the caller decides which
    /// settings are worth showing rather than this module knowing about
    /// `NestConfigDto`.
    pub settings: Vec<(String, String)>,
    /// The manufacturability verdict, if one was run. `None` prints nothing
    /// at all rather than an implied pass - a report that says "PASSED"
    /// about a nest nobody checked is worse than one that stays silent.
    #[allow(clippy::struct_field_names)]
    pub audit: Option<ReportAudit>,
}

/// The audit verdict as the report prints it.
///
/// Deliberately pre-rendered strings rather than the `nesting::audit` types:
/// `geometry` must not depend on `nesting`, and the report only ever prints
/// this - it never reasons about an issue's kind. `passed` stays a bool
/// because the heading is the one part that is not free text.
#[derive(Clone, Debug)]
pub struct ReportAudit {
    pub passed: bool,
    /// One-line verdict, e.g. "PASSED - no overlaps, all pieces on the sheet".
    pub headline: String,
    /// Individual findings, already formatted. Truncated by the caller.
    pub issues: Vec<String>,
}

/// Renders the report and returns the PDF bytes.
#[must_use]
pub fn export_report(layouts: &[SheetLayout], meta: &ReportMeta) -> Vec<u8> {
    let mut pages: Vec<String> = Vec::new();

    let stats: Vec<SheetStats> = layouts.iter().map(sheet_stats).collect();
    pages.extend(summary_pages(meta, layouts, &stats));
    for (index, layout) in layouts.iter().enumerate() {
        pages.push(sheet_page(layout, index, &stats[index]));
    }

    assemble(&pages)
}

struct SheetStats {
    parts: usize,
    sheet_area: f64,
    used_area: f64,
}

impl SheetStats {
    fn utilisation(&self) -> f64 {
        if self.sheet_area > 0.0 {
            self.used_area / self.sheet_area * 100.0
        } else {
            0.0
        }
    }
}

/// Measured off the very geometry being drawn, not from a parallel totals
/// computation - a report whose numbers and picture can disagree is worse
/// than no report.
fn sheet_stats(layout: &SheetLayout) -> SheetStats {
    SheetStats {
        parts: layout.parts.len(),
        sheet_area: polygon_area(&layout.sheet.points).abs(),
        used_area: layout.parts.iter().map(|p| polygon_material_area(&placed_geometry(p))).sum(),
    }
}

fn placed_geometry(part: &PlacedShape) -> LayeredPolygon {
    shift_layered_polygon(&rotate_layered_polygon(&part.shape, part.rotation), part.x, part.y)
}

// --- content streams ---------------------------------------------------

/// A paginating content-stream builder.
///
/// The summary is four stacked tables whose lengths all come from the job -
/// fifteen sheets and forty part types do not fit on one page, and silently
/// running off the bottom is the failure mode a report must not have. `ensure`
/// starts a fresh page when the next block would not fit; everything else just
/// writes and lets the cursor fall.
struct Doc {
    pages: Vec<String>,
    cur: String,
    y: f64,
}

/// Where a page's cursor starts.
const TOP: f64 = PAGE_H - MARGIN - 18.0;

impl Doc {
    fn new() -> Self {
        Self { pages: Vec::new(), cur: String::new(), y: TOP }
    }

    fn ensure(&mut self, space: f64) {
        if self.y - space < MARGIN {
            self.pages.push(std::mem::take(&mut self.cur));
            self.y = TOP;
        }
    }

    fn line(&mut self, value: &str, size: f64) {
        self.ensure(size + 5.0);
        text(&mut self.cur, value, MARGIN, self.y, size);
        self.y -= size + 5.0;
    }

    /// A section title with a rule under it. Reserves enough room that a
    /// heading can never be the last thing on a page with its table overleaf.
    fn heading(&mut self, value: &str) {
        self.ensure(60.0);
        self.y -= 10.0;
        text(&mut self.cur, value, MARGIN, self.y, 12.0);
        self.y -= 5.0;
        self.rule();
        self.y -= 13.0;
    }

    /// One table row. `cells` are `(x offset from the left margin, text)`.
    fn row(&mut self, cells: &[(f64, String)], size: f64) {
        self.ensure(size + 4.0);
        for (x, value) in cells {
            text(&mut self.cur, value, MARGIN + x, self.y, size);
        }
        self.y -= size + 4.0;
    }

    fn rule(&mut self) {
        let _ = writeln!(
            self.cur,
            "0.6 0.6 0.6 RG 0.5 w {:.2} {:.2} m {:.2} {:.2} l S 0 0 0 RG",
            MARGIN,
            self.y,
            PAGE_W - MARGIN,
            self.y
        );
    }

    fn finish(mut self) -> Vec<String> {
        self.pages.push(self.cur);
        self.pages
    }
}

/// Column layouts, as x offsets in points from the left margin. Mirrors the
/// three tables a SuperNesting job report prints, in its column order, so the
/// two can be read side by side.
const PART_COLS: &[f64] = &[0.0, 150.0, 215.0, 270.0, 320.0, 395.0, 470.0, 555.0, 650.0];
const NEST_COLS: &[f64] = &[0.0, 90.0, 170.0, 250.0, 330.0, 420.0, 510.0, 610.0];
const REMNANT_COLS: &[f64] = &[0.0, 90.0, 180.0, 250.0, 360.0, 470.0];

fn cells<const N: usize>(cols: &[f64], values: [String; N]) -> Vec<(f64, String)> {
    cols.iter().copied().zip(values).collect()
}

/// How many separate closed contours a piece has - its outline plus every
/// hole, all the way down. This is what a machine actually cuts, and it is the
/// column SuperNesting calls `CtrQty`.
fn contour_count(shape: &LayeredPolygon) -> usize {
    1 + shape.children.iter().map(contour_count).sum::<usize>()
}

/// Total length of every contour, in millimetres - outline and holes. Cut time
/// tracks this far more closely than it tracks area, which is why the
/// reference report carries it on every row.
fn cut_length(shape: &LayeredPolygon) -> f64 {
    let outline: f64 = (0..shape.points.len())
        .map(|i| shape.points[i].distance_to(shape.points[(i + 1) % shape.points.len()]))
        .sum();
    outline + shape.children.iter().map(cut_length).sum::<f64>()
}

/// One row of the nests table: a distinct layout, and how many sheets are cut
/// from it.
struct NestGroup {
    /// Index into `stats` of the first sheet with this layout.
    first: usize,
    duplicate: usize,
}

/// Groups sheets that are laid out identically, so the report can say "cut
/// this one three times" instead of listing three indistinguishable rows.
///
/// This is the column the reference tool's own report leads with, and it is
/// also the measurement for `PLAN.md` 1.2 - a nester that finds one good
/// arrangement and reuses it produces few groups with high duplicate counts,
/// and one that re-solves every sheet from scratch produces a wall of ones.
///
/// Two sheets match when their outlines match and every piece sits in the same
/// place, rounded to 0.1mm - far below any tolerance that matters, and well
/// above the float noise two independently-computed placements carry.
fn nest_groups(layouts: &[SheetLayout]) -> Vec<NestGroup> {
    fn signature(layout: &SheetLayout) -> String {
        let mut parts: Vec<String> = layout
            .parts
            .iter()
            .map(|p| format!("{:.1}/{:.1}/{:.1}/{:.0}", p.x, p.y, p.rotation, polygon_area(&p.shape.points).abs()))
            .collect();
        // Sorted: the same arrangement reached in a different placement order
        // is the same pattern to whoever has to cut it.
        parts.sort();
        let size = get_polygon_bounds(&layout.sheet.points).map_or_else(String::new, |b| format!("{:.1}x{:.1}", b.width, b.height));
        format!("{size}|{}", parts.join(";"))
    }

    let mut groups: Vec<NestGroup> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (index, layout) in layouts.iter().enumerate() {
        let sig = signature(layout);
        if let Some(at) = seen.iter().position(|s| *s == sig) {
            groups[at].duplicate += 1;
        } else {
            seen.push(sig);
            groups.push(NestGroup { first: index, duplicate: 1 });
        }
    }
    groups
}

/// `1234567.8` -> `1 234 568`. Areas in mm2 run to seven digits on a real
/// sheet, and an ungrouped run of digits is genuinely hard to read off a
/// printed page.
fn grouped(value: f64) -> String {
    let digits = format!("{:.0}", value.max(0.0));
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// What one sheet has left over once every piece on it is cut.
struct Offcut {
    /// How many separate leftover pieces the sheet breaks into.
    count: usize,
    /// The biggest one's largest inscribed rectangle - the size worth writing
    /// on a label.
    usable_w: f64,
    usable_h: f64,
    /// True free area across every leftover piece on the sheet.
    reclaimable: f64,
}

/// Measures the offcuts on one sheet with `remnant::sheet_remnants`, the same
/// call the offcut shelf itself uses - so what the report promises is exactly
/// what the library will offer back on the next job, not a second estimate of
/// it.
fn sheet_offcut(layout: &SheetLayout, spacing: f64) -> Offcut {
    let placed: Vec<crate::remnant::PlacedOutline> = layout.parts.iter().map(|p| placed_geometry(p).points).collect();
    let remnants = crate::remnant::sheet_remnants(&layout.sheet.points, &placed, spacing);
    let reclaimable = remnants.iter().map(|r| r.area).sum();
    let largest = remnants.iter().max_by(|a, b| a.area.total_cmp(&b.area));
    Offcut {
        count: remnants.len(),
        usable_w: largest.map_or(0.0, |r| r.usable.width),
        usable_h: largest.map_or(0.0, |r| r.usable.height),
        reclaimable,
    }
}

fn summary_pages(meta: &ReportMeta, layouts: &[SheetLayout], stats: &[SheetStats]) -> Vec<String> {
    let mut doc = Doc::new();

    let total_sheet: f64 = stats.iter().map(|s| s.sheet_area).sum();
    let total_used: f64 = stats.iter().map(|s| s.used_area).sum();
    let utilisation = if total_sheet > 0.0 { total_used / total_sheet * 100.0 } else { 0.0 };
    let placed: usize = stats.iter().map(|s| s.parts).sum();
    let ordered: usize = meta.parts.iter().map(|p| p.quantity).sum();
    let groups = nest_groups(layouts);

    text(&mut doc.cur, &meta.title, MARGIN, doc.y, 18.0);
    text(&mut doc.cur, &format!("Generated {}", meta.generated), PAGE_W / 2.0 + 150.0, doc.y, 10.0);
    doc.y -= 26.0;

    doc.heading("SUMMARY");
    for line in [
        format!("Sheets used: {}", stats.len()),
        format!("Distinct layouts: {} (see SHEET LIST)", groups.len()),
        format!("Utilisation: {utilisation:.1}%"),
        format!("Material used: {} mm2 of {} mm2", grouped(total_used), grouped(total_sheet)),
        format!("Waste: {} mm2", grouped(total_sheet - total_used)),
        format!("Pieces placed: {placed} of {ordered}"),
    ] {
        doc.line(&line, 11.0);
    }
    if placed < ordered {
        doc.line(&format!("NOT PLACED: {} piece(s) did not fit.", ordered - placed), 11.0);
    }

    // High on the page, immediately under the headline numbers: this is the
    // line someone signs against, so it must not be something you have to
    // turn a page to find.
    if let Some(audit) = &meta.audit {
        doc.heading("MANUFACTURABILITY CHECK");
        doc.line(&audit.headline, 11.0);
        for issue in &audit.issues {
            doc.line(&format!("  {issue}"), 10.0);
        }
    }

    doc.heading("PART LIST");
    doc.row(
        &cells(
            PART_COLS,
            [
                "Name".into(),
                "Total".into(),
                "Nested".into(),
                "Left".into(),
                "Width [mm]".into(),
                "Height [mm]".into(),
                "Contours".into(),
                "Area [m2]".into(),
                "Length [m]".into(),
            ],
        ),
        10.0,
    );
    let mut part_length = 0.0;
    for part in &meta.parts {
        let b = get_polygon_bounds(&part.shape.points).unwrap_or(crate::polygon::Bounds { x: 0.0, y: 0.0, width: 0.0, height: 0.0 });
        let length = cut_length(&part.shape);
        part_length += length * part.nested as f64;
        doc.row(
            &cells(
                PART_COLS,
                [
                    part.name.clone(),
                    format!("{}", part.quantity),
                    format!("{}", part.nested),
                    format!("{}", part.quantity.saturating_sub(part.nested)),
                    format!("{:.2}", b.width),
                    format!("{:.2}", b.height),
                    format!("{}", contour_count(&part.shape)),
                    format!("{:.3}", polygon_material_area(&part.shape) / 1e6),
                    format!("{:.3}", length / 1000.0),
                ],
            ),
            10.0,
        );
    }
    doc.row(
        &cells(
            PART_COLS,
            [
                "Total".into(),
                format!("{ordered}"),
                format!("{placed}"),
                format!("{}", ordered.saturating_sub(placed)),
                String::new(),
                String::new(),
                String::new(),
                format!("{:.3}", total_used / 1e6),
                format!("{:.3}", part_length / 1000.0),
            ],
        ),
        10.0,
    );

    doc.heading("SHEET LIST");
    doc.line("One row per distinct layout. Duplicate is how many sheets are cut from it - identical nests are not relisted.", 10.0);
    doc.row(
        &cells(
            NEST_COLS,
            [
                "Name".into(),
                "Duplicate".into(),
                "Width [mm]".into(),
                "Height [mm]".into(),
                "Pieces".into(),
                "Contours".into(),
                "Util [%]".into(),
                "Length [m]".into(),
            ],
        ),
        10.0,
    );
    let mut sheet_length = 0.0;
    for (n, group) in groups.iter().enumerate() {
        let layout = &layouts[group.first];
        let sheet = &stats[group.first];
        let b = get_polygon_bounds(&layout.sheet.points).unwrap_or(crate::polygon::Bounds { x: 0.0, y: 0.0, width: 0.0, height: 0.0 });
        let contours: usize = layout.parts.iter().map(|p| contour_count(&p.shape)).sum();
        let length: f64 = layout.parts.iter().map(|p| cut_length(&p.shape)).sum();
        sheet_length += length * group.duplicate as f64;
        doc.row(
            &cells(
                NEST_COLS,
                [
                    format!("nest{}", n + 1),
                    format!("{}", group.duplicate),
                    format!("{:.2}", b.width),
                    format!("{:.2}", b.height),
                    format!("{}", sheet.parts),
                    format!("{contours}"),
                    format!("{:.2}", sheet.utilisation()),
                    format!("{:.3}", length / 1000.0),
                ],
            ),
            10.0,
        );
    }
    doc.row(
        &cells(
            NEST_COLS,
            [
                "Total".into(),
                format!("{}", stats.len()),
                String::new(),
                String::new(),
                format!("{placed}"),
                String::new(),
                format!("{utilisation:.2}"),
                format!("{:.3}", sheet_length / 1000.0),
            ],
        ),
        10.0,
    );

    doc.heading("REMNANT INFO");
    // Spelled out because waste and remnant are not the same number and the
    // difference is money: waste is everything not cut into a piece, a remnant
    // is the part of it big enough and clear enough to put back on the shelf
    // and nest onto again.
    doc.line("What is left of each nest once every piece is cut, and can go back on the shelf as stock. Width and height", 10.0);
    doc.line("are the largest rectangle that fits inside it - what is safe to label and book in. Area is the true free area,", 10.0);
    doc.line("which is larger and rarely rectangular. Both already allow for the cut clearance.", 10.0);
    doc.row(
        &cells(REMNANT_COLS, ["Name".into(), "Width [mm]".into(), "Height [mm]".into(), "Qty".into(), "Area [m2]".into(), "Total [m2]".into()]),
        10.0,
    );
    let mut reclaimable_total = 0.0;
    for (n, group) in groups.iter().enumerate() {
        let offcut = sheet_offcut(&layouts[group.first], meta.spacing);
        let total = offcut.reclaimable * group.duplicate as f64;
        reclaimable_total += total;
        if offcut.count == 0 {
            continue;
        }
        doc.row(
            &cells(
                REMNANT_COLS,
                [
                    format!("nest{}", n + 1),
                    format!("{:.0}", offcut.usable_w),
                    format!("{:.0}", offcut.usable_h),
                    format!("{}", group.duplicate),
                    format!("{:.3}", offcut.reclaimable / 1e6),
                    format!("{:.3}", total / 1e6),
                ],
            ),
            10.0,
        );
    }
    doc.row(
        &cells(
            REMNANT_COLS,
            ["Total".into(), String::new(), String::new(), String::new(), String::new(), format!("{:.3}", reclaimable_total / 1e6)],
        ),
        10.0,
    );

    doc.heading("SETTINGS");
    for (label, value) in &meta.settings {
        doc.line(&format!("{label}: {value}"), 10.0);
    }

    doc.finish()
}

fn sheet_page(layout: &SheetLayout, index: usize, stats: &SheetStats) -> String {
    let mut out = String::new();
    text(
        &mut out,
        &format!("Sheet {} - {} pieces - {:.1}% utilisation", index + 1, stats.parts, stats.utilisation()),
        MARGIN,
        PAGE_H - MARGIN - 4.0,
        12.0,
    );

    let Some(bounds) = get_polygon_bounds(&layout.sheet.points) else {
        return out;
    };
    // Fit the sheet into the page's drawing box, preserving aspect ratio.
    let box_w = PAGE_W - 2.0 * MARGIN;
    let box_h = PAGE_H - 2.0 * MARGIN - 30.0;
    let scale = (box_w / bounds.width.max(1e-9)).min(box_h / bounds.height.max(1e-9));
    let ox = MARGIN + (box_w - bounds.width * scale) / 2.0;
    let oy = MARGIN + (box_h - bounds.height * scale) / 2.0;
    // PDF's y axis points up, same as this codebase's, so this is a plain
    // scale-and-offset - no flip anywhere.
    let map = |p: Point| (ox + (p.x - bounds.x) * scale, oy + (p.y - bounds.y) * scale);

    let _ = writeln!(out, "0.55 0.55 0.55 RG 0.8 w");
    path(&mut out, &layout.sheet.points, map);

    let _ = writeln!(out, "0 0 0 RG 0.6 w");
    for (n, part) in layout.parts.iter().enumerate() {
        let geometry = placed_geometry(part);
        stroke_tree(&mut out, &geometry, map);

        // Labelled by position on this sheet, not by the run's internal part
        // id and not by layer name (which is very often the DXF default "0",
        // i.e. no information at all). "Sheet 2, piece 5" is what someone
        // matching this page against a pile of cut parts actually needs, and
        // it needs no data this module would otherwise have to be told.
        if let Some(b) = get_polygon_bounds(&geometry.points) {
            let (cx, cy) = map(Point::new(b.x + b.width / 2.0, b.y + b.height / 2.0));
            let label = format!("#{}", n + 1);
            text(&mut out, &label, cx - 5.0, cy - 3.0, 9.0);
        }
    }

    out
}

fn stroke_tree(out: &mut String, shape: &LayeredPolygon, map: impl Fn(Point) -> (f64, f64) + Copy) {
    path(out, &shape.points, map);
    for child in &shape.children {
        stroke_tree(out, child, map);
    }
}

fn path(out: &mut String, points: &[Point], map: impl Fn(Point) -> (f64, f64)) {
    let Some((first, rest)) = points.split_first() else { return };
    let (x, y) = map(*first);
    let _ = writeln!(out, "{x:.3} {y:.3} m");
    for p in rest {
        let (x, y) = map(*p);
        let _ = writeln!(out, "{x:.3} {y:.3} l");
    }
    let _ = writeln!(out, "h S");
}

fn text(out: &mut String, value: &str, x: f64, y: f64, size: f64) {
    let _ = writeln!(out, "BT /F1 {size} Tf {x:.2} {y:.2} Td ({}) Tj ET", pdf_string(value));
}

/// Escapes a string for a PDF literal and drops anything the built-in
/// WinAnsi Helvetica can't represent - see this module's doc comment.
fn pdf_string(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '(' => "\\(".to_string(),
            ')' => "\\)".to_string(),
            '\\' => "\\\\".to_string(),
            c if c.is_ascii_graphic() || c == ' ' => c.to_string(),
            _ => "?".to_string(),
        })
        .collect()
}

// --- the PDF container itself ------------------------------------------

/// Wraps the content streams into a minimal, valid PDF 1.4 file: catalog,
/// page tree, one page + one stream per content string, one standard font,
/// then the cross-reference table every reader needs to find them.
fn assemble(pages: &[String]) -> Vec<u8> {
    let mut objects: Vec<String> = Vec::new();
    // 1 = catalog, 2 = page tree, 3 = font, then (page, stream) pairs.
    let first_page_obj = 4;
    let kids: Vec<String> = (0..pages.len()).map(|i| format!("{} 0 R", first_page_obj + i * 2)).collect();

    objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_string());
    objects.push(format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string());

    for (i, content) in pages.iter().enumerate() {
        let stream_obj = first_page_obj + i * 2 + 1;
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {PAGE_W:.2} {PAGE_H:.2}] /Resources << /Font << /F1 3 0 R >> >> /Contents {stream_obj} 0 R >>"
        ));
        objects.push(format!("<< /Length {} >>\nstream\n{content}endstream", content.len()));
    }

    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        let _ = write!(out, "{} 0 obj\n{body}\nendobj\n", i + 1);
    }

    let xref_at = out.len();
    // Every xref entry is exactly 20 bytes, trailing space included - that's
    // the format's own fixed-width rule, not padding for looks.
    let _ = writeln!(out, "xref\n0 {}\n0000000000 65535 f ", objects.len() + 1);
    for offset in &offsets {
        let _ = writeln!(out, "{offset:010} 00000 n ");
    }
    let _ = write!(out, "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n", objects.len() + 1);

    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxf_import::LayeredPolygon;

    fn square(size: f64) -> LayeredPolygon {
        LayeredPolygon {
            points: vec![Point::new(0.0, 0.0), Point::new(size, 0.0), Point::new(size, size), Point::new(0.0, size)],
            layer: "CUT".into(),
            is_circle: None,
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        }
    }

    fn layout(parts: usize) -> SheetLayout {
        SheetLayout {
            sheet: square(100.0),
            parts: (0..parts)
                .map(|i| PlacedShape { shape: square(10.0), x: i as f64 * 12.0, y: 0.0, rotation: 0.0 })
                .collect(),
        }
    }

    fn meta() -> ReportMeta {
        ReportMeta {
            title: "Test job".into(),
            generated: "2026-01-01 09:00".into(),
            spacing: 2.0,
            parts: vec![ReportPart { name: "widget".into(), quantity: 4, nested: 3, shape: square(10.0) }],
            settings: vec![("Spacing".into(), "2 mm".into())],
            audit: None,
        }
    }

    /// The verdict has to reach the page. A report that silently drops it is
    /// exactly as useless as no audit at all, and nothing else here would
    /// notice - the PDF still renders perfectly well without it.
    #[test]
    fn the_audit_verdict_is_printed_on_the_summary_page() {
        let mut meta = meta();
        meta.audit = Some(ReportAudit {
            passed: false,
            headline: "FAILED - 2 fatal issue(s), 0 warning(s). DO NOT CUT.".into(),
            issues: vec!["OVERLAP - sheet 1, #3 + #7".into()],
        });
        let text = String::from_utf8(export_report(&[layout(2)], &meta)).expect("ASCII only");
        assert!(text.contains("MANUFACTURABILITY CHECK"), "the section heading must appear");
        assert!(text.contains("DO NOT CUT"), "the verdict must appear");
        assert!(text.contains("OVERLAP - sheet 1"), "the individual finding must appear");
    }

    /// No audit must print nothing at all - never an implied pass. A report
    /// claiming a nest is fine when nobody checked it is the one output here
    /// that could get someone to cut a bad sheet.
    #[test]
    fn no_audit_prints_no_verdict_rather_than_an_implied_pass() {
        let text = String::from_utf8(export_report(&[layout(2)], &meta())).expect("ASCII only");
        assert!(!text.contains("MANUFACTURABILITY CHECK"), "an unchecked nest must not get a verdict section");
        assert!(!text.contains("PASSED"), "an unchecked nest must never read as passed");
    }

    #[test]
    fn writes_a_structurally_valid_pdf_with_one_page_per_sheet_plus_a_summary() {
        let bytes = export_report(&[layout(2), layout(1)], &meta());
        let text = String::from_utf8(bytes).expect("the writer only emits ASCII");

        assert!(text.starts_with("%PDF-1.4"), "must announce itself as a PDF");
        assert!(text.trim_end().ends_with("%%EOF"), "must be terminated");
        // One page per sheet, plus however many pages the summary tables
        // needed - asserted by what is drawn on them rather than by a fixed
        // total, so growing the summary does not read as losing a sheet.
        assert_eq!(text.matches("Sheet 1 - ").count(), 1, "sheet 1 gets its own page");
        assert_eq!(text.matches("Sheet 2 - ").count(), 1, "sheet 2 gets its own page");
        let pages = text.matches("/Type /Page\n").count() + text.matches("/Type /Page ").count();
        assert!(pages >= 3, "expected at least a summary page plus one per sheet, got {pages}");
        assert!(text.contains("startxref"), "readers need the xref offset");
        // The xref table must have one entry per object plus the free entry.
        let object_count = text.matches(" 0 obj").count();
        assert!(text.contains(&format!("/Size {}", object_count + 1)));
    }

    /// The report's numbers are derived from the geometry it draws, so this
    /// is a real cross-check rather than a tautology.
    #[test]
    fn the_summary_reports_utilisation_measured_off_the_drawn_geometry() {
        // Three 10x10 parts on a 100x100 sheet: 300 of 10000 = 3%.
        let bytes = export_report(&[layout(3)], &meta());
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("Utilisation: 3.0%"), "expected 3% utilisation in:\n{text}");
        assert!(text.contains("Pieces placed: 3 of 4"));
        assert!(text.contains("NOT PLACED: 1 piece"), "a shortfall must be called out, not left to arithmetic");
    }

    #[test]
    fn an_empty_result_still_produces_a_readable_one_page_report() {
        let bytes = export_report(&[], &meta());
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("%PDF-1.4"));
        assert!(text.contains("Sheets used: 0"));
        assert!(text.contains("Pieces placed: 0 of 4"));
    }

    /// The reference tool's report leads with a Duplicate column, and it is
    /// the only thing on the page that says whether the nester found one good
    /// arrangement and reused it or re-solved every sheet from scratch. Two
    /// identical sheets must collapse to one row saying "x2", not two rows.
    #[test]
    fn identically_laid_out_sheets_collapse_into_one_nest_row() {
        let groups = nest_groups(&[layout(3), layout(3), layout(2)]);
        assert_eq!(groups.len(), 2, "two distinct layouts, not three sheets");
        assert_eq!(groups[0].duplicate, 2, "the repeated layout is counted, not relisted");
        assert_eq!(groups[1].duplicate, 1);
        assert_eq!(groups[1].first, 2, "a group points at the first sheet that had its layout");
    }

    /// Same pieces in the same places, reached in a different placement order,
    /// is the same pattern to whoever has to cut it.
    #[test]
    fn placement_order_does_not_split_a_nest_group() {
        let mut shuffled = layout(3);
        shuffled.parts.reverse();
        assert_eq!(nest_groups(&[layout(3), shuffled]).len(), 1);
    }

    /// The part list is the half of the report the summary totals cannot give
    /// you: which piece is short, and by how many. Its columns mirror the
    /// reference tool's own Part-List so the two can be read side by side.
    #[test]
    fn the_part_list_prints_ordered_nested_and_left_with_the_measured_columns() {
        let mut meta = meta();
        meta.parts = vec![ReportPart { name: "bracket".into(), quantity: 50, nested: 47, shape: square(404.0) }];
        let text = String::from_utf8(export_report(&[layout(2)], &meta)).expect("ASCII only");
        assert!(text.contains("PART LIST"), "the section must exist");
        assert!(text.contains("bracket"));
        assert!(text.contains("404.00"), "size in mm, measured off the shape");
        assert!(text.contains("Contours"), "the cut-contour count is a column the machine cares about");
        assert!(text.contains("Length"), "cut length is a column");
        // 50 ordered, 47 nested, 3 left - the last one is the whole point.
        assert!(text.contains("(47)"), "nested count");
        assert!(text.contains("(3)"), "left count");
    }

    /// Contours and cut length are per-piece manufacturing numbers, and both
    /// have to count holes or they understate every real part.
    #[test]
    fn contours_and_cut_length_include_every_hole() {
        let mut part = square(10.0);
        part.children.push(square(2.0));
        assert_eq!(contour_count(&part), 2, "outline plus one hole");
        // 4x10 outline + 4x2 hole = 48mm of cutting.
        assert!((cut_length(&part) - 48.0).abs() < 1e-9, "got {}", cut_length(&part));
    }

    #[test]
    fn big_numbers_are_grouped_and_negatives_clamp_rather_than_printing_a_sign() {
        assert_eq!(grouped(1_234_567.8), "1 234 568");
        assert_eq!(grouped(999.0), "999");
        assert_eq!(grouped(0.0), "0");
        assert_eq!(grouped(-5.0), "0", "waste can go very slightly negative on rounding");
    }

    /// A long job must not run off the bottom of the page - the numbers would
    /// simply be missing, and nothing else here would notice.
    #[test]
    fn a_job_with_more_rows_than_fit_spills_onto_another_summary_page() {
        let mut meta = meta();
        meta.parts = (0..60).map(|i| ReportPart { name: format!("part-{i}"), quantity: 1, nested: 1, shape: square(1.0) }).collect();
        let layouts: Vec<SheetLayout> = (0..40).map(|i| layout(i % 5 + 1)).collect();
        let stats: Vec<SheetStats> = layouts.iter().map(sheet_stats).collect();
        let pages = summary_pages(&meta, &layouts, &stats);
        assert!(pages.len() > 1, "expected the summary to paginate, got {} page(s)", pages.len());
        assert!(pages.iter().all(|p| p.contains("Tj")), "an emitted page must not be blank");
    }

    /// Waste and remnant are different numbers and the report must not let
    /// them be read as the same one: waste is everything not cut into a piece,
    /// a remnant is the part of it big enough to nest onto again.
    #[test]
    fn the_remnant_section_says_what_is_reusable_and_explains_itself() {
        let text = String::from_utf8(export_report(&[layout(3)], &meta())).expect("ASCII only");
        assert!(text.contains("REMNANT INFO"), "the section must exist");
        assert!(text.contains("back on the shelf as stock"), "and must say what a remnant actually is");
        // Three 10x10 parts on a 100x100 sheet leaves a large offcut, so this
        // must be a real measurement rather than a zero placeholder.
        let offcut = sheet_offcut(&layout(3), 2.0);
        assert!(offcut.reclaimable > 5_000.0, "expected most of the sheet reclaimable, got {}", offcut.reclaimable);
        assert!(offcut.usable_w > 0.0 && offcut.usable_h > 0.0, "a usable rectangle must be reported");
    }

    /// A sheet with nothing on it is untouched stock, not an offcut - filing it
    /// as one would put whole sheets on the remnant shelf.
    #[test]
    fn an_empty_sheet_reports_no_offcut_rather_than_a_whole_one() {
        let offcut = sheet_offcut(&layout(0), 2.0);
        assert_eq!(offcut.count, 0);
        assert_eq!(offcut.reclaimable, 0.0);
    }

    #[test]
    fn text_that_would_break_the_container_is_escaped() {
        assert_eq!(pdf_string("a (b) \\c"), "a \\(b\\) \\\\c");
        // Non-ASCII degrades to a placeholder rather than emitting bytes the
        // built-in font can't map - see the module doc.
        assert_eq!(pdf_string("cắt"), "c?t");
    }
}
