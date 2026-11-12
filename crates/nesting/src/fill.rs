//! Filler parts: "as many as fit". A filler has no quantity. Once the nest
//! has placed every required part, the space left on the sheets it already
//! opened is filled with copies of each filler. It never opens a sheet.
//!
//! **A post-pass, not genes.** Handing fillers to the GA would have it rank
//! every individual by how many copies it failed to place (the 1e8 unplaced
//! penalty) and open new sheets for them, which is exactly what a filler must
//! not do. So the nest runs without them and this tops each sheet up.
//!
//! **Ids.** A copy needs its own part id, and every consumer resolves a
//! placed id back to its shape. A copy's id carries its template's in the low
//! bits under `FILLER_ID_BIT`: `template_of` gets it back without a lookup
//! table, so a report can count copies under the right row.

use std::collections::HashMap;

use geometry::dxf_import::{rotate_layered_polygon, LayeredPolygon};
use geometry::polygon::polygon_area;

use crate::cache::NfpCache;
use crate::placement::{place_on_sheet, rotation_steps, PlacedObstacle, PlacedPart, PlacementConfig, SheetPlacement};

/// Above `dispatch::MIRROR_ID_BIT` (1 << 20) and every real part id.
pub const FILLER_ID_BIT: usize = 1 << 40;
/// Low bits holding the template id. Part ids stay below the mirror bit.
const TEMPLATE_MASK: usize = (1 << 20) - 1;
/// Most copies one sheet can take, so a degenerate (zero-area) filler cannot
/// spin forever. Far beyond anything a real sheet holds.
const MAX_COPIES_PER_SHEET: usize = 10_000;

/// The id of copy `n` of filler `template`.
#[must_use]
pub fn copy_id(template: usize, n: usize) -> usize {
    FILLER_ID_BIT | (n << 21) | template
}

/// The id a placed part's shape is registered under: its template for a
/// filler copy, itself otherwise.
#[must_use]
pub fn template_of(id: usize) -> usize {
    if id & FILLER_ID_BIT != 0 {
        id & TEMPLATE_MASK
    } else {
        id
    }
}

