//! Linux: the header bar's close button, drawn by the user's own libadwaita.
//!
//! GNOME users restyle window controls through `~/.config/gtk-4.0/gtk.css`,
//! and some distros patch libadwaita itself. Neither can be read off a
//! setting, so rather than imitate them Lookout asks libadwaita to draw its
//! real close button and paints the result.
//!
//! The drawing happens in `lookout-window-controls`, a separate binary
//! installed next to `lookout` (`src/bin/lookout_window_controls.rs`): GTK 3
//! and GTK 4 can't share a process, and `lookout` links GTK 3 for WebKitGTK.
//! The helper `dlopen`s libadwaita, so it is never a dependency, and renders
//! the real close button once per state — normal, hover, pressed, backdrop —
//! in light and dark.
//!
//! The PNGs are cached. At launch the cached set is painted straight away
//! and a background check re-renders only when something that could change
//! the button did (the user's `gtk.css`, libadwaita, accent, contrast, icon
//! theme); the same check runs when a window regains focus. Anything that
//! fails leaves the built-in button. `LOOKOUT_THEMED_CONTROLS=0` turns it off.

use std::sync::Arc;

use gpui_kit::gpui::RenderImage;

/// Index into [`ThemedClose::light`] / [`ThemedClose::dark`].
#[derive(Clone, Copy)]
pub enum CloseState {
    Normal = 0,
    Hover = 1,
    Pressed = 2,
    Backdrop = 3,
}

/// libadwaita's close button, ready to paint.
pub struct ThemedClose {
    /// Logical size of every image: the button plus [`MARGIN`] on each side
    /// (room for a theme's shadow or glow), centred on Lookout's own slot.
    pub size: (f32, f32),
    pub light: [Arc<RenderImage>; 4],
    pub dark: [Arc<RenderImage>; 4],
}

impl ThemedClose {
    pub fn image(&self, dark: bool, state: CloseState) -> Arc<RenderImage> {
        (if dark { &self.dark } else { &self.light })[state as usize].clone()
    }
}

#[cfg(target_os = "linux")]
pub use imp::{current, init, recheck};

