//! SVG entity -> polygon-tree conversion. Produces the same
//! `dxf_import::LayeredPolygon` shape DXF import does (same `build_polygon_tree`
//! containment nesting, same `.is_circle` fast-path metadata), so a part
//! imported from an SVG is indistinguishable from a DXF one to everything
//! downstream. DXF stays the primary/first-class import path (raw file
//! units, no conversion at all - see `dxf_import`'s module doc); SVG import
//! is additive, not a replacement, and does one thing DXF import doesn't
//! have to: resolve the file's own coordinate system into millimeters.
//!
//! **Metric only, by explicit product decision - this is enforced, not just
//! preferred.** An SVG's root `width`/`height` may be given in `mm`, `cm`,
//! `m`, `px`, or left unitless (unitless and `px` are treated identically,
//! matching the CSS/SVG reference-pixel definition of 96px = 1in used only
//! as a numeric scale constant here, never exposed as a selectable "inches"
//! import option). `in`/`pt`/`pc`/`ft`/`yd` are rejected outright with a
//! clear error - not silently converted - so an accidentally-imperial file
//! never produces a silently-wrong-sized part.
//!
//! Supported elements: `<rect>` (sharp corners only - `rx`/`ry` rounding is
//! not supported, same "reduce to what nesting needs" simplification
//! `dxf_import` already makes for text formatting), `<circle>`, `<ellipse>`,
//! `<polygon>`, `<polyline>` (only if explicitly closed - first point equals
//! last, same parity as an open `LWPOLYLINE` in `dxf_import`), and `<path>`
//! (`M`/`L`/`H`/`V`/`C`/`S`/`Q`/`T`/`A`/`Z`, both absolute and relative,
//! including implicit command repetition). Only `Z`-terminated subpaths of a
//! `<path>` become profiles - an open subpath (or trailing data after the
//! last `Z`) has no closed boundary to nest against, same skip `dxf_import`
//! already applies to an open `LWPOLYLINE`. `<text>`/`<tspan>`, `<defs>`,
//! `<use>`/`<symbol>` (no block-expansion, mirrors `dxf_import` not
//! supporting `INSERT`), `<style>`/CSS-driven geometry, and rounded-rect
//! corners are all deliberately not handled - not a smaller version of this
//! module, a separate scope.
//!
//! `transform` (on any element or `<g>`) is fully composed down the tree -
//! `translate`/`scale`/`rotate`/`matrix` (`skewX`/`skewY` are rejected, not
//! silently dropped, since ignoring a skew would silently distort a part).
//! A `<circle>`'s `.is_circle` fast-path metadata only survives a transform
//! that's a similarity (uniform scale + rotation, no shear/non-uniform
//! scale) - see `uniform_scale`; anything else still tessellates correctly,
//! it just loses the circular-hole NFP fast path for that one shape.
//!
//! Layer: the nearest ancestor `<g>`'s `id` attribute (defaulting to `"0"`,
//! matching `dxf_import`'s untagged-entity default) - deliberately not
//! `inkscape:label` too, which would need namespaced-attribute lookup for a
//! nicety most real cut files don't need (group `id`s are already
//! human-assigned layer names in the common Inkscape-layers-panel case).

use roxmltree::Node;

use crate::circular_nfp::Circle;
use crate::dxf_import::{segment_count, LayeredPolygon};
use crate::point::Point;

/// CSS/SVG reference pixel: 96px per inch, expressed directly in mm. A pure
/// numeric scale constant - not a supported "inches" import unit (see module
/// doc), it only exists to give an unadorned/`px` coordinate a real-world mm
/// size, exactly like every SVG renderer already assumes.
const MM_PER_PX: f64 = 25.4 / 96.0;