/// Tops up every sheet in `placements` with copies of `fillers` (template
/// ids, keys of `parts_by_id`), largest filler first, each at every angle its
/// rule allows. Returns `(copy id, template id)` for every copy placed.
///
/// The caller decides which sheets are eligible by what it passes - the last
/// sheet is left out so its offcut survives.
#[allow(clippy::too_many_arguments)]
pub fn fill_sheets(
    sheets: &[LayeredPolygon],
    placements: &mut [SheetPlacement],
    fillers: &[usize],
    parts_by_id: &HashMap<usize, LayeredPolygon>,
    shape_ids: &HashMap<usize, usize>,
    config: &PlacementConfig,
    cache: &NfpCache,
    should_cancel: &(impl Fn() -> bool + Sync),
) -> Vec<(usize, usize)> {
    let source_of = |id: usize| shape_ids.get(&id).copied().unwrap_or(id);
    let mut order: Vec<usize> = fillers.iter().copied().filter(|t| parts_by_id.contains_key(t)).collect();
    order.sort_by(|a, b| polygon_area(&parts_by_id[b].points).abs().total_cmp(&polygon_area(&parts_by_id[a].points).abs()));

    let mut next_copy: HashMap<usize, usize> = HashMap::new();
    let mut added = Vec::new();
    for placement in placements.iter_mut() {
        let Some(sheet) = sheets.get(placement.sheet_index) else { continue };
        let mut obstacles: Vec<PlacedObstacle> = placement
            .parts
            .iter()
            .filter_map(|p| {
                let template = template_of(p.id);
                Some(PlacedObstacle {
                    polygon: rotate_layered_polygon(parts_by_id.get(&template)?, p.rotation),
                    id: p.id,
                    source_id: source_of(template),
                    rotation: p.rotation,
                    placement: p.placement,
                })
            })
            .collect();

        for &template in &order {
            let base = &parts_by_id[&template];
            for _ in 0..MAX_COPIES_PER_SHEET {
                if should_cancel() {
                    return added;
                }
                let best = rotation_steps(config, template, 0.0)
                    .into_iter()
                    .filter_map(|(angle, _)| {
                        let rotated = rotate_layered_polygon(base, angle);
                        let result = place_on_sheet(&rotated, source_of(template), angle, sheet, placement.sheet_index, &obstacles, config, cache)?;
                        Some((result, angle, rotated))
                    })
                    .min_by(|a, b| a.0.minarea.total_cmp(&b.0.minarea));
                let Some((result, angle, rotated)) = best else { break };
                let n = next_copy.entry(template).or_insert(0);
                let id = copy_id(template, *n);
                *n += 1;
                obstacles.push(PlacedObstacle { polygon: rotated, id, source_id: source_of(template), rotation: angle, placement: result.position });
                placement.parts.push(PlacedPart { id, placement: result.position, rotation: angle });
                added.push((id, template));
            }
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placement::{Placement, PlacementType, DEFAULT_DOMINANT_PART_AREA_THRESHOLD};
    use geometry::point::Point;

    fn rect(w: f64, h: f64) -> LayeredPolygon {
        LayeredPolygon::new(vec![Point::new(0.0, 0.0), Point::new(w, 0.0), Point::new(w, h), Point::new(0.0, h)], "0".into(), None)
    }

    fn config() -> PlacementConfig {
        PlacementConfig {
            placement_type: PlacementType::Gravity,
            rotations: 4,
            dominant_part_area_threshold: DEFAULT_DOMINANT_PART_AREA_THRESHOLD,
            curve_tolerance: 0.3,
            part_rules: Default::default(),
            banded_pass: false,
            small_centre: None,
        }
    }

    #[test]
    fn a_copy_id_names_its_template_and_never_collides_with_a_real_or_mirrored_id() {
        for template in [0, 7, (1 << 20) - 1] {
            for n in [0, 1, 99_999] {
                let id = copy_id(template, n);
                assert_eq!(template_of(id), template);
                assert_ne!(id & crate::dispatch::MIRROR_ID_BIT, crate::dispatch::MIRROR_ID_BIT, "a copy must never read as mirrored");
            }
        }
        assert_eq!(template_of(42), 42, "a real id is its own template");
    }

    /// One 59.5x101 part on a 100x101 sheet leaves a 40.5x101 strip: four 20x50
    /// fillers fit there and no more, all on the sheet that was already open.
    /// Not an exact fit on purpose - a slot exactly a part wide is a line in
    /// NFP space, and a line does not survive a polygon difference (see
    /// `placement::find_best_candidate`).
    #[test]
    fn fillers_fill_the_open_sheet_and_stop_when_it_is_full() {
        let sheets = vec![rect(100.0, 101.0)];
        let parts_by_id: HashMap<usize, LayeredPolygon> = HashMap::from([(0, rect(59.5, 101.0)), (1, rect(20.0, 50.0))]);
        let mut placements = vec![SheetPlacement { sheet_index: 0, parts: vec![PlacedPart { id: 0, placement: Placement { x: 0.0, y: 0.0 }, rotation: 0.0 }] }];
        let added = fill_sheets(&sheets, &mut placements, &[1], &parts_by_id, &HashMap::new(), &config(), &NfpCache::new(), &|| false);
        assert_eq!(added.len(), 4, "a 40.5x101 strip holds four 20x50 copies");
        assert_eq!(placements.len(), 1, "a filler never opens a sheet");
        assert_eq!(placements[0].parts.len(), 5);
        assert!(added.iter().all(|&(id, t)| t == 1 && template_of(id) == 1));
        let mut ids: Vec<usize> = added.iter().map(|&(id, _)| id).collect();
        ids.dedup();
        assert_eq!(ids.len(), 4, "every copy has its own id");
    }
}
