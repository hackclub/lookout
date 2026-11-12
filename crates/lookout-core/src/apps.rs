//! The app list behind the Filtered Apps page: installed applications
//! (scanned once per process) merged with the ones currently running, and
//! their icons.

use serde::Serialize;

use crate::sources::should_exclude_window;

#[cfg(target_os = "macos")]
use objc2_core_foundation::CGRect;

/// One entry in the app list shown on the Filtered Apps page.
#[derive(Clone, Serialize)]
pub struct AppEntry {
    pub name: String,
    /// Platform-specific icon lookup key, passed back to `get_app_icon`:
    /// macOS = .app bundle path, Windows = Start Menu .lnk path,
    /// Linux = the .desktop entry's Icon= value.
    pub path: Option<String>,
    /// Whether the app is currently running (used to sort open apps first).
    pub running: bool,
}

/// Read an app bundle's display name (CFBundleDisplayName, falling back to
/// CFBundleName). These are what `kCGWindowOwnerName` reports for the app's
/// windows, so blacklist entries created from this list match redaction.
#[cfg(target_os = "macos")]
fn bundle_display_name(path: &std::path::Path) -> Option<String> {
    use objc2_foundation::{NSBundle, NSString};

    let ns_path = NSString::from_str(path.to_str()?);
    let bundle = NSBundle::bundleWithPath(&ns_path)?;
    for key in ["CFBundleDisplayName", "CFBundleName"] {
        let key = NSString::from_str(key);
        if let Some(value) = bundle.objectForInfoDictionaryKey(&key) {
            if let Ok(s) = value.downcast::<NSString>() {
                let s = s.to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
    }
    None
}

/// Scan the standard application folders for installed .app bundles.
/// Slow-ish (reads each bundle's Info.plist), so callers cache the result.
#[cfg(target_os = "macos")]
fn scan_installed_apps() -> Vec<AppEntry> {
    let mut queue: Vec<(std::path::PathBuf, u8)> = vec![
        ("/Applications".into(), 0),
        ("/System/Applications".into(), 0),
    ];
    if let Ok(home) = std::env::var("HOME") {
        queue.push((std::path::Path::new(&home).join("Applications"), 0));
    }

    let mut apps = Vec::new();
    // Scan one folder level deep: /Applications/Utilities/X.app and vendor
    // folders like /Applications/Adobe .../X.app are common.
    while let Some((dir, depth)) = queue.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if file_name.starts_with('.') {
                continue;
            }
            if file_name.ends_with(".app") {
                let name = bundle_display_name(&path)
                    .unwrap_or_else(|| file_name.trim_end_matches(".app").to_string());
                if name.is_empty() || name == "Lookout" || should_exclude_window(&name, "") {
                    continue;
                }
                apps.push(AppEntry {
                    name,
                    path: Some(path.to_string_lossy().into_owned()),
                    running: false,
                });
            } else if depth < 1 && path.is_dir() {
                queue.push((path, depth + 1));
            }
        }
    }
    apps
}

/// Scan Start Menu shortcuts — the canonical "installed apps" on Windows.
#[cfg(target_os = "windows")]
fn scan_installed_apps() -> Vec<AppEntry> {
    let mut queue: Vec<(std::path::PathBuf, u8)> = Vec::new();
    if let Ok(program_data) = std::env::var("ProgramData") {
        queue.push((
            std::path::Path::new(&program_data).join(r"Microsoft\Windows\Start Menu\Programs"),
            0,
        ));
    }
    if let Ok(app_data) = std::env::var("APPDATA") {
        queue.push((
            std::path::Path::new(&app_data).join(r"Microsoft\Windows\Start Menu\Programs"),
            0,
        ));
    }

    let mut apps = Vec::new();
    while let Some((dir, depth)) = queue.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // `entry.file_type()` reads the attributes the directory
            // enumeration already returned; `entry.path().is_dir()` would stat
            // each entry again. On Windows that is a real syscall per shortcut,
            // through whatever filter drivers and AV hooks are installed, and
            // the Start Menu tree has hundreds of entries.
            // ...but file_type() does NOT follow symlinks where is_dir() did,
            // so a directory junction in the Start Menu would stop being
            // traversed. Fall back to the stat only for that rare case.
            let path = entry.path();
            let is_dir = match entry.file_type() {
                Ok(t) if t.is_symlink() => path.is_dir(),
                Ok(t) => t.is_dir(),
                Err(_) => path.is_dir(),
            };
            if is_dir {
                if depth < 3 {
                    queue.push((path, depth + 1));
                }
                continue;
            }
            let is_lnk = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
            if !is_lnk {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|n| n.to_str()) else {
                continue;
            };
            let name = name.to_string();
            let lower = name.to_lowercase();
            if name.is_empty()
                || name == "Lookout"
                || should_exclude_window(&name, "")
                || lower.starts_with("uninstall")
                || lower.contains("uninstaller")
            {
                continue;
            }
            apps.push(AppEntry {
                name,
                path: Some(path.to_string_lossy().into_owned()),
                running: false,
            });
        }
    }
    apps
}

