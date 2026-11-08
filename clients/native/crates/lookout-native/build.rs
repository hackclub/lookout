//! macOS: link CoreGraphics (permission APIs) and the SwiftUI menu-bar item
//! and add menu (`clients/native/swift/lookout-tray`, shared with the Tauri
//! app). `LOOKOUT_SKIP_SWIFT=1` skips the Swift build for cross type-checks
//! from a machine without the Swift toolchain.
//!
//! Windows: the exe's icon and version info. lookout.exe is also its own
//! installer, so this is the icon on the downloaded file, the Start menu
//! shortcut and "Installed apps" (`DisplayIcon` points at `lookout.exe,0`).

fn main() {
    println!("cargo:rerun-if-env-changed=LOOKOUT_SKIP_SWIFT");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        windows_resources();
        return;
    }
    if target_os != "macos" {
        return;
    }
    println!("cargo:rustc-link-lib=framework=CoreGraphics");
    println!("cargo:rustc-link-lib=framework=AVFoundation");
    println!("cargo:rustc-link-lib=framework=CoreMedia");
    println!("cargo:rustc-link-lib=framework=CoreVideo");
    println!("cargo:rustc-link-lib=framework=QuartzCore");
    if std::env::var("LOOKOUT_SKIP_SWIFT").is_ok() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        swift_rs::SwiftLinker::new("10.15").with_package("lookout-tray", "../../swift/lookout-tray").link();
        // Resolve the Swift runtime to the OS copy (see the Tauri shell's build.rs).
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}

fn windows_resources() {
    let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = dir.join("../../assets/lookout.ico").canonicalize().expect("assets/lookout.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let nums: Vec<u64> = version.split(['.', '-', '+']).take(3).map(|p| p.parse().unwrap_or(0)).collect();
    let (a, b, c) = (nums.first().copied().unwrap_or(0), nums.get(1).copied().unwrap_or(0), nums.get(2).copied().unwrap_or(0));
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Hack Club"
      VALUE "FileDescription", "Lookout"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "lookout"
      VALUE "LegalCopyright", "© Hack Club, a 501(c)(3) nonprofit (EIN: 81-2908499)"
      VALUE "OriginalFilename", "lookout.exe"
      VALUE "ProductName", "Lookout"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        // No `\\?\` verbatim prefix (canonicalize adds it on Windows): rc can't read it.
        icon = icon.display().to_string().trim_start_matches(r"\\?\").replace('\\', "\\\\"),
    );
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("lookout.rc");
    std::fs::write(&out, rc).unwrap();
    embed_resource::compile(&out, embed_resource::NONE).manifest_optional().unwrap();
}
