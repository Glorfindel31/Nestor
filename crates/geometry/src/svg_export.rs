//! Writes a nested layout back out to SVG - the SVG counterpart to
//! `dxf_export`, sharing its exact `SheetLayout`/`PlacedShape` input shape
//! (and its `pack_unplaced_parts` helper) so a caller builds one layout and
//! can export it as either format interchangeably. Every placed part (and
//! sheet, if requested) becomes one `<path>` inside a `<g id="{layer}">`
//! grouped by layer - the part's own tree (true outer boundary plus every
//! child/hole), same as DXF export writes one `LWPOLYLINE` per node.
//!
//! Multiple sheets are laid out left-to-right (separated by
//! `sheet_spacing`), mirroring `dxf_export::export_dxf`'s own cursor-based
//! layout loop closely enough that the same `SheetLayout` slice produces
//! the same physical arrangement in either format.
//!
//! **Y-axis flip, symmetric with `svg_import`'s own**: this codebase's
//! internal geometry is Y-up (DXF/CAD convention); SVG is Y-down (spec
//! mandated). Each sheet's own points get flipped within that sheet's own
//! height (`sheet_height - y`, same per-sheet-independent normalization
//! `export_dxf`'s `-b.y` shift already uses) so the exported file displays
//! right-side-up in any SVG viewer, matching how it would look opened as
//! the original DXF - the same flip `svg_import` applies in reverse on the
//! way in (see that module's doc for the real bug this fixed when it was
//! missing on import).
//!
//! **Metric-only output**: `width`/`height` are always written in `mm`,
//! matching `svg_import`'s metric-only *input* contract - an SVG this
//! module writes and `svg_import::parse_svg` re-reads round-trips its scale
//! exactly, no unit-picker prompt needed on that re-import.
//!
//! **Simplifications, matching `svg_import`'s own documented scope**: no
//! `texts` (`TEXT`/`MTEXT`-equivalent) are exported - `svg_import` doesn't
//! read them on the way in either, so there's nothing round-trippable yet;
//! revisit both together if SVG text support is ever needed. A part that
//! was a true circle on import round-trips as a real SVG `<circle>` (same
//! mechanism as `dxf_export`'s `CIRCLE` entity - `is_circle`'s center gets
//! the same Y-flip `collect_paths` already applies to `points`, radius is
//! unaffected since the flip is a pure reflection, not a scale). Rounded-
//! corner bulge arcs (`LayeredPolygon::real_boundary`) do NOT get the same
//! treatment `dxf_export` gives them - not because SVG lacks an arc
//! primitive (its `<path>` `A` command is one, and `svg_import` already
//! tessellates it on the way in), just not implemented for this pass, so
//! those still round-trip as their tessellated polygon approximation.

use crate::dxf_export::SheetLayout;
use crate::dxf_import::{rotate_layered_polygon, shift_layered_polygon, LayeredPolygon};
use crate::polygon::get_polygon_bounds;

/// Lays every sheet out left-to-right (separated by `sheet_spacing`) and
/// writes one `<path>` per layer-tagged node into a `<g id="layer">` -
/// the sheet's own outline too, if `include_sheet_outline` is set. Returns
/// a complete, self-contained SVG document string.
pub fn export_svg(sheets: &[SheetLayout], sheet_spacing: f64, include_sheet_outline: bool) -> String {
    let mut cursor_x = 0.0f64;
    let mut overall_height = 0.0f64;
    let mut layers: Vec<(String, String)> = Vec::new();

    for layout in sheets {
        let sheet_bounds = get_polygon_bounds(&layout.sheet.points);
        let (offset_x, sheet_width, sheet_height) = match sheet_bounds {
            Some(b) => (cursor_x - b.x, b.width, b.height),
            None => (cursor_x, 0.0, 0.0),
        };
        let offset_y = sheet_bounds.map(|b| -b.y).unwrap_or(0.0);

        if include_sheet_outline {
            let shifted = shift_layered_polygon(&layout.sheet, offset_x, offset_y);
            collect_paths(&shifted, sheet_height, &mut layers);
        }

        for placed in &layout.parts {
            let rotated = rotate_layered_polygon(&placed.shape, placed.rotation);
            let positioned = shift_layered_polygon(&rotated, placed.x + offset_x, placed.y + offset_y);
            collect_paths(&positioned, sheet_height, &mut layers);
        }

        cursor_x += sheet_width + sheet_spacing;
        overall_height = overall_height.max(sheet_height);
    }
    let overall_width = (cursor_x - sheet_spacing).max(0.0);

    let body: String = layers.iter().map(|(layer, paths)| format!("<g id=\"{}\">{paths}</g>", xml_escape(layer))).collect();

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{overall_width}mm\" height=\"{overall_height}mm\" \
         viewBox=\"0 0 {overall_width} {overall_height}\" fill=\"none\" stroke=\"black\" stroke-width=\"0.1\">\n\
         {body}\n\
         </svg>\n"
    )
}

