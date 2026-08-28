//! What an outward simplification of the padded outline buys the NFP.
//!
//! `curvy.dxf` spends ~65s inside one `obstacle_nfp`, and "the outline has
//! 303 points" does not explain that - the NFP runs on the *padded* outline,
//! and `clearance::prepare_part`'s round join nearly doubles the count.
//! Minkowski cost is superlinear in that count, so this measures the trade:
//! offset outward by an extra `t`, simplify at `t`, and the result still
//! contains the true buffer while carrying far fewer points.

use std::time::Instant;

use geometry::clipper::{offset_round, outer_nfp};
use geometry::dxf_import::{build_polygon_tree, entities_to_polygons};
use geometry::polygon::polygon_area;
use geometry::simplify::simplify;

fn main() {
    let name = std::env::args().nth(1).unwrap_or_else(|| "curvy.dxf".into());
    let spacing: f64 = std::env::args().nth(2).map_or(5.0, |s| s.parse().expect("spacing"));
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(&name);
    let drawing = dxf::Drawing::load_file(&path).unwrap_or_else(|e| panic!("couldn't parse {}: {e}", path.display()));
    let tree = build_polygon_tree(entities_to_polygons(drawing.entities(), 0.3));

    let Some(part) = tree.first() else { return };
    println!("{name}: raw {} pts, spacing {spacing}", part.points.len());
    for t in [0.0f64, 0.05, 0.1, 0.25, 0.5] {
        let padded = offset_round(&part.points, spacing / 2.0 + t)
            .into_iter()
            .max_by(|a, b| polygon_area(a).abs().total_cmp(&polygon_area(b).abs()))
            .expect("padding");
        let simplified = if t > 0.0 { simplify(&padded, Some(t), true) } else { padded.clone() };
        let started = Instant::now();
        let nfp = outer_nfp(&simplified, &simplified);
        println!("  t={t:<5} padded {:>4} -> {:>4} pts   self outer_nfp {:>8.2}s -> {} pts", padded.len(), simplified.len(), started.elapsed().as_secs_f64(), nfp.map_or(0, |n| n.len()));
    }
}
