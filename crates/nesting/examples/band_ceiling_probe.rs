//! What is the real ceiling of band packing on a job, and what moves it?
//! (`docs/PLAN.md` 2.1, the "prove it before plumbing anything" step.)
//!
//! Written to test one hypothesis and it disproved it. `PLAN.md` held that
//! the reference tool's 16-part / 88.1% sheet needed **common-line cutting**,
//! on the arithmetic that two of these triangles at zero clearance pair into
//! the bare part box, 776.5 + 422.4 = 1198.9 <= the usable height. Measured
//! against the real outlines, they do not: the tightest legal zero-clearance
//! pair box is 776.7 x 452.5, thirty millimetres taller than the part box,
//! because the apex sits part way along the top edge so a 180-degree copy
//! tiles a parallelogram. Searching band sequences over the whole Pareto
//! front of pair boxes tops out at **14 parts either way** - common lines buy
//! nothing at all here.
//!
//! What does buy it is the **row step**. A row of parallelograms interlocks:
//! consecutive copies overlap in x by the slanted overhang, so the lattice
//! period is ~62mm shorter than the bounding box. Advance a row by the
//! measured step instead of the box width and the same search finds **16
//! parts, 88.11%** - the reference tool's number to the second decimal, at
//! the full 6mm spacing, with no common line anywhere. That is now
//! `banded::row_step`, and `banded_real_geometry.rs`'s
//! `a_row_advances_by_the_lattice_step_not_the_box_width` holds it.
//!
//! Kept as the thing that answers "why is the ceiling here and not there" for
//! the next job that disappoints. It changes nothing - it measures pair boxes
//! and runs a DP over band sequences, and prints.
//!
//!   cargo run --release -p nesting --example band_ceiling_probe
//!   cargo run --release -p nesting --example band_ceiling_probe -- three.dxf 6

use std::path::PathBuf;

use dxf::Drawing;
use geometry::clearance::prepare_part;
use geometry::dxf_import::{build_polygon_tree, entities_to_polygons, rotate_layered_polygon, shift_layered_polygon, LayeredPolygon};
use geometry::obstacle_nfp::obstacle_nfp;
use geometry::point::Point;
use geometry::polygon::{get_polygon_bounds, polygon_area};

const SHEET_W: f64 = 2440.0;
const SHEET_H: f64 = 1220.0;
const CURVE_TOLERANCE: f64 = 0.1;
/// Same angles `banded::build_units` tries.
const PAIR_ANGLES: [f64; 4] = [180.0, 90.0, 270.0, 0.0];

/// A pair box: its size, and the placement of the second member that produced
/// it.
#[derive(Clone, Copy, Debug)]
struct Box2 {
    w: f64,
    h: f64,
    rotation: f64,
    sx: f64,
    sy: f64,
}

fn load_profiles(name: &str) -> Vec<LayeredPolygon> {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join("tests/fixtures").join(name);
    let drawing = Drawing::load_file(&fixture).unwrap_or_else(|e| panic!("couldn't parse {}: {e}", fixture.display()));
    let mut tree = build_polygon_tree(entities_to_polygons(drawing.entities(), CURVE_TOLERANCE));
    tree.sort_by(|a, b| polygon_area(&b.points).abs().total_cmp(&polygon_area(&a.points).abs()));
    tree
}

fn padded(shape: &LayeredPolygon, gap: f64) -> LayeredPolygon {
    if gap == 0.0 {
        return shape.clone();
    }
    let points = prepare_part(&shape.points, gap).expect("part should offset cleanly");
    LayeredPolygon { points, ..shape.clone() }
}

