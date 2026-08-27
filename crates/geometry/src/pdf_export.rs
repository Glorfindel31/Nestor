//! Writes a nest result out as a printable PDF job report, laid out to match
//! the reference nester's own report page for page: a Part-List / Sheet-List /
//! Remnant Info summary, then one page per *distinct layout* showing that
//! sheet drawn to scale with its own part table.
//!
//! **One page per distinct layout, not per sheet.** Eleven sheets cut from two
//! arrangements is two pages, not eleven, and the Duplicate column is what
//! says how many times each is cut. That is what the reference report does and
//! it is what someone at a machine actually wants - the eleven identical pages
//! were paper, not information.
//!
//! **Hand-rolled, no PDF crate.** The obvious candidate (`printpdf`) resolves
//! to a 30-plus-crate tree - font shaping, image codecs, a vector-graphics
//! engine, an RNG - for a page that strokes polylines and writes a few lines
//! of Times. What this module actually needs is a subset of PDF 1.4 that has
//! not changed since 2001: a content stream of `m`/`l`/`h`/`S` path operators,
//! `BT`/`Tf`/`Td`/`Tj`/`ET` for text, and two of the 14 standard fonts, which
//! need no embedding.
//!
//! ponytail: WinAnsi + the built-in Times means **the report is English-only**
//! - a Vietnamese UI string or a non-ASCII layer name is transliterated to `?`
//! rather than rendered (see `pdf_string`). Embedding a TrueType font with a
//! real Unicode CMap is the upgrade path, and roughly triples this file; not
//! worth it until someone actually needs it.
//!
//! ponytail: text is positioned with one average-width constant per point
//! size (`text_width`) rather than the real Times metrics, because everything
//! centred here is a heading or a short numeric cell where being a point out
//! is invisible. Embed the AFM widths if a long centred string ever looks off.
//!
//! Geometry comes in as the same `SheetLayout`/`PlacedShape` values
//! `dxf_export`/`svg_export` take, straight from the caller's
//! `build_export_layouts`, so the drawn page can never disagree with the
//! exported DXF.

use std::fmt::Write as _;

use crate::dxf_export::{PlacedShape, SheetLayout};
use crate::dxf_import::{polygon_material_area, rotate_layered_polygon, shift_layered_polygon, LayeredPolygon};
use crate::point::Point;
use crate::polygon::{get_polygon_bounds, polygon_area, Bounds};

/// What an empty outline measures - only reachable for a degenerate shape,
/// and printing zeros is better than refusing to draw the page.
const NO_BOUNDS: Bounds = Bounds { x: 0.0, y: 0.0, width: 0.0, height: 0.0 };

/// A4 landscape in PostScript points (1/72 inch), the unit PDF's default
/// coordinate system uses.
const PAGE_W: f64 = 841.89;
const PAGE_H: f64 = 595.28;

/// The page frame and footer, in the reference report's own coordinates.
const FRAME_L: f64 = 18.38;
const FRAME_R: f64 = 823.51;
const FRAME_T: f64 = 576.90;
const FRAME_B: f64 = 18.38;
const RULE_L: f64 = 18.75;
const RULE_R: f64 = 823.14;
const FOOTER_RULE_Y: f64 = 36.29;

/// Where a table's own left and right edges sit.
const TABLE_L: f64 = 20.63;
const TABLE_R: f64 = 821.26;

/// Vertical rhythm inside a table, measured off the reference report.
const HEAD_DROP: f64 = 13.5;
const HEAD_RULE_DROP: f64 = 16.8;
const FIRST_ROW_DROP: f64 = 28.4;
const ROW_PITCH: f64 = 16.91;
const ROW_RULE_DROP: f64 = 8.5;
const TOTAL_DROP: f64 = 12.6;
const TABLE_TAIL: f64 = 5.8;

/// One line of the report's part table.
///
/// Only the three things this module cannot know: what the piece is called,
/// how many were ordered, and how many the result placed. `nested` has to be
/// told because a `PlacedShape` carries no part identity by the time the
/// report draws it; the caller knows, because it handed out the ids. Size,
/// area, contour count and cut length are all measured off `shape` here.
#[derive(Clone, Debug)]
pub struct ReportPart {
    pub name: String,
    /// How many were ordered.
    pub quantity: usize,
    /// How many of them the result actually placed.
    pub nested: usize,
    /// The piece itself, holes included, at its original orientation.
    pub shape: LayeredPolygon,
}

/// Everything the report says that isn't derivable from the drawn geometry.
/// Anything that *is* derivable is computed from the layouts themselves, so
/// the printed numbers can never disagree with the printed picture.
#[derive(Clone, Debug)]
pub struct ReportMeta {
    /// The report's heading, and the name in the top-left of page 1.
    pub title: String,
    /// When the report was produced, already formatted. `geometry` owns no
    /// clock and has no business knowing the user's timezone.
    pub generated: String,
    /// The job's inter-part clearance, in mm. Needed to work out the offcuts:
    /// an offcut is only real material if it is at least a cut's width away
    /// from every piece around it.
    pub spacing: f64,
    pub parts: Vec<ReportPart>,
}

pub fn export_report(layouts: &[SheetLayout], meta: &ReportMeta) -> Vec<u8> {
    let stats: Vec<SheetStats> = layouts.iter().map(sheet_stats).collect();
    let groups = nest_groups(layouts);
    let materials = material_names(layouts);

    let mut pages = summary_pages(meta, layouts, &stats, &groups, &materials);
    for (n, group) in groups.iter().enumerate() {
        pages.extend(layout_pages(meta, &layouts[group.first], &stats[group.first], group, n, &materials));
    }
    if pages.is_empty() {
        pages.push(String::new());
    }

    // The frame carries "page n / total", so it can only be drawn once every
    // page exists.
    let total = pages.len();
    let framed: Vec<String> =
        pages.iter().enumerate().map(|(i, body)| format!("{}{body}", frame(i + 1, total, &meta.generated))).collect();
    assemble(&framed)
}

struct SheetStats {
    parts: usize,
    sheet_area: f64,
    used_area: f64,
}

impl SheetStats {
    fn utilisation(&self) -> f64 {
        if self.sheet_area > 0.0 {
            self.used_area / self.sheet_area * 100.0
        } else {
            0.0
        }
    }
}

/// Measured off the very geometry being drawn, not from a parallel totals
/// computation - a report whose numbers and picture can disagree is worse
/// than no report.
fn sheet_stats(layout: &SheetLayout) -> SheetStats {
    SheetStats {
        parts: layout.parts.len(),
        sheet_area: polygon_area(&layout.sheet.points).abs(),
        used_area: layout.parts.iter().map(|p| polygon_material_area(&placed_geometry(p))).sum(),
    }
}

