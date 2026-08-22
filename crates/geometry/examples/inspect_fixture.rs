//! Prints what a DXF fixture actually contains - entity counts, layers, and
//! the shape of the polygon tree it imports to.
//!
//! Exists because every fixture-backed test in this repo asserts concrete
//! numbers (how many outer parts, which layers, how many holes), and those
//! numbers have to come from somewhere. Guessing them and then relaxing the
//! assertion until it passes produces a test that cannot fail; reading them
//! off the real file produces one that can.
//!
//! Usage: `cargo run -p geometry --example inspect_fixture -- two.dxf`

use geometry::dxf_import::{build_polygon_tree, entities_to_polygons, entities_to_polygons_chained, LayeredPolygon};
use geometry::polygon::{get_polygon_bounds, polygon_area};

const CURVE_TOLERANCE: f64 = 0.01;

fn describe(tree: &[LayeredPolygon], depth: usize, shown: &mut usize) {
    const MAX_SHOWN: usize = 12;
    for node in tree {
        if *shown >= MAX_SHOWN {
            return;
        }
        *shown += 1;
        let b = get_polygon_bounds(&node.points);
        let (w, h) = b.map(|b| (b.width, b.height)).unwrap_or((0.0, 0.0));
        println!(
            "{:indent$}layer={:<12} pts={:<5} area={:>12.1} bbox={:.1}x{:.1} circle={} children={} texts={}",
            "",
            node.layer,
            node.points.len(),
            polygon_area(&node.points).abs(),
            w,
            h,
            node.is_circle.is_some(),
            node.children.len(),
            node.texts.len(),
            indent = depth * 2
        );
        describe(&node.children, depth + 1, shown);
    }
}

fn main() {
    let name = std::env::args().nth(1).unwrap_or_else(|| "two.dxf".into());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(&name);
    let drawing = dxf::Drawing::load_file(&path).unwrap_or_else(|e| panic!("couldn't parse {}: {e}", path.display()));

    println!("== {name} ==");
    println!("entities            {}", drawing.entities().count());

    let flat = entities_to_polygons(drawing.entities(), CURVE_TOLERANCE);
    println!("closed profiles     {}", flat.len());
    println!("  of which circles  {}", flat.iter().filter(|p| p.is_circle.is_some()).count());

    let chained = entities_to_polygons_chained(drawing.entities(), CURVE_TOLERANCE);
    println!("with line-chaining  {}", chained.len());

    let mut layers: Vec<&str> = flat.iter().map(|p| p.layer.as_str()).collect();
    layers.sort_unstable();
    layers.dedup();
    println!("layers              {layers:?}");

    let tree = build_polygon_tree(flat);
    println!("outer parts (tree)  {}", tree.len());
    println!("total holes         {}", tree.iter().map(|t| t.children.len()).sum::<usize>());
    println!("max nesting depth   {}", depth_of(&tree));
    println!("tree (first few):");
    let mut shown = 0;
    describe(&tree, 1, &mut shown);
}

fn depth_of(tree: &[LayeredPolygon]) -> usize {
    tree.iter().map(|n| 1 + depth_of(&n.children)).max().unwrap_or(0)
}
