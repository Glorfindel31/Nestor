//! Regenerates `tests/fixtures/one.svg` from `one.dxf` at
//! full float precision.
//!
//! The original fixture was hand-exported with coordinates rounded to 8
//! decimals, which makes its *edge lengths* wrong by up to 8.4e-9 - eight
//! times `polygon::TOL`. For an exactly-interlocking tessellation like the
//! hat monotile that is not a rounding detail, it is a different shape (see
//! `crates/nesting/examples/hat_test_svg.rs`).
//!
//! Coordinates are printed with Rust's default `{}` float formatting, which
//! emits the shortest decimal that round-trips back to the identical `f64` -
//! so re-importing this file reproduces the DXF's own values bit for bit.
//!
//! Run with `cargo run -p geometry --example gen_hat_svg`.

use dxf::Drawing;
use geometry::dxf_import::entities_to_polygons_chained;

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
}

fn main() {
    let drawing = Drawing::load_file(fixture("one.dxf")).unwrap();
    let points = entities_to_polygons_chained(drawing.entities(), 0.1).remove(0).points;

    let min_x = points.iter().map(|p| p.x).fold(f64::MAX, f64::min);
    let min_y = points.iter().map(|p| p.y).fold(f64::MAX, f64::min);
    let max_x = points.iter().map(|p| p.x).fold(f64::MIN, f64::max);
    let max_y = points.iter().map(|p| p.y).fold(f64::MIN, f64::max);
    let (w, h) = (max_x - min_x, max_y - min_y);

    // SVG is Y-down, this codebase is Y-up: flip on the way out so
    // `svg_import`'s own flip brings it back to exactly these values.
    let d: Vec<String> = points.iter().map(|p| format!("{},{}", p.x - min_x, max_y - p.y)).collect();

    let svg = format!(
        concat!(
            "<?xml version=\"1.0\"?>\n",
            "<!-- Regenerated from one.dxf by crates/geometry/examples/gen_hat_svg.rs.\n",
            "     Coordinates are full-precision on purpose: the previous 8-decimal version\n",
            "     was a measurably different shape at the engine's 1e-9 tolerance. -->\n",
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}mm\" height=\"{h}mm\" viewBox=\"0 0 {w} {h}\">\n",
            "    <g fill=\"none\" stroke=\"black\" stroke-width=\"0.1\">\n",
            "        <path d=\"M{first} L{rest} Z\" />\n",
            "    </g>\n",
            "</svg>\n"
        ),
        w = w,
        h = h,
        first = d[0],
        rest = d[1..].join(" L")
    );

    let path = fixture("one.svg");
    std::fs::write(&path, svg).unwrap();
    println!("wrote {}", path.display());
}