/// Scan .desktop entries — the canonical "installed apps" on Linux.
#[cfg(target_os = "linux")]
fn scan_installed_apps() -> Vec<AppEntry> {
    let mut dirs: Vec<std::path::PathBuf> = vec![
        "/usr/share/applications".into(),
        "/usr/local/share/applications".into(),
        "/var/lib/flatpak/exports/share/applications".into(),
    ];
    if let Ok(home) = std::env::var("HOME") {
        let home = std::path::Path::new(&home);
        dirs.push(home.join(".local/share/applications"));
        dirs.push(home.join(".local/share/flatpak/exports/share/applications"));
    }

    let mut apps = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some((name, icon)) = parse_desktop_entry(&content) else {
                continue;
            };
            if name == "Lookout" || should_exclude_window(&name, "") {
                continue;
            }
            apps.push(AppEntry {
                name,
                path: icon,
                running: false,
            });
        }
    }
    apps
}

/// Parse the fields we need from a .desktop file's [Desktop Entry] section.
/// Returns (name, icon) or None if the entry isn't a visible application.
#[cfg(target_os = "linux")]
fn parse_desktop_entry(content: &str) -> Option<(String, Option<String>)> {
    let mut in_section = false;
    let mut name = None;
    let mut icon = None;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            if in_section {
                break; // end of [Desktop Entry]
            }
            in_section = line == "[Desktop Entry]";
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(value) = line.strip_prefix("NoDisplay=") {
            if value.trim() == "true" {
                return None;
            }
        } else if let Some(value) = line.strip_prefix("Type=") {
            if value.trim() != "Application" {
                return None;
            }
        } else if let Some(value) = line.strip_prefix("Name=") {
            name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Icon=") {
            icon = Some(value.trim().to_string());
        }
    }
    Some((name.filter(|n| !n.is_empty())?, icon))
}

fn installed_apps_cached() -> &'static [AppEntry] {
    static CACHE: std::sync::OnceLock<Vec<AppEntry>> = std::sync::OnceLock::new();
    CACHE.get_or_init(scan_installed_apps)
}

/// (name, icon-lookup key) pairs for currently running apps. Names come from
/// the same source redaction matches against (kCGWindowOwnerName on macOS,
/// xcap `app_name` elsewhere), so a running app always blacklists correctly
/// even when its installed entry is named differently.
fn running_apps() -> Vec<(String, Option<String>)> {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};

        let workspace = NSWorkspace::sharedWorkspace();
        workspace
            .runningApplications()
            .iter()
            .filter(|app| app.activationPolicy() == NSApplicationActivationPolicy::Regular)
            .filter_map(|app| {
                let name = app.localizedName()?.to_string();
                let path = app
                    .bundleURL()
                    .and_then(|url| url.path())
                    .map(|p| p.to_string());
                Some((name, path))
            })
            .collect()
    }

    #[cfg(not(target_os = "macos"))]
    {
        use xcap::Window;
        let mut names = std::collections::BTreeSet::new();
        if let Ok(windows) = Window::all() {
            for w in windows {
                if let Ok(name) = w.app_name() {
                    if !name.is_empty() && !should_exclude_window(&name, &w.title().unwrap_or_default()) {
                        names.insert(name);
                    }
                }
            }
        }
        names.into_iter().map(|name| (name, None)).collect()
    }
}

