//! Writes a nested layout back out to DXF - **new scope, not a port**: the
//! original Electron app never wrote DXF locally at all (see
//! `docs/PORT_STATUS.md`'s Phase 7 table). Every placed part (and sheet, if
//! requested) becomes one `LWPOLYLINE` per layer-tagged node - the part's
//! own tree (true outer boundary plus every child/hole), not just its
//! outer profile, so a part's interior layers (e.g. drilled holes) survive
//! the round trip the same way they do on import.
//!
//! Multiple sheets can't share one DXF drawing space without overlapping,
//! since a DXF file is a single flat coordinate system - each sheet used in
//! the result is laid out left-to-right, separated by `sheet_spacing`, in
//! the order given.
//!
//! A part that was a true circle on import (`LayeredPolygon::is_circle`) is
//! written back out as a real DXF `CIRCLE` entity, not its tessellated
//! polygon approximation - `is_circle`'s center already survives
//! `rotate_layered_polygon`/`shift_layered_polygon` untouched (both
//! transform it the same way they transform `points`), so `add_node` just
//! reads it straight off the already-placed shape. A part with retained
//! bulge geometry (`LayeredPolygon::real_boundary` - rounded-corner
//! LWPOLYLINE segments) similarly writes real per-vertex arc `bulge`
//! values instead of `0.0`. Only geometry with neither (hand-drawn shapes,
//! SVG-imported parts, unsupported entity types) falls back to the plain
//! tessellated `LWPOLYLINE`.
//!
//! Each node's `texts` (labels/part numbers attached on import - see
//! `dxf_import`'s module doc) are written back out too, as the same entity
//! kind (`TEXT`/`MTEXT`) they came from, already transformed by whatever
//! `rotate_layered_polygon`/`shift_layered_polygon` calls the caller applied
//! to the shape - `add_node` just emits their current position/rotation
//! as-is, the same way it does for `points`.

use dxf::entities::{Circle, Entity, EntityCommon, EntityType, LwPolyline, MText, Text};
use dxf::{Drawing, LwPolylineVertex, Point as DxfPoint};

use crate::dxf_import::{rotate_layered_polygon, shift_layered_polygon, LayeredPolygon, TextAnnotation};
use crate::point::Point;
use crate::polygon::get_polygon_bounds;

/// One part's true (unpadded) geometry plus where the engine placed it -
/// mirrors `nesting::placement::PlacedPart` but carries the actual shape
/// (with its hole/layer tree) rather than just an id, since this module
/// has no `parts_by_id` lookup of its own to resolve one.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedShape {
    pub shape: LayeredPolygon,
    pub x: f64,
    pub y: f64,
    pub rotation: f64,
}

/// One sheet actually used by the result, plus every part placed on it.
#[derive(Clone, Debug, PartialEq)]
pub struct SheetLayout {
    pub sheet: LayeredPolygon,
    pub parts: Vec<PlacedShape>,
}

/// Widest a packed row of unplaced parts is allowed to grow before wrapping
/// to a new row - arbitrary but generous (most real jobs' individual parts
/// are well under this), just enough to keep a long run of small parts from
/// producing one absurdly wide single row.
const UNPLACED_MAX_ROW_WIDTH: f64 = 1000.0;

