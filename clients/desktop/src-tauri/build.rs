fn main() {
    // Link CoreGraphics on macOS for screen capture permission APIs
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-lib=framework=CoreGraphics");
        // Native menu-bar item: compiles swift/lookout-tray (SwiftUI with the
        // numericText digit-roll animation) and links it into the binary.
        swift_rs::SwiftLinker::new("10.15")
            .with_package("lookout-tray", "./swift/lookout-tray")
            .link();
        // The Swift runtime must resolve to the OS copy. With a deployment
        // target below 10.14.4 ld records @rpath/libswiftCore.dylib in the
        // x86_64 slice, and swift-rs adds no LC_RPATH — every Intel Mac then
        // dies at launch with "Library not loaded: @rpath/libswiftCore.dylib".
        // minimumSystemVersion in tauri.conf.json keeps the target at 10.15+;
        // this rpath is the safety net if that ever regresses.
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }

    tauri_build::build()
}