/// Pre-warm the installed-app cache so the first visit to Filtered Apps doesn't
/// pay for the app scan while the user waits.
///
/// DEFERRED on purpose. The scan is disk-bound — a Start Menu tree walk on
/// Windows, /Applications on macOS, .desktop files on Linux — and launch is
/// already the most I/O-contended moment in the process's life: the webview is
/// loading its own assets at the same time. Starting the scan immediately would
/// trade a faster Settings page for a slower app open, which is the wrong way
/// round. A few seconds' delay is still far earlier than anyone navigates to
/// Filtered Apps, and by then the launch I/O has settled.
pub fn prewarm_installed_apps() {
    std::thread::Builder::new()
        .name("app-scan-prewarm".into())
        .spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(5));
            let _ = installed_apps_cached();
        })
        // A failed prewarm is not worth failing startup over: the cache just
        // fills lazily on first use, exactly as it did before.
        .ok();
}

/// List apps for the Filtered Apps page, sorted by name: every installed app
/// (scanned once per process and cached) merged with currently running apps.
/// Only real applications appear — helper/XPC processes that merely own
/// windows (e.g. "CursorUIViewService") don't.
///
/// The work is BLOCKING — a Start Menu tree walk on the first call, and a
/// window enumeration on every call — so it runs on the blocking pool rather
/// than on the async runtime. `async fn` alone was not enough: the body never
/// yields, so it occupied a tokio worker for its whole duration, and the
/// capture loop lives on those same workers. A slow enumeration could
/// therefore delay a capture tick, not just the Settings page.
/// Run it on a blocking pool, never on an async worker.
pub fn list_installed_apps_blocking() -> Vec<AppEntry> {
    // name -> (path, running); BTreeMap keeps the result sorted by name.
    let mut apps: std::collections::BTreeMap<String, (Option<String>, bool)> =
        installed_apps_cached()
            .iter()
            .map(|a| (a.name.clone(), (a.path.clone(), false)))
            .collect();

    for (name, path) in running_apps() {
        if name.is_empty() || name == "Lookout" || should_exclude_window(&name, "") {
            continue;
        }
        match apps.entry(name) {
            std::collections::btree_map::Entry::Occupied(mut e) => {
                let (existing_path, running) = e.get_mut();
                if existing_path.is_none() {
                    *existing_path = path;
                }
                *running = true;
            }
            std::collections::btree_map::Entry::Vacant(e) => {
                e.insert((path, true));
            }
        }
    }

    apps.into_iter()
        .map(|(name, (path, running))| AppEntry {
            name,
            path,
            running,
        })
        .collect()
}

/// Return a small PNG (base64) of an app's icon. `path` is the icon lookup
/// key from `AppEntry.path`. Cached per key. Blocking — icons rasterize on
/// a miss — so keep it off the UI thread (a sync command froze the UI while
/// icons rasterized).
pub fn app_icon(path: String) -> Option<String> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Option<String>>>,
    > = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&path)
    {
        return hit.clone();
    }

    let result = compute_app_icon(&path);
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path, result.clone());
    result
}

#[cfg(target_os = "macos")]
fn compute_app_icon(path: &str) -> Option<String> {
    use base64::Engine as _;
    use objc2::AnyThread as _;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSWorkspace};
    use objc2_core_foundation::{CGPoint, CGSize};
    use objc2_foundation::{NSDictionary, NSString};

    let icon = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(path));
    // Ask for a small rect so IconServices hands back the small icon
    // representation instead of rasterizing the full 1024px artwork.
    let mut rect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: 32.0,
            height: 32.0,
        },
    };
    unsafe { icon.CGImageForProposedRect_context_hints(&mut rect, None, None) }
        .and_then(|cg| {
            let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &cg);
            unsafe {
                rep.representationUsingType_properties(
                    NSBitmapImageFileType::PNG,
                    &NSDictionary::new(),
                )
            }
        })
        .map(|png| base64::engine::general_purpose::STANDARD.encode(png.to_vec()))
}

