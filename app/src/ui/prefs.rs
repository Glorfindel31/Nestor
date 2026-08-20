//! Display preferences: language, text scale, and whether the help overlay
//! has been dismissed.
//!
//! The accent colour used to live here too, as a `#rrggbb` string with five
//! quick-pick swatches and a free hex field behind it. It is now a constant
//! (`theme::ACCENT`) - see that module's own doc comment for why the app
//! having one colour of its own beat letting every install pick a different
//! one.
//!
//! The web UI kept these in four separate `localStorage` keys
//! (`rustynesting-lang` / `-accent` / `-scale` / `-help-dismissed`). Here
//! they're one struct behind one `eframe::Storage` key, which is the same
//! mechanism (a small JSON blob next to the app's own data) with fewer
//! moving parts - eframe already persists it on exit and reloads it on
//! start, so there's no explicit save call to forget.
//!
//! Deliberately *not* stored here: anything about a nest. The nest config
//! lives in `config.json` (`commands::save_config`) because it's a job
//! parameter the user reasons about, not a display preference, and the
//! best-result recovery file is separate again.

#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Prefs {
    pub lang: super::i18n::Lang,
    pub scale: Scale,
    pub help_dismissed: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { lang: Default::default(), scale: Scale::Normal, help_dismissed: false }
    }
}

/// Three steps, not a free slider - matching the small/normal/large labels
/// the UI offers. Multiplies the font size of every text style
/// (`theme::apply`), which is what the web version's root font-size did.
///
/// Deliberately *not* `ctx.set_zoom_factor`: that scales stroke widths and
/// spacing along with the text, so the 2px chiselled bevel this look is
/// built on thickens and the design reads as a different design rather
/// than the same one larger.
///
/// The original 0.875/1.0/1.15 ladder was calibrated on a laptop panel and
/// is unreadable on a large high-resolution monitor; `Small` is now what
/// `Normal` used to be, so the old size is still one click away.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scale {
    Small,
    Normal,
    Large,
}

impl Scale {
    pub const ALL: [Scale; 3] = [Scale::Small, Scale::Normal, Scale::Large];

    pub fn factor(self) -> f32 {
        match self {
            Scale::Small => 1.0,
            Scale::Normal => 1.25,
            Scale::Large => 1.5,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Scale::Small => "scale_small",
            Scale::Normal => "scale_normal",
            Scale::Large => "scale_large",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefs_file_from_before_the_fixed_accent_still_loads() {
        // The stored blob still carries `accent` for anyone who ran an older
        // build; `#[serde(default)]` plus an unknown field being ignored is
        // what keeps that from resetting language and scale to defaults.
        let json = r##"{"lang":"Vi","accent":"#e8db1f","scale":"Large","help_dismissed":true}"##;
        let p: Prefs = serde_json::from_str(json).expect("old prefs should still parse");
        assert_eq!(p.scale, Scale::Large);
        assert!(p.help_dismissed);
    }
}