/// Parses `svg_text` into a flat list of closed profiles (no parent/hole
/// nesting yet - pass the result through `dxf_import::build_polygon_tree`,
/// same as `dxf_import::entities_to_polygons`'s own caller does).
///
/// `unit_override`, when `Some`, names the real-world unit one raw SVG user
/// unit represents (`"mm"`/`"cm"`/`"m"`/`"px"`) and skips
/// `viewBox`/`width`/`height`-based auto-detection entirely - the frontend
/// prompts for this on every SVG import (many real-world SVGs carry no
/// physically-meaningful `width`/`height` at all, or ones a design tool
/// invented rather than the part's true intended size, so auto-detection
/// alone isn't trustworthy enough to apply silently). `None` keeps the
/// original auto-detect behavior (`resolve_scale`). Either path still
/// rejects an imperial unit outright - `unit_override` only changes *which*
/// unit string gets validated/resolved, never loosens the metric-only rule.
pub fn parse_svg(svg_text: &str, curve_tolerance: f64, unit_override: Option<&str>) -> Result<Vec<LayeredPolygon>, String> {
    let doc = roxmltree::Document::parse(svg_text).map_err(|e| format!("couldn't parse SVG: {e}"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "svg" {
        return Err("not an SVG document (root element isn't <svg>)".to_string());
    }
    let (scale_x, scale_y) = match unit_override {
        Some(unit) => {
            let f = mm_per_unit(unit)?;
            (f, f)
        }
        None => resolve_scale(&root)?,
    };
    // SVG's user coordinate system has +Y pointing *down* (SVG spec); every
    // other geometry in this codebase (DXF import, the whole Clipper2/NFP
    // pipeline) uses +Y *up*, the standard CAD/math convention. Without this
    // negation, an imported SVG part is silently mirrored vertically - which
    // also reverses its winding direction (CCW becomes CW), corrupting every
    // winding-sensitive algorithm downstream (Clipper2 offset direction, NFP
    // tracing) even though area/bounding-box checks alone wouldn't catch it
    // (confirmed against a real fixture: hat-monotile.svg and
    // hat-monotile.dxf describe the same physical part, and without this
    // flip their `polygon_area` signs disagree even though the unsigned
    // areas match exactly - see svg_fixtures.rs's cross-format parity test).
    let base = Mat { a: scale_x, b: 0.0, c: 0.0, d: -scale_y, e: 0.0, f: 0.0 };

    let mut out = Vec::new();
    walk(root, base, "0", curve_tolerance, &mut out)?;
    Ok(out)
}

// ---------------------------------------------------------------------
// Units
// ---------------------------------------------------------------------

fn parse_length(raw: &str) -> Result<(f64, String), String> {
    let s = raw.trim();
    let split_at = s.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+')).unwrap_or(s.len());
    let (num_part, unit_part) = s.split_at(split_at);
    let value: f64 = num_part.parse().map_err(|_| format!("couldn't parse SVG length '{raw}'"))?;
    Ok((value, unit_part.trim().to_ascii_lowercase()))
}

fn mm_per_unit(unit: &str) -> Result<f64, String> {
    match unit {
        "mm" => Ok(1.0),
        "cm" => Ok(10.0),
        "m" => Ok(1000.0),
        "px" | "" => Ok(MM_PER_PX),
        "in" | "pt" | "pc" | "ft" | "yd" | "mi" => Err(format!(
            "SVG uses the imperial unit '{unit}' - this app is metric-only. Re-export using mm, cm, m, or px."
        )),
        "%" => Err("percentage-based SVG width/height aren't supported (no reference viewport to resolve against)".to_string()),
        other => Err(format!("unsupported SVG length unit '{other}'")),
    }
}

fn parse_viewbox(s: &str) -> Result<(f64, f64, f64, f64), String> {
    let nums: Vec<f64> = s
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .map(|t| t.parse().map_err(|_| format!("bad viewBox '{s}'")))
        .collect::<Result<_, String>>()?;
    match nums.as_slice() {
        [minx, miny, w, h] => Ok((*minx, *miny, *w, *h)),
        _ => Err(format!("viewBox must have exactly 4 numbers, got '{s}'")),
    }
}

/// Resolves the root `<svg>`'s coordinate system into a (scale_x, scale_y)
/// pair mapping raw path coordinates to millimeters. When a `viewBox` and a
/// unit-bearing `width`/`height` are both present, uses their ratio (the
/// dominant real-world case - Inkscape/Illustrator both emit this pair for a
/// physically-sized document). Otherwise falls back to treating raw
/// coordinates as CSS px 1:1 - correct per the SVG spec's own default when no
/// `viewBox` establishes a different user-unit scale.
/// Whether this document's real-world size had to be *guessed*: it carries a
/// `viewBox` but no usable `width`/`height`, so nothing in the file says how
/// big the drawing actually is and `resolve_scale` falls back to 96dpi CSS
/// pixels.
///
/// **This is the one import failure that produces a perfectly clean result at
/// the wrong size.** `curvy.svg` is the standing example: its units are
/// PostScript points, so the fallback lands every part at exactly 3/4 of its
/// true size and nothing about the geometry looks wrong. No parser can tell -
/// only the person who drew it can - so the only honest handling is to say so
/// out loud. Asserted both ways in `crates/geometry/tests/curvy_fixtures.rs`.
///
/// Returns `false` on anything it cannot parse: this is a warning, and a
/// warning that fires on a file that is about to fail anyway is noise.
#[must_use]
pub fn size_is_guessed(svg_text: &str) -> bool {
    let Ok(doc) = roxmltree::Document::parse(svg_text) else { return false };
    let root = doc.root_element();
    if root.tag_name().name() != "svg" {
        return false;
    }
    matches!(resolve_scale(&root), Ok((sx, _)) if sx == MM_PER_PX) && root.attribute("viewBox").is_some()
}

fn resolve_scale(root: &Node) -> Result<(f64, f64), String> {
    let width = root.attribute("width").map(parse_length).transpose()?;
    let height = root.attribute("height").map(parse_length).transpose()?;
    // Validate units unconditionally (even if this pair ends up unused
    // below) - an explicit `width="8.5in"` should hard-error, not be
    // silently ignored just because there's no viewBox to pair it with.
    if let Some((_, u)) = &width {
        mm_per_unit(u)?;
    }
    if let Some((_, u)) = &height {
        mm_per_unit(u)?;
    }

    let viewbox = root.attribute("viewBox").map(parse_viewbox).transpose()?;
    match (viewbox, width, height) {
        (Some((_, _, vw, vh)), Some((wv, wu)), Some((hv, hu))) if vw > 0.0 && vh > 0.0 => {
            let w_mm = wv * mm_per_unit(&wu)?;
            let h_mm = hv * mm_per_unit(&hu)?;
            Ok((w_mm / vw, h_mm / vh))
        }
        _ => Ok((MM_PER_PX, MM_PER_PX)),
    }
}

// ---------------------------------------------------------------------
// 2D affine transform
// ---------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Mat {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Mat {
    const IDENTITY: Mat = Mat { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    /// `self ∘ other`: `other` applied first, then `self` - i.e.
    /// `self.mul(other).apply(p) == self.apply(other.apply(p))`.
    fn mul(self, other: Mat) -> Mat {
        Mat {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            e: self.a * other.e + self.c * other.f + self.e,
            f: self.b * other.e + self.d * other.f + self.f,
        }
    }

    fn apply(self, p: Point) -> Point {
        Point::new(self.a * p.x + self.c * p.y + self.e, self.b * p.x + self.d * p.y + self.f)
    }
}

/// True (with the uniform scale factor) if `m` is a similarity transform -
/// rotation plus uniform scale, no shear/non-uniform scale - the only case a
/// transformed circle is still exactly a circle.
fn uniform_scale(m: Mat) -> Option<f64> {
    let s1 = (m.a * m.a + m.b * m.b).sqrt();
    let s2 = (m.c * m.c + m.d * m.d).sqrt();
    let dot = m.a * m.c + m.b * m.d;
    if s1 > 1e-9 && (s1 - s2).abs() < 1e-6 * s1 && dot.abs() < 1e-6 * s1 * s1 {
        Some(s1)
    } else {
        None
    }
}

/// Area-based approximation of `m`'s local scale factor, used to convert a
/// real-world (post-transform) `curve_tolerance` into an equivalent
/// pre-transform tolerance for tessellation - doesn't need to be exact, only
/// close enough that tessellation stays proportionate under any combination
/// of scale/rotation.
fn approx_scale(m: Mat) -> f64 {
    (m.a * m.d - m.b * m.c).abs().sqrt().max(1e-9)
}

fn parse_transform(s: &str) -> Result<Mat, String> {
    let mut result = Mat::IDENTITY;
    let mut rest = s.trim();
    while !rest.is_empty() {
        let open = rest.find('(').ok_or_else(|| format!("malformed transform '{s}'"))?;
        let name = rest[..open].trim();
        let close = rest[open..].find(')').ok_or_else(|| format!("malformed transform '{s}'"))? + open;
        let args_str = &rest[open + 1..close];
        let args: Vec<f64> = args_str
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|t| !t.is_empty())
            .map(|t| t.parse().map_err(|_| format!("bad transform arg in '{s}'")))
            .collect::<Result<_, String>>()?;
        let m = match name {
            "translate" => match args.as_slice() {
                [tx] => Mat { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: *tx, f: 0.0 },
                [tx, ty] => Mat { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: *tx, f: *ty },
                _ => return Err(format!("translate() needs 1-2 args in '{s}'")),
            },
            "scale" => match args.as_slice() {
                [sx] => Mat { a: *sx, b: 0.0, c: 0.0, d: *sx, e: 0.0, f: 0.0 },
                [sx, sy] => Mat { a: *sx, b: 0.0, c: 0.0, d: *sy, e: 0.0, f: 0.0 },
                _ => return Err(format!("scale() needs 1-2 args in '{s}'")),
            },
            "rotate" => match args.as_slice() {
                [deg] => rotate_mat(*deg, 0.0, 0.0),
                [deg, cx, cy] => rotate_mat(*deg, *cx, *cy),
                _ => return Err(format!("rotate() needs 1 or 3 args in '{s}'")),
            },
            "matrix" => match args.as_slice() {
                [a, b, c, d, e, f] => Mat { a: *a, b: *b, c: *c, d: *d, e: *e, f: *f },
                _ => return Err(format!("matrix() needs 6 args in '{s}'")),
            },
            "skewX" | "skewY" => return Err(format!("skew transforms aren't supported ('{name}' in '{s}')")),
            other => return Err(format!("unsupported transform function '{other}' in '{s}'")),
        };
        result = result.mul(m);
        rest = rest[close + 1..].trim_start_matches(|c: char| c == ',' || c.is_whitespace());
    }
    Ok(result)
}

fn rotate_mat(deg: f64, cx: f64, cy: f64) -> Mat {
    let (s, c) = deg.to_radians().sin_cos();
    let t1 = Mat { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: cx, f: cy };
    let r = Mat { a: c, b: s, c: -s, d: c, e: 0.0, f: 0.0 };
    let t2 = Mat { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: -cx, f: -cy };
    t1.mul(r).mul(t2)
}

// ---------------------------------------------------------------------
// Tree walk
// ---------------------------------------------------------------------

fn walk(node: Node, ctm: Mat, layer: &str, tol: f64, out: &mut Vec<LayeredPolygon>) -> Result<(), String> {
    for child in node.children().filter(|n| n.is_element()) {
        let own_transform = match child.attribute("transform") {
            Some(t) => parse_transform(t)?,
            None => Mat::IDENTITY,
        };
        let child_ctm = ctm.mul(own_transform);
        let tag = child.tag_name().name();
        let child_layer = if tag == "g" {
            child.attribute("id").unwrap_or(layer).to_string()
        } else {
            layer.to_string()
        };

        match tag {
            "g" | "svg" | "a" => walk(child, child_ctm, &child_layer, tol, out)?,
            "rect" => out.extend(rect_to_polygon(&child, child_ctm, &child_layer)),
            "circle" => out.extend(circle_to_polygon(&child, child_ctm, &child_layer, tol)),
            "ellipse" => out.extend(ellipse_to_polygon(&child, child_ctm, &child_layer, tol)),
            "polygon" => out.extend(poly_points_to_polygon(&child, child_ctm, &child_layer, true)),
            "polyline" => out.extend(poly_points_to_polygon(&child, child_ctm, &child_layer, false)),
            "path" => path_to_polygons(&child, child_ctm, &child_layer, tol, out)?,
            // <defs>, <text>, <style>, <metadata>, <use>, etc: no closed
            // profile to extract (yet) - see module doc for what's
            // deliberately out of scope.
            _ => {}
        }
    }
    Ok(())
}

fn attr_f64(node: &Node, name: &str) -> Option<f64> {
    node.attribute(name)?.trim().parse().ok()
}

fn rect_to_polygon(node: &Node, ctm: Mat, layer: &str) -> Option<LayeredPolygon> {
    let x = attr_f64(node, "x").unwrap_or(0.0);
    let y = attr_f64(node, "y").unwrap_or(0.0);
    let w = attr_f64(node, "width")?;
    let h = attr_f64(node, "height")?;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let pts = [Point::new(x, y), Point::new(x + w, y), Point::new(x + w, y + h), Point::new(x, y + h)]
        .into_iter()
        .map(|p| ctm.apply(p))
        .collect();
    Some(LayeredPolygon::new(pts, layer.to_string(), None))
}

fn circle_to_polygon(node: &Node, ctm: Mat, layer: &str, tol: f64) -> Option<LayeredPolygon> {
    let cx = attr_f64(node, "cx").unwrap_or(0.0);
    let cy = attr_f64(node, "cy").unwrap_or(0.0);
    let r = attr_f64(node, "r")?;
    if r <= 0.0 {
        return None;
    }
    let local_tol = tol / approx_scale(ctm);
    let pts: Vec<Point> = crate::dxf_import::tessellate_circle(cx, cy, r, local_tol).into_iter().map(|p| ctm.apply(p)).collect();
    let is_circle = uniform_scale(ctm).map(|s| {
        let center = ctm.apply(Point::new(cx, cy));
        Circle { cx: center.x, cy: center.y, r: r * s }
    });
    Some(LayeredPolygon::new(pts, layer.to_string(), is_circle))
}

fn tessellate_ellipse(cx: f64, cy: f64, rx: f64, ry: f64, tol: f64) -> Vec<Point> {
    let n = segment_count(2.0 * std::f64::consts::PI, rx.max(ry), tol);
    (0..n)
        .map(|i| {
            let t = 2.0 * std::f64::consts::PI * (i as f64) / (n as f64);
            Point::new(cx + rx * t.cos(), cy + ry * t.sin())
        })
        .collect()
}

fn ellipse_to_polygon(node: &Node, ctm: Mat, layer: &str, tol: f64) -> Option<LayeredPolygon> {
    let cx = attr_f64(node, "cx").unwrap_or(0.0);
    let cy = attr_f64(node, "cy").unwrap_or(0.0);
    let rx = attr_f64(node, "rx")?;
    let ry = attr_f64(node, "ry")?;
    if rx <= 0.0 || ry <= 0.0 {
        return None;
    }
    let local_tol = tol / approx_scale(ctm);
    let pts = tessellate_ellipse(cx, cy, rx, ry, local_tol).into_iter().map(|p| ctm.apply(p)).collect();
    Some(LayeredPolygon::new(pts, layer.to_string(), None))
}

fn poly_points_to_polygon(node: &Node, ctm: Mat, layer: &str, always_closed: bool) -> Option<LayeredPolygon> {
    let raw = node.attribute("points")?;
    let nums: Vec<f64> = raw.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()).map(|t| t.parse().ok()).collect::<Option<_>>()?;
    if nums.len() < 6 || nums.len() % 2 == 1 {
        return None;
    }
    let mut local_pts: Vec<Point> = nums.chunks(2).map(|c| Point::new(c[0], c[1])).collect();
    if !always_closed {
        let first = *local_pts.first()?;
        let last = *local_pts.last()?;
        if (first.x - last.x).abs() > 1e-9 || (first.y - last.y).abs() > 1e-9 {
            return None; // open polyline: not a closed profile, skip
        }
        local_pts.pop(); // drop the duplicated closing point
        if local_pts.len() < 3 {
            return None;
        }
    }
    let pts = local_pts.into_iter().map(|p| ctm.apply(p)).collect();
    Some(LayeredPolygon::new(pts, layer.to_string(), None))
}