/// Appends one `<path>` for `node.points` (already positioned in the final
/// drawing's absolute x, flipped into SVG's Y-down space within this
/// sheet's own `sheet_height`) onto whichever `layers` entry matches
/// `node.layer`, creating a new entry (in first-seen order) if this is a
/// new layer - then recurses into every child, same tree walk
/// `dxf_export::add_node` does.
fn collect_paths(node: &LayeredPolygon, sheet_height: f64, layers: &mut Vec<(String, String)>) {
    if let Some(circle) = node.is_circle {
        let entry = match layers.iter_mut().position(|(layer, _)| *layer == node.layer) {
            Some(idx) => &mut layers[idx],
            None => {
                layers.push((node.layer.clone(), String::new()));
                layers.last_mut().expect("just pushed")
            }
        };
        entry.1.push_str(&format!("<circle cx=\"{:.4}\" cy=\"{:.4}\" r=\"{:.4}\"/>", circle.cx, sheet_height - circle.cy, circle.r));
    } else if node.points.len() >= 2 {
        let d: String = node
            .points
            .iter()
            .enumerate()
            .map(|(i, p)| format!("{}{:.4},{:.4} ", if i == 0 { "M" } else { "L" }, p.x, sheet_height - p.y))
            .collect();
        let entry = match layers.iter_mut().position(|(layer, _)| *layer == node.layer) {
            Some(idx) => &mut layers[idx],
            None => {
                layers.push((node.layer.clone(), String::new()));
                layers.last_mut().expect("just pushed")
            }
        };
        entry.1.push_str(&format!("<path d=\"{d}Z\"/>"));
    }
    for child in &node.children {
        collect_paths(child, sheet_height, layers);
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxf_export::PlacedShape;
    use crate::dxf_import::TextAnnotation;
    use crate::point::Point;
    use crate::svg_import::parse_svg;

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

    #[test]
    fn one_sheet_one_part_writes_two_groups_by_default() {
        let layout = SheetLayout {
            sheet: {
                let mut s = square(100.0);
                s.layer = "SHEET".into();
                s
            },
            parts: vec![PlacedShape { shape: square(10.0), x: 5.0, y: 5.0, rotation: 0.0 }],
        };

        let svg = export_svg(std::slice::from_ref(&layout), 20.0, true);

        assert!(svg.contains("id=\"SHEET\""), "sheet outline should be written when requested: {svg}");
        assert!(svg.contains("id=\"CUT\""), "part layer missing: {svg}");
    }

    #[test]
    fn sheet_outline_is_omitted_when_not_requested() {
        let layout = SheetLayout { sheet: square(100.0), parts: vec![PlacedShape { shape: square(10.0), x: 5.0, y: 5.0, rotation: 0.0 }] };
        let svg = export_svg(std::slice::from_ref(&layout), 20.0, false);
        assert_eq!(svg.matches("<path").count(), 1, "should have exactly one path (the part, no sheet outline): {svg}");
    }

    #[test]
    fn a_holes_layer_survives_the_export() {
        let mut part = square(20.0);
        part.children.push({
            let mut hole = square(5.0);
            hole.layer = "DRILL".into();
            hole
        });
        let layout = SheetLayout { sheet: square(100.0), parts: vec![PlacedShape { shape: part, x: 0.0, y: 0.0, rotation: 0.0 }] };

        let svg = export_svg(std::slice::from_ref(&layout), 20.0, false);

        assert!(svg.contains("id=\"CUT\""), "the part's own outer layer");
        assert!(svg.contains("id=\"DRILL\""), "the hole's own layer must survive the round trip");
    }

    #[test]
    fn two_sheets_are_laid_out_side_by_side_without_overlap() {
        let layouts = vec![
            SheetLayout { sheet: square(100.0), parts: Vec::new() },
            SheetLayout { sheet: square(100.0), parts: vec![PlacedShape { shape: square(10.0), x: 0.0, y: 0.0, rotation: 0.0 }] },
        ];

        let svg = export_svg(&layouts, 15.0, false);

        // the second sheet's part should start at 100 (first sheet's width)
        // + 15 (spacing) = 115, not overlap the first sheet's [0,100] span
        assert!(svg.contains("M115.0000,"), "expected the second sheet's part to start at x=115: {svg}");
    }

    #[test]
    fn no_texts_are_written_matching_svg_imports_own_scope() {
        let mut part = square(10.0);
        part.texts.push(TextAnnotation { position: Point::new(1.0, 0.0), rotation_deg: 0.0, height: 2.0, value: "PART-7".into(), is_multiline: false });
        let layout = SheetLayout { sheet: square(200.0), parts: vec![PlacedShape { shape: part, x: 0.0, y: 0.0, rotation: 0.0 }] };

        let svg = export_svg(std::slice::from_ref(&layout), 20.0, false);

        assert!(!svg.contains("<text"), "SVG export doesn't support text yet, matching svg_import's own scope: {svg}");
    }

    /// A part that was a true circle on import must round-trip as a real
    /// `<circle>` element, not a tessellated `<path>` - center reflects the
    /// placement's translation and the sheet's own Y-flip.
    #[test]
    fn a_true_circle_part_exports_as_a_real_circle_element() {
        let circle_shape = LayeredPolygon {
            points: crate::dxf_import::tessellate_circle(0.0, 0.0, 5.0, 0.1),
            layer: "CUT".into(),
            is_circle: Some(crate::circular_nfp::Circle { cx: 0.0, cy: 0.0, r: 5.0 }),
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        };
        let layout = SheetLayout { sheet: square(200.0), parts: vec![PlacedShape { shape: circle_shape, x: 50.0, y: 30.0, rotation: 0.0 }] };

        let svg = export_svg(std::slice::from_ref(&layout), 20.0, false);

        assert!(svg.contains("<circle "), "circle part must export as a real <circle> element, not a tessellated <path>: {svg}");
        assert!(!svg.contains("<path"), "no tessellated path should be written for a true circle: {svg}");
        // sheet height 200, placed at y=30 -> flipped y = 200 - 30 = 170
        assert!(svg.contains("cx=\"50.0000\""), "expected cx=50: {svg}");
        assert!(svg.contains("cy=\"170.0000\""), "expected the Y-flipped cy=170: {svg}");
        assert!(svg.contains("r=\"5.0000\""), "expected r=5: {svg}");
    }

    /// Round-trip regression: export a real (non-square, so a mirror bug
    /// can't hide behind symmetry) shape to SVG, re-import it, and check the
    /// result is congruent to the original - a direct check that the export
    /// Y-flip correctly complements `svg_import`'s own Y-flip rather than
    /// compounding it (which would silently double-mirror on a second
    /// round-trip while still looking fine on the very first export/import).
    #[test]
    fn export_then_reimport_round_trips_a_non_symmetric_shape() {
        let hat = LayeredPolygon {
            points: vec![
                Point::new(0.0, 0.0),
                Point::new(10.0, 0.0),
                Point::new(10.0, 5.0),
                Point::new(20.0, 5.0),
                Point::new(20.0, 20.0),
                Point::new(0.0, 12.0),
            ],
            layer: "CUT".into(),
            is_circle: None,
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        };
        let original_area = crate::polygon::polygon_area(&hat.points);

        let layout = SheetLayout { sheet: square(200.0), parts: vec![PlacedShape { shape: hat, x: 0.0, y: 0.0, rotation: 0.0 }] };
        let svg = export_svg(std::slice::from_ref(&layout), 20.0, false);

        let reimported = parse_svg(&svg, 0.1, Some("mm")).expect("exported SVG should re-parse");
        assert_eq!(reimported.len(), 1);
        let reimported_area = crate::polygon::polygon_area(&reimported[0].points);

        assert!((original_area.abs() - reimported_area.abs()).abs() < 0.01, "area changed across export/reimport: {original_area} vs {reimported_area}");
        assert_eq!(original_area.signum(), reimported_area.signum(), "winding direction must survive an export/reimport round trip (original {original_area}, reimported {reimported_area})");
    }
}
