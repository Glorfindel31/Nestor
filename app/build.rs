// Stamps the Windows exe with an icon and its VERSIONINFO block.
//
// The icon is cosmetic; the version fields are not. Defender's ML classifier
// (`Trojan:Win32/Wacatac.B!ml` and friends) weighs missing publisher metadata
// as a feature, because packers and droppers leave it blank and real software
// does not. `winresource` fills in FileVersion/ProductVersion/ProductName from
// `CARGO_PKG_*` on its own and leaves CompanyName, LegalCopyright,
// OriginalFilename and InternalName empty, so those are set here by hand.
//
// `OriginalFilename` is the *binary's* name, not the release asset's - see
// `.github/workflows/release.yml`, which ships it as `Nestor_x64.exe`. Naming
// the asset here instead would make the field a lie, which is a worse signal
// than the mismatch. The real fix for AV false positives is an Authenticode
// signature; this only removes the free strikes. See `RELEASING.md`.
//
// `winresource` is a no-op on non-Windows hosts, so the unconditional call is
// fine.
fn main() {
    // Without these the stamped VERSIONINFO goes stale: Cargo caches a build
    // script's output and re-runs it only when something it was told to watch
    // changes, and the crate version is not something it watches by default.
    // A `2.7.0` build shipped an exe reading `FileVersion 2.6.8` before this
    // line existed - the same class of miss as the release that went out
    // under the previous version number (see `RELEASING.md` §1).
    //
    // Declaring any `rerun-if-*` replaces the "re-run when any file in the
    // package changed" default, so the icon and this file have to be named
    // explicitly too.
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=icons/icon.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("icons/icon.ico")
            .set("ProductName", "Nestor")
            .set("FileDescription", "Nestor - DXF and SVG nesting for sheet material")
            .set("CompanyName", "Glorfindel31")
            .set("LegalCopyright", "Copyright (C) 2026 Glorfindel31")
            .set("InternalName", "rustynesting")
            .set("OriginalFilename", "rustynesting.exe")
            .compile()
            .unwrap();
    }
}