/// Lays a flat list of never-placed parts out in a simple non-overlapping,
/// row-wrapping grid - **not** a nesting pass (no rotation search, no NFP,
/// no attempt to pack tightly). These parts are "unplaced" precisely
/// because the real engine couldn't fit them anywhere on any sheet; this
/// only needs to keep them from overlapping each other so a caller can see
/// and manually handle them, not nest them well.
///
/// Returns a `SheetLayout` whose own `sheet` is a synthetic bounding
/// rectangle sized to the packed result, on a `"UNPLACED"` layer so it
/// reads as distinct from a real sheet if `include_sheet_outline` is on.
/// Handing this back as one more `SheetLayout` (appended to the real ones)
/// lets both `export_dxf`/`svg_export::export_svg` place it automatically
/// via their own existing left-to-right multi-sheet layout - no separate
/// code path needed in either exporter.
pub fn pack_unplaced_parts(parts: &[LayeredPolygon], spacing: f64) -> SheetLayout {
    let mut placed = Vec::new();
    let mut cursor_x = 0.0;
    let mut cursor_y = 0.0;
    let mut row_height = 0.0f64;
    let mut max_x = 0.0f64;

    for shape in parts {
        let Some(b) = get_polygon_bounds(&shape.points) else { continue };
        if cursor_x > 0.0 && cursor_x + b.width > UNPLACED_MAX_ROW_WIDTH {
            cursor_x = 0.0;
            cursor_y += row_height + spacing;
            row_height = 0.0;
        }
        // Shift so this part's own bounding-box corner lands at
        // (cursor_x, cursor_y) - same "x/y is a translation of the shape's
        // own raw points" convention `export_dxf`'s `placed.x/y` already
        // uses below, not "the target position of its bounding box".
        placed.push(PlacedShape { shape: shape.clone(), x: cursor_x - b.x, y: cursor_y - b.y, rotation: 0.0 });
        cursor_x += b.width + spacing;
        row_height = row_height.max(b.height);
        max_x = max_x.max(cursor_x - spacing);
    }
    let total_height = cursor_y + row_height;

    let sheet_points = vec![
        Point::new(0.0, 0.0),
        Point::new(max_x, 0.0),
        Point::new(max_x, total_height),
        Point::new(0.0, total_height),
    ];
    SheetLayout {
        sheet: LayeredPolygon { points: sheet_points, layer: "UNPLACED".to_string(), is_circle: None, children: Vec::new(), texts: Vec::new(), real_boundary: None },
        parts: placed,
    }
}

/// Lays every sheet out left-to-right (separated by `sheet_spacing`) and
/// writes one `LWPOLYLINE` per layer-tagged node - the sheet's own outline
/// too, if `include_sheet_outline` is set. Returns the in-memory `Drawing`;
/// saving it to a path is the caller's job (`Drawing::save_file`).
pub fn export_dxf(sheets: &[SheetLayout], sheet_spacing: f64, include_sheet_outline: bool) -> Drawing {
    let mut drawing = Drawing::new();
    // Drawing::new()'s default header targets R12 (pre-dates LWPOLYLINE,
    // introduced in R2000/AC1015) - the writer silently drops any entity
    // type unsupported by the target version, so every LWPOLYLINE this
    // function adds below would otherwise vanish with no error at all
    // (confirmed by writing then re-reading a minimal file: an empty
    // ENTITIES section, no panic, no Err).
    drawing.header.version = dxf::enums::AcadVersion::R2000;
    let mut cursor_x = 0.0;

    for layout in sheets {
        let sheet_bounds = get_polygon_bounds(&layout.sheet.points);
        let (offset_x, sheet_width) = match sheet_bounds {
            Some(b) => (cursor_x - b.x, b.width),
            None => (cursor_x, 0.0),
        };
        let offset_y = sheet_bounds.map(|b| -b.y).unwrap_or(0.0);

        if include_sheet_outline {
            let shifted = shift_layered_polygon(&layout.sheet, offset_x, offset_y);
            add_node(&mut drawing, &shifted);
        }

        for placed in &layout.parts {
            let rotated = rotate_layered_polygon(&placed.shape, placed.rotation);
            let positioned = shift_layered_polygon(&rotated, placed.x + offset_x, placed.y + offset_y);
            add_node(&mut drawing, &positioned);
        }

        cursor_x += sheet_width + sheet_spacing;
    }

    drawing
}

