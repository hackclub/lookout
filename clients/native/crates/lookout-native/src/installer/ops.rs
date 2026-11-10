//! What installing and uninstalling actually do. On Windows `lookout.exe`
//! does it itself, with the same per-user folder, registry keys and Start
//! menu shortcut as the Tauri app's NSIS installer, so it upgrades Tauri
//! installs in place.
//!
//! Elsewhere (and in debug `LOOKOUT_INSTALLER=demo` runs) [`Ops::demo`]
//! walks through the same steps with pauses and touches nothing, so the
//! window can be designed and tested on any desktop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Set when this process is the elevated copy an installer started (it never
/// asks for elevation again, whatever it finds).
static ELEVATED: AtomicBool = AtomicBool::new(false);

pub fn set_elevated() {
    ELEVATED.store(true, Ordering::Relaxed);
}

pub const EXE: &str = "lookout.exe";
pub const PRODUCT: &str = "Lookout";
pub const PUBLISHER: &str = "Hack Club";
/// The Tauri bundle's keys (manufacturer "hackclub", from com.hackclub.lookout).
pub const REG_APP: &str = r"Software\hackclub\Lookout";
pub const REG_UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Lookout";
pub const REG_PROTOCOL: &str = r"Software\Classes\lookout";

pub const WEBVIEW2: &str = "Microsoft Edge WebView2";
/// Demo runs: the simulated admin install of WebView2 has happened.
static DEMO_WEBVIEW2: AtomicBool = AtomicBool::new(false);

/// What's already on this machine.
#[derive(Clone, Debug)]
pub struct Existing {
    pub dir: PathBuf,
    pub version: Option<String>,
}

/// One step's progress for the window: overall fraction and what's happening.
#[derive(Clone, Debug)]
pub struct Progress {
    pub fraction: f32,
    /// NSIS's detail line ("Extract: lookout.exe... 45%").
    pub step: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ops {
    Real,
    Demo,
}

impl Ops {
    pub fn default_dir(self) -> PathBuf {
        dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join(PRODUCT)
    }

    /// Where to install: an existing install's folder (updated in place,
    /// with admin rights if it's somewhere like Program Files), else
    /// `%LOCALAPPDATA%\Lookout`.
    pub fn preferred_dir(self) -> PathBuf {
        self.existing().map(|e| e.dir).unwrap_or_else(|| self.default_dir())
    }

    /// Whether writing to `dir` takes administrator rights (the UAC shield
    /// goes on the button and the work runs in an elevated copy).
    pub fn needs_admin(self, dir: &Path) -> bool {
        match self {
            Ops::Demo => std::env::var("LOOKOUT_INSTALLER_DEMO_ADMIN").is_ok(),
            Ops::Real => cfg!(windows) && !ELEVATED.load(Ordering::Relaxed) && !writable(dir),
        }
    }

    pub fn existing(self) -> Option<Existing> {
        match self {
            Ops::Real => existing_real(),
            Ops::Demo => match std::env::var("LOOKOUT_INSTALLER_DEMO_EXISTING").ok() {
                Some(v) if !v.is_empty() => Some(Existing { dir: self.default_dir(), version: Some(v) }),
                _ => None,
            },
        }
    }

    /// Installs (or updates) into `dir`; returns the installed exe.
    pub fn install(self, dir: &Path, progress: &dyn Fn(Progress)) -> Result<PathBuf, String> {
        match self {
            Ops::Real if self.needs_admin(dir) => {
                let mut args = vec!["/S".to_string()];
                // /D= is last: NSIS-style, it takes the rest of the line.
                args.push(format!("/D={}", dir.display()));
                run_elevated(args, progress)?;
                Ok(dir.join(EXE))
            }
            Ops::Real => install_real(dir, progress),
            Ops::Demo => {
                // With LOOKOUT_INSTALLER_DEMO_NO_WEBVIEW2 the per-user
                // WebView2 install "fails", so the finish page offers the admin one.
                let mut steps = vec!["Extract: lookout.exe...", "Create shortcut: Lookout.lnk", "Writing registry"];
                if self.webview2_missing() {
                    steps.push("Installing Microsoft Edge WebView2...");
                }
                demo_steps(progress, &steps)?;
                Ok(dir.join(EXE))
            }
        }
    }

