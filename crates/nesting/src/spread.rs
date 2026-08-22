//! How evenly a multi-sheet nest packed - the distribution, not the average.
//!
//! **Why an average is the wrong number here.** A job that puts 30 sheets at
//! 66% and 3 at 75% reports about 67% overall and reads as "mediocre, needs
//! tuning". It is nothing of the kind: the engine *found* a 75% arrangement
//! and then failed to reproduce it on the other 30 sheets. Those nine points
//! are whole sheets of material, and the aggregate figure every other
//! benchmark in this repo prints cannot see them - by construction, since
//! averaging is exactly the operation that destroys the evidence.
//!
//! So this module reports the shape of the distribution instead, and the one
//! derived figure that turns it into a decision: `wasted_sheets`, how many
//! sheets a perfect replication of the best sheet would have saved. That is
//! the size of the prize, in the unit someone actually buys.
//!
//! **Reading the histogram is the diagnosis.** One tight cluster means the
//! engine is consistent and simply isn't packing well - a placement-quality
//! problem. A small high group plus a large low group means it can pack well
//! and doesn't do it every time - a replication problem. Those have entirely
//! different fixes, and telling them apart is the whole point.
//!
//! Utilisation is computed from *true* part areas, never padded ones: the
//! question is "how much of this sheet becomes product", and counting each
//! part's clearance ring as product would flatter every number here.

use std::collections::HashMap;

use crate::placement::PlaceResult;

/// Percentage points below the best sheet before a sheet counts as a
/// laggard. Under this is arrangement noise - one part landing differently -
/// rather than a genuinely worse packing.
pub const LAGGARD_TOLERANCE: f64 = 2.0;

/// Per-sheet utilisation for one nest result, plus what the spread costs.
#[derive(Clone, Debug)]
pub struct Spread {
    /// Every *used* sheet's own utilisation percentage, best first. Empty
    /// sheets are excluded: an untouched sheet is unused stock, and counting
    /// it as a 0% sheet would drag every statistic here toward a number that
    /// describes the sheet allowance rather than the packing.
    pub per_sheet: Vec<f64>,
    /// True (unpadded) part area actually placed, mm^2.
    pub placed_area: f64,
    /// One sheet's usable area, mm^2.
    pub sheet_area: f64,
}

impl Spread {
    /// Builds a spread from a result and a true-area-per-part-id map.
    #[must_use]
    pub fn of(result: &PlaceResult, true_area_by_id: &HashMap<usize, f64>, sheet_area: f64) -> Self {
        let mut per_sheet = Vec::new();
        let mut placed_area = 0.0;
        for placement in &result.placements {
            if placement.parts.is_empty() {
                continue;
            }
            let used: f64 = placement.parts.iter().filter_map(|p| true_area_by_id.get(&p.id)).sum();
            placed_area += used;
            per_sheet.push(if sheet_area > 0.0 { used / sheet_area * 100.0 } else { 0.0 });
        }
        per_sheet.sort_by(|a, b| b.total_cmp(a));
        Self { per_sheet, placed_area, sheet_area }
    }

    #[must_use]
    pub fn sheets_used(&self) -> usize {
        self.per_sheet.len()
    }

    #[must_use]
    pub fn best(&self) -> f64 {
        self.per_sheet.first().copied().unwrap_or(0.0)
    }

    #[must_use]
    pub fn worst(&self) -> f64 {
        self.per_sheet.last().copied().unwrap_or(0.0)
    }

    #[must_use]
    pub fn mean(&self) -> f64 {
        if self.per_sheet.is_empty() {
            return 0.0;
        }
        self.per_sheet.iter().sum::<f64>() / self.per_sheet.len() as f64
    }

    #[must_use]
    pub fn median(&self) -> f64 {
        if self.per_sheet.is_empty() {
            return 0.0;
        }
        self.per_sheet[self.per_sheet.len() / 2]
    }

    #[must_use]
    pub fn stddev(&self) -> f64 {
        if self.per_sheet.len() < 2 {
            return 0.0;
        }
        let mean = self.mean();
        (self.per_sheet.iter().map(|u| (u - mean).powi(2)).sum::<f64>() / self.per_sheet.len() as f64).sqrt()
    }