// ---------------------------------------------------------------------
// <path> `d` parsing
// ---------------------------------------------------------------------

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(s: &'a str) -> Self {
        Cursor { bytes: s.as_bytes(), pos: 0 }
    }

    fn skip_sep(&mut self) {
        while matches!(self.bytes.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r' | b',')) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn read_command(&mut self) -> Option<u8> {
        self.skip_sep();
        match self.peek() {
            Some(c) if c.is_ascii_alphabetic() => {
                self.pos += 1;
                Some(c)
            }
            _ => None,
        }
    }

    fn read_number(&mut self) -> Option<f64> {
        self.skip_sep();
        let start = self.pos;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        let mut seen_digit = false;
        let mut seen_dot = false;
        loop {
            match self.peek() {
                Some(b'0'..=b'9') => {
                    seen_digit = true;
                    self.pos += 1;
                }
                Some(b'.') if !seen_dot => {
                    seen_dot = true;
                    self.pos += 1;
                }
                Some(b'e' | b'E') if seen_digit => {
                    let save = self.pos;
                    self.pos += 1;
                    if matches!(self.peek(), Some(b'+' | b'-')) {
                        self.pos += 1;
                    }
                    if matches!(self.peek(), Some(b'0'..=b'9')) {
                        while matches!(self.peek(), Some(b'0'..=b'9')) {
                            self.pos += 1;
                        }
                    } else {
                        self.pos = save;
                        break;
                    }
                }
                _ => break,
            }
        }
        if !seen_digit {
            self.pos = start;
            return None;
        }
        std::str::from_utf8(&self.bytes[start..self.pos]).ok()?.parse().ok()
    }

    fn read_flag(&mut self) -> Option<f64> {
        self.skip_sep();
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                Some(0.0)
            }
            Some(b'1') => {
                self.pos += 1;
                Some(1.0)
            }
            _ => None,
        }
    }

    fn at_end(&mut self) -> bool {
        self.skip_sep();
        self.pos >= self.bytes.len()
    }

    fn peek_is_command(&mut self) -> bool {
        self.skip_sep();
        matches!(self.peek(), Some(c) if c.is_ascii_alphabetic())
    }
}

