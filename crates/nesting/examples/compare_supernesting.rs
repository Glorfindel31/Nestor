//! One-off analysis (not a benchmark): extracts ground-truth numbers from the
//! "supernesting" tool's own reference layouts for the aperiodic "hat"
//! monotile, at 2/20/252 parts on a 500x500mm sheet
//! (`tests/fixtures/supernesting {2,20,252}part(s) 500x500.dxf`). Each
//! reference file embeds a `SheetMat`-layer 500x500 sheet outline plus N
//! already-placed copies of the same hat shape as `tests/fixtures/one.dxf`
//! (confirmed by edge-length signature: 12.99/7.5mm edges, sqrt(3):1 ratio,
//! matching the hat's kite construction) - `build_polygon_tree`'s containment
//! nesting puts every part as a child of the sheet root for free, since none
//! of the (disjoint) parts contain each other.
//!
//! Reports reference utilisation (what we need to beat/match) and, per part,
//! its rotation relative to the canonical hat shape (edge0's angle - every
//! placed part shares the same point order/winding as the canonical shape,
//! since these are plain rotate+translate copies) and whether it was
//! mirrored (signed-area sign flip) - direct evidence for whether
//! supernesting draws from a small discrete rotation grid or something wider,
//! and whether mirroring is actually in play here.
//!
//! The three reference files above are **not checked into this repo**
//! (third-party output, pulled manually from the supernesting tool) - this
//! example only runs for whoever has copied them into `tests/fixtures/`
//! locally. The numbers this example was already used to derive are recorded
//! as-is in `hat_test.rs`'s `target_utilisation_pct`; missing files here
//! don't invalidate those, they just mean this particular derivation can't
//! be re-run/audited without sourcing the files again.
//!
//! Usage: `cargo run --release -p nesting --example compare_supernesting`

use std::path::PathBuf;

use dxf::Drawing;
use geometry::dxf_import::{build_polygon_tree, entities_to_polygons};
use geometry::point::Point;
use geometry::polygon::polygon_area;

const CURVE_TOLERANCE: f64 = 0.1;
const REFERENCE_FILES: &[&str] = &["supernesting 2parts 500x500.dxf", "supernesting 20part 500x500.dxf", "supernesting 252parts 500x500.dxf"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn edge_angle(points: &[Point]) -> f64 {
    let dx = points[1].x - points[0].x;
    let dy = points[1].y - points[0].y;
    dy.atan2(dx).to_degrees()
}

fn main() {
    let hat_path = repo_root().join("tests/fixtures/one.dxf");
    let hat_drawing = Drawing::load_file(&hat_path).unwrap_or_else(|e| panic!("couldn't parse {}: {e}", hat_path.display()));
    let hat_flat = entities_to_polygons(hat_drawing.entities(), CURVE_TOLERANCE);
    let hat_tree = build_polygon_tree(hat_flat);
    assert_eq!(hat_tree.len(), 1, "expected exactly one closed profile in one.dxf");
    let hat = &hat_tree[0];
    let hat_area = polygon_area(&hat.points).abs();
    let hat_signed_area = polygon_area(&hat.points);
    let hat_angle = edge_angle(&hat.points);

    println!("canonical hat: {} vertices, area {:.3}mm2, signed_area {:.3}, edge0 angle {:.2}deg\n", hat.points.len(), hat_area, hat_signed_area, hat_angle);

    for name in REFERENCE_FILES {
        let path = repo_root().join("tests/fixtures").join(name);
        if !path.exists() {
            println!("=== {name} ===\n  skipped: not present locally (see module doc - these reference files aren't checked into the repo)\n");
            continue;
        }
        let drawing = Drawing::load_file(&path).unwrap_or_else(|e| panic!("couldn't parse {}: {e}", path.display()));
        let flat = entities_to_polygons(drawing.entities(), CURVE_TOLERANCE);
        let tree = build_polygon_tree(flat);
        assert_eq!(tree.len(), 1, "expected exactly one root (the sheet) in {name}, got {}", tree.len());
        let sheet = &tree[0];
        let sheet_area = polygon_area(&sheet.points).abs();
        let parts = &sheet.children;

        let mut total_part_area = 0.0;
        let mut mirrored_count = 0usize;
        let mut area_min = f64::INFINITY;
        let mut area_max = f64::NEG_INFINITY;
        let mut rotations: Vec<f64> = Vec::new();
        for part in parts {
            let area = polygon_area(&part.points).abs();
            total_part_area += area;
            area_min = area_min.min(area);
            area_max = area_max.max(area);
            let signed = polygon_area(&part.points);
            if signed.signum() != hat_signed_area.signum() {
                mirrored_count += 1;
            }
            let angle = (edge_angle(&part.points) - hat_angle).rem_euclid(360.0);
            rotations.push(angle);
        }
        rotations.sort_by(f64::total_cmp);

        let mut buckets: Vec<i64> = rotations.iter().map(|r| r.round() as i64).collect();
        buckets.sort_unstable();
        buckets.dedup();

        let utilisation = total_part_area / sheet_area * 100.0;
        println!("=== {name} ===");
        println!("  sheet area: {sheet_area:.1}mm2, parts: {}", parts.len());
        println!("  per-part area: min {area_min:.3}, max {area_max:.3} (canonical {hat_area:.3}) - sanity check these all match");
        println!("  reference utilisation: {utilisation:.2}%");
        println!("  mirrored parts: {mirrored_count}/{}", parts.len());
        println!("  distinct rotation values (rounded to nearest degree): {} -> {:?}", buckets.len(), buckets);
        println!("  full rotation list: {:?}\n", rotations.iter().map(|r| format!("{r:.1}")).collect::<Vec<_>>());
    }
}
