//! UI strings, in nine languages.
//!
//! The dictionaries are **not** in this file. Each language is one flat
//! JSON file in `app/assets/i18n/` - `en.json`, `fr.json`, `ja.json` and so
//! on - baked into the binary with `include_str!` at compile time. See
//! `app/assets/i18n/README.md`, which is written for the person doing the
//! translating rather than for a Rust programmer.
//!
//! **Why files rather than the `match` arm per key this used to be.** With
//! two languages a `("EN", "VI")` tuple per key was fine. With nine it is a
//! nine-column table nobody can read, in a file a non-programmer cannot
//! safely open, in a language they would have to compile to check. Moving
//! the strings out means a translator edits one self-contained JSON file
//! with their language and nothing else in it, and `cargo test` tells them
//! whether they broke it. Nothing about the *mechanism* changed: same flat
//! key -> string map, same `{placeholder}` substitution, same
//! fall-back-to-English rule.
//!
//! That fallback is what makes a half-finished translation safe to ship. A
//! missing key, and equally a key present but left as `""`, falls through to
//! English - so a contributor can send in fifty rows without having to
//! finish all 274, and the app shows English for the rest instead of blanks.
//!
//! ponytail: only the primary UI (labels, buttons, hints, tooltips, status
//! messages, dialogs) is translated - the CONSOLE window's own narration
//! (per-file import lines, per-generation/tick progress, run start/complete
//! events) stays English-only, exactly as in the `i18n.js` this began as.
//! That's debug telemetry, not something a non-English-reading operator
//! needs to act on. Extend the same way (add the key to every JSON file,
//! wrap the string in `t()`) if that ever needs to change.
//!
//! CJK note: Japanese, Korean and Chinese need glyphs none of this app's
//! own faces carry. `theme::install_fonts` finds a CJK face on the operating
//! system rather than embedding one - see its own comment for why, and for
//! what happens on a machine that has none.

use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Lang {
    #[default]
    En,
    Vi,
    Fr,
    Es,
    It,
    De,
    Ja,
    Ko,
    Zh,
}

impl Lang {
    /// Picker order: English first as the source language, then the rest by
    /// script - Latin, then CJK.
    pub const ALL: [Lang; 9] = [Lang::En, Lang::Vi, Lang::Fr, Lang::Es, Lang::It, Lang::De, Lang::Ja, Lang::Ko, Lang::Zh];

    /// What the language picker shows - each language's own endonym, not its
    /// English name, so a speaker can find their own row without having to
    /// read English first.
    ///
    /// Written with their real marks and scripts, not stripped to ASCII.
    /// These labels are the one place whose job is to prove the app can
    /// render the script it is offering; written flat they prove the
    /// opposite.
    pub fn label(self) -> &'static str {
        match self {
            Lang::En => "ENGLISH",
            Lang::Vi => "TIẾNG VIỆT",
            Lang::Fr => "FRANÇAIS",
            Lang::Es => "ESPAÑOL",
            Lang::It => "ITALIANO",
            Lang::De => "DEUTSCH",
            Lang::Ja => "日本語",
            Lang::Ko => "한국어",
            Lang::Zh => "中文",
        }
    }

    /// The `assets/i18n/<code>.json` this language reads. Only the tests
    /// need it - `SOURCES` pairs file to language positionally - but it is
    /// what makes their failure messages name the file to go and fix.
    #[cfg(test)]
    fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Vi => "vi",
            Lang::Fr => "fr",
            Lang::Es => "es",
            Lang::It => "it",
            Lang::De => "de",
            Lang::Ja => "ja",
            Lang::Ko => "ko",
            Lang::Zh => "zh",
        }
    }
}

/// The raw files, in `Lang::ALL` order. `include_str!` needs a literal path,
/// so this list is the one place a new language has to be named twice - once
/// here and once as a `Lang` variant.
const SOURCES: [&str; 9] = [
    include_str!("../../assets/i18n/en.json"),
    include_str!("../../assets/i18n/vi.json"),
    include_str!("../../assets/i18n/fr.json"),
    include_str!("../../assets/i18n/es.json"),
    include_str!("../../assets/i18n/it.json"),
    include_str!("../../assets/i18n/de.json"),
    include_str!("../../assets/i18n/ja.json"),
    include_str!("../../assets/i18n/ko.json"),
    include_str!("../../assets/i18n/zh.json"),
];

/// Parsed once, on the first string anything asks for.
///
/// A file that does not parse becomes an **empty** dictionary rather than a
/// panic: a contributor's stray comma should make their language render as
/// English, not stop the app from starting. `every_file_parses` below is
/// what makes sure that never reaches a release unnoticed.
fn dicts() -> &'static [HashMap<String, String>; 9] {
    static DICTS: OnceLock<[HashMap<String, String>; 9]> = OnceLock::new();
    DICTS.get_or_init(|| SOURCES.map(|raw| serde_json::from_str(raw).unwrap_or_default()))
}

/// A key's own entry in one language, if it has a non-empty one. Empty is
/// treated as absent so a translator can leave a row blank rather than
/// having to delete it.
fn entry(lang: Lang, key: &str) -> Option<&'static str> {
    dicts()[lang as usize].get(key).map(String::as_str).filter(|s| !s.is_empty())
}

/// Looks up `key`, falling back to English for anything the chosen language
/// is missing, and to the key itself if English is missing it too - so a
/// typo is loud in the UI instead of showing up as a blank label.
pub fn t(lang: Lang, key: &str) -> &str {
    entry(lang, key).or_else(|| entry(Lang::En, key)).unwrap_or(key)
}