/// Parses a `<path>`'s `d` attribute into its `Z`-closed subpaths only (each
/// a `Vec<Point>` in the path's own local coordinate space) - an open
/// subpath has no closed boundary to nest against, so it's dropped here
/// rather than returned for the caller to filter (mirrors `dxf_import`'s
/// open-`LWPOLYLINE` skip).
fn parse_path_d(d: &str, tolerance: f64) -> Result<Vec<Vec<Point>>, String> {
    let mut cur = Cursor::new(d);
    let mut subpaths: Vec<Vec<Point>> = Vec::new();
    let mut points: Vec<Point> = Vec::new();
    let mut current = Point::new(0.0, 0.0);
    let mut subpath_start = Point::new(0.0, 0.0);
    let mut last_cubic_ctrl: Option<Point> = None;
    let mut last_quad_ctrl: Option<Point> = None;

    let mut cmd = match cur.read_command() {
        Some(c) => c,
        None => return Ok(Vec::new()), // empty `d`: no profiles, not an error
    };
    if !matches!(cmd, b'M' | b'm') {
        return Err(format!("path data must start with M/m, found '{}'", cmd as char));
    }

    loop {
        if points.is_empty() && !matches!(cmd, b'M' | b'm') {
            points.push(current);
        }
        match cmd {
            b'M' | b'm' => {
                points.clear(); // discard any unterminated (non-Z-closed) previous subpath
                let x = cur.read_number().ok_or("expected x after M")?;
                let y = cur.read_number().ok_or("expected y after M")?;
                current = if cmd == b'm' { Point::new(current.x + x, current.y + y) } else { Point::new(x, y) };
                subpath_start = current;
                points.push(current);
                last_cubic_ctrl = None;
                last_quad_ctrl = None;
                let implicit_relative = cmd == b'm';
                while !cur.at_end() && !cur.peek_is_command() {
                    let x = cur.read_number().ok_or("expected x")?;
                    let y = cur.read_number().ok_or("expected y")?;
                    current = if implicit_relative { Point::new(current.x + x, current.y + y) } else { Point::new(x, y) };
                    points.push(current);
                }
            }
            b'L' | b'l' => loop {
                let x = cur.read_number().ok_or("expected x after L")?;
                let y = cur.read_number().ok_or("expected y after L")?;
                current = if cmd == b'l' { Point::new(current.x + x, current.y + y) } else { Point::new(x, y) };
                points.push(current);
                last_cubic_ctrl = None;
                last_quad_ctrl = None;
                if cur.at_end() || cur.peek_is_command() {
                    break;
                }
            },
            b'H' | b'h' => loop {
                let x = cur.read_number().ok_or("expected x after H")?;
                current = if cmd == b'h' { Point::new(current.x + x, current.y) } else { Point::new(x, current.y) };
                points.push(current);
                last_cubic_ctrl = None;
                last_quad_ctrl = None;
                if cur.at_end() || cur.peek_is_command() {
                    break;
                }
            },
            b'V' | b'v' => loop {
                let y = cur.read_number().ok_or("expected y after V")?;
                current = if cmd == b'v' { Point::new(current.x, current.y + y) } else { Point::new(current.x, y) };
                points.push(current);
                last_cubic_ctrl = None;
                last_quad_ctrl = None;
                if cur.at_end() || cur.peek_is_command() {
                    break;
                }
            },
            b'C' | b'c' => loop {
                let x1 = cur.read_number().ok_or("bad C args")?;
                let y1 = cur.read_number().ok_or("bad C args")?;
                let x2 = cur.read_number().ok_or("bad C args")?;
                let y2 = cur.read_number().ok_or("bad C args")?;
                let x = cur.read_number().ok_or("bad C args")?;
                let y = cur.read_number().ok_or("bad C args")?;
                let (c1, c2, end) = if cmd == b'c' {
                    (Point::new(current.x + x1, current.y + y1), Point::new(current.x + x2, current.y + y2), Point::new(current.x + x, current.y + y))
                } else {
                    (Point::new(x1, y1), Point::new(x2, y2), Point::new(x, y))
                };
                flatten_cubic(current, c1, c2, end, tolerance, &mut points);
                last_cubic_ctrl = Some(c2);
                last_quad_ctrl = None;
                current = end;
                if cur.at_end() || cur.peek_is_command() {
                    break;
                }
            },
            b'S' | b's' => loop {
                let x2 = cur.read_number().ok_or("bad S args")?;
                let y2 = cur.read_number().ok_or("bad S args")?;
                let x = cur.read_number().ok_or("bad S args")?;
                let y = cur.read_number().ok_or("bad S args")?;
                let c1 = match last_cubic_ctrl {
                    Some(p) => Point::new(2.0 * current.x - p.x, 2.0 * current.y - p.y),
                    None => current,
                };
                let (c2, end) = if cmd == b's' {
                    (Point::new(current.x + x2, current.y + y2), Point::new(current.x + x, current.y + y))
                } else {
                    (Point::new(x2, y2), Point::new(x, y))
                };
                flatten_cubic(current, c1, c2, end, tolerance, &mut points);
                last_cubic_ctrl = Some(c2);
                last_quad_ctrl = None;
                current = end;
                if cur.at_end() || cur.peek_is_command() {
                    break;
                }
            },
            b'Q' | b'q' => loop {
                let x1 = cur.read_number().ok_or("bad Q args")?;
                let y1 = cur.read_number().ok_or("bad Q args")?;
                let x = cur.read_number().ok_or("bad Q args")?;
                let y = cur.read_number().ok_or("bad Q args")?;
                let (c1, end) = if cmd == b'q' {
                    (Point::new(current.x + x1, current.y + y1), Point::new(current.x + x, current.y + y))
                } else {
                    (Point::new(x1, y1), Point::new(x, y))
                };
                flatten_quad(current, c1, end, tolerance, &mut points);
                last_quad_ctrl = Some(c1);
                last_cubic_ctrl = None;
                current = end;
                if cur.at_end() || cur.peek_is_command() {
                    break;
                }
            },
            b'T' | b't' => loop {
                let x = cur.read_number().ok_or("bad T args")?;
                let y = cur.read_number().ok_or("bad T args")?;
                let c1 = match last_quad_ctrl {
                    Some(p) => Point::new(2.0 * current.x - p.x, 2.0 * current.y - p.y),
                    None => current,
                };
                let end = if cmd == b't' { Point::new(current.x + x, current.y + y) } else { Point::new(x, y) };
                flatten_quad(current, c1, end, tolerance, &mut points);
                last_quad_ctrl = Some(c1);
                last_cubic_ctrl = None;
                current = end;
                if cur.at_end() || cur.peek_is_command() {
                    break;
                }
            },
            b'A' | b'a' => loop {
                let rx = cur.read_number().ok_or("bad A args")?;
                let ry = cur.read_number().ok_or("bad A args")?;
                let x_axis_rotation = cur.read_number().ok_or("bad A args")?;
                let large_arc = cur.read_flag().ok_or("bad A large-arc-flag")?;
                let sweep = cur.read_flag().ok_or("bad A sweep-flag")?;
                let x = cur.read_number().ok_or("bad A args")?;
                let y = cur.read_number().ok_or("bad A args")?;
                let end = if cmd == b'a' { Point::new(current.x + x, current.y + y) } else { Point::new(x, y) };
                flatten_arc(current, rx, ry, x_axis_rotation, large_arc != 0.0, sweep != 0.0, end, tolerance, &mut points);
                last_cubic_ctrl = None;
                last_quad_ctrl = None;
                current = end;
                if cur.at_end() || cur.peek_is_command() {
                    break;
                }
            },
            b'Z' | b'z' => {
                current = subpath_start;
                if points.last() != Some(&subpath_start) {
                    points.push(subpath_start);
                }
                subpaths.push(std::mem::take(&mut points));
                last_cubic_ctrl = None;
                last_quad_ctrl = None;
            }
            other => return Err(format!("unsupported path command '{}'", other as char)),
        }

        if cur.at_end() {
            break;
        }
        cmd = cur.read_command().ok_or_else(|| format!("expected a path command letter in '{d}'"))?;
    }

    Ok(subpaths)
}

