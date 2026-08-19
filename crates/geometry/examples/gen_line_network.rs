//! One-off generator for `tests/fixtures/line-network.dxf`: an 80x60 outer
//! profile drawn purely as four bare `LINE`s (no closed polyline anywhere),
//! containing a rounded-slot hole drawn as two `LINE`s plus two partial
//! `ARC`s, plus a `CIRCLE` on a second layer so the fixture also proves
//! chained rings and normally-converted ones still nest together correctly.
//!
//! Every entity here is one that `entity_to_polygon` rejects on its own -
//! before `entities_to_polygons_chained` this whole file imported as exactly
//! one shape (the circle) instead of three.
//!
//! Run with `cargo run -p geometry --example gen_line_network` and commit the
//! result; this exists so the fixture is reproducible rather than a mystery
//! binary blob.

use dxf::entities::{Arc, Circle, Entity, EntityCommon, EntityType, Line};
use dxf::{Drawing, Point};

fn line(drawing: &mut Drawing, layer: &str, from: (f64, f64), to: (f64, f64)) {
    drawing.add_entity(Entity {
        common: EntityCommon { layer: layer.to_string(), ..Default::default() },
        specific: EntityType::Line(Line { p1: Point::new(from.0, from.1, 0.0), p2: Point::new(to.0, to.1, 0.0), ..Default::default() }),
    });
}

fn arc(drawing: &mut Drawing, layer: &str, center: (f64, f64), radius: f64, start: f64, end: f64) {
    drawing.add_entity(Entity {
        common: EntityCommon { layer: layer.to_string(), ..Default::default() },
        specific: EntityType::Arc(Arc {
            center: Point::new(center.0, center.1, 0.0),
            radius,
            start_angle: start,
            end_angle: end,
            ..Default::default()
        }),
    });
}

fn main() {
    let mut drawing = Drawing::new();
    drawing.header.version = dxf::enums::AcadVersion::R2000;

    // Outer 80x60 profile, as four loose lines, deliberately out of order and
    // with mixed directions.
    line(&mut drawing, "CUT", (80.0, 0.0), (80.0, 60.0));
    line(&mut drawing, "CUT", (80.0, 60.0), (0.0, 60.0));
    line(&mut drawing, "CUT", (0.0, 0.0), (80.0, 0.0));
    line(&mut drawing, "CUT", (0.0, 60.0), (0.0, 0.0));

    // A rounded slot hole: 30 long, 10 wide, centred at (40, 30).
    line(&mut drawing, "CUT", (25.0, 35.0), (55.0, 35.0));
    arc(&mut drawing, "CUT", (55.0, 30.0), 5.0, 270.0, 90.0);
    line(&mut drawing, "CUT", (55.0, 25.0), (25.0, 25.0));
    arc(&mut drawing, "CUT", (25.0, 30.0), 5.0, 90.0, 270.0);

    // A plain drilled hole on its own layer - converts without chaining.
    drawing.add_entity(Entity {
        common: EntityCommon { layer: "DRILL".to_string(), ..Default::default() },
        specific: EntityType::Circle(Circle { center: Point::new(12.0, 12.0, 0.0), radius: 4.0, ..Default::default() }),
    });

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/line-network.dxf");
    drawing.save_file(path).expect("write fixture");
    println!("wrote {path}");
}
