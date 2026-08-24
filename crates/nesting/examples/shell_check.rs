//! Does `banded::shell_of` pair this part on its true outline, or on a hull?
//!
//! The distinction is worth whole sheets. `shell_of` pairs on the real outline
//! up to a point-count bound and on a coarse convex shell above it, purely as
//! a cost decision - and for a concave part the material a hull fills in *is*
//! the interlocking opportunity, so crossing that bound silently caps the band
//! packer at the bounding-box answer. `nestTest03` lost a sheet to exactly
//! that until the bound was introduced.
//!
//! The number that matters is the count of the **padded** outline, since that
//! is what `place_parts` hands the packer - and a round clearance offset adds
//! points at every convex corner. Reading it off the raw fixture under-counts.
//!
//! Usage: `cargo run --release -p nesting --example shell_check -- <spacing> <tolerance> <file.dxf>...`

use dxf::Drawing;
use geometry::clearance::prepare_part;
use geometry::dxf_import::{build_polygon_tree, entities_to_polygons};
use geometry::hull_polygon::hull;
use geometry::polygon::polygon_area;

fn main() {
    let mut args = std::env::args().skip(1);
    let spacing: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let tolerance: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0.1);

    println!("spacing {spacing}, curve tolerance {tolerance}");
    println!("{:<16} {:>7} {:>7} {:>9} {:>9} {:>7}", "file", "raw", "padded", "area", "hullarea", "fill");
    for path in args {
        let Ok(drawing) = Drawing::load_file(&path) else {
            eprintln!("{path}: could not parse");
            continue;
        };
        for shape in build_polygon_tree(entities_to_polygons(drawing.entities(), tolerance)) {
            let Some(padded) = prepare_part(&shape.points, spacing) else { continue };
            let area = polygon_area(&padded).abs();
            let hull_area = hull(&padded).map_or(f64::NAN, |h| polygon_area(&h).abs());
            println!(
                "{:<16} {:>7} {:>7} {:>9.0} {:>9.0} {:>6.1}%",
                std::path::Path::new(&path).file_name().unwrap_or_default().to_string_lossy(),
                shape.points.len(),
                padded.len(),
                area,
                hull_area,
                area / hull_area * 100.0
            );
        }
    }
}