fn path_to_polygons(node: &Node, ctm: Mat, layer: &str, tol: f64, out: &mut Vec<LayeredPolygon>) -> Result<(), String> {
    let d = match node.attribute("d") {
        Some(d) => d,
        None => return Ok(()),
    };
    let local_tol = tol / approx_scale(ctm);
    let subpaths = parse_path_d(d, local_tol)?;
    for mut pts in subpaths {
        if pts.len() >= 2 {
            let first = pts[0];
            let last = *pts.last().unwrap();
            if (first.x - last.x).abs() < 1e-9 && (first.y - last.y).abs() < 1e-9 {
                pts.pop();
            }
        }
        if pts.len() < 3 {
            continue;
        }
        let world_pts = pts.into_iter().map(|p| ctm.apply(p)).collect();
        out.push(LayeredPolygon::new(world_pts, layer.to_string(), None));
    }
    Ok(())
}

fn point_line_distance(p: Point, a: Point, b: Point) -> f64 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-12 {
        return p.distance_to(a);
    }
    ((p.x - a.x) * dy - (p.y - a.y) * dx).abs() / len
}

const MAX_SUBDIVISION_DEPTH: u32 = 24;

fn flatten_cubic(p0: Point, c1: Point, c2: Point, p3: Point, tol: f64, out: &mut Vec<Point>) {
    subdivide_cubic(p0, c1, c2, p3, tol, 0, out);
}