#[cfg(not(target_os = "linux"))]
pub fn current() -> Option<Arc<ThemedClose>> {
    None
}
#[cfg(not(target_os = "linux"))]
pub fn init(_: &mut gpui_kit::gpui::App) {}
#[cfg(not(target_os = "linux"))]
pub fn recheck(_: &mut gpui_kit::gpui::App) {}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use gpui_kit::gpui::App;

    /// Must match the helper's output names.
    const STATES: [&str; 4] = ["normal", "hover", "pressed", "backdrop"];
    /// Bump when the helper's output changes shape.
    const FORMAT: u32 = 1;

    static CURRENT: Mutex<Option<Arc<ThemedClose>>> = Mutex::new(None);
    static CHECKING: AtomicBool = AtomicBool::new(false);
    static LAST_CHECK: Mutex<Option<Instant>> = Mutex::new(None);

    pub fn current() -> Option<Arc<ThemedClose>> {
        CURRENT.lock().ok()?.clone()
    }

    fn enabled() -> bool {
        crate::platform::desktop_appearance::env_flag(std::env::var("LOOKOUT_THEMED_CONTROLS").ok().as_deref())
            != Some(false)
            && crate::platform::desktop_appearance::client_frames()
    }

    fn cache_dir() -> PathBuf {
        dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join(crate::store::APP_ID).join("window-controls")
    }

    /// Paint whatever is cached now, then check it's current in the background.
    pub fn init(cx: &mut App) {
        if !enabled() {
            return;
        }
        let dir = cache_dir();
        if let Some(set) = load(&dir) {
            *CURRENT.lock().unwrap() = Some(Arc::new(set));
        }
        check(cx);
    }

    /// A window came back into focus: the theme may have changed meanwhile.
    pub fn recheck(cx: &mut App) {
        if !enabled() {
            return;
        }
        let recent = LAST_CHECK.lock().unwrap().is_some_and(|t| t.elapsed() < Duration::from_secs(3));
        if !recent {
            check(cx);
        }
    }

    fn check(cx: &mut App) {
        if CHECKING.swap(true, Ordering::SeqCst) {
            return;
        }
        *LAST_CHECK.lock().unwrap() = Some(Instant::now());
        let job = cx.background_executor().spawn(async move { refresh(&cache_dir()) });
        cx.spawn(async move |cx| {
            let changed = job.await;
            CHECKING.store(false, Ordering::SeqCst);
            if let Some(set) = changed {
                *CURRENT.lock().unwrap() = set.map(Arc::new);
                cx.update(|cx| cx.refresh_windows());
            }
        })
        .detach();
    }

    /// Re-render if the key moved. `None`: nothing changed; `Some(None)`:
    /// libadwaita can't draw it any more (use the built-in button).
    fn refresh(dir: &Path) -> Option<Option<ThemedClose>> {
        let key = cache_key();
        if std::fs::read_to_string(dir.join("key")).ok().as_deref() == Some(key.as_str()) {
            return None;
        }
        let started = Instant::now();
        let tmp = dir.with_extension(format!("tmp{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let rendered = std::fs::create_dir_all(&tmp).is_ok() && run_helper(&tmp);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::create_dir_all(dir.parent().unwrap_or(dir));
        if rendered && std::fs::rename(&tmp, dir).is_ok() {
            log::info!("[controls] rendered libadwaita's close button in {}ms", started.elapsed().as_millis());
        } else {
            let _ = std::fs::remove_dir_all(&tmp);
            let _ = std::fs::create_dir_all(dir);
            log::info!("[controls] libadwaita couldn't draw the close button: using the built-in one");
        }
        // Written either way, so a machine without libadwaita doesn't retry
        // on every launch.
        let _ = std::fs::write(dir.join("key"), &key);
        Some(load(dir))
    }

    fn run_helper(out: &Path) -> bool {
        let Some(exe) = std::env::current_exe().ok().map(|e| e.with_file_name(HELPER)) else { return false };
        let mut cmd = std::process::Command::new(exe);
        cmd.arg(out);
        // Inside the AppImage, the GTK hook points GTK at the bundled GTK 3
        // runtime; the helper wants the host's own GTK 4 and libadwaita.
        if let Some(appdir) = std::env::var_os("APPDIR") {
            for var in HOST_GTK_UNSET {
                cmd.env_remove(var);
            }
            if let Some(dirs) = std::env::var_os("XDG_DATA_DIRS") {
                let appdir = std::path::PathBuf::from(appdir);
                let host: Vec<_> = std::env::split_paths(&dirs).filter(|d| !d.starts_with(&appdir)).collect();
                if let Ok(joined) = std::env::join_paths(host) {
                    cmd.env("XDG_DATA_DIRS", joined);
                }
            }
        }
        let Ok(mut child) = cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            return false;
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status.success(),
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
            }
        }
    }

    fn load(dir: &Path) -> Option<ThemedClose> {
        let meta = std::fs::read_to_string(dir.join("meta")).ok()?;
        let mut it = meta.split_whitespace().map(|v| v.parse::<f32>().ok());
        let (w, h) = (it.next()??, it.next()??);
        let variant = |name: &str| -> Option<[Arc<RenderImage>; 4]> {
            let mut out = Vec::with_capacity(4);
            for s in STATES {
                let bytes = std::fs::read(dir.join(format!("close-{name}-{s}.png"))).ok()?;
                out.push(crate::images::decode_to_render_image(&bytes).ok()?);
            }
            out.try_into().ok()
        };
        Some(ThemedClose { size: (w, h), light: variant("light")?, dark: variant("dark")? })
    }

    /// Everything that can change how libadwaita draws the button.
    fn cache_key() -> String {
        let stamp = |p: &Path| {
            std::fs::metadata(p)
                .ok()
                .map(|m| format!("{}:{:?}", m.len(), m.modified().ok()))
                .unwrap_or_else(|| "-".into())
        };
        let config = dirs::config_dir().unwrap_or_default();
        let adw = LIBADWAITA_DIRS
            .iter()
            .map(|d| Path::new(d).join("libadwaita-1.so.0"))
            .find(|p| p.exists())
            .map(|p| format!("{}={}", p.display(), stamp(&p)))
            .unwrap_or_else(|| "no-adw".into());
        let settings = [
            ("org.gnome.desktop.interface", "accent-color"),
            ("org.gnome.desktop.interface", "icon-theme"),
            ("org.gnome.desktop.a11y.interface", "high-contrast"),
        ]
        .map(|(s, k)| gsetting(s, k).unwrap_or_default())
        .join(",");
        format!(
            "{FORMAT}|{}|{adw}|{}|{}|{settings}",
            env!("CARGO_PKG_VERSION"),
            stamp(&config.join("gtk-4.0/gtk.css")),
            stamp(&config.join("gtk-4.0/gtk-dark.css")),
        )
    }

    const HELPER: &str = "lookout-window-controls";

    /// What the AppImage's GTK hook sets that would steer a host GTK 4.
    const HOST_GTK_UNSET: [&str; 10] = [
        "LD_LIBRARY_PATH",
        "GTK_DATA_PREFIX",
        "GTK_EXE_PREFIX",
        "GTK_PATH",
        "GTK_IM_MODULE_FILE",
        "GTK_THEME",
        "GDK_PIXBUF_MODULE_FILE",
        "GSETTINGS_SCHEMA_DIR",
        "GIO_MODULE_DIR",
        "GIO_EXTRA_MODULES",
    ];

    const LIBADWAITA_DIRS: [&str; 6] = [
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/usr/lib64",
        "/usr/lib",
        "/usr/local/lib",
        "/run/host/usr/lib",
    ];

    fn gsetting(schema: &str, key: &str) -> Option<String> {
        let out = std::process::Command::new("gsettings").args(["get", schema, key]).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

}