fn placed_geometry(part: &PlacedShape) -> LayeredPolygon {
    shift_layered_polygon(&rotate_layered_polygon(&part.shape, part.rotation), part.x, part.y)
}

/// The stock name each sheet is cut from, as the report's MatName column.
///
/// Sheets of the same size are the same material to whoever is ordering it, so
/// they share a name; a job mixing two stock sizes gets `sheet-0`/`sheet-1`.
fn material_names(layouts: &[SheetLayout]) -> Vec<String> {
    let mut sizes: Vec<(f64, f64)> = Vec::new();
    layouts
        .iter()
        .map(|layout| {
            let b = get_polygon_bounds(&layout.sheet.points).map_or((0.0, 0.0), |b| (b.width, b.height));
            let at = sizes.iter().position(|s| (s.0 - b.0).abs() < 0.05 && (s.1 - b.1).abs() < 0.05).unwrap_or_else(|| {
                sizes.push(b);
                sizes.len() - 1
            });
            format!("sheet-{at}")
        })
        .collect()
}

// --- tables -------------------------------------------------------------

/// One drawn cell: where it goes and whether it hangs off that x or straddles
/// it. The reference report left-aligns the Name column and centres every
/// other one, headers included.
#[derive(Clone)]
enum Cell {
    Left(f64, String),
    Centre(f64, String),
}

impl Cell {
    fn draw(&self, out: &mut String, y: f64, size: f64, bold: bool) {
        match self {
            Cell::Left(x, value) => text_at(out, value, *x, y, size, bold),
            Cell::Centre(cx, value) => text_at(out, value, cx - text_width(value, size) / 2.0, y, size, bold),
        }
    }
}

/// A bordered table: header band, a dashed rule under it, body rows, a solid
/// rule, then a totals row. Draws itself top-down from `y_top` and reports
/// where it ended so the next block can follow.
struct Table {
    left: f64,
    right: f64,
    header: Vec<Cell>,
    /// `(cells, optional part outline to draw as a thumbnail at this x)`.
    rows: Vec<(Vec<Cell>, Option<(f64, LayeredPolygon)>)>,
    totals: Vec<Cell>,
    /// The reference bolds the Sheet-List totals and not the Part-List's.
    bold_totals: bool,
}

impl Table {
    fn draw(&self, out: &mut String, y_top: f64) -> f64 {
        hline(out, self.left, self.right, y_top);
        for cell in &self.header {
            cell.draw(out, y_top - HEAD_DROP, 10.0, true);
        }
        dashed_hline(out, self.left, self.right, y_top - HEAD_RULE_DROP);

        let mut y = y_top - FIRST_ROW_DROP;
        for (cells, thumb) in &self.rows {
            for cell in cells {
                cell.draw(out, y, 10.0, false);
            }
            if let Some((x, shape)) = thumb {
                thumbnail(out, shape, *x, y - 5.7);
            }
            y -= ROW_PITCH;
        }
        // `y` has already stepped past the last row.
        let last = y + ROW_PITCH;
        let rule_y = last - ROW_RULE_DROP;
        hline(out, self.left + 1.87, self.right - 1.87, rule_y);

        let bottom = if self.totals.is_empty() {
            rule_y - TABLE_TAIL
        } else {
            for cell in &self.totals {
                cell.draw(out, rule_y - TOTAL_DROP, 10.0, self.bold_totals);
            }
            rule_y - TOTAL_DROP - TABLE_TAIL
        };
        hline(out, self.left, self.right, bottom);
        vline(out, self.left, y_top + 0.37, bottom);
        vline(out, self.right, y_top + 0.37, bottom);
        bottom
    }

    /// Height this table will occupy, so a caller can decide whether it fits.
    fn height(&self) -> f64 {
        let body = FIRST_ROW_DROP + ROW_PITCH * (self.rows.len().max(1) - 1) as f64;
        body + ROW_RULE_DROP + if self.totals.is_empty() { 0.0 } else { TOTAL_DROP } + TABLE_TAIL
    }
}

// --- the summary page ---------------------------------------------------

/// Column centres, straight off the reference report's own page 1. The Name
/// column is left-aligned; every other column, header and value alike, is
/// centred on these.
const PART_NAME_X: f64 = 24.3;
const PART_THUMB_X: f64 = 259.8;
const PART_COLS: [f64; 8] = [303.3, 347.3, 393.8, 452.3, 532.3, 594.1, 663.3, 759.6];
const PART_HEADS: [&str; 8] = ["Total", "Nested", "Left", "Width [mm]", "Height [mm]", "CtrQty", "Area [m²]", "Length [m]"];

const SHEET_NAME_X: f64 = 58.4;
const SHEET_COLS: [f64; 7] = [172.2, 269.1, 369.0, 470.8, 570.6, 673.5, 771.8];
const SHEET_HEADS: [&str; 7] = ["Duplicate", "MatName", "Width [mm]", "Height [mm]", "CtrQty", "Util [%]", "Length [m]"];

const REMNANT_L: f64 = 21.75;
const REMNANT_R: f64 = 321.75;
const REMNANT_COLS: [f64; 4] = [60.0, 143.4, 218.2, 289.8];
const REMNANT_HEADS: [&str; 4] = ["Width [mm]", "Height [mm]", "Qty", "Area [m²]"];

/// Where page 1's content starts, and where any page must stop.
const CONTENT_TOP: f64 = 513.45;
const CONTENT_BOTTOM: f64 = 44.0;

