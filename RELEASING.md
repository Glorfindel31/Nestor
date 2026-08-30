# Releasing

Everything to check before pushing a `v*` tag. `.github/workflows/release.yml`
does the rest.

The list exists because most of what breaks here is invisible to `cargo build`
and `cargo test`: an unsigned binary getting quarantined, a nest that got
slower, a config panel that only misdraws at Large text scale. Work down it in
order — the cheap checks first, the ones that need a running window last.

---

## 1. Version

`Cargo.toml`'s `[workspace.package] version` is the **only** literal. The UI
reads `env!("CARGO_PKG_VERSION")` and `winresource` stamps the exe from the
same place.

- [ ] Bump `[workspace.package] version` in the root `Cargo.toml`.
- [ ] `cargo check` once so `Cargo.lock` picks the new version up.
- [ ] `git diff Cargo.lock` shows the three workspace crates moving, nothing else.

> This has been shipped wrong once: `v2.4.0: audit, shape library, remnants`
> never touched `Cargo.toml`, so 36 commits went out under `2.3.1`.

## 2. Tests

- [ ] `cargo test --workspace` — all green.
- [ ] `cargo test -p rustynesting` — the `commands.rs` suite is the engine
      regression net. **If a UI-layer change broke one of these, something
      touched the engine that shouldn't have.** Note the count; it should never
      go down.

## 3. Nest quality

The GUI cannot tell you this and the unit tests do not measure it.

- [ ] `sh bench.sh` — compare every row against the last release. A row getting
      worse is a release blocker, not a curiosity.
- [ ] Drive the headless CLI **at margin 0 and spacing 0**:

      cargo run --release --bin nest -- "tests/fixtures/two.dxf" --qty 50 \
          --spacing 0 --margin 0 --generations 3 --rotations 4 --json

      Two bugs have shipped hiding at zero clearance because nothing ever
      audited there. `audit` must report no fatal issues.
- [ ] Fixture changes, additions and deletions alike, are committed with the
      code that needs them.

## 4. The exe itself

Run against `target/release/rustynesting.exe` after a `cargo build --release`.

- [ ] **VERSIONINFO is populated.** Empty publisher fields are a weighted input
      to Defender's ML classifier (see §5).

      powershell -NoProfile -Command "(Get-Item 'target/release/rustynesting.exe').VersionInfo |
          Format-List CompanyName,FileDescription,ProductName,LegalCopyright,OriginalFilename,InternalName,FileVersion"

      All seven must be non-empty, and `FileVersion` must match §1. They are
      set in `app/build.rs`.
- [ ] **No console window.** Read the PE header's subsystem field: `2` is GUI,
      `3` is console. A missing `windows_subsystem` attribute in
      `app/src/main.rs:1` has regressed this before.
- [ ] **Nothing new spawns a process.** `grep -rn "Command::new" app/src crates/*/src`
      should still only match `benchmark_log.rs` (a dev-only `git` call) and
      `nest.rs`'s `ExitCode`. A GUI app shelling out is one of the strongest
      dropper signals there is, and it is how a "quick fix" like calling
      PowerShell to play a sound would get the whole build quarantined.
- [ ] **No new outbound network call.** `app/src/update.rs`'s one GET to
      `api.github.com` is the only one. Anything else needs a deliberate
      decision, because launch-time network traffic is the other strong signal.

## 5. Antivirus false positives

Nestor is unsigned, so it starts every release at zero reputation. What you are
managing here is the *rate* of false positives, not their possibility.

**Why it gets flagged.** `Trojan:Win32/Wacatac.B!ml` is Defender's generic
machine-learning bucket — the `!ml` suffix means heuristic, not a signature
match. The classifier weighs: no Authenticode signature; low prevalence (a
brand-new hash nobody has run); missing publisher metadata; a large, high-
entropy, statically linked binary that looks packed (`lto = "fat"` and
`codegen-units = 1` guarantee this); GUI subsystem with no console; a launch-
time outbound HTTPS request; writes to `%APPDATA%` and `%LOCALAPPDATA%`; and
Mark-of-the-Web from a GitHub download. Each is innocuous. The combination is
the dropper template, and Rust release binaries hit most of it by construction.

