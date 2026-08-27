# Translating Nestor

You do not need to be a programmer to do this, and you do not need to
install anything to try it. Everything the app says is in this folder, one
file per language.

| File | Language |
| --- | --- |
| `en.json` | English — **the source. Do not translate this one.** |
| `vi.json` | Tiếng Việt |
| `fr.json` | Français |
| `es.json` | Español |
| `it.json` | Italiano |
| `de.json` | Deutsch |
| `ja.json` | 日本語 |
| `ko.json` | 한국어 |
| `zh.json` | 中文 |

## What a file looks like

```json
{
  "btn_export": "EXPORT",
  "sheet_caption": "SHEET {n} — {parts} part(s), {util}% used"
}
```

Each line is a pair. The part on the **left** is the key — it is the app's
internal name for that string. **Never change the left side.** The part on
the **right** is what a person reads on screen. That is the only thing you
change.

## The five rules

1. **Only edit the right-hand side of the colon.** If the left side changes,
   the app stops finding that string and shows English instead.
2. **Keep everything in `{curly braces}` exactly as it is** — `{n}`,
   `{util}`, `{file}`. The app replaces them with real numbers and filenames
   while it runs. You may *move* them to wherever the sentence needs them,
   but do not rename, translate, or delete them.
3. **Leave the quotation marks, the colon, and the commas alone.** The safest
   way to work is to change only the words between the last pair of `"` on a
   line.
4. **A line you have not translated yet can be left as `""` or deleted.**
   Anything missing falls back to English, so the app never shows a blank
   label. You do not have to finish the whole file in one sitting.
5. **If a language needs a `"` inside the text**, write it as `\"`. Curly
   quotes (`“ ”`, `« »`, `「 」`) need no escaping and usually read better
   anyway.

## Style notes for this app

- The people using Nestor work in a **workshop**: sheet metal, wood, laser
  and plasma cutting. Prefer the word a cutter would say over the word a
  translation dictionary offers. "Sheet" is the stock material, not a piece
  of paper.
- Button labels and column headings are **UPPERCASE in English**. Keep that
  only where it is natural in your language — scripts without capital
  letters obviously just use their normal form.
- Tooltips (the long ones) are full sentences and are meant to explain
  something to someone who has never nested before. Translate the meaning,
  not the words.
- Words that appear on a button are often referred to by name inside a
  tooltip ("press SAVE TO LIBRARY"). If you rename the button, rename it in
  the tooltips too, or the instructions stop matching the screen.
- `NESTOR` is the product name. Leave it as it is.

## Checking your work

If you have the code and Rust installed:

```
cargo test -p rustynesting i18n
```

That checks every file parses, that no file has invented a key that does not
exist in `en.json`, and that every `{placeholder}` survived translation. If
it passes, your file is safe to ship.

If you do not have Rust, paste the file into any online JSON validator — a
missing comma or quote is by far the most common mistake, and that will
catch it.

## Adding a language that is not listed

That part needs a programmer — three lines in `app/src/ui/i18n.rs` (a `Lang`
variant, its name in the picker, and the file in `SOURCES`). Copy `en.json`
to `xx.json`, translate it, and open a pull request; wiring it up is a
two-minute job for whoever merges it.

Languages whose script is not Latin, Vietnamese, or CJK may also need a font
that carries their glyphs — see `theme::install_fonts`.