    pub fn uninstall(self, progress: &dyn Fn(Progress)) -> Result<(), String> {
        match self {
            Ops::Real if existing_real().is_some_and(|e| self.needs_admin(&e.dir)) => {
                run_elevated(vec!["--uninstall".into(), "/S".into()], progress)
            }
            Ops::Real => uninstall_real(progress),
            Ops::Demo => demo_steps(progress, &["Delete file: lookout.exe", "Delete file: Lookout.lnk", "Remove folder: Lookout"]),
        }
    }

    /// Whether the WebView2 Runtime (program pages in the app) is missing.
    pub fn webview2_missing(self) -> bool {
        match self {
            Ops::Real => cfg!(windows) && !webview2_present(),
            Ops::Demo => std::env::var("LOOKOUT_INSTALLER_DEMO_NO_WEBVIEW2").is_ok() && !DEMO_WEBVIEW2.load(Ordering::Relaxed),
        }
    }

    /// Installs the WebView2 Runtime for everyone on this PC, through UAC:
    /// the window's offer when the per-user install in [`Ops::install`]
    /// didn't take.
    pub fn install_webview2_admin(self, progress: &dyn Fn(Progress)) -> Result<(), String> {
        progress(Progress { fraction: 0.1, step: format!("Installing {WEBVIEW2}...") });
        match self {
            Ops::Real => install_webview2_real(true)?,
            Ops::Demo => {
                demo_steps(progress, &["Downloading MicrosoftEdgeWebview2Setup.exe", "Installing Microsoft Edge WebView2..."])?;
                DEMO_WEBVIEW2.store(true, Ordering::Relaxed);
            }
        }
        progress(Progress { fraction: 1.0, step: "Completed".into() });
        Ok(())
    }

    /// Starts the installed app, detached.
    pub fn launch(self, exe: &Path) {
        match self {
            Ops::Real => {
                if let Err(e) = std::process::Command::new(exe).current_dir(exe.parent().unwrap_or(exe)).spawn() {
                    log::error!("[installer] couldn't start {}: {e}", exe.display());
                }
            }
            Ops::Demo => log::info!("[installer] (demo) would start {}", exe.display()),
        }
    }
}

fn demo_steps(progress: &dyn Fn(Progress), steps: &[&'static str]) -> Result<(), String> {
    let fail = std::env::var("LOOKOUT_INSTALLER_DEMO_FAIL").is_ok();
    for (i, step) in steps.iter().enumerate() {
        for k in 0..10 {
            progress(Progress { fraction: (i as f32 + k as f32 / 10.0) / steps.len() as f32, step: step.to_string() });
            std::thread::sleep(Duration::from_millis(70));
        }
        if fail && i == 1 {
            return Err("Couldn't write to the Start menu folder (demo).".into());
        }
    }
    progress(Progress { fraction: 1.0, step: "Completed".into() });
    Ok(())
}

/// Free space on the drive that holds `dir` (or its nearest existing
/// parent), for the "Space available" line.
pub fn space_available(dir: &Path) -> Option<u64> {
    let mut p = dir.to_path_buf();
    while !p.exists() {
        p = p.parent()?.to_path_buf();
    }
    free_space(&p)
}

/// The size of this exe, for the "Space required" line.
pub fn space_required() -> Option<u64> {
    std::env::current_exe().ok().and_then(|e| std::fs::metadata(e).ok()).map(|m| m.len())
}

#[cfg(windows)]
fn free_space(p: &Path) -> Option<u64> {
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows::core::HSTRING;
    let mut free = 0u64;
    unsafe { GetDiskFreeSpaceExW(&HSTRING::from(p.as_os_str()), Some(&mut free), None, None) }.ok()?;
    Some(free)
}

#[cfg(unix)]
fn free_space(p: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0).then(|| st.f_bavail as u64 * st.f_frsize as u64)
}

#[cfg(not(any(windows, unix)))]
fn free_space(_: &Path) -> Option<u64> {
    None
}

/// NSIS's sizes: "27.1 MB", "420.1 GB".
pub fn fmt_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1024f64.powi(3) {
        format!("{:.1} GB", b / 1024f64.powi(3))
    } else if b >= 1024f64.powi(2) {
        format!("{:.1} MB", b / 1024f64.powi(2))
    } else {
        format!("{:.1} KB", b / 1024.0)
    }
}