fn subdivide_cubic(p0: Point, c1: Point, c2: Point, p3: Point, tol: f64, depth: u32, out: &mut Vec<Point>) {
    let flat = point_line_distance(c1, p0, p3).max(point_line_distance(c2, p0, p3)) <= tol;
    if depth >= MAX_SUBDIVISION_DEPTH || flat {
        out.push(p3);
        return;
    }
    let p01 = p0.midpoint(c1);
    let p12 = c1.midpoint(c2);
    let p23 = c2.midpoint(p3);
    let p012 = p01.midpoint(p12);
    let p123 = p12.midpoint(p23);
    let mid = p012.midpoint(p123);
    subdivide_cubic(p0, p01, p012, mid, tol, depth + 1, out);
    subdivide_cubic(mid, p123, p23, p3, tol, depth + 1, out);
}

fn flatten_quad(p0: Point, c: Point, p2: Point, tol: f64, out: &mut Vec<Point>) {
    subdivide_quad(p0, c, p2, tol, 0, out);
}

fn subdivide_quad(p0: Point, c: Point, p2: Point, tol: f64, depth: u32, out: &mut Vec<Point>) {
    if depth >= MAX_SUBDIVISION_DEPTH || point_line_distance(c, p0, p2) <= tol {
        out.push(p2);
        return;
    }
    let p01 = p0.midpoint(c);
    let p12 = c.midpoint(p2);
    let mid = p01.midpoint(p12);
    subdivide_quad(p0, p01, mid, tol, depth + 1, out);
    subdivide_quad(mid, p12, p2, tol, depth + 1, out);
}