    /// Sheets more than `LAGGARD_TOLERANCE` below the best one found.
    #[must_use]
    pub fn laggards(&self) -> usize {
        let best = self.best();
        self.per_sheet.iter().filter(|u| best - **u > LAGGARD_TOLERANCE).count()
    }

    /// The last sheet of a job is almost always a partial remainder, and
    /// counting it as a failure to replicate would flag every well-nested job
    /// ever run. This is the spread among the sheets that were actually meant
    /// to be full.
    #[must_use]
    pub fn laggards_excluding_remainder(&self) -> usize {
        if self.per_sheet.len() < 2 {
            return 0;
        }
        let best = self.best();
        self.per_sheet[..self.per_sheet.len() - 1].iter().filter(|u| best - **u > LAGGARD_TOLERANCE).count()
    }

    /// How many sheets this job would have taken if every sheet packed as
    /// well as the best one did. Rounded up - three-quarters of a sheet still
    /// means opening a sheet.
    #[must_use]
    pub fn ideal_sheets(&self) -> usize {
        let best = self.best();
        if best <= 0.0 || self.sheet_area <= 0.0 {
            return self.per_sheet.len();
        }
        (self.placed_area / (self.sheet_area * best / 100.0)).ceil() as usize
    }

    /// **The headline figure.** Sheets lost purely to the engine not
    /// reproducing its own best arrangement.
    #[must_use]
    pub fn wasted_sheets(&self) -> usize {
        self.per_sheet.len().saturating_sub(self.ideal_sheets())
    }

    /// A compact ASCII histogram of the per-sheet utilisations, densest
    /// bucket scaled to full width.
    #[must_use]
    pub fn histogram(&self) -> String {
        const BUCKETS: usize = 10;
        const BAR_WIDTH: usize = 40;
        if self.per_sheet.is_empty() {
            return "  (no sheets used)\n".to_string();
        }
        let (lo, hi) = (self.worst(), self.best());
        // A run where every sheet packed identically has no range to bucket.
        // Saying so is more informative than one bar holding everything.
        if hi - lo < 0.05 {
            return format!("  all {} sheet(s) within 0.05% of {hi:.1}% - perfectly consistent\n", self.per_sheet.len());
        }
        let width = (hi - lo) / BUCKETS as f64;
        let mut counts = [0usize; BUCKETS];
        for u in &self.per_sheet {
            counts[(((u - lo) / width) as usize).min(BUCKETS - 1)] += 1;
        }
        let peak = counts.iter().copied().max().unwrap_or(1).max(1);
        let mut out = String::new();
        for (i, count) in counts.iter().enumerate().rev() {
            let bar = "#".repeat((*count * BAR_WIDTH).div_ceil(peak));
            out.push_str(&format!("  {:5.1}-{:5.1}% |{bar:<BAR_WIDTH$}| {count}\n", lo + width * i as f64, lo + width * (i + 1) as f64));
        }
        out
    }