/// `t` plus `{name}` substitution, mirroring `i18n.js`'s `t(key, vars)`.
/// Allocates, so it's used only where a value really is interpolated; plain
/// labels go through `t`.
pub fn tv(lang: Lang, key: &str, vars: &[(&str, &str)]) -> String {
    let mut s = t(lang, key).to_string();
    for (name, value) in vars {
        s = s.replace(&format!("{{{name}}}"), value);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// English is the source of truth: it is the only dictionary allowed to
    /// define a key, and the only one every lookup can fall back to.
    fn english() -> &'static HashMap<String, String> {
        &dicts()[Lang::En as usize]
    }

    /// The whole scheme rests on the files being readable. `dicts()`
    /// deliberately swallows a parse error into an empty map at runtime, so
    /// without this test a broken translation would ship as a silently
    /// all-English language.
    #[test]
    fn every_file_parses() {
        for lang in Lang::ALL {
            let parsed: Result<HashMap<String, String>, _> = serde_json::from_str(SOURCES[lang as usize]);
            assert!(parsed.is_ok(), "assets/i18n/{}.json is not valid JSON: {}", lang.code(), parsed.unwrap_err());
        }
    }

    /// A key whose text carries `{name}` must be rendered through `tv`, never
    /// `t` - `t` returns the template verbatim, so the user reads a literal
    /// `{n}` or `{err}`. Five keys had drifted onto plain `t` this way,
    /// including both failure messages, which meant a nest that failed showed
    /// "nest failed: {err}" and never the reason.
    ///
    /// Scans the UI source rather than the dictionaries, because the mistake
    /// is at the call site. `include_str!` so it runs against the tree being
    /// compiled, with no path or working-directory assumptions.
    #[test]
    fn no_placeholder_key_is_rendered_through_plain_t() {
        const UI_SOURCES: &[&str] = &[
            include_str!("mod.rs"),
            include_str!("import.rs"),
            include_str!("result.rs"),
            include_str!("shell.rs"),
            include_str!("shapes.rs"),
            include_str!("config.rs"),
            include_str!("library.rs"),
            include_str!("console.rs"),
            include_str!("prefs.rs"),
            include_str!("canvas.rs"),
            include_str!("state.rs"),
            include_str!("history_chart.rs"),
        ];

        let templated: Vec<&String> = english().iter().filter(|(_, v)| v.contains('{')).map(|(k, _)| k).collect();
        assert!(!templated.is_empty(), "the dictionary should have templated keys, or this test proves nothing");

        for key in templated {
            let plain = format!("t(\"{key}\")");
            for src in UI_SOURCES {
                assert!(
                    !src.contains(&plain),
                    "`{key}` interpolates {{...}} but is rendered with plain t() - use tv() and pass the value"
                );
            }
        }
    }

    /// The fall-back chain is what makes a partial dictionary safe. If an
    /// unknown key ever stops echoing itself, every typo renders as empty
    /// space instead of something a bug report can name.
    #[test]
    fn an_unknown_key_is_visible_rather_than_blank() {
        for lang in Lang::ALL {
            assert_eq!(t(lang, "definitely_not_a_key"), "definitely_not_a_key");
        }
    }

    #[test]
    fn english_is_complete() {
        assert!(english().len() > 250, "the English dictionary lost keys: {}", english().len());
        for (key, value) in english() {
            assert!(!value.is_empty(), "{key} is empty in English, which has nothing to fall back to");
        }
    }

    /// Catches the commonest contributor mistake by far: a mistyped or
    /// renamed key, which would otherwise sit in the file looking translated
    /// while the UI quietly shows English.
    #[test]
    fn no_translation_invents_a_key() {
        for lang in Lang::ALL {
            for key in dicts()[lang as usize].keys() {
                assert!(english().contains_key(key), "assets/i18n/{}.json has `{key}`, which is not a key in en.json", lang.code());
            }
        }
    }

    /// A `{placeholder}` is code, not prose: dropping or renaming one leaves
    /// a message with a hole in it where a part id or a filename should be.
    #[test]
    fn placeholders_survive_translation() {
        fn placeholders(s: &str) -> Vec<&str> {
            let mut found: Vec<&str> = s.match_indices('{').filter_map(|(i, _)| s[i..].find('}').map(|j| &s[i + 1..i + j])).collect();
            found.sort_unstable();
            found
        }
        for lang in Lang::ALL {
            for (key, translated) in &dicts()[lang as usize] {
                let Some(source) = english().get(key) else { continue };
                assert_eq!(placeholders(source), placeholders(translated), "assets/i18n/{}.json `{key}` does not use the same {{placeholders}} as en.json", lang.code());
            }
        }
    }

    #[test]
    fn placeholders_are_substituted() {
        assert_eq!(tv(Lang::En, "pin_locked", &[("id", "7")]), t(Lang::En, "pin_locked").replace("{id}", "7"));
        // A var with no matching placeholder is a no-op, not an error - same
        // as `i18n.js`'s `replaceAll` behaviour.
        assert_eq!(tv(Lang::En, "btn_run", &[("nope", "x")]), t(Lang::En, "btn_run"));
    }

    /// Not a completeness requirement - a partial translation is explicitly
    /// allowed - but the picker should not offer a language that is entirely
    /// English, which would just look broken.
    #[test]
    fn every_offered_language_is_actually_translated() {
        for lang in Lang::ALL.into_iter().filter(|l| *l != Lang::En) {
            let done = english().keys().filter(|k| entry(lang, k).is_some()).count();
            assert!(done * 2 > english().len(), "{} is only {done}/{} translated - too thin to offer in the picker", lang.code(), english().len());
        }
    }
}
