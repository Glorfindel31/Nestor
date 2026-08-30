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