/// Adds one `LWPOLYLINE` for `shape.points` on `shape.layer`, then recurses
/// into every child - a shape (part or sheet) is a tree, and every node in
/// it needs to survive the round trip on its own original layer.
fn add_node(drawing: &mut Drawing, shape: &LayeredPolygon) {
    if let Some(circle) = shape.is_circle {
        drawing.add_entity(Entity {
            common: EntityCommon { layer: shape.layer.clone(), ..Default::default() },
            specific: EntityType::Circle(Circle { center: DxfPoint::new(circle.cx, circle.cy, 0.0), radius: circle.r, ..Default::default() }),
        });
    } else if let Some(real_boundary) = &shape.real_boundary {
        let mut poly = LwPolyline {
            vertices: real_boundary.iter().map(|v| LwPolylineVertex { x: v.point.x, y: v.point.y, bulge: v.bulge, ..Default::default() }).collect(),
            ..Default::default()
        };
        poly.set_is_closed(true);
        drawing.add_entity(Entity {
            common: EntityCommon { layer: shape.layer.clone(), ..Default::default() },
            specific: EntityType::LwPolyline(poly),
        });
    } else if shape.points.len() >= 2 {
        let mut poly = LwPolyline {
            vertices: shape.points.iter().map(|p| LwPolylineVertex { x: p.x, y: p.y, bulge: 0.0, ..Default::default() }).collect(),
            ..Default::default()
        };
        poly.set_is_closed(true);
        drawing.add_entity(Entity {
            common: EntityCommon { layer: shape.layer.clone(), ..Default::default() },
            specific: EntityType::LwPolyline(poly),
        });
    }
    for text in &shape.texts {
        add_text(drawing, &shape.layer, text);
    }
    for child in &shape.children {
        add_node(drawing, child);
    }
}