/// Whether this user can create files in `dir`, or in its nearest existing
/// parent when it doesn't exist yet. Tried, not read from ACLs (inherited
/// rights make those misleading).
fn writable(dir: &Path) -> bool {
    let mut dir = dir.to_path_buf();
    while !dir.exists() {
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => return false,
        }
    }
    let probe = dir.join(format!(".lookout-write-test-{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Same folder, ignoring case and `\\?\` prefixes (Windows paths).
pub fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let s = p.to_string_lossy().trim_start_matches(r"\\?\").trim_end_matches(['\\', '/']).to_string();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    norm(a) == norm(b)
}

/// Deletes `lookout.old-*.exe` files a previous install set aside because the
/// app was still running at the time. Best effort, at app start.
pub fn remove_leftovers() {
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) else { return };
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_lowercase();
        if name.starts_with("lookout.old-") && name.ends_with(".exe") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

// ── Windows ─────────────────────────────────────────────────────────────

#[cfg(windows)]
fn existing_real() -> Option<Existing> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let dir: String = hkcu
        .open_subkey(REG_APP)
        .and_then(|k| k.get_value(""))
        .or_else(|_| hkcu.open_subkey(REG_UNINSTALL).and_then(|k| k.get_value("InstallLocation")))
        .ok()?;
    let dir = PathBuf::from(dir.trim_matches('"'));
    if !dir.join(EXE).is_file() {
        return None;
    }
    let version = hkcu.open_subkey(REG_UNINSTALL).and_then(|k| k.get_value::<String, _>("DisplayVersion")).ok();
    Some(Existing { dir, version })
}

#[cfg(not(windows))]
fn existing_real() -> Option<Existing> {
    None
}

#[cfg(windows)]
fn install_real(dir: &Path, progress: &dyn Fn(Progress)) -> Result<PathBuf, String> {
    use std::io::{Read, Write};

    let me = std::env::current_exe().map_err(|e| format!("Couldn't find this installer's file: {e}"))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't create {}: {e}", dir.display()))?;
    let target = dir.join(EXE);

    // Copy ourselves in, unless this already is the installed copy.
    if !(target.exists() && same_file(&me, &target)) {
        progress(Progress { fraction: 0.02, step: format!("Output folder: {}", dir.display()) });
        make_room(&target)?;
        let tmp = dir.join("lookout.exe.partial");
        let mut src = std::fs::File::open(&me).map_err(|e| format!("Couldn't read the installer: {e}"))?;
        let total = src.metadata().map(|m| m.len()).unwrap_or(1).max(1);
        let mut dst = std::fs::File::create(&tmp).map_err(|e| format!("Couldn't write to {}: {e}", dir.display()))?;
        let mut buf = vec![0u8; 1 << 20];
        let mut done = 0u64;
        loop {
            let n = src.read(&mut buf).map_err(|e| format!("Couldn't read the installer: {e}"))?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).map_err(|e| format!("Couldn't write Lookout: {e}"))?;
            done += n as u64;
            progress(Progress {
                fraction: 0.05 + 0.75 * done as f32 / total as f32,
                step: format!("Extract: {EXE}... {}%", done * 100 / total),
            });
        }
        dst.sync_all().map_err(|e| format!("Couldn't write Lookout: {e}"))?;
        drop(dst);
        std::fs::rename(&tmp, &target).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("Couldn't replace {}: {e}", target.display())
        })?;
    }

    // What the NSIS and Tauri installers left behind; lookout.exe is its own
    // uninstaller and carries its own icon now.
    for old in ["uninstall.exe", "lookout.ico"] {
        let _ = std::fs::remove_file(dir.join(old));
    }

    if let Some(lnk) = start_menu_shortcut() {
        progress(Progress { fraction: 0.85, step: format!("Create shortcut: {}", lnk.display()) });
        create_shortcut(&lnk, &target).map_err(|e| format!("Couldn't add the Start menu shortcut: {e}"))?;
    }

    progress(Progress { fraction: 0.93, step: "Writing registry".into() });
    write_registry(dir, &target).map_err(|e| format!("Couldn't register Lookout with Windows: {e}"))?;
    unsafe {
        use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }

    // Almost always already there (Windows 11, and Windows 10 through Windows
    // Update), and then this is skipped. Not fatal: without it program pages
    // open in the browser, and the window offers an admin install afterwards.
    if !webview2_present() {
        progress(Progress { fraction: 0.95, step: format!("Installing {WEBVIEW2}...") });
        if let Err(e) = install_webview2_real(false) {
            log::warn!("[installer] {WEBVIEW2} wasn't installed: {e}");
        }
    }
    progress(Progress { fraction: 1.0, step: "Completed".into() });
    Ok(target)
}