fn summary_pages(
    meta: &ReportMeta,
    layouts: &[SheetLayout],
    stats: &[SheetStats],
    groups: &[NestGroup],
    materials: &[String],
) -> Vec<String> {
    let mut pages: Vec<String> = Vec::new();
    let mut out = String::new();

    // Title band.
    text_centre(&mut out, &meta.title, PAGE_W / 2.0, 545.13, 20.0, true);
    text_at(&mut out, "Date:", 626.34, 552.1, 10.0, false);
    text_at(&mut out, &meta.generated, 651.0, 552.1, 10.0, false);
    hline(&mut out, RULE_L, RULE_R, CONTENT_TOP);

    let mut y = CONTENT_TOP;

    // --- Part-List ---
    y = section(&mut pages, &mut out, y, "Part-List");
    let part_rows: Vec<(Vec<Cell>, Option<(f64, LayeredPolygon)>)> = meta
        .parts
        .iter()
        .enumerate()
        .map(|(i, part)| {
            let b = get_polygon_bounds(&part.shape.points).unwrap_or(NO_BOUNDS);
            let values = [
                part.quantity.to_string(),
                part.nested.to_string(),
                part.quantity.saturating_sub(part.nested).to_string(),
                format!("{:.2}", b.width),
                format!("{:.2}", b.height),
                contour_count(&part.shape).to_string(),
                format!("{:.3}", polygon_material_area(&part.shape) / 1e6),
                format!("{:.3}", cut_length(&part.shape) / 1000.0),
            ];
            let mut cells = vec![Cell::Left(PART_NAME_X, format!("{}  {}", i + 1, part.name))];
            cells.extend(PART_COLS.iter().zip(values).map(|(&cx, v)| Cell::Centre(cx, v)));
            (cells, Some((PART_THUMB_X, part.shape.clone())))
        })
        .collect();
    let ordered: usize = meta.parts.iter().map(|p| p.quantity).sum();
    let nested: usize = meta.parts.iter().map(|p| p.nested).sum();
    let part_table = Table {
        left: TABLE_L,
        right: TABLE_R,
        header: std::iter::once(Cell::Left(PART_NAME_X, "Name".into()))
            .chain(PART_COLS.iter().zip(PART_HEADS).map(|(&cx, h)| Cell::Centre(cx, h.into())))
            .collect(),
        rows: part_rows,
        totals: vec![Cell::Centre(PART_COLS[0], ordered.to_string()), Cell::Centre(PART_COLS[1], nested.to_string())],
        bold_totals: false,
    };
    y = fit(&mut pages, &mut out, y, part_table.height());
    y = part_table.draw(&mut out, y);

    // The little boxed count under the part table.
    y -= 0.37;
    let box_bottom = y - 15.28;
    text_at(&mut out, &format!("Total Parts Count: {}", meta.parts.len()), TABLE_L + 2.17, y - 12.35, 10.0, true);
    hline(&mut out, TABLE_L - 0.38, TABLE_L + 137.17, y);
    hline(&mut out, TABLE_L - 0.38, TABLE_L + 137.17, box_bottom);
    vline(&mut out, TABLE_L, y, box_bottom);
    vline(&mut out, TABLE_L + 136.79, y, box_bottom);
    y = box_bottom;

    // --- Sheet-List ---
    y = section(&mut pages, &mut out, y, "Sheet-List");
    let mut total_length = 0.0;
    let mut total_pieces = 0usize;
    let sheet_rows: Vec<(Vec<Cell>, Option<(f64, LayeredPolygon)>)> = groups
        .iter()
        .enumerate()
        .map(|(n, group)| {
            let layout = &layouts[group.first];
            let stat = &stats[group.first];
            let b = get_polygon_bounds(&layout.sheet.points).unwrap_or(NO_BOUNDS);
            let length: f64 = layout.parts.iter().map(|p| cut_length(&p.shape)).sum::<f64>() / 1000.0;
            total_length += length * group.duplicate as f64;
            total_pieces += stat.parts * group.duplicate;
            let values = [
                group.duplicate.to_string(),
                materials.get(group.first).cloned().unwrap_or_default(),
                format!("{:.2}", b.width),
                format!("{:.2}", b.height),
                stat.parts.to_string(),
                format!("{:.2}", stat.utilisation()),
                format!("{length:.3}"),
            ];
            let mut cells = vec![Cell::Left(SHEET_NAME_X, format!("sheet{}", n + 1))];
            cells.extend(SHEET_COLS.iter().zip(values).map(|(&cx, v)| Cell::Centre(cx, v)));
            (cells, None)
        })
        .collect();
    let sheet_table = Table {
        left: TABLE_L,
        right: TABLE_R,
        header: std::iter::once(Cell::Left(SHEET_NAME_X, "Name".into()))
            .chain(SHEET_COLS.iter().zip(SHEET_HEADS).map(|(&cx, h)| Cell::Centre(cx, h.into())))
            .collect(),
        rows: sheet_rows,
        totals: vec![
            Cell::Centre(SHEET_COLS[0], layouts.len().to_string()),
            Cell::Centre(SHEET_COLS[4], total_pieces.to_string()),
            Cell::Centre(SHEET_COLS[6], format!("Total:{total_length:.3}")),
        ],
        bold_totals: true,
    };
    y = fit(&mut pages, &mut out, y, sheet_table.height());
    y = sheet_table.draw(&mut out, y);

    // --- Remnant Info ---
    let remnants = remnant_rows(layouts, groups, meta.spacing);
    if !remnants.is_empty() {
        y = fit(&mut pages, &mut out, y, 40.0 + ROW_PITCH * remnants.len() as f64);
        y -= 15.97;
        text_centre(&mut out, "Remnant Info", (REMNANT_L + REMNANT_R) / 2.0, y, 10.0, true);
        y -= 2.81;
        hline(&mut out, REMNANT_L, REMNANT_R, y);
        for (&cx, head) in REMNANT_COLS.iter().zip(REMNANT_HEADS) {
            text_centre(&mut out, head, cx, y - 14.47, 10.0, true);
        }
        let mut row_y = y - 30.63;
        for (w, h, qty) in &remnants {
            let values = [grouped(*w), grouped(*h), qty.to_string(), format!("{:.3}", w * h / 1e6)];
            for (&cx, value) in REMNANT_COLS.iter().zip(values) {
                text_centre(&mut out, &value, cx, row_y, 10.0, false);
            }
            row_y -= ROW_PITCH;
        }
    }

    pages.push(out);
    pages
}

/// A centred section heading, starting a new page first if it would not have
/// its table under it.
fn section(pages: &mut Vec<String>, out: &mut String, y: f64, title: &str) -> f64 {
    let y = fit(pages, out, y, 80.0);
    text_centre(out, title, PAGE_W / 2.0, y - 21.55, 18.0, true);
    y - 26.0
}

/// Breaks to a new page when `space` would not fit under `y`.
fn fit(pages: &mut Vec<String>, out: &mut String, y: f64, space: f64) -> f64 {
    if y - space < CONTENT_BOTTOM {
        pages.push(std::mem::take(out));
        FRAME_T - 20.0
    } else {
        y
    }
}

/// Distinct usable offcut rectangles across the job, with how many sheets
/// yield each - the reference's own Remnant Info shape.
///
/// Sized in whole millimetres because that is what goes on the label, and two
/// offcuts that round to the same label are the same stock item.
fn remnant_rows(layouts: &[SheetLayout], groups: &[NestGroup], spacing: f64) -> Vec<(f64, f64, usize)> {
    let mut rows: Vec<(f64, f64, usize)> = Vec::new();
    for group in groups {
        let offcut = sheet_offcut(&layouts[group.first], spacing);
        let (w, h) = (offcut.usable_w.floor(), offcut.usable_h.floor());
        if w < 1.0 || h < 1.0 {
            continue;
        }
        match rows.iter_mut().find(|r| (r.0 - w).abs() < 0.5 && (r.1 - h).abs() < 0.5) {
            Some(row) => row.2 += group.duplicate,
            None => rows.push((w, h, group.duplicate)),
        }
    }
    rows
}