    /// The whole report, ready to print.
    #[must_use]
    pub fn report(&self, unplaced: usize) -> String {
        let mut out = String::new();
        out.push_str(&format!("  sheets used     {}\n", self.sheets_used()));
        out.push_str(&format!("  unplaced        {unplaced}\n"));
        out.push_str(&format!("  best sheet      {:.1}%\n", self.best()));
        out.push_str(&format!("  median sheet    {:.1}%\n", self.median()));
        out.push_str(&format!("  mean sheet      {:.1}%\n", self.mean()));
        out.push_str(&format!("  worst sheet     {:.1}%\n", self.worst()));
        out.push_str(&format!("  spread          {:.1} points (stddev {:.1})\n", self.best() - self.worst(), self.stddev()));
        out.push_str(&format!(
            "  laggards        {} of {} sheet(s) >{LAGGARD_TOLERANCE:.0} points below best ({} excluding the remainder sheet)\n",
            self.laggards(),
            self.sheets_used(),
            self.laggards_excluding_remainder()
        ));
        out.push('\n');
        out.push_str(&self.histogram());
        out.push('\n');
        out.push_str(&format!("  >> if every sheet matched the best: {} sheets instead of {}\n", self.ideal_sheets(), self.sheets_used()));
        out.push_str(&format!("  >> COST OF INCONSISTENCY: {} wasted sheet(s)\n", self.wasted_sheets()));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spread(per_sheet: &[f64], sheet_area: f64) -> Spread {
        let mut v = per_sheet.to_vec();
        v.sort_by(|a, b| b.total_cmp(a));
        let placed_area = v.iter().map(|u| u / 100.0 * sheet_area).sum();
        Spread { per_sheet: v, placed_area, sheet_area }
    }

    /// The reported symptom, as a unit test: 30 sheets at 66% and 3 at 75%
    /// must be identified as costing real sheets, not averaged into "67%".
    #[test]
    fn the_reported_symptom_is_quantified_in_wasted_sheets() {
        let mut per_sheet = vec![75.0; 3];
        per_sheet.extend(vec![66.0; 30]);
        let s = spread(&per_sheet, 1000.0);
        assert_eq!(s.sheets_used(), 33);
        assert_eq!(s.best(), 75.0);
        assert_eq!(s.laggards(), 30, "every 66% sheet is a laggard against the 75% best");
        // 3*75 + 30*66 = 2205 sheet-percent of material; at 75% each that is
        // 29.4 -> 30 sheets, against the 33 actually used.
        assert_eq!(s.ideal_sheets(), 30);
        assert_eq!(s.wasted_sheets(), 3, "the inconsistency costs 3 whole sheets");
    }

    /// The opposite case must report zero: a perfectly consistent run has no
    /// replication problem, however low its utilisation is. Conflating "packs
    /// badly" with "packs inconsistently" would send anyone reading this at
    /// the wrong fix.
    #[test]
    fn a_uniformly_mediocre_run_reports_no_inconsistency_cost() {
        let s = spread(&[45.9; 25], 1000.0);
        assert_eq!(s.laggards(), 0);
        assert_eq!(s.wasted_sheets(), 0);
        assert!(s.stddev() < 1e-9, "identical sheets must have no spread, got {}", s.stddev());
        assert!(s.histogram().contains("perfectly consistent"), "{}", s.histogram());
    }

    /// A partial last sheet is normal and must not read as a failure to
    /// replicate - otherwise every well-nested job in existence is flagged.
    #[test]
    fn a_partial_remainder_sheet_is_excluded_from_the_laggard_count() {
        let s = spread(&[66.5, 66.5, 66.5, 66.5, 11.1], 1000.0);
        assert_eq!(s.laggards(), 1, "the remainder is still a laggard by the raw count");
        assert_eq!(s.laggards_excluding_remainder(), 0, "...but not once the remainder is set aside");
    }

    #[test]
    fn an_empty_result_produces_no_nans() {
        let s = spread(&[], 1000.0);
        assert_eq!(s.sheets_used(), 0);
        for v in [s.best(), s.worst(), s.mean(), s.median(), s.stddev()] {
            assert!(v.is_finite(), "{v}");
        }
        assert_eq!(s.wasted_sheets(), 0);
    }

    #[test]
    fn a_zero_area_sheet_does_not_divide_by_zero() {
        let s = Spread { per_sheet: vec![0.0], placed_area: 0.0, sheet_area: 0.0 };
        assert!(s.mean().is_finite());
        assert_eq!(s.ideal_sheets(), 1);
    }

    #[test]
    fn the_histogram_scales_its_densest_bucket_to_full_width() {
        let mut per_sheet = vec![75.0; 3];
        per_sheet.extend(vec![66.0; 30]);
        let text = spread(&per_sheet, 1000.0).histogram();
        assert!(text.contains("| 30"), "the 66% bucket should hold 30 sheets:\n{text}");
        assert!(text.contains("| 3"), "the 75% bucket should hold 3:\n{text}");
    }
}