/// Writes one `TextAnnotation` back out as the same entity kind it was
/// parsed from - see `dxf_import::entity_to_text`'s doc comment for the
/// `TEXT`-is-degrees-`MTEXT`-is-radians rotation quirk this has to invert
/// symmetrically on the way back out.
fn add_text(drawing: &mut Drawing, layer: &str, text: &TextAnnotation) {
    let specific = if text.is_multiline {
        EntityType::MText(MText {
            insertion_point: DxfPoint::new(text.position.x, text.position.y, 0.0),
            initial_text_height: text.height,
            text: text.value.clone(),
            rotation_angle: text.rotation_deg.to_radians(),
            ..Default::default()
        })
    } else {
        EntityType::Text(Text {
            location: DxfPoint::new(text.position.x, text.position.y, 0.0),
            text_height: text.height,
            value: text.value.clone(),
            rotation: text.rotation_deg,
            ..Default::default()
        })
    };
    drawing.add_entity(Entity {
        common: EntityCommon { layer: layer.to_string(), ..Default::default() },
        specific,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::point::Point;

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

    fn entities_on_layer<'a>(drawing: &'a Drawing, layer: &str) -> Vec<&'a LwPolyline> {
        drawing
            .entities()
            .filter_map(|e| match &e.specific {
                EntityType::LwPolyline(p) if e.common.layer == layer => Some(p),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn one_sheet_one_part_writes_two_polylines_by_default() {
        let layout = SheetLayout {
            sheet: {
                let mut s = square(100.0);
                s.layer = "SHEET".into();
                s
            },
            parts: vec![PlacedShape { shape: square(10.0), x: 5.0, y: 5.0, rotation: 0.0 }],
        };

        let drawing = export_dxf(std::slice::from_ref(&layout), 20.0, true);

        assert_eq!(entities_on_layer(&drawing, "SHEET").len(), 1, "sheet outline should be written when requested");
        assert_eq!(entities_on_layer(&drawing, "CUT").len(), 1);
    }

    #[test]
    fn sheet_outline_is_omitted_when_not_requested() {
        let layout = SheetLayout {
            sheet: {
                let mut s = square(100.0);
                s.layer = "SHEET".into();
                s
            },
            parts: vec![PlacedShape { shape: square(10.0), x: 5.0, y: 5.0, rotation: 0.0 }],
        };

        let drawing = export_dxf(std::slice::from_ref(&layout), 20.0, false);

        assert_eq!(entities_on_layer(&drawing, "SHEET").len(), 0);
        assert_eq!(entities_on_layer(&drawing, "CUT").len(), 1);
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

        let drawing = export_dxf(std::slice::from_ref(&layout), 20.0, false);

        assert_eq!(entities_on_layer(&drawing, "CUT").len(), 1, "the part's own outer layer");
        assert_eq!(entities_on_layer(&drawing, "DRILL").len(), 1, "the hole's own layer must survive the round trip");
    }

    #[test]
    fn two_sheets_are_laid_out_side_by_side_without_overlap() {
        let layouts = vec![
            SheetLayout { sheet: square(100.0), parts: Vec::new() },
            SheetLayout { sheet: square(100.0), parts: vec![PlacedShape { shape: square(10.0), x: 0.0, y: 0.0, rotation: 0.0 }] },
        ];

        let drawing = export_dxf(&layouts, 15.0, false);

        // the second sheet's part should start at 100 (first sheet's
        // width) + 15 (spacing) = 115, not overlap the first sheet's [0,100] span
        let parts = entities_on_layer(&drawing, "CUT");
        assert_eq!(parts.len(), 1);
        let min_x = parts[0].vertices.iter().map(|v| v.x).fold(f64::INFINITY, f64::min);
        assert!((min_x - 115.0).abs() < 1e-6, "expected the second sheet's part to start at x=115, got {min_x}");
    }

    /// Regression test for the "text is silently dropped" bug: a part's
    /// attached `TEXT` must survive the full rotate+shift+export pipeline,
    /// ending up at the part's actual placed position/rotation, not its
    /// local pre-placement one.
    #[test]
    fn a_parts_attached_text_is_written_out_at_its_placed_position() {
        let mut part = square(10.0);
        part.texts.push(TextAnnotation { position: Point::new(1.0, 0.0), rotation_deg: 0.0, height: 2.0, value: "PART-7".into(), is_multiline: false });

        // placed at (50, 30) rotated 90 degrees
        let layout = SheetLayout { sheet: square(200.0), parts: vec![PlacedShape { shape: part, x: 50.0, y: 30.0, rotation: 90.0 }] };
        let drawing = export_dxf(std::slice::from_ref(&layout), 20.0, false);

        let texts: Vec<&Text> = drawing.entities().filter_map(|e| if let EntityType::Text(t) = &e.specific { Some(t) } else { None }).collect();
        assert_eq!(texts.len(), 1);
        let text = texts[0];
        assert_eq!(text.value, "PART-7");
        // local (1,0) rotated 90 degrees -> (0,1), then shifted by (50,30)
        assert!((text.location.x - 50.0).abs() < 1e-9, "x was {}", text.location.x);
        assert!((text.location.y - 31.0).abs() < 1e-9, "y was {}", text.location.y);
        assert!((text.rotation - 90.0).abs() < 1e-9, "rotation was {}", text.rotation);
        assert_eq!(text.text_height, 2.0);
    }

    #[test]
    fn a_parts_attached_mtext_round_trips_with_the_rotation_unit_converted_back_to_radians() {
        let mut part = square(10.0);
        part.texts.push(TextAnnotation { position: Point::new(0.0, 0.0), rotation_deg: 90.0, height: 3.0, value: "MULTI".into(), is_multiline: true });

        let layout = SheetLayout { sheet: square(200.0), parts: vec![PlacedShape { shape: part, x: 0.0, y: 0.0, rotation: 0.0 }] };
        let drawing = export_dxf(std::slice::from_ref(&layout), 20.0, false);

        let mtexts: Vec<&MText> = drawing.entities().filter_map(|e| if let EntityType::MText(t) = &e.specific { Some(t) } else { None }).collect();
        assert_eq!(mtexts.len(), 1);
        assert_eq!(mtexts[0].text, "MULTI");
        assert!((mtexts[0].rotation_angle - std::f64::consts::FRAC_PI_2).abs() < 1e-9, "MTEXT rotation must be written back in radians, got {}", mtexts[0].rotation_angle);
    }

    #[test]
    fn pack_unplaced_parts_lays_parts_out_without_overlapping() {
        let parts = vec![square(10.0), square(20.0), square(15.0)];
        let layout = pack_unplaced_parts(&parts, 5.0);

        assert_eq!(layout.sheet.layer, "UNPLACED");
        assert_eq!(layout.parts.len(), 3);

        // every placed part's actual (shifted) bounds, so overlap can be
        // checked directly instead of trusting the packer's own bookkeeping
        let bounds: Vec<_> = layout
            .parts
            .iter()
            .map(|p| {
                let shifted = shift_layered_polygon(&p.shape, p.x, p.y);
                get_polygon_bounds(&shifted.points).unwrap()
            })
            .collect();
        for i in 0..bounds.len() {
            for j in (i + 1)..bounds.len() {
                let a = &bounds[i];
                let b = &bounds[j];
                let overlap_x = a.x < b.x + b.width && b.x < a.x + a.width;
                let overlap_y = a.y < b.y + b.height && b.y < a.y + a.height;
                assert!(!(overlap_x && overlap_y), "parts {i} and {j} overlap: {a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn pack_unplaced_parts_wraps_to_a_new_row_past_the_max_width() {
        // Ten parts of 150mm each (1500mm total) must not fit in one 1000mm
        // row - the packer should wrap to at least a second row, i.e. the
        // synthetic sheet's height must exceed a single part's own height.
        let parts: Vec<_> = (0..10).map(|_| square(150.0)).collect();
        let layout = pack_unplaced_parts(&parts, 5.0);
        let sheet_bounds = get_polygon_bounds(&layout.sheet.points).unwrap();
        assert!(sheet_bounds.height > 150.0, "expected wrapping to a second row, got height {}", sheet_bounds.height);
    }

    #[test]
    fn pack_unplaced_parts_handles_an_empty_list() {
        let layout = pack_unplaced_parts(&[], 5.0);
        assert!(layout.parts.is_empty());
    }

    /// A part that was a true circle on import must round-trip as a real
    /// `CIRCLE` entity on export, not a tessellated `LWPOLYLINE` - and its
    /// center must reflect the placement's own rotation+translation, the
    /// same as `points` would.
    #[test]
    fn a_true_circle_part_exports_as_a_real_circle_entity_at_its_placed_position() {
        let circle_shape = LayeredPolygon {
            points: crate::dxf_import::tessellate_circle(0.0, 0.0, 5.0, 0.1),
            layer: "CUT".into(),
            is_circle: Some(crate::circular_nfp::Circle { cx: 0.0, cy: 0.0, r: 5.0 }),
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        };
        // placed at (50, 30) - rotation is a no-op for a circle's center at
        // the origin, but exercises the same rotate-then-shift pipeline
        // every other part goes through.
        let layout = SheetLayout { sheet: square(200.0), parts: vec![PlacedShape { shape: circle_shape, x: 50.0, y: 30.0, rotation: 45.0 }] };
        let drawing = export_dxf(std::slice::from_ref(&layout), 20.0, false);

        let circles: Vec<&Circle> = drawing.entities().filter_map(|e| if let EntityType::Circle(c) = &e.specific { Some(c) } else { None }).collect();
        assert_eq!(circles.len(), 1, "circle part must export as a real CIRCLE entity, not a tessellated LwPolyline");
        assert!((circles[0].center.x - 50.0).abs() < 1e-9, "x was {}", circles[0].center.x);
        assert!((circles[0].center.y - 30.0).abs() < 1e-9, "y was {}", circles[0].center.y);
        assert!((circles[0].radius - 5.0).abs() < 1e-9);
        assert_eq!(entities_on_layer(&drawing, "CUT").len(), 0, "no tessellated LwPolyline should be written for a true circle");
    }

    /// A part with a retained bulge boundary (rounded corner) must export
    /// with real per-vertex `bulge` values, not the flat `0.0` a purely
    /// tessellated part gets - and those vertices must reflect the
    /// placement's rotation+translation.
    #[test]
    fn a_part_with_a_real_boundary_exports_with_real_bulge_values() {
        let bulge_shape = LayeredPolygon {
            points: vec![Point::new(1.0, 0.0), Point::new(0.0, 1.0)], // tessellated approximation, deliberately coarse/wrong-looking
            layer: "CUT".into(),
            is_circle: None,
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: Some(vec![
                crate::dxf_import::RealVertex { point: Point::new(1.0, 0.0), bulge: 0.5 },
                crate::dxf_import::RealVertex { point: Point::new(0.0, 1.0), bulge: 0.0 },
            ]),
        };
        let layout = SheetLayout { sheet: square(200.0), parts: vec![PlacedShape { shape: bulge_shape, x: 10.0, y: 0.0, rotation: 0.0 }] };
        let drawing = export_dxf(std::slice::from_ref(&layout), 20.0, false);

        let parts = entities_on_layer(&drawing, "CUT");
        assert_eq!(parts.len(), 1);
        let bulges: Vec<f64> = parts[0].vertices.iter().map(|v| v.bulge).collect();
        assert_eq!(bulges, vec![0.5, 0.0], "the real bulge value must be written, not the tessellated 0.0");
        assert!((parts[0].vertices[0].x - 11.0).abs() < 1e-9, "real_boundary point must be shifted the same as points");
    }
}
