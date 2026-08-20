//! Display preferences: language, accent colour, text scale, and whether the
//! help overlay has been dismissed.
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

use egui::Color32;

use super::theme;

#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Prefs {
    pub lang: super::i18n::Lang,
    /// Stored as `#rrggbb` rather than a `Color32` so a hand-edited prefs
    /// file stays readable, and so the hex field in the settings menu has a
    /// canonical form to round-trip through.
    pub accent: String,
    pub scale: Scale,
    pub help_dismissed: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { lang: Default::default(), accent: to_hex(theme::ACCENT), scale: Scale::Normal, help_dismissed: false }
    }
}

impl Prefs {
    pub fn accent_color(&self) -> Color32 {
        parse_hex(&self.accent).unwrap_or(theme::ACCENT)
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

pub fn to_hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// Accepts `#rgb` and `#rrggbb`, matching the web UI's `HEX_RE` exactly -
/// anything else returns `None` so a half-typed value in the hex field is
/// ignored rather than snapping the accent to black on every keystroke.
pub fn parse_hex(s: &str) -> Option<Color32> {
    let h = s.strip_prefix('#')?;
    let expand = |c: u8| c * 17; // #abc -> #aabbcc
    match h.len() {
        3 => {
            let v: Vec<u8> = h.chars().map(|c| c.to_digit(16).map(|d| expand(d as u8))).collect::<Option<_>>()?;
            Some(Color32::from_rgb(v[0], v[1], v[2]))
        }
        6 => {
            let v: Vec<u8> = (0..3).map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).ok()).collect::<Option<_>>()?;
            Some(Color32::from_rgb(v[0], v[1], v[2]))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_rejects_partial_input() {
        for c in theme::ACCENTS {
            assert_eq!(parse_hex(&to_hex(c)), Some(c));
        }
        assert_eq!(parse_hex("#abc"), Some(Color32::from_rgb(0xaa, 0xbb, 0xcc)));
        // Everything a user types on the way to a valid colour must be
        // rejected, not partially applied.
        for bad in ["", "#", "#a", "#ab", "#abcd", "#abcde", "#abcdefg", "abcdef", "#gggggg"] {
            assert_eq!(parse_hex(bad), None, "{bad} should not parse");
        }
    }

    #[test]
    fn a_broken_stored_accent_falls_back_instead_of_going_black() {
        let p = Prefs { accent: "not a colour".into(), ..Default::default() };
        assert_eq!(p.accent_color(), theme::ACCENT);
    }
}