#[cfg(not(windows))]
fn install_real(_: &Path, _: &dyn Fn(Progress)) -> Result<PathBuf, String> {
    Err("Installing is only supported on Windows.".into())
}

/// Frees `target` for the new copy: waits a few seconds for a running Lookout
/// to let go of it, then renames it aside (Windows allows renaming a running
/// exe) so the new one can go in while the old process finishes.
/// Never kills by name: this installer may itself be called lookout.exe.
#[cfg(windows)]
fn make_room(target: &Path) -> Result<(), String> {
    if !target.exists() {
        return Ok(());
    }
    for _ in 0..40 {
        if std::fs::OpenOptions::new().write(true).open(target).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let aside = target.with_file_name(format!("lookout.old-{stamp}.exe"));
    std::fs::rename(target, &aside).map_err(|e| format!("Lookout is still running and couldn't be replaced ({e}). Quit it and try again."))
}

#[cfg(windows)]
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase(),
        _ => false,
    }
}

#[cfg(windows)]
fn start_menu_shortcut() -> Option<PathBuf> {
    // %APPDATA%\Microsoft\Windows\Start Menu\Programs (FOLDERID_Programs).
    dirs::config_dir().map(|d| d.join(r"Microsoft\Windows\Start Menu\Programs").join(format!("{PRODUCT}.lnk")))
}

#[cfg(windows)]
fn create_shortcut(lnk: &Path, exe: &Path) -> windows::core::Result<()> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{HSTRING, Interface};

    if let Some(p) = lnk.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    unsafe {
        // This runs on a worker thread of its own, so it owns COM there.
        let inited = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let r = (|| {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(&HSTRING::from(exe.as_os_str()))?;
            if let Some(dir) = exe.parent() {
                link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
            }
            link.SetDescription(&HSTRING::from("Record timelapses for Hack Club"))?;
            link.SetIconLocation(&HSTRING::from(exe.as_os_str()), 0)?;
            let file: IPersistFile = link.cast()?;
            file.Save(&HSTRING::from(lnk.as_os_str()), true)
        })();
        if inited {
            CoUninitialize();
        }
        r
    }
}

// ── WebView2 ────────────────────────────────────────────────────────────
//
// The program panel is a WebView2 (wry). wry links WebView2's loader
// statically, so the app runs without the runtime; only that panel needs it.
// Detection and the bootstrapper are Microsoft's "online-only deployment"
// (learn.microsoft.com/microsoft-edge/webview2/concepts/distribution): run
// unelevated it installs for this user, no UAC; run elevated, for everyone.

#[cfg(windows)]
const WEBVIEW2_CLIENT: &str = r"Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
/// Microsoft's Evergreen Bootstrapper (~2 MB; fetches the right runtime).
#[cfg(windows)]
const WEBVIEW2_BOOTSTRAPPER: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