// --- one page per distinct layout ---------------------------------------

const LSHEET_NAME_X: f64 = 74.84;
const LSHEET_COLS: [f64; 5] = [222.2, 352.6, 485.9, 621.0, 757.0];
const LSHEET_HEADS: [&str; 5] = ["Duplicate", "MatName", "Width [mm]", "Height [mm]", "Util [%]"];

const LPART_L: f64 = 27.19;
const LPART_R: f64 = 814.69;
const LPART_NO_X: f64 = 45.8;
const LPART_NAME_X: f64 = 65.74;
const LPART_THUMB_X: f64 = 383.74;
const LPART_COLS: [f64; 5] = [458.8, 550.4, 622.0, 690.1, 771.8];
const LPART_HEADS: [&str; 5] = ["Width [mm]", "Height [mm]", "CtrQty", "Area [m²]", "Quantity"];

/// The drawing box, exactly the reference's own.
const DRAW_X: f64 = 22.5;
const DRAW_W: f64 = 796.88;
const DRAW_H: f64 = 252.0;

fn layout_pages(
    meta: &ReportMeta,
    layout: &SheetLayout,
    stat: &SheetStats,
    group: &NestGroup,
    n: usize,
    materials: &[String],
) -> Vec<String> {
    // Every y on this page is the reference report's own. The sheet's summary
    // row sits *above* the drawing frame, not inside it - drawing it as an
    // ordinary bordered table put the layout straight through the header.
    const HEAD_DASH_Y: f64 = 540.70;
    const HEAD_TEXT_Y: f64 = 528.72;
    const HEAD_ROW_Y: f64 = 514.56;
    const FRAME_TOP_Y: f64 = 511.26;
    const FRAME_BOT_Y: f64 = 255.51;
    const DRAW_Y: f64 = 257.38;
    const PARTS_HEAD_Y: f64 = 242.03;
    const PARTS_DASH_Y: f64 = 238.73;
    const PARTS_ROW_Y: f64 = 227.12;

    let mut out = String::new();
    let name = format!("sheet{}", n + 1);
    text_centre(&mut out, &name, PAGE_W / 2.0, 545.15, 18.0, true);

    // The sheet's own row.
    dashed_hline(&mut out, TABLE_L + 0.37, TABLE_R - 0.38, HEAD_DASH_Y);
    let b = get_polygon_bounds(&layout.sheet.points).unwrap_or(NO_BOUNDS);
    let material = materials.get(group.first).cloned().unwrap_or_default();
    let values = [
        group.duplicate.to_string(),
        material,
        format!("{:.2}", b.width),
        format!("{:.2}", b.height),
        format!("{:.2}", stat.utilisation()),
    ];
    text_at(&mut out, "Name", LSHEET_NAME_X, HEAD_TEXT_Y, 10.0, true);
    text_at(&mut out, &name, LSHEET_NAME_X, HEAD_ROW_Y, 10.0, false);
    for ((&cx, head), value) in LSHEET_COLS.iter().zip(LSHEET_HEADS).zip(values) {
        text_centre(&mut out, head, cx, HEAD_TEXT_Y, 10.0, true);
        text_centre(&mut out, &value, cx, HEAD_ROW_Y, 10.0, false);
    }

    // The pieces on this sheet, grouped by which part they are. Worked out
    // before anything is drawn, because the drawing captions each piece with
    // the row number the table below is about to give it.
    let matched: Vec<Option<usize>> = layout.parts.iter().map(|p| match_part(meta, &p.shape)).collect();
    let mut counts: Vec<usize> = vec![0; meta.parts.len()];
    let mut unknown = 0usize;
    for at in &matched {
        match at {
            Some(at) => counts[*at] += 1,
            None => unknown += 1,
        }
    }
    // Row number per part, in the order the table lists them - parts with no
    // piece on this sheet get no row and so no number.
    let mut row_of: Vec<Option<usize>> = vec![None; meta.parts.len()];
    let mut next_row = 0usize;
    for (at, &count) in counts.iter().enumerate() {
        if count > 0 {
            next_row += 1;
            row_of[at] = Some(next_row);
        }
    }
    let unlisted_row = (unknown > 0).then(|| next_row + 1);
    let labels: Vec<Option<usize>> = matched.iter().map(|at| at.map_or(unlisted_row, |at| row_of[at])).collect();

    // The drawing frame, and the sheet inside it.
    hline(&mut out, TABLE_L - 0.38, TABLE_R + 0.37, FRAME_TOP_Y);
    hline(&mut out, TABLE_L + 0.37, TABLE_R - 0.38, FRAME_BOT_Y);
    vline(&mut out, TABLE_L, FRAME_TOP_Y + 0.37, FRAME_BOT_Y - 0.38);
    vline(&mut out, TABLE_R, FRAME_TOP_Y + 0.37, FRAME_BOT_Y - 0.38);
    draw_sheet(&mut out, layout, DRAW_X, DRAW_Y, &labels);

    text_at(&mut out, "No.", LPART_NO_X - text_width("No.", 10.0) / 2.0, PARTS_HEAD_Y, 10.0, true);
    text_at(&mut out, "Name", LPART_NAME_X, PARTS_HEAD_Y, 10.0, true);
    for (&cx, head) in LPART_COLS.iter().zip(LPART_HEADS) {
        text_centre(&mut out, head, cx, PARTS_HEAD_Y, 10.0, true);
    }
    dashed_hline(&mut out, LPART_L, LPART_R, PARTS_DASH_Y);

    // The last row that still clears the footer rule, with room for the totals
    // row and the remnant line under it. Everything past it collapses into a
    // single "+N more" - a table that silently runs off the page is the one
    // failure a report must not have.
    let last_row_y = FOOTER_RULE_Y + TOTAL_DROP + 16.0 + ROW_PITCH;

    let mut y = PARTS_ROW_Y;
    let mut no = 0usize;
    let mut total_contours = 0usize;
    let mut total_qty = 0usize;
    let mut hidden = 0usize;
    for (part, &count) in meta.parts.iter().zip(counts.iter()) {
        if count == 0 {
            continue;
        }
        no += 1;
        if y < last_row_y {
            hidden += 1;
            total_contours += contour_count(&part.shape) * count;
            total_qty += count;
            continue;
        }
        let pb = get_polygon_bounds(&part.shape.points).unwrap_or(NO_BOUNDS);
        let contours = contour_count(&part.shape);
        total_contours += contours * count;
        total_qty += count;
        let values = [
            format!("{:.2}", pb.width),
            format!("{:.2}", pb.height),
            contours.to_string(),
            format!("{:.3}", polygon_material_area(&part.shape) * count as f64 / 1e6),
            count.to_string(),
        ];
        text_centre(&mut out, &no.to_string(), LPART_NO_X, y, 10.0, false);
        text_at(&mut out, &part.name, LPART_NAME_X, y, 10.0, false);
        thumbnail(&mut out, &part.shape, LPART_THUMB_X, y - 5.7);
        for (&cx, value) in LPART_COLS.iter().zip(values) {
            text_centre(&mut out, &value, cx, y, 10.0, false);
        }
        y -= ROW_PITCH;
    }
    if unknown > 0 {
        no += 1;
        total_qty += unknown;
        if y >= last_row_y {
            text_centre(&mut out, &no.to_string(), LPART_NO_X, y, 10.0, false);
            text_at(&mut out, "(unlisted)", LPART_NAME_X, y, 10.0, false);
            text_centre(&mut out, &unknown.to_string(), LPART_COLS[4], y, 10.0, false);
            y -= ROW_PITCH;
        } else {
            hidden += 1;
        }
    }
    if hidden > 0 {
        text_at(&mut out, &format!("... and {hidden} more part type(s)"), LPART_NAME_X, y, 10.0, false);
        y -= ROW_PITCH;
    }

    let rule_y = y + ROW_PITCH - ROW_RULE_DROP;
    hline(&mut out, LPART_L, LPART_R, rule_y);
    text_centre(&mut out, &total_contours.to_string(), LPART_COLS[2], rule_y - TOTAL_DROP, 10.0, false);
    text_centre(&mut out, &total_qty.to_string(), LPART_COLS[4], rule_y - TOTAL_DROP, 10.0, false);

    let offcut = sheet_offcut(layout, meta.spacing);
    if offcut.usable_w >= 1.0 && offcut.usable_h >= 1.0 {
        text_at(
            &mut out,
            &format!(
                "Remnant Size: {:.2}x{:.2} Area:{:.3}",
                offcut.usable_w,
                offcut.usable_h,
                offcut.usable_w * offcut.usable_h / 1e6
            ),
            LPART_L,
            rule_y - TOTAL_DROP - 16.0,
            10.0,
            false,
        );
    }

    vec![out]
}

