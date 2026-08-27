//! Unified NFP cache-key format, replacing the two duplicated
//! implementations in the Electron app: `NfpCache.makeKey`/`normalizeRotation`
//! (`main/nfpDb.ts`) and `nfpCacheKey`/`normalizeNfpRotation` (`main.js`,
//! kept manually in sync with a comment reminding whoever touches one to
//! update the other - this file is the whole reason that reminder can be
//! deleted instead of honored).
//!
//! **Caller convention to preserve** (not enforced by this function itself,
//! since it's a property of *how the inner-NFP cache is queried*, not of the
//! key format): `getInnerNfp` always hardcodes `Arotation: 0` when looking up
//! an inner-fit NFP (`background.js`: `window.db.find({A: A.source, B:
//! B.source, Arotation: 0, Brotation: B.rotation}, true)`), since the
//! container polygon conceptually doesn't rotate in that scenario - only `B`
//! does. Whatever calls this from the `nesting` cache layer (Phase 4/5) must
//! keep passing `0` for `a_rotation` on inner-NFP lookups, the same way both
//! original implementations' callers did.

/// Normalizes a rotation value to `[0, 360)`. Matches both original
/// implementations exactly: `parseInt(rotation) || 0` (fall back to 0 for
/// anything that doesn't parse as an integer, including NaN) then
/// `((n % 360) + 360) % 360` (handles negative values correctly, unlike a
/// plain `% 360`).
#[must_use]
pub fn normalize_rotation(rotation: f64) -> i64 {
    let n = if rotation.is_finite() { rotation.trunc() as i64 } else { 0 };
    ((n % 360) + 360) % 360
}

/// Which namespace a cache-key source id belongs to. The original encoded
/// this as a string prefix (`"p{id}"` / `"s{index}"`); here it is the high
/// bit of an integer, so a key can be compared and hashed without ever
/// touching the heap.
const SHEET_TAG: u64 = 1 << 63;

/// A part's shape identity or a sheet's index, in one integer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SourceId(u64);

impl SourceId {
    #[must_use]
    pub fn part(source_id: usize) -> Self {
        Self(source_id as u64 & !SHEET_TAG)
    }

    #[must_use]
    pub fn sheet(index: usize) -> Self {
        Self((index as u64 & !SHEET_TAG) | SHEET_TAG)
    }
}

/// Port of `NfpCache.makeKey` / `nfpCacheKey`: the single NFP cache-key both
/// call sites share. `a`/`b` are the part/sheet source identifiers
/// (`A.source`/`B.source` in the original); `a_flipped`/`b_flipped` are
/// always `false` here, and deliberately so. This app *does* have mirroring,
/// but a mirrored copy is registered as its own shape identity
/// (`dispatch::MIRROR_ID_BIT`, grouped by real geometry in
/// `commands::prepare_nest_inputs`), so it already gets its own cache
/// entries through `a`/`b` - which it must have, since the mirror of a shape
/// has a different NFP against everything. Do not "wire up" these flags to
/// fix mirroring; it is not broken, and doing so would split every entry in
/// two. They are kept as real parameters rather than dropped because they
/// are part of the key format's identity.
///
/// **A `Copy` struct, not the original's `format!`ed `String`.** The string
/// form was faithful to the original and cost three heap allocations per
/// lookup (`part_source`'s two, plus the key itself) - on the hat benchmark
/// that is ~11 million allocations to look up six distinct values, since
/// every lookup is a cache hit. Rotations are stored normalized, so
/// geometrically identical angles still collide into one entry exactly as
/// the string form did; `i16` because `normalize_rotation` returns `[0, 360)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NfpKey {
    a: SourceId,
    b: SourceId,
    a_rotation: i16,
    b_rotation: i16,
    a_flipped: bool,
    b_flipped: bool,
}

#[must_use]
pub fn nfp_cache_key(a: SourceId, b: SourceId, a_rotation: f64, b_rotation: f64, a_flipped: bool, b_flipped: bool) -> NfpKey {
    NfpKey {
        a,
        b,
        a_rotation: normalize_rotation(a_rotation) as i16,
        b_rotation: normalize_rotation(b_rotation) as i16,
        a_flipped,
        b_flipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_a_plain_angle() {
        assert_eq!(normalize_rotation(90.0), 90);
    }

    #[test]
    fn folds_a_full_rotation_cycle_back_to_zero() {
        // the ">= not >" quirk this guards against lives in background.js's
        // rotation-increment loop (Phase 3/4), not in this function itself -
        // this modulo formula already handles any integer input correctly,
        // boundary or not
        assert_eq!(normalize_rotation(360.0), 0);
        assert_eq!(normalize_rotation(720.0), 0);
    }

    #[test]
    fn normalizes_a_negative_rotation() {
        assert_eq!(normalize_rotation(-90.0), 270);
    }

    #[test]
    fn non_finite_rotation_falls_back_to_zero() {
        assert_eq!(normalize_rotation(f64::NAN), 0);
    }

    #[test]
    fn key_identity_survives_the_move_off_strings() {
        let key = nfp_cache_key(SourceId::part(1), SourceId::part(2), 90.0, 180.0, false, false);
        assert_eq!(key, nfp_cache_key(SourceId::part(1), SourceId::part(2), 90.0, 180.0, false, false));
        // a part and a sheet with the same numeric id are different keys
        assert_ne!(nfp_cache_key(SourceId::part(1), SourceId::part(2), 0.0, 0.0, false, false), nfp_cache_key(SourceId::sheet(1), SourceId::part(2), 0.0, 0.0, false, false));
    }

    #[test]
    fn geometrically_identical_angles_share_a_key() {
        let k1 = nfp_cache_key(SourceId::part(0), SourceId::part(1), 360.0, 0.0, false, false);
        let k2 = nfp_cache_key(SourceId::part(0), SourceId::part(1), 0.0, 0.0, false, false);
        assert_eq!(k1, k2);
    }

    #[test]
    fn flipped_flags_change_the_key() {
        let k1 = nfp_cache_key(SourceId::part(0), SourceId::part(1), 0.0, 0.0, false, false);
        let k2 = nfp_cache_key(SourceId::part(0), SourceId::part(1), 0.0, 0.0, true, false);
        assert_ne!(k1, k2);
    }
}