/// Windows: shell icon for the Start Menu .lnk (resolves to the target
/// exe's icon), converted HICON -> RGBA -> PNG.
#[cfg(target_os = "windows")]
fn compute_app_icon(path: &str) -> Option<String> {
    use base64::Engine as _;
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{
        DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};

    // SHGetFileInfoW needs COM for .lnk resolution; commands run on worker
    // threads, so initialize per call (no-op if already initialized).
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut info = SHFILEINFOW::default();
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if ok == 0 || info.hIcon.is_invalid() {
        return None;
    }

    let png = (|| {
        let mut icon_info = ICONINFO::default();
        unsafe { GetIconInfo(info.hIcon, &mut icon_info) }.ok()?;

        let result = (|| {
            let mut bmp = BITMAP::default();
            let got = unsafe {
                GetObjectW(
                    icon_info.hbmColor.into(),
                    std::mem::size_of::<BITMAP>() as i32,
                    Some(&mut bmp as *mut _ as *mut _),
                )
            };
            if got == 0 || bmp.bmWidth <= 0 || bmp.bmHeight <= 0 {
                return None;
            }
            let (w, h) = (bmp.bmWidth, bmp.bmHeight);

            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = w;
            bmi.bmiHeader.biHeight = -h; // negative = top-down rows
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB.0;

            let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
            let hdc = unsafe { GetDC(None) };
            let lines = unsafe {
                GetDIBits(
                    hdc,
                    icon_info.hbmColor,
                    0,
                    h as u32,
                    Some(buf.as_mut_ptr() as *mut _),
                    &mut bmi,
                    DIB_RGB_COLORS,
                )
            };
            unsafe { ReleaseDC(None, hdc) };
            if lines == 0 {
                return None;
            }

            // BGRA -> RGBA; some icons come back with an empty alpha
            // channel, which would render as fully transparent.
            for px in buf.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
            if buf.chunks_exact(4).all(|px| px[3] == 0) {
                for px in buf.chunks_exact_mut(4) {
                    px[3] = 255;
                }
            }

            let img = image::RgbaImage::from_raw(w as u32, h as u32, buf)?;
            let mut out = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(img)
                .write_to(&mut out, image::ImageFormat::Png)
                .ok()?;
            Some(out.into_inner())
        })();

        unsafe {
            let _ = DeleteObject(icon_info.hbmColor.into());
            let _ = DeleteObject(icon_info.hbmMask.into());
        }
        result
    })();

    unsafe {
        let _ = DestroyIcon(info.hIcon);
    }
    png.map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Linux: resolve the .desktop Icon= value against the hicolor theme and
/// pixmaps dirs (PNG only) and return the file as-is.
#[cfg(target_os = "linux")]
fn compute_app_icon(icon: &str) -> Option<String> {
    use base64::Engine as _;

    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if icon.starts_with('/') {
        candidates.push(icon.into());
    } else {
        let mut base_dirs: Vec<String> = vec![
            "/usr/share".into(),
            "/usr/local/share".into(),
            "/var/lib/flatpak/exports/share".into(),
        ];
        if let Ok(home) = std::env::var("HOME") {
            base_dirs.push(format!("{home}/.local/share"));
            base_dirs.push(format!("{home}/.local/share/flatpak/exports/share"));
        }
        for base in &base_dirs {
            for size in ["48x48", "64x64", "32x32", "128x128", "256x256"] {
                candidates.push(format!("{base}/icons/hicolor/{size}/apps/{icon}.png").into());
            }
            candidates.push(format!("{base}/pixmaps/{icon}.png").into());
        }
    }

    for path in candidates {
        let is_png = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("png"));
        if !is_png {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&path) {
            return Some(base64::engine::general_purpose::STANDARD.encode(bytes));
        }
    }
    None
}