/// A `pv` version above 0.0.0.0 under the per-machine (64- or 32-bit view)
/// or per-user EdgeUpdate client key.
#[cfg(windows)]
fn webview2_present() -> bool {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let has = |root, path: String| {
        RegKey::predef(root)
            .open_subkey(path)
            .and_then(|k| k.get_value::<String, _>("pv"))
            .is_ok_and(|v| !v.trim().is_empty() && v.trim() != "0.0.0.0")
    };
    has(HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\WOW6432Node\{WEBVIEW2_CLIENT}"))
        || has(HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\{WEBVIEW2_CLIENT}"))
        || has(HKEY_CURRENT_USER, format!(r"Software\{WEBVIEW2_CLIENT}"))
}

#[cfg(not(windows))]
fn webview2_present() -> bool {
    true
}

/// Downloads the bootstrapper and runs it `/silent /install`: as is (per-user
/// unless this process is already elevated), or through UAC with `admin`.
#[cfg(windows)]
fn install_webview2_real(admin: bool) -> Result<(), String> {
    use windows::Win32::System::Com::IBindStatusCallback;
    use windows::Win32::System::Com::Urlmon::URLDownloadToFileW;
    use windows::core::{HSTRING, IUnknown};

    let setup = std::env::temp_dir().join(format!("MicrosoftEdgeWebview2Setup-{}.exe", std::process::id()));
    unsafe {
        URLDownloadToFileW(
            None::<&IUnknown>,
            &HSTRING::from(WEBVIEW2_BOOTSTRAPPER),
            &HSTRING::from(setup.as_os_str()),
            0,
            None::<&IBindStatusCallback>,
        )
    }
    .map_err(|e| format!("Couldn't download {WEBVIEW2}: {}", e.message()))?;
    let ran = if admin {
        shell_run_wait(&setup, "/silent /install", true).map(|_| ())
    } else {
        std::process::Command::new(&setup)
            .args(["/silent", "/install"])
            .status()
            .map(|_| ())
            .map_err(|e| format!("Couldn't start the {WEBVIEW2} installer: {e}"))
    };
    let _ = std::fs::remove_file(&setup);
    ran?;
    // The exit code isn't documented; the registry is the answer.
    if webview2_present() { Ok(()) } else { Err(format!("{WEBVIEW2} didn't install.")) }
}

#[cfg(not(windows))]
fn install_webview2_real(_admin: bool) -> Result<(), String> {
    Err("WebView2 is only installed on Windows.".into())
}

#[cfg(windows)]
fn write_registry(dir: &Path, exe: &Path) -> std::io::Result<()> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let exe_s = exe.to_string_lossy().to_string();
    let dir_s = dir.to_string_lossy().to_string();
    let icon = format!("\"{exe_s}\",0");

    // lookout:// links (session links and program pairing callbacks).
    let (proto, _) = hkcu.create_subkey(REG_PROTOCOL)?;
    proto.set_value("", &"URL:Lookout")?;
    proto.set_value("URL Protocol", &"")?;
    hkcu.create_subkey(format!(r"{REG_PROTOCOL}\DefaultIcon"))?.0.set_value("", &icon)?;
    hkcu.create_subkey(format!(r"{REG_PROTOCOL}\shell\open\command"))?.0.set_value("", &format!("\"{exe_s}\" \"%1\""))?;

    hkcu.create_subkey(REG_APP)?.0.set_value("", &dir_s)?;

    let (un, _) = hkcu.create_subkey(REG_UNINSTALL)?;
    un.set_value("DisplayName", &PRODUCT)?;
    un.set_value("DisplayIcon", &icon)?;
    un.set_value("DisplayVersion", &crate::app::VERSION)?;
    un.set_value("Publisher", &PUBLISHER)?;
    un.set_value("URLInfoAbout", &"https://lookout.hackclub.com")?;
    un.set_value("InstallLocation", &dir_s)?;
    un.set_value("UninstallString", &format!("\"{exe_s}\" --uninstall"))?;
    un.set_value("QuietUninstallString", &format!("\"{exe_s}\" --uninstall /S"))?;
    un.set_value("NoModify", &1u32)?;
    un.set_value("NoRepair", &1u32)?;
    let kb = std::fs::metadata(exe).map(|m| (m.len() / 1024) as u32).unwrap_or(0);
    un.set_value("EstimatedSize", &kb)?;
    Ok(())
}

/// Removes the install this exe belongs to. The running exe can't delete
/// itself, so it moves itself to %TEMP% first and a hidden `cmd` deletes it
/// a moment after this process exits. User data (%APPDATA% and
/// %LOCALAPPDATA%\com.hackclub.lookout) is kept, as the NSIS uninstaller did.
#[cfg(windows)]
fn uninstall_real(progress: &dyn Fn(Progress)) -> Result<(), String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = existing_real().map(|e| e.dir).or_else(|| me.parent().map(Path::to_path_buf)).ok_or("No install found.")?;

    let target = dir.join(EXE);
    progress(Progress { fraction: 0.1, step: format!("Delete file: {}", target.display()) });
    if target.exists() {
        let parked = std::env::temp_dir().join(format!("lookout-uninstall-{}.exe", std::process::id()));
        if same_file(&me, &target) {
            std::fs::rename(&target, &parked)
                .or_else(|_| std::fs::copy(&target, &parked).map(|_| ()))
                .map_err(|e| format!("Couldn't remove Lookout: {e}"))?;
            delete_after_exit(&parked);
        } else {
            make_room(&target)?;
            let _ = std::fs::remove_file(&target);
        }
    }
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_lowercase();
        if name.starts_with("lookout.old-") || ["uninstall.exe", "lookout.ico", "lookout.exe.partial"].contains(&name.as_str()) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    // Only an empty folder: someone may have installed into a folder with
    // other things in it.
    progress(Progress { fraction: 0.4, step: format!("Remove folder: {}", dir.display()) });
    let _ = std::fs::remove_dir(&dir);

    if let Some(lnk) = start_menu_shortcut() {
        progress(Progress { fraction: 0.6, step: format!("Delete file: {}", lnk.display()) });
        let _ = std::fs::remove_file(lnk);
    }
    if let Some(desk) = dirs::desktop_dir() {
        let _ = std::fs::remove_file(desk.join(format!("{PRODUCT}.lnk")));
    }

    progress(Progress { fraction: 0.85, step: "Delete registry key".into() });
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for k in [REG_PROTOCOL, REG_UNINSTALL, REG_APP] {
        let _ = hkcu.delete_subkey_all(k);
    }
    unsafe {
        use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
    progress(Progress { fraction: 1.0, step: "Completed".into() });
    Ok(())
}

#[cfg(not(windows))]
fn uninstall_real(_: &dyn Fn(Progress)) -> Result<(), String> {
    Err("Uninstalling is only supported on Windows.".into())
}

#[cfg(windows)]
fn delete_after_exit(file: &Path) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let pid = std::process::id();
    let script = format!(
        "@echo off\r\n:wait\r\ntasklist /FI \"PID eq {pid}\" | find \"{pid}\" >nul && (timeout /t 1 /nobreak >nul & goto wait)\r\ndel /f /q \"{f}\"\r\n(goto) 2>nul & del \"%~f0\"\r\n",
        f = file.display()
    );
    let path = std::env::temp_dir().join(format!("lookout-uninstall-{pid}.cmd"));
    if std::fs::write(&path, script).is_ok() {
        let _ = std::process::Command::new("cmd").arg("/C").arg(&path).creation_flags(CREATE_NO_WINDOW).spawn();
    }
}

