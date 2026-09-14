//! Embed the application icon and version metadata into the Windows
//! executable. Failing to find the resource compiler must never break a
//! build, so errors are reported as warnings.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/icon.ico")
        .set("ProductName", "CrabBoy")
        .set(
            "FileDescription",
            "CrabBoy Game Boy / Game Boy Color / Game Boy Advance emulator",
        )
        .set("LegalCopyright", "AGPL-3.0-or-later");
    if let Err(e) = res.compile() {
        println!("cargo:warning=windows resources not embedded: {e}");
    }
}