/// SVG spec appendix F.6.5 endpoint-to-center arc parametrization, tessellated
/// at `segment_count`'s same tolerance-driven angular step `dxf_import` uses
/// for DXF `ARC`/bulge segments.
#[allow(clippy::too_many_arguments)]
fn flatten_arc(p0: Point, rx: f64, ry: f64, x_axis_rotation_deg: f64, large_arc: bool, sweep: bool, p1: Point, tol: f64, out: &mut Vec<Point>) {
    let mut rx = rx.abs();
    let mut ry = ry.abs();
    if rx < 1e-9 || ry < 1e-9 || (p0.x - p1.x).abs() < 1e-12 && (p0.y - p1.y).abs() < 1e-12 {
        out.push(p1);
        return;
    }

    let phi = x_axis_rotation_deg.to_radians();
    let (sin_phi, cos_phi) = phi.sin_cos();

    let dx2 = (p0.x - p1.x) / 2.0;
    let dy2 = (p0.y - p1.y) / 2.0;
    let x1p = cos_phi * dx2 + sin_phi * dy2;
    let y1p = -sin_phi * dx2 + cos_phi * dy2;

    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }

    let sign = if large_arc != sweep { 1.0 } else { -1.0 };
    let num = (rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p).max(0.0);
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let coef = if den < 1e-12 { 0.0 } else { sign * (num / den).sqrt() };
    let cxp = coef * (rx * y1p / ry);
    let cyp = coef * (-ry * x1p / rx);

    let cx = cos_phi * cxp - sin_phi * cyp + (p0.x + p1.x) / 2.0;
    let cy = sin_phi * cxp + cos_phi * cyp + (p0.y + p1.y) / 2.0;

    let angle = |ux: f64, uy: f64, vx: f64, vy: f64| -> f64 {
        let dot = ux * vx + uy * vy;
        let len = ((ux * ux + uy * uy) * (vx * vx + vy * vy)).sqrt();
        let mut a = (dot / len).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 {
            a = -a;
        }
        a
    };
    let theta1 = angle(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut dtheta = angle((x1p - cxp) / rx, (y1p - cyp) / ry, (-x1p - cxp) / rx, (-y1p - cyp) / ry);
    if !sweep && dtheta > 0.0 {
        dtheta -= 2.0 * std::f64::consts::PI;
    }
    if sweep && dtheta < 0.0 {
        dtheta += 2.0 * std::f64::consts::PI;
    }

    let n = segment_count(dtheta.abs(), rx.max(ry), tol);
    for i in 1..=n {
        let t = theta1 + dtheta * (i as f64) / (n as f64);
        let (st, ct) = t.sin_cos();
        out.push(Point::new(cx + rx * ct * cos_phi - ry * st * sin_phi, cy + rx * ct * sin_phi + ry * st * cos_phi));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::polygon::{get_polygon_bounds, polygon_area};

    #[test]
    fn parses_a_plain_rect_in_mm() {
        let svg = r#"<svg viewBox="0 0 100 50" width="100mm" height="50mm">
            <rect x="10" y="10" width="20" height="10" />
        </svg>"#;
        let polys = parse_svg(svg, 0.1, None).expect("should parse");
        assert_eq!(polys.len(), 1);
        let b = get_polygon_bounds(&polys[0].points).unwrap();
        assert!((b.width - 20.0).abs() < 1e-6);
        assert!((b.height - 10.0).abs() < 1e-6);
    }

    #[test]
    fn cm_and_mm_units_agree_on_final_mm_size() {
        let svg_mm = r#"<svg viewBox="0 0 10 10" width="10mm" height="10mm"><rect x="0" y="0" width="10" height="10"/></svg>"#;
        let svg_cm = r#"<svg viewBox="0 0 10 10" width="1cm" height="1cm"><rect x="0" y="0" width="10" height="10"/></svg>"#;
        let a = parse_svg(svg_mm, 0.1, None).unwrap();
        let b = parse_svg(svg_cm, 0.1, None).unwrap();
        let ba = get_polygon_bounds(&a[0].points).unwrap();
        let bb = get_polygon_bounds(&b[0].points).unwrap();
        assert!((ba.width - bb.width).abs() < 1e-9);
    }

    #[test]
    fn inch_width_is_rejected_not_silently_converted() {
        let svg = r#"<svg viewBox="0 0 10 10" width="1in" height="1in"><rect x="0" y="0" width="10" height="10"/></svg>"#;
        let err = parse_svg(svg, 0.1, None).unwrap_err();
        assert!(err.contains("imperial"), "error was: {err}");
    }

    #[test]
    fn point_and_pica_units_are_also_rejected() {
        for unit in ["pt", "pc", "ft"] {
            let svg = format!(r#"<svg viewBox="0 0 10 10" width="10{unit}" height="10{unit}"><rect x="0" y="0" width="10" height="10"/></svg>"#);
            let err = parse_svg(&svg, 0.1, None).unwrap_err();
            assert!(err.contains("imperial"), "unit {unit} should be rejected, got: {err}");
        }
    }

    #[test]
    fn unitless_falls_back_to_px_scale() {
        let svg = r#"<svg viewBox="0 0 96 96"><rect x="0" y="0" width="96" height="96"/></svg>"#;
        let polys = parse_svg(svg, 0.1, None).unwrap();
        let b = get_polygon_bounds(&polys[0].points).unwrap();
        // 96 px == 1 inch == 25.4mm
        assert!((b.width - 25.4).abs() < 1e-6, "width was {}", b.width);
    }

    #[test]
    fn unit_override_bypasses_the_file_s_own_width_height() {
        // width/height claim mm, but the override says every raw coordinate
        // is actually in cm - the override must win outright, not blend
        // with or validate against the file's own (wrong/irrelevant) units.
        let svg = r#"<svg viewBox="0 0 10 10" width="10mm" height="10mm"><rect x="0" y="0" width="10" height="10"/></svg>"#;
        let polys = parse_svg(svg, 0.1, Some("cm")).unwrap();
        let b = get_polygon_bounds(&polys[0].points).unwrap();
        assert!((b.width - 100.0).abs() < 1e-6, "width was {} (10 user units at 1cm each = 100mm)", b.width);
    }

    #[test]
    fn unit_override_still_rejects_an_imperial_unit() {
        let svg = r#"<svg viewBox="0 0 10 10"><rect x="0" y="0" width="10" height="10"/></svg>"#;
        let err = parse_svg(svg, 0.1, Some("in")).unwrap_err();
        assert!(err.contains("imperial"), "error was: {err}");
    }

    #[test]
    fn circle_tessellates_and_keeps_is_circle_metadata_under_uniform_transform() {
        let svg = r#"<svg viewBox="0 0 100 100" width="100mm" height="100mm">
            <g transform="translate(10,10) rotate(30)"><circle cx="5" cy="5" r="3" /></g>
        </svg>"#;
        let polys = parse_svg(svg, 0.01, None).unwrap();
        assert_eq!(polys.len(), 1);
        let circle = polys[0].is_circle.expect("uniform transform should preserve circularity");
        assert!((circle.r - 3.0).abs() < 1e-6);
        let area = polygon_area(&polys[0].points).abs();
        assert!((area - std::f64::consts::PI * 9.0).abs() / (std::f64::consts::PI * 9.0) < 0.01);
    }

    #[test]
    fn circle_loses_is_circle_metadata_under_non_uniform_scale() {
        let svg = r#"<svg viewBox="0 0 100 100" width="100mm" height="100mm">
            <g transform="scale(2,1)"><circle cx="5" cy="5" r="3" /></g>
        </svg>"#;
        let polys = parse_svg(svg, 0.01, None).unwrap();
        assert!(polys[0].is_circle.is_none());
    }

    #[test]
    fn closed_polygon_element_converts() {
        let svg = r#"<svg viewBox="0 0 10 10" width="10mm" height="10mm">
            <polygon points="0,0 10,0 10,10 0,10" />
        </svg>"#;
        let polys = parse_svg(svg, 0.1, None).unwrap();
        assert_eq!(polys.len(), 1);
        assert!((polygon_area(&polys[0].points).abs() - 100.0).abs() < 1e-6);
    }

    #[test]
    fn open_polyline_is_skipped() {
        let svg = r#"<svg viewBox="0 0 10 10" width="10mm" height="10mm">
            <polyline points="0,0 10,0 10,10" />
        </svg>"#;
        let polys = parse_svg(svg, 0.1, None).unwrap();
        assert!(polys.is_empty());
    }

    #[test]
    fn closed_triangle_path_with_explicit_z_converts() {
        let svg = r#"<svg viewBox="0 0 10 10" width="10mm" height="10mm">
            <path d="M0,0 L10,0 L5,10 Z" />
        </svg>"#;
        let polys = parse_svg(svg, 0.1, None).unwrap();
        assert_eq!(polys.len(), 1);
        assert!((polygon_area(&polys[0].points).abs() - 50.0).abs() < 1e-6);
    }

    #[test]
    fn open_path_without_z_is_skipped() {
        let svg = r#"<svg viewBox="0 0 10 10" width="10mm" height="10mm">
            <path d="M0,0 L10,0 L5,10" />
        </svg>"#;
        let polys = parse_svg(svg, 0.1, None).unwrap();
        assert!(polys.is_empty());
    }

    #[test]
    fn path_with_two_z_closed_subpaths_produces_two_polygons() {
        // An outer square and, in the same path, a separate inner square -
        // build_polygon_tree (not exercised here) would nest these, but
        // parse_svg itself must at least return both as separate profiles.
        let svg = r#"<svg viewBox="0 0 20 20" width="20mm" height="20mm">
            <path d="M0,0 L20,0 L20,20 L0,20 Z M5,5 L15,5 L15,15 L5,15 Z" />
        </svg>"#;
        let polys = parse_svg(svg, 0.1, None).unwrap();
        assert_eq!(polys.len(), 2);
    }

    #[test]
    fn cubic_bezier_path_approximates_expected_area() {
        // A closed "D" shape built from a straight edge and a cubic bezier
        // approximating a semicircle of radius 1 (kappa-scaled control
        // points), same shape/expected-area idea as dxf_import's own bulge
        // semicircle test.
        let k = 4.0 / 3.0;
        let d = format!("M -1,0 L 1,0 C 1,{k} -1,{k} -1,0 Z");
        let svg = format!(r#"<svg viewBox="-2 -2 4 4" width="4mm" height="4mm"><path d="{d}" /></svg>"#);
        let polys = parse_svg(&svg, 0.001, None).unwrap();
        assert_eq!(polys.len(), 1);
        let area = polygon_area(&polys[0].points).abs();
        assert!((area - std::f64::consts::FRAC_PI_2).abs() < 0.05, "area was {area}");
    }

    #[test]
    fn elliptical_arc_path_stays_within_its_semi_axes() {
        // A path using an A command to draw a half-ellipse (rx=2, ry=1) and
        // close back via a straight line - every tessellated point must stay
        // within the ellipse's bounding box.
        let svg = r#"<svg viewBox="-3 -3 6 6" width="6mm" height="6mm">
            <path d="M -2,0 A 2,1 0 1,1 2,0 Z" />
        </svg>"#;
        let polys = parse_svg(svg, 0.01, None).unwrap();
        assert_eq!(polys.len(), 1);
        for p in &polys[0].points {
            assert!(p.x >= -2.0001 && p.x <= 2.0001, "x out of range: {}", p.x);
            assert!(p.y >= -1.0001 && p.y <= 1.0001, "y out of range: {}", p.y);
        }
    }

    #[test]
    fn nested_group_id_is_used_as_layer() {
        let svg = r#"<svg viewBox="0 0 10 10" width="10mm" height="10mm">
            <g id="DRILL"><rect x="0" y="0" width="1" height="1" /></g>
        </svg>"#;
        let polys = parse_svg(svg, 0.1, None).unwrap();
        assert_eq!(polys[0].layer, "DRILL");
    }

    #[test]
    fn untagged_shape_defaults_to_layer_zero() {
        let svg = r#"<svg viewBox="0 0 10 10" width="10mm" height="10mm"><rect x="0" y="0" width="1" height="1" /></svg>"#;
        let polys = parse_svg(svg, 0.1, None).unwrap();
        assert_eq!(polys[0].layer, "0");
    }

    #[test]
    fn skew_transform_is_a_hard_error_not_a_silent_distortion() {
        let svg = r#"<svg viewBox="0 0 10 10" width="10mm" height="10mm">
            <rect x="0" y="0" width="1" height="1" transform="skewX(20)" />
        </svg>"#;
        let err = parse_svg(svg, 0.1, None).unwrap_err();
        assert!(err.contains("skew"), "error was: {err}");
    }

    #[test]
    fn non_svg_root_is_rejected() {
        let err = parse_svg("<html></html>", 0.1, None).unwrap_err();
        assert!(err.contains("svg"));
    }
}
