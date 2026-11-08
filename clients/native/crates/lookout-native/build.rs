//! macOS: link CoreGraphics (permission APIs) and the SwiftUI menu-bar item
//! and add menu (`clients/native/swift/lookout-tray`, shared with the Tauri
//! app). `LOOKOUT_SKIP_SWIFT=1` skips the Swift build for cross type-checks
//! from a machine without the Swift toolchain.

fn main() {
    println!("cargo:rerun-if-env-changed=LOOKOUT_SKIP_SWIFT");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        return;
    }
    println!("cargo:rustc-link-lib=framework=CoreGraphics");
    println!("cargo:rustc-link-lib=framework=AVFoundation");
    println!("cargo:rustc-link-lib=framework=CoreMedia");
    println!("cargo:rustc-link-lib=framework=CoreVideo");
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