/// Which `meta.parts` entry a placed shape is a copy of.
///
/// Matched on measurements rather than identity because a `PlacedShape` has
/// none by the time it reaches here: material area, contour count and cut
/// length together separate any two parts a person would call different, and
/// all three are rotation-invariant, which the bounding box is not.
fn match_part(meta: &ReportMeta, shape: &LayeredPolygon) -> Option<usize> {
    let area = polygon_material_area(shape);
    let contours = contour_count(shape);
    let length = cut_length(shape);
    meta.parts.iter().position(|p| {
        contour_count(&p.shape) == contours
            && (polygon_material_area(&p.shape) - area).abs() <= area.abs().mul_add(1e-6, 1e-6)
            && (cut_length(&p.shape) - length).abs() <= length.mul_add(1e-6, 1e-6)
    })
}

/// The sheet, to scale, inside the reference's own drawing box - blue line
/// art, each piece captioned with its row number in this page's part table.
///
/// The number, not a `#n` position label: it is what lets someone holding a
/// cut piece find the row that says how big it is and how many there are, and
/// it is what the reference report draws.
fn draw_sheet(out: &mut String, layout: &SheetLayout, x: f64, y: f64, labels: &[Option<usize>]) {
    let Some(bounds) = get_polygon_bounds(&layout.sheet.points) else { return };
    let scale = (DRAW_W / bounds.width.max(1e-9)).min(DRAW_H / bounds.height.max(1e-9));
    let ox = x + (DRAW_W - bounds.width * scale) / 2.0;
    let oy = y + (DRAW_H - bounds.height * scale) / 2.0;
    // PDF's y axis points up, same as this codebase's, so this is a plain
    // scale-and-offset - no flip anywhere.
    let map = |p: Point| (ox + (p.x - bounds.x) * scale, oy + (p.y - bounds.y) * scale);

    let _ = writeln!(out, "q 0 0 1 RG 0.66 w");
    path(out, &layout.sheet.points, map);
    let mut captions: Vec<(usize, f64, f64)> = Vec::new();
    for (i, part) in layout.parts.iter().enumerate() {
        let geometry = placed_geometry(part);
        stroke_tree(out, &geometry, map);
        if let (Some(no), Some(b)) = (labels.get(i).copied().flatten(), get_polygon_bounds(&geometry.points)) {
            let (cx, cy) = map(Point::new(b.x + b.width / 2.0, b.y + b.height / 2.0));
            captions.push((no, cx, cy));
        }
    }
    let _ = writeln!(out, "Q");
    for (no, cx, cy) in captions {
        text_centre(out, &no.to_string(), cx, cy - 3.0, 9.0, false);
    }
}

/// A part's outline shrunk into a 14.4pt square, the size the reference puts
/// beside every table row.
fn thumbnail(out: &mut String, shape: &LayeredPolygon, x: f64, y: f64) {
    const SIZE: f64 = 14.4;
    let Some(b) = get_polygon_bounds(&shape.points) else { return };
    let scale = (SIZE / b.width.max(1e-9)).min(SIZE / b.height.max(1e-9));
    let ox = x + (SIZE - b.width * scale) / 2.0;
    let oy = y + (SIZE - b.height * scale) / 2.0;
    let map = |p: Point| (ox + (p.x - b.x) * scale, oy + (p.y - b.y) * scale);
    let _ = writeln!(out, "q 0 0 1 RG 0.4 w");
    stroke_tree(out, shape, map);
    let _ = writeln!(out, "Q");
}

// --- the page frame -----------------------------------------------------