// ── Elevation ───────────────────────────────────────────────────────────
//
// The window never runs elevated: when the folder needs admin rights it
// starts this exe again through UAC ("runas") with the silent flags plus
// `/ELEVATED --report=<file>`, and follows the copy's progress through that
// file: `P\t<fraction>\t<detail>` lines, then `OK` or `ERR\t<message>`.
// The app is never started from the elevated copy (that would run Lookout
// as admin); whoever asked for the install starts it afterwards.

/// Appends one line to a report file (the elevated side).
pub fn report_line(report: &Path, line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(report) {
        let _ = writeln!(f, "{line}");
    }
}

/// The report line for a progress update.
pub fn report_progress(p: &Progress) -> String {
    format!("P\t{}\t{}", p.fraction, p.step.replace(['\n', '\t'], " "))
}

/// Reads a report: the last progress, and the outcome once there is one.
fn read_report(report: &Path) -> (Option<Progress>, Option<Result<(), String>>) {
    let text = std::fs::read_to_string(report).unwrap_or_default();
    let (mut last, mut done) = (None, None);
    for line in text.lines() {
        let mut parts = line.splitn(3, '\t');
        match parts.next() {
            Some("P") => {
                let fraction = parts.next().and_then(|f| f.parse().ok()).unwrap_or(0.0);
                last = Some(Progress { fraction, step: parts.next().unwrap_or("").to_string() });
            }
            Some("OK") => done = Some(Ok(())),
            Some("ERR") => done = Some(Err(parts.collect::<Vec<_>>().join("\t"))),
            _ => {}
        }
    }
    (last, done)
}