- [ ] **Asset name stays stable.** `Nestor_x64.exe`, no version in it.
      Reputation keys on hash *and* filename; a new name every tag resets
      prevalence to zero permanently. Do not "improve" this back.
- [ ] **Scan before announcing.** Upload the built exe to
      <https://www.virustotal.com/> and record the score in the release notes
      thread. Under ~5/70, all generic/ML names: normal for an unsigned Rust
      binary. A *specific* family name (not `!ml`, not `Generic`, not
      `Unsigned`) means stop and investigate — that is not a false positive
      pattern.
- [ ] **Submit the false positive to Microsoft**, every release, as soon as the
      asset is uploaded: <https://www.microsoft.com/en-us/wdsi/filesubmission>
      → "Software developer". Free, no account, 1–3 day turnaround, and the
      cloud exclusion clears it for **every** user rather than just the one who
      complained. This is the single highest-value action on this page.
- [ ] **Record the SHA-256** of each published asset in the release notes, so a
      user who gets a warning can verify they have the file you built:

      certutil -hashfile Nestor_x64.exe SHA256

- [ ] If a user reports a detection, ask for the **exact detection name and the
      product**. `Wacatac.B!ml` from Defender is the known false positive.
      Anything else is a new question.

**The real fix, when it is worth paying for.** Code signing is the only thing
that ends this rather than managing it:

| Option | Cost | Effect |
| --- | --- | --- |
| Azure Trusted Signing | ~$10/month | Cheapest legitimate route. Needs a registered business 3+ years old. |
| OV certificate | ~$200–400/yr | Removes "unknown publisher"; SmartScreen reputation still has to accumulate. |
| EV certificate | ~$300–600/yr | Immediate SmartScreen trust. Hardware token. |

Once signed, set `CompanyName` and `LegalCopyright` in `app/build.rs` to the
**legal entity on the certificate** — a mismatch between the signature and the
metadata is worse than today's blank-but-honest state.

## 6. The running window

`cargo test` cannot see any of this, and it is **the user's pass, not the
assistant's** - driving this window programmatically takes over the machine's
real mouse, keyboard and focus. Hand over the list below rather than running it.

- [ ] Launch the release build. Import
      `tests/fixtures/two.dxf` (4 parts, 12 drill holes on a `VISIBLE` layer —
      exercises hole geometry and layer identity, both of which have been
      silently dropped before), run a nest, export a DXF and a PDF report.
- [ ] Open CONFIGURE and ADVANCED SETTINGS. The four scaling rows (rotations,
      population, mutation, generations) should line up in three columns:
      value, `+` growth box, on/off switch. No other row has them.
- [ ] **Check at Large text scale too**, in SETTINGS. `shell::LABEL_W` is a
      fixed width; a longer translation or a bigger font overflows it and
      pushes a row's controls out of column. Cosmetic, but this is the panel
      the operator uses all day.
- [ ] The finish chime plays once when a nest completes, and stays silent when
      you cancel one.
- [ ] The PDF report's Part-List, Sheet-List and Remnant Info all stay above
      the page footer on a job with **30+ distinct parts** - not a two-part
      fixture. Each continuation page must repeat its column headers, and the
      totals row must appear once, at the end. `pdf_export`'s own tests assert
      the coordinates; this is the eyeball pass on the same thing.

## 7. Translations

Missing keys fall back to English, so a partial dictionary is safe to ship.

- [ ] Any new `t()` / `tv()` key exists in `app/assets/i18n/en.json` — English
      is the only dictionary allowed to define a key, and the only fallback.
- [ ] `cargo test -p rustynesting i18n` passes.

## 8. Tag

- [ ] Commit, then push the tag:

      git tag v<version> && git push origin v<version>

- [ ] The workflow creates the release as a **draft**. Check all three assets
      uploaded, fill in the notes (with the SHA-256s from §5), then publish.
- [ ] mac and linux are `continue-on-error` and have never been verified on
      real hardware. Do not let a failure there hold up the Windows release,
      and do not claim in the notes that they work.