/// The border, the footer rule and "date ... n / total" - identical on every
/// page, drawn before that page's own content.
fn frame(page: usize, total: usize, generated: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "q 0 0 0 RG 0.75 w");
    hline(&mut out, FRAME_L, FRAME_R, FRAME_T);
    hline(&mut out, FRAME_L, FRAME_R, FRAME_B);
    vline(&mut out, FRAME_L, FRAME_T + 0.38, FRAME_B);
    vline(&mut out, FRAME_R, FRAME_T + 0.38, FRAME_B);
    hline(&mut out, RULE_L, RULE_R, FOOTER_RULE_Y);
    let _ = writeln!(out, "Q");
    text_at(&mut out, generated, 20.55, 24.68, 10.0, false);
    text_at(&mut out, &page.to_string(), 665.56, 23.18, 10.0, false);
    text_at(&mut out, "/", 718.18, 23.18, 10.0, false);
    text_at(&mut out, &total.to_string(), 770.81, 23.18, 10.0, false);
    out
}

// --- drawing primitives -------------------------------------------------

fn hline(out: &mut String, x0: f64, x1: f64, y: f64) {
    let _ = writeln!(out, "0.75 w 0 0 0 RG {x0:.2} {y:.2} m {x1:.2} {y:.2} l S");
}

fn dashed_hline(out: &mut String, x0: f64, x1: f64, y: f64) {
    let _ = writeln!(out, "q [2.25 1.5] 0 d 0.75 w 0 0 0 RG {x0:.2} {y:.2} m {x1:.2} {y:.2} l S Q");
}

fn vline(out: &mut String, x: f64, y0: f64, y1: f64) {
    let _ = writeln!(out, "0.75 w 0 0 0 RG {x:.2} {y0:.2} m {x:.2} {y1:.2} l S");
}

fn stroke_tree(out: &mut String, shape: &LayeredPolygon, map: impl Fn(Point) -> (f64, f64) + Copy) {
    path(out, &shape.points, map);
    for child in &shape.children {
        stroke_tree(out, child, map);
    }
}

fn path(out: &mut String, points: &[Point], map: impl Fn(Point) -> (f64, f64)) {
    let Some((first, rest)) = points.split_first() else { return };
    let (x, y) = map(*first);
    let _ = writeln!(out, "{x:.3} {y:.3} m");
    for p in rest {
        let (x, y) = map(*p);
        let _ = writeln!(out, "{x:.3} {y:.3} l");
    }
    let _ = writeln!(out, "h S");
}

fn text_at(out: &mut String, value: &str, x: f64, y: f64, size: f64, bold: bool) {
    let font = if bold { "/F2" } else { "/F1" };
    let _ = writeln!(out, "BT {font} {size} Tf {x:.2} {y:.2} Td ({}) Tj ET", pdf_string(value));
}

fn text_centre(out: &mut String, value: &str, cx: f64, y: f64, size: f64, bold: bool) {
    text_at(out, value, cx - text_width(value, size) / 2.0, y, size, bold);
}

/// See this module's doc comment: one average advance width, not the real
/// Times metrics.
fn text_width(value: &str, size: f64) -> f64 {
    value.chars().count() as f64 * size * 0.487
}

/// Escapes a string for a PDF literal and drops anything the built-in
/// WinAnsi Times can't represent - see this module's doc comment.
fn pdf_string(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '(' => "\\(".to_string(),
            ')' => "\\)".to_string(),
            '\\' => "\\\\".to_string(),
            // WinAnsi does carry a superscript two, and the reference report
            // heads its area columns `[m²]` with it - so emit it by its octal
            // code rather than dropping it into the `?` bucket below.
            '²' => "\\262".to_string(),
            c if c.is_ascii_graphic() || c == ' ' => c.to_string(),
            _ => "?".to_string(),
        })
        .collect()
}

// --- derived numbers ----------------------------------------------------

/// How many separate closed contours a piece has - its outline plus every
/// hole, all the way down. This is what a machine actually cuts, and it is the
/// column the reference report calls `CtrQty`.
fn contour_count(shape: &LayeredPolygon) -> usize {
    1 + shape.children.iter().map(contour_count).sum::<usize>()
}

/// Total length of every contour, in millimetres - outline and holes. Cut time
/// tracks this far more closely than it tracks area, which is why the
/// reference report carries it on every row.
fn cut_length(shape: &LayeredPolygon) -> f64 {
    let ring: f64 = shape.points.iter().enumerate().map(|(i, p)| p.distance_to(shape.points[(i + 1) % shape.points.len()])).sum();
    ring + shape.children.iter().map(cut_length).sum::<f64>()
}

/// One row of the sheet table: a distinct layout, and how many sheets are cut
/// from it.
struct NestGroup {
    /// Index into `stats` of the first sheet with this layout.
    first: usize,
    duplicate: usize,
}

/// Groups sheets that are laid out identically, so the report can say "cut
/// this one three times" instead of listing three indistinguishable rows -
/// and print one page for it instead of three.
///
/// This is the column the reference tool's own report leads with, and it is
/// also the measurement for `PLAN.md` 1.2 - a nester that finds one good
/// arrangement and reuses it produces few groups with high duplicate counts,
/// and one that re-solves every sheet from scratch produces a wall of ones.
///
/// Two sheets match when their outlines match and every piece sits in the same
/// place, rounded to 0.05mm - far below any tolerance that matters, and well
/// above the float noise two independently-computed placements carry.
fn nest_groups(layouts: &[SheetLayout]) -> Vec<NestGroup> {
    /// Millimetres. Two placements this close are the same placement - well
    /// under any tolerance a machine or a person cares about, and well above
    /// the float noise two independently-computed placements carry.
    const TOL: f64 = 0.05;

    /// One sheet's arrangement, in a canonical order.
    ///
    /// Sorted, because the same arrangement reached in a different placement
    /// order is the same pattern to whoever has to cut it.
    fn slots(layout: &SheetLayout) -> Vec<(f64, f64, f64, f64)> {
        let mut parts: Vec<(f64, f64, f64, f64)> =
            layout.parts.iter().map(|p| (p.x, p.y, p.rotation, polygon_area(&p.shape.points).abs())).collect();
        parts.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.total_cmp(&b.2)));
        parts
    }

    /// **Compared within a tolerance, never by formatted text.**
    ///
    /// Rounding each coordinate to a fixed number of decimals and comparing
    /// the strings looks equivalent and is not: any part that happens to sit
    /// on a rounding boundary lands in a different bucket for the sake of a
    /// difference far below what anyone can measure, and one such part out of
    /// hundreds splits the whole sheet into its own group. Caught on a real
    /// 253-piece result whose two sheets were identical down to the drawn
    /// PDF operators and still reported as two distinct layouts.
    fn same(a: &[(f64, f64, f64, f64)], b: &[(f64, f64, f64, f64)]) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).all(|(p, q)| {
                (p.0 - q.0).abs() < TOL && (p.1 - q.1).abs() < TOL && (p.2 - q.2).abs() < TOL && (p.3 - q.3).abs() < 1.0
            })
    }

    let mut groups: Vec<NestGroup> = Vec::new();
    let mut seen: Vec<(Vec<(f64, f64, f64, f64)>, f64, f64)> = Vec::new();
    for (index, layout) in layouts.iter().enumerate() {
        let size = get_polygon_bounds(&layout.sheet.points).map_or((0.0, 0.0), |b| (b.width, b.height));
        let here = slots(layout);
        let at = seen
            .iter()
            .position(|(other, w, h)| (w - size.0).abs() < TOL && (h - size.1).abs() < TOL && same(other, &here));
        if let Some(at) = at {
            groups[at].duplicate += 1;
        } else {
            seen.push((here, size.0, size.1));
            groups.push(NestGroup { first: index, duplicate: 1 });
        }
    }
    groups
}