#[cfg(windows)]
fn run_elevated(mut args: Vec<String>, progress: &dyn Fn(Progress)) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let report = std::env::temp_dir().join(format!("lookout-install-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&report);
    // Before the /D= that has to stay last.
    let at = args.iter().position(|a| a.starts_with("/D=")).unwrap_or(args.len());
    args.insert(at, "/ELEVATED".into());
    args.insert(at + 1, format!("--report={}", report.display()));
    let params = args
        .iter()
        .map(|a| if a.contains(' ') && !a.starts_with("/D=") { format!("\"{a}\"") } else { a.clone() })
        .collect::<Vec<_>>()
        .join(" ");

    let code = shell_run_wait_with(&me, &params, true, &mut || {
        if let (Some(p), _) = read_report(&report) {
            progress(p);
        }
    })
    .map_err(|e| {
        if e == UAC_DECLINED {
            "Administrator permission is needed to install to this folder. Choose another folder, or click Retry and allow it.".into()
        } else {
            e
        }
    })?;
    let (_, outcome) = read_report(&report);
    let _ = std::fs::remove_file(&report);
    match outcome {
        Some(r) => r,
        None if code == 0 => Ok(()),
        None => Err(format!("The elevated installer stopped unexpectedly (exit code {code}).")),
    }
}

#[cfg(windows)]
const UAC_DECLINED: &str = "Administrator permission wasn't given.";

#[cfg(windows)]
fn shell_run_wait(file: &Path, params: &str, runas: bool) -> Result<u32, String> {
    shell_run_wait_with(file, params, runas, &mut || {})
}

/// Starts `file` (through UAC with `runas`), hidden, and waits for it,
/// calling `tick` every 120 ms; returns its exit code.
#[cfg(windows)]
fn shell_run_wait_with(file: &Path, params: &str, runas: bool, tick: &mut dyn FnMut()) -> Result<u32, String> {
    use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use windows::core::{HSTRING, PCWSTR, w};

    let file = HSTRING::from(file.as_os_str());
    let params = HSTRING::from(params);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: if runas { w!("runas") } else { PCWSTR::null() },
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    if let Err(e) = unsafe { ShellExecuteExW(&mut info) } {
        return Err(if e.code() == ERROR_CANCELLED.to_hresult() {
            UAC_DECLINED.into()
        } else if runas {
            format!("Couldn't get administrator permission: {}", e.message())
        } else {
            format!("Couldn't start {}: {}", file, e.message())
        });
    }
    let process = info.hProcess;
    loop {
        let waited = unsafe { WaitForSingleObject(process, 120) };
        tick();
        if waited == WAIT_OBJECT_0 {
            break;
        }
    }
    let mut code = 1u32;
    unsafe {
        let _ = GetExitCodeProcess(process, &mut code);
        let _ = CloseHandle(process);
    }
    Ok(code)
}

#[cfg(not(windows))]
fn run_elevated(_: Vec<String>, _: &dyn Fn(Progress)) -> Result<(), String> {
    let _ = read_report;
    Err("Administrator rights are only requested on Windows.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_round_trip() {
        let f = std::env::temp_dir().join(format!("lookout-report-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&f);
        report_line(&f, &report_progress(&Progress { fraction: 0.5, step: "Extract: lookout.exe... 50%".into() }));
        let (p, done) = read_report(&f);
        assert_eq!(p.map(|p| (p.fraction, p.step)), Some((0.5, "Extract: lookout.exe... 50%".to_string())));
        assert!(done.is_none());
        report_line(&f, "ERR\tCouldn't replace it");
        assert_eq!(read_report(&f).1, Some(Err("Couldn't replace it".to_string())));
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn same_dir_ignores_trailing_separators() {
        let d = std::env::temp_dir();
        let mut s = d.to_string_lossy().to_string();
        s.push(std::path::MAIN_SEPARATOR);
        assert!(same_dir(&d, Path::new(&s)));
        assert!(!same_dir(&d, &d.join("nope")));
    }
}
