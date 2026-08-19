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
#[derive(Clone, Debug)]
pub struct ReportPart {
    pub name: String,
    pub quantity: usize,
}

/// Everything the report says that isn't derivable from the drawn geometry.
/// Anything that *is* derivable is computed from the layouts themselves (see
/// `export_report`), so the printed numbers can never disagree with the
/// printed picture.
#[derive(Clone, Debug)]
pub struct ReportMeta {
    pub title: String,
    pub parts: Vec<ReportPart>,
    /// `(label, value)` pairs, printed verbatim - the caller decides which
    /// settings are worth showing rather than this module knowing about
    /// `NestConfigDto`.
    pub settings: Vec<(String, String)>,
}

/// Renders the report and returns the PDF bytes.
#[must_use]
pub fn export_report(layouts: &[SheetLayout], meta: &ReportMeta) -> Vec<u8> {
    let mut pages: Vec<String> = Vec::new();

    let stats: Vec<SheetStats> = layouts.iter().map(sheet_stats).collect();
    pages.push(summary_page(meta, &stats));
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

fn summary_page(meta: &ReportMeta, stats: &[SheetStats]) -> String {
    let mut out = String::new();
    let mut y = PAGE_H - MARGIN - 18.0;

    let total_sheet: f64 = stats.iter().map(|s| s.sheet_area).sum();
    let total_used: f64 = stats.iter().map(|s| s.used_area).sum();
    let utilisation = if total_sheet > 0.0 { total_used / total_sheet * 100.0 } else { 0.0 };
    let placed: usize = stats.iter().map(|s| s.parts).sum();
    let ordered: usize = meta.parts.iter().map(|p| p.quantity).sum();

    text(&mut out, &meta.title, MARGIN, y, 18.0);
    y -= 30.0;

    for line in [
        format!("Sheets used: {}", stats.len()),
        format!("Utilisation: {utilisation:.1}%"),
        format!("Material used: {:.0} mm2 of {:.0} mm2", total_used, total_sheet),
        format!("Waste: {:.0} mm2", (total_sheet - total_used).max(0.0)),
        format!("Pieces placed: {placed} of {ordered}"),
    ] {
        text(&mut out, &line, MARGIN, y, 11.0);
        y -= 16.0;
    }

    if placed < ordered {
        y -= 4.0;
        text(&mut out, &format!("NOT PLACED: {} piece(s) did not fit.", ordered - placed), MARGIN, y, 11.0);
        y -= 16.0;
    }

    y -= 14.0;
    text(&mut out, "PIECES", MARGIN, y, 13.0);
    y -= 18.0;
    for part in &meta.parts {
        text(&mut out, &format!("{}   x{}", part.name, part.quantity), MARGIN, y, 10.0);
        y -= 13.0;
        if y < MARGIN + 120.0 {
            break; // one page of listing is plenty; the sheet pages follow
        }
    }

    // Per-sheet table and settings share the right-hand half, so a long part
    // list can't push them off the page.
    let right = PAGE_W / 2.0 + 20.0;
    let mut ry = PAGE_H - MARGIN - 48.0;
    text(&mut out, "SHEETS", right, ry, 13.0);
    ry -= 18.0;
    for (i, sheet) in stats.iter().enumerate() {
        text(&mut out, &format!("Sheet {}   {} pieces   {:.1}%", i + 1, sheet.parts, sheet.utilisation()), right, ry, 10.0);
        ry -= 13.0;
    }

    ry -= 14.0;
    text(&mut out, "SETTINGS", right, ry, 13.0);
    ry -= 18.0;
    for (label, value) in &meta.settings {
        text(&mut out, &format!("{label}: {value}"), right, ry, 10.0);
        ry -= 13.0;
    }

    out
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
            parts: vec![ReportPart { name: "widget".into(), quantity: 4 }],
            settings: vec![("Spacing".into(), "2 mm".into())],
        }
    }

    #[test]
    fn writes_a_structurally_valid_pdf_with_one_page_per_sheet_plus_a_summary() {
        let bytes = export_report(&[layout(2), layout(1)], &meta());
        let text = String::from_utf8(bytes).expect("the writer only emits ASCII");

        assert!(text.starts_with("%PDF-1.4"), "must announce itself as a PDF");
        assert!(text.trim_end().ends_with("%%EOF"), "must be terminated");
        assert_eq!(text.matches("/Type /Page\n").count() + text.matches("/Type /Page ").count(), 3, "summary + 2 sheets");
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

    #[test]
    fn text_that_would_break_the_container_is_escaped() {
        assert_eq!(pdf_string("a (b) \\c"), "a \\(b\\) \\\\c");
        // Non-ASCII degrades to a placeholder rather than emitting bytes the
        // built-in font can't map - see the module doc.
        assert_eq!(pdf_string("cắt"), "c?t");
    }
}