/// `1234567.8` -> `1,234,568`. The reference report groups with commas.
fn grouped(value: f64) -> String {
    let digits = format!("{:.0}", value.max(0.0));
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What one sheet has left over once every piece on it is cut.
///
/// `count`/`reclaimable` are measured but not printed - the reference report's
/// Remnant Info carries only the labelled rectangle. They stay because they
/// are what the module's own tests check the printed rectangle against: a
/// label larger than the true free area would be material that isn't there.
#[cfg_attr(not(test), allow(dead_code))]
struct Offcut {
    /// How many separate leftover pieces the sheet breaks into.
    count: usize,
    /// The biggest one's largest inscribed rectangle - the size worth writing
    /// on a label.
    usable_w: f64,
    usable_h: f64,
    /// True free area across every leftover piece on the sheet.
    reclaimable: f64,
}

/// Measures the offcuts on one sheet with `remnant::sheet_remnants`, the same
/// call the offcut shelf itself uses - so what the report promises is exactly
/// what the library will offer back on the next job, not a second estimate of
/// it.
fn sheet_offcut(layout: &SheetLayout, spacing: f64) -> Offcut {
    let placed: Vec<crate::remnant::PlacedOutline> = layout.parts.iter().map(|p| placed_geometry(p).points).collect();
    let remnants = crate::remnant::sheet_remnants(&layout.sheet.points, &placed, spacing);
    let reclaimable = remnants.iter().map(|r| r.area).sum();
    let largest = remnants.iter().max_by(|a, b| a.area.total_cmp(&b.area));
    Offcut {
        count: remnants.len(),
        usable_w: largest.map_or(0.0, |r| r.usable.width),
        usable_h: largest.map_or(0.0, |r| r.usable.height),
        reclaimable,
    }
}

// --- the PDF container itself ------------------------------------------

/// Wraps the content streams into a minimal, valid PDF 1.4 file: catalog,
/// page tree, one page + one stream per content string, two standard fonts,
/// then the cross-reference table every reader needs to find them.
fn assemble(pages: &[String]) -> Vec<u8> {
    let mut objects: Vec<String> = Vec::new();
    // 1 = catalog, 2 = page tree, 3 = roman, 4 = bold, then (page, stream) pairs.
    let first_page_obj = 5;
    let kids: Vec<String> = (0..pages.len()).map(|i| format!("{} 0 R", first_page_obj + i * 2)).collect();

    objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_string());
    objects.push(format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman /Encoding /WinAnsiEncoding >>".to_string());
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Times-Bold /Encoding /WinAnsiEncoding >>".to_string());

    for (i, content) in pages.iter().enumerate() {
        let stream_obj = first_page_obj + i * 2 + 1;
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {PAGE_W:.2} {PAGE_H:.2}] /Resources << /Font << /F1 3 0 R /F2 4 0 R >> >> /Contents {stream_obj} 0 R >>"
        ));
        objects.push(format!("<< /Length {} >>\nstream\n{content}endstream", content.len()));
    }

    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        let _ = write!(out, "{} 0 obj\n{body}\nendobj\n", i + 1);
    }

    let xref_at = out.len();
    // Every xref entry is exactly 20 bytes, trailing space included - that's
    // the format's own fixed-width rule, not padding for looks.
    let _ = writeln!(out, "xref\n0 {}\n0000000000 65535 f ", objects.len() + 1);
    for offset in &offsets {
        let _ = writeln!(out, "{offset:010} 00000 n ");
    }
    let _ = write!(out, "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n", objects.len() + 1);

    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxf_import::LayeredPolygon;

    fn square(size: f64) -> LayeredPolygon {
        LayeredPolygon::new(vec![Point::new(0.0, 0.0), Point::new(size, 0.0), Point::new(size, size), Point::new(0.0, size)], "CUT".into(), None)
    }

    fn layout(parts: usize) -> SheetLayout {
        SheetLayout {
            sheet: square(100.0),
            parts: (0..parts)
                .map(|i| PlacedShape { shape: square(10.0), x: i as f64 * 12.0, y: 0.0, rotation: 0.0 })
                .collect(),
        }
    }

    fn meta() -> ReportMeta {
        ReportMeta {
            title: "nestresult".into(),
            generated: "2026-01-01 09:00".into(),
            spacing: 2.0,
            parts: vec![ReportPart { name: "widget".into(), quantity: 4, nested: 3, shape: square(10.0) }],
        }
    }

    #[test]
    fn writes_a_structurally_valid_pdf() {
        let bytes = export_report(&[layout(2), layout(1)], &meta());
        let text = String::from_utf8(bytes).expect("the writer only emits ASCII");

        assert!(text.starts_with("%PDF-1.4"), "must announce itself as a PDF");
        assert!(text.trim_end().ends_with("%%EOF"), "must be terminated");
        assert!(text.contains("startxref"), "readers need the xref offset");
        let object_count = text.matches(" 0 obj").count();
        assert!(text.contains(&format!("/Size {}", object_count + 1)));
        assert!(text.contains("/Times-Roman") && text.contains("/Times-Bold"), "both fonts must be declared");
    }

    /// The report is the reference's three tables and nothing else - no
    /// summary block, no settings dump, no audit section.
    #[test]
    fn the_summary_page_is_the_reference_tables_and_nothing_else() {
        let text = String::from_utf8(export_report(&[layout(2)], &meta())).expect("ASCII only");
        for wanted in ["Part-List", "Sheet-List", "Total Parts Count: 1", "MatName", "CtrQty", "Duplicate"] {
            assert!(text.contains(wanted), "missing {wanted}");
        }
        for unwanted in ["SUMMARY", "SETTINGS", "MANUFACTURABILITY", "Utilisation:", "Generated"] {
            assert!(!text.contains(unwanted), "the reference report has no {unwanted} and neither should this one");
        }
    }

    /// **One page per distinct layout, not per sheet.** Eleven sheets from two
    /// arrangements is a three-page report; the old one-page-per-sheet version
    /// printed nine identical pages of nothing new.
    #[test]
    fn identical_sheets_share_one_page_instead_of_repeating() {
        let layouts: Vec<SheetLayout> = vec![layout(3), layout(3), layout(3), layout(2)];
        let text = String::from_utf8(export_report(&layouts, &meta())).expect("ASCII only");
        let pages = text.matches("/Type /Page ").count();
        assert_eq!(pages, 3, "summary + two distinct layouts, got {pages}");
        assert!(text.contains("sheet1") && text.contains("sheet2"));
        assert!(!text.contains("sheet3"), "four sheets, two layouts - there is no sheet3 page");
    }

    /// Every page carries the frame and its own "n / total".
    #[test]
    fn every_page_is_framed_and_numbered() {
        let text = String::from_utf8(export_report(&[layout(3), layout(2)], &meta())).expect("ASCII only");
        let pages = text.matches("/Type /Page ").count();
        assert_eq!(text.matches("(/) Tj").count(), pages, "one page-number separator per page");
        assert!(text.matches(&format!("({pages}) Tj")).count() >= pages, "each page prints the total");
    }

    #[test]
    fn an_empty_result_still_produces_a_readable_one_page_report() {
        let text = String::from_utf8(export_report(&[], &meta())).expect("ASCII only");
        assert!(text.starts_with("%PDF-1.4"));
        assert!(text.contains("Part-List"));
        assert_eq!(text.matches("/Type /Page ").count(), 1);
    }

    /// The part list is the half of the report the drawing cannot give you:
    /// which piece is short, and by how many.
    #[test]
    fn the_part_list_prints_ordered_nested_and_left_with_the_measured_columns() {
        let mut meta = meta();
        meta.parts = vec![ReportPart { name: "bracket".into(), quantity: 50, nested: 47, shape: square(404.0) }];
        let text = String::from_utf8(export_report(&[layout(2)], &meta)).expect("ASCII only");
        assert!(text.contains("bracket"));
        assert!(text.contains("(404.00) Tj"), "size in mm, measured off the shape");
        assert!(text.contains("(50) Tj") && text.contains("(47) Tj") && text.contains("(3) Tj"), "ordered/nested/left");
    }

    /// Contours and cut length are per-piece manufacturing numbers, and both
    /// have to count holes or they understate every real part.
    #[test]
    fn contours_and_cut_length_include_every_hole() {
        let mut part = square(10.0);
        part.children.push(square(2.0));
        assert_eq!(contour_count(&part), 2, "outline plus one hole");
        // 4x10 outline + 4x2 hole = 48mm of cutting.
        assert!((cut_length(&part) - 48.0).abs() < 1e-9, "got {}", cut_length(&part));
    }

    #[test]
    fn big_numbers_are_grouped_and_negatives_clamp_rather_than_printing_a_sign() {
        assert_eq!(grouped(1_234_567.8), "1,234,568");
        assert_eq!(grouped(999.0), "999");
        assert_eq!(grouped(0.0), "0");
        assert_eq!(grouped(-5.0), "0");
    }

    /// A long job must not run off the bottom of the page - the numbers would
    /// simply be missing, and nothing else here would notice.
    #[test]
    fn a_job_with_more_rows_than_fit_spills_onto_another_summary_page() {
        let mut meta = meta();
        meta.parts = (0..60).map(|i| ReportPart { name: format!("part-{i}"), quantity: 1, nested: 1, shape: square(1.0) }).collect();
        let layouts: Vec<SheetLayout> = (0..40).map(|i| layout(i % 5 + 1)).collect();
        let stats: Vec<SheetStats> = layouts.iter().map(sheet_stats).collect();
        let groups = nest_groups(&layouts);
        let materials = material_names(&layouts);
        let pages = summary_pages(&meta, &layouts, &stats, &groups, &materials);
        assert!(pages.len() > 1, "expected the summary to paginate, got {} page(s)", pages.len());
        assert!(pages.iter().all(|p| p.contains("Tj")), "an emitted page must not be blank");
    }

    /// A placed shape has no identity by the time the report draws it, so the
    /// per-sheet table has to recognise it by measurement. Two parts of the
    /// same area but different outlines must not be confused.
    #[test]
    fn a_placed_shape_is_matched_back_to_the_part_it_is_a_copy_of() {
        let mut meta = meta();
        let tall = LayeredPolygon { points: vec![Point::new(0.0, 0.0), Point::new(4.0, 0.0), Point::new(4.0, 25.0), Point::new(0.0, 25.0)], ..square(1.0) };
        meta.parts = vec![
            ReportPart { name: "square".into(), quantity: 1, nested: 1, shape: square(10.0) },
            ReportPart { name: "strip".into(), quantity: 1, nested: 1, shape: tall.clone() },
        ];
        assert_eq!(match_part(&meta, &square(10.0)), Some(0));
        assert_eq!(match_part(&meta, &tall), Some(1), "same 100mm2 area, different cut length");
        assert_eq!(match_part(&meta, &square(7.0)), None);
    }

    /// Waste and remnant are different numbers: waste is everything not cut
    /// into a piece, a remnant is the part of it big enough to nest onto again.
    #[test]
    fn the_remnant_row_is_the_usable_rectangle_not_the_whole_offcut() {
        let offcut = sheet_offcut(&layout(3), 2.0);
        assert!(offcut.reclaimable > 5_000.0, "expected most of the sheet reclaimable, got {}", offcut.reclaimable);
        assert!(offcut.usable_w > 0.0 && offcut.usable_h > 0.0, "a usable rectangle must be reported");
        assert!(
            offcut.usable_w * offcut.usable_h <= offcut.reclaimable + 1.0,
            "the labelled rectangle can never exceed the true free area"
        );
    }

    /// A sheet with nothing on it is untouched stock, not an offcut - filing it
    /// as one would put whole sheets on the remnant shelf.
    #[test]
    fn an_empty_sheet_reports_no_offcut_rather_than_a_whole_one() {
        let offcut = sheet_offcut(&layout(0), 2.0);
        assert_eq!(offcut.count, 0);
        assert_eq!(offcut.reclaimable, 0.0);
    }

    #[test]
    fn text_that_would_break_the_container_is_escaped() {
        assert_eq!(pdf_string("a (b) \\c"), "a \\(b\\) \\\\c");
        // Non-ASCII degrades to a placeholder rather than emitting bytes the
        // built-in font can't map - see the module doc.
        assert_eq!(pdf_string("cắt"), "c?t");
    }
}