/// Every pair box reachable by walking the NFP of two copies held `gap` apart,
/// Pareto-thinned. Boxes are measured on the *true* outlines, so a `gap` of 0
/// gives the bare part box the reference tool's pattern unit is built on.
fn pair_boxes(a_shape: &LayeredPolygon, b_shape: &LayeredPolygon, gap: f64) -> Vec<Box2> {
    let a_pad = padded(a_shape, gap);
    let ab = get_polygon_bounds(&a_shape.points).expect("bounds");
    let mut out: Vec<Box2> = Vec::new();

    for extra in PAIR_ANGLES {
        let b_true = rotate_layered_polygon(b_shape, extra);
        let b_pad = rotate_layered_polygon(&padded(b_shape, gap), extra);
        let Some(bb) = get_polygon_bounds(&b_true.points) else { continue };
        let Some(nfp) = obstacle_nfp(&a_pad, &b_pad, CURVE_TOLERANCE) else { continue };
        let b_ref = b_pad.points.first().copied().unwrap_or(Point::new(0.0, 0.0));
        // The NFP moves the padded B; every box below is measured on the true
        // one, so carry the offset between the two bounding boxes.
        let pad_off = get_polygon_bounds(&b_pad.points).map(|p| (bb.x - p.x, bb.y - p.y)).unwrap_or((0.0, 0.0));

        let n = nfp.outer.len() as f64;
        let (cx, cy) = (nfp.outer.iter().map(|p| p.x).sum::<f64>() / n, nfp.outer.iter().map(|p| p.y).sum::<f64>() / n);
        for (i, from) in nfp.outer.iter().enumerate() {
            let to = nfp.outer[(i + 1) % nfp.outer.len()];
            let samples = ((from.distance_to(to) / 0.5).ceil() as usize).max(1);
            for k in 0..samples {
                let t = k as f64 / samples as f64;
                let (mut vx, mut vy) = (from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
                let (ox, oy) = (vx - cx, vy - cy);
                let len = ox.hypot(oy);
                if len > 0.0 {
                    // Outward nudge, same reason as `banded`: a sample sitting
                    // exactly on the NFP is a coin toss for the overlap test.
                    vx += ox / len * 0.01;
                    vy += oy / len * 0.01;
                }
                let (sx, sy) = (vx - b_ref.x, vy - b_ref.y);
                let (mx, my) = (bb.x + sx + pad_off.0, bb.y + sy + pad_off.1);
                let x0 = ab.x.min(mx);
                let y0 = ab.y.min(my);
                let w = (ab.x + ab.width).max(mx + bb.width) - x0;
                let h = (ab.y + ab.height).max(my + bb.height) - y0;
                if w > 0.0 && h > 0.0 {
                    out.push(Box2 { w, h, rotation: extra, sx, sy });
                }
            }
        }
    }

    // Pareto front on (w, h). Unlike `banded::pareto_front` this does not snap
    // to a 5mm grid first - the whole question here is whether a pair box
    // clears the sheet height by a few millimetres, and rounding 5mm up in
    // both dimensions is ten times the margin being measured.
    out.sort_by(|a, b| a.w.total_cmp(&b.w).then(a.h.total_cmp(&b.h)));
    let mut front: Vec<Box2> = Vec::new();
    let mut best_h = f64::INFINITY;
    for b in out {
        if b.h < best_h {
            best_h = b.h;
            front.push(b);
        }
    }
    front
}

/// Do the two members of this pair actually clear each other by `gap`?
fn pair_is_legal(a_shape: &LayeredPolygon, b_shape: &LayeredPolygon, b: &Box2, gap: f64) -> bool {
    let members = unit_members(a_shape, b_shape, b, gap);
    !nesting::placement::has_material_overlap(&members[0], &members[1])
}

/// The unit as real geometry, padded by `gap`, in its own coordinates.
/// `pad` is the clearance to grow each member by - `gap` when asking whether
/// the pair is legal, 0 when measuring the box or handing it to `row_step`
/// (which applies the row's own spacing itself).
fn unit_members(a_shape: &LayeredPolygon, b_shape: &LayeredPolygon, b: &Box2, pad: f64) -> Vec<LayeredPolygon> {
    vec![padded(a_shape, pad), shift_layered_polygon(&rotate_layered_polygon(&padded(b_shape, pad), b.rotation), b.sx, b.sy)]
}

/// The tightest horizontal step at which this unit can repeat along a row,
/// with every part `spacing` from its neighbours.
///
/// **This is the number band packing never asks for, and it is the whole
/// point of this probe.** `banded` advances a row by the unit's bounding-box
/// width, which for two triangles paired into a parallelogram throws away the
/// slanted overhang - about 62mm per unit on this fixture, or a whole part
/// every third one. Two parallelograms interlock: the step is the *lattice*
/// period, not the box width. Measured by bisection on the real outlines
/// (padded to `spacing`, so this is honest clearance, not a common line).
fn row_step(members: &[LayeredPolygon], spacing: f64, box_w: f64) -> f64 {
    // Members are already padded by whatever gap the *pair* uses, which may be
    // 0 for a common line; a neighbour in the row is a different unit and gets
    // the full spacing regardless.
    let members: Vec<_> = members.iter().map(|m| LayeredPolygon { points: prepare_part(&m.points, spacing).unwrap_or_else(|| m.points.clone()), ..m.clone() }).collect();
    let clear = |dx: f64| {
        let shifted: Vec<_> = members.iter().map(|m| shift_layered_polygon(m, dx, 0.0)).collect();
        members.iter().all(|a| shifted.iter().all(|b| !nesting::placement::has_material_overlap(a, b)))
    };
    let mut lo = 0.0;
    let mut hi = box_w + spacing;
    if !clear(hi) {
        return hi;
    }
    while hi - lo > 0.05 {
        let mid = (lo + hi) / 2.0;
        if clear(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
}

/// A band-packable unit: its bounding box, how many parts it holds, and the
/// horizontal period at which it repeats along a row.
#[derive(Clone, Debug)]
struct Unit2 {
    w: f64,
    h: f64,
    step: f64,
    parts: usize,
    label: String,
}

/// Builds both orientations of one unit, each with its own measured row step.
fn unit_pair(members: &[LayeredPolygon], parts: usize, spacing: f64, label: &str) -> Vec<Unit2> {
    [0.0, 90.0]
        .iter()
        .filter_map(|&turn| {
            let turned: Vec<_> = members.iter().map(|m| rotate_layered_polygon(m, turn)).collect();
            let mut min_x = f64::INFINITY;
            let mut min_y = f64::INFINITY;
            let mut max_x = f64::NEG_INFINITY;
            let mut max_y = f64::NEG_INFINITY;
            for m in &turned {
                let b = get_polygon_bounds(&m.points)?;
                min_x = min_x.min(b.x);
                min_y = min_y.min(b.y);
                max_x = max_x.max(b.x + b.width);
                max_y = max_y.max(b.y + b.height);
            }
            let (w, h) = (max_x - min_x, max_y - min_y);
            Some(Unit2 { w, h, step: row_step(&turned, spacing, w), parts, label: format!("{label} @{turn}") })
        })
        .collect()
}

/// The best sheet reachable by stacking bands of these units, at `spacing`
/// between neighbouring units and between bands, margin 0.
///
/// A DP over sheet height rather than the two hand-picked bands an earlier
/// version of this probe assumed: nothing says the two bands have to be the
/// same box turned on its side, and restricting it that way is how you
/// measure your own assumption instead of the geometry. Every unit is offered
/// in both orientations; heights are bucketed at `BUCKET`, rounded *up*, so
/// the answer is never optimistic.
fn best_band_sheet(units: &[Unit2], spacing: f64) -> (usize, Vec<(Unit2, usize)>) {
    const BUCKET: f64 = 0.1;
    let buckets = (SHEET_H / BUCKET) as usize + 1;
    let options: Vec<(Unit2, usize)> = units
        .iter()
        .filter_map(|u| {
            if u.h > SHEET_H || u.w > SHEET_W {
                return None;
            }
            // One unit at x = 0, then one every `step` while the last still
            // ends inside the sheet.
            let across = ((SHEET_W - u.w) / u.step).floor() + 1.0;
            (across >= 1.0).then(|| (u.clone(), u.parts * across as usize))
        })
        .collect();
    let mut best = vec![0usize; buckets];
    let mut from = vec![usize::MAX; buckets];
    for b in 1..buckets {
        best[b] = best[b - 1];
        from[b] = from[b - 1];
        for (i, (u, n)) in options.iter().enumerate() {
            // Bands after the first pay `spacing` to the one below; charging
            // it to every band just costs the sheet one extra gap and keeps
            // this a plain one-dimensional recurrence.
            let cost = ((u.h + spacing) / BUCKET).ceil() as usize;
            if cost <= b && best[b - cost] + n > best[b] {
                best[b] = best[b - cost] + n;
                from[b] = i;
            }
        }
    }
    let mut bands = Vec::new();
    let mut b = buckets - 1;
    while b > 0 && from[b] != usize::MAX {
        if best[b] == best[b - 1] && from[b] == from[b - 1] {
            b -= 1;
            continue;
        }
        let (u, n) = options[from[b]].clone();
        b -= ((u.h + spacing) / BUCKET).ceil() as usize;
        bands.push((u, n));
    }
    (best[buckets - 1], bands)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let file = args.next().unwrap_or_else(|| "two.dxf".into());
    let spacing: f64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(6.0);
    let sheet_area = SHEET_W * SHEET_H;
    let profiles = load_profiles(&file);
    let area = polygon_area(&profiles[0].points).abs();

    for (i, shape) in profiles.iter().enumerate() {
        let bounds = get_polygon_bounds(&shape.points).expect("bounds");
        println!("profile #{} - {:.2} x {:.2}, area {:.0} mm^2", i + 1, bounds.width, bounds.height, polygon_area(&shape.points).abs());
    }
    println!("
sheet {SHEET_W} x {SHEET_H}, margin 0, spacing {spacing}
");

    for gap in [spacing, 0.0] {
        for lattice in [false, true] {
            let mut catalogue: Vec<Unit2> = Vec::new();
            for (i, a) in profiles.iter().enumerate() {
                catalogue.extend(unit_pair(std::slice::from_ref(a), 1, spacing, &format!("#{}", i + 1)).into_iter().map(|u| squash(u, lattice, spacing)));
                for (j, b) in profiles.iter().enumerate().skip(i) {
                    // Every profile against every profile: all four here have
                    // the same area to the square millimetre, so they may well
                    // be one shape at four orientations.
                    let legal: Vec<Box2> = pair_boxes(a, b, gap).into_iter().filter(|x| pair_is_legal(a, b, x, gap)).collect();
                    for x in &legal {
                        let members = unit_members(a, b, x, 0.0);
                        catalogue.extend(unit_pair(&members, 2, spacing, &format!("#{}+#{}", i + 1, j + 1)).into_iter().map(|u| squash(u, lattice, spacing)));
                    }
                }
            }
            let (parts, bands) = best_band_sheet(&catalogue, spacing);
            println!("gap {gap}, rows {}: {parts} parts/sheet, {:.2}% util", if lattice { "interlock (measured step)" } else { "box-to-box (banded before row_step)" }, parts as f64 * area / sheet_area * 100.0);
            for (u, n) in bands {
                println!("    band {:.1} tall: {} unit {:.1} x {:.1}, step {:.1}, {n} parts", u.h, u.label, u.w, u.h, u.step);
            }
        }
    }
}

/// `banded` used to advance a row by the unit's bounding-box width; the whole
/// question is what the measured lattice step buys instead, so the
/// box-to-box behaviour is reproduced by simply forcing the step back to it.
fn squash(u: Unit2, lattice: bool, spacing: f64) -> Unit2 {
    if lattice {
        u
    } else {
        Unit2 { step: u.w + spacing, ..u }
    }
}
