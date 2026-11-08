//! macOS's own icons for the setup page's permission rows (what Codex's
//! onboarding shows): the Screenshot app for Screen Recording, Photo Booth or
//! FaceTime for Camera, the Accessibility settings icon for Accessibility.
//! PNG bytes, so the page can draw them like any other image; `None` where
//! none of them exist (other platforms, or a future macOS that moved them),
//! and the page falls back to its own glyphs.

use super::permissions::Kind;

/// The icon for `kind`, `px` pixels square, as PNG.
pub fn png(kind: Kind, px: u32) -> Option<Vec<u8>> {
    imp::png(kind, px)
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;
    pub fn png(_kind: Kind, _px: u32) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use objc2::AnyThread;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSWorkspace};
    use objc2_foundation::{NSDictionary, NSFileManager, NSPoint, NSRect, NSSize, NSString};

    fn candidates(kind: Kind) -> &'static [&'static str] {
        match kind {
            Kind::Screen => &["/System/Applications/Utilities/Screenshot.app"],
            Kind::Camera => &["/System/Applications/Photo Booth.app", "/System/Applications/FaceTime.app"],
            Kind::Accessibility => &[
                "/System/Library/ExtensionKit/Extensions/AccessibilitySettingsExtension.appex",
                "/System/Library/PreferencePanes/UniversalAccessPref.prefPane",
            ],
        }
    }

    pub fn png(kind: Kind, px: u32) -> Option<Vec<u8>> {
        let fm = NSFileManager::defaultManager();
        let path = candidates(kind).iter().map(|p| NSString::from_str(p)).find(|p| fm.fileExistsAtPath(p))?;
        let icon = NSWorkspace::sharedWorkspace().iconForFile(&path);
        let side = px as f64;
        icon.setSize(NSSize::new(side, side));
        let mut rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(side, side));
        let cg = unsafe { icon.CGImageForProposedRect_context_hints(&mut rect, None, None) }?;
        let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &cg);
        let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }?;
        Some(data.to_vec())
    }
}
