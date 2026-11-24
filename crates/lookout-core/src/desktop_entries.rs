//! The display name of a Linux app, from its `.desktop` entry.
//!
//! Activity names the frontmost app by its `WM_CLASS` (X11) or app id
//! (Wayland): `code`, `Code`, `org.mozilla.firefox`, `discord`. Those differ
//! from the macOS bundle id and the Windows exe name for the same app, so a
//! reviewer can't line them up. The `.desktop` entry's `Name` ("Visual
//! Studio Code", "Firefox") is what the launcher shows, and is close to what
//! macOS and Windows report.
//!
//! An app is found by its desktop id (the file name, which a Wayland app id
//! usually is) or by the entry's `StartupWMClass` (what an X11 class
//! matches, and how snaps' `code_code.desktop` is found). Directories are
//! searched in XDG order, so a user's own entry wins over the system's.
//!
//! The index is built on first use and rebuilt on a miss at most every few
//! minutes (an app installed mid-session), never on every sample.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a miss waits before the directories are read again.
const REBUILD_AFTER: Duration = Duration::from_secs(300);
/// Entries read per directory, at most: a runaway directory must not stall
/// the sampler.
const MAX_FILES_PER_DIR: usize = 4_000;
/// A `.desktop` file past this is not one.
const MAX_FILE_BYTES: u64 = 256 * 1024;

#[derive(Default)]
pub(crate) struct Index {
    /// Lower-cased desktop id -> Name.
    by_id: HashMap<String, String>,
    /// Lower-cased StartupWMClass -> Name.
    by_class: HashMap<String, String>,
}

impl Index {
    pub(crate) fn get(&self, app: &str) -> Option<&str> {
        let key = app.trim().to_lowercase();
        if key.is_empty() {
            return None;
        }
        self.by_id
            .get(&key)
            .or_else(|| self.by_class.get(&key))
            .map(String::as_str)
    }

    /// Index every `.desktop` file under `dirs`, earlier directories first.
    pub(crate) fn build(dirs: &[PathBuf]) -> Index {
        let mut index = Index::default();
        for dir in dirs {
            let mut files = Vec::new();
            collect(dir, "", &mut files, 0);
            for (id, path) in files {
                let Some(entry) = read_entry(&path) else { continue };
                index.by_id.entry(id.to_lowercase()).or_insert_with(|| entry.name.clone());
                if let Some(class) = entry.wm_class {
                    index.by_class.entry(class.to_lowercase()).or_insert(entry.name);
                }
            }
        }
        index
    }
}

/// `.desktop` files under `dir`, with their desktop ids: a file in a
/// subdirectory gets the path joined by `-` (`kde4/foo.desktop` is
/// `kde4-foo`), as the spec says.
fn collect(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>, depth: usize) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten().take(MAX_FILES_PER_DIR) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            collect(&path, &format!("{prefix}{name}-"), out, depth + 1);
        } else if let Some(stem) = name.strip_suffix(".desktop") {
            out.push((format!("{prefix}{stem}"), path));
        }
    }
}

struct Entry {
    name: String,
    wm_class: Option<String>,
}

fn read_entry(path: &Path) -> Option<Entry> {
    if std::fs::metadata(path).ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    parse(&std::fs::read_to_string(path).ok()?)
}

/// The unlocalized `Name` and `StartupWMClass` of the `[Desktop Entry]`
/// group. `None` for an entry with no name, or one marked `Hidden` (the
/// spec's "deleted").
fn parse(text: &str) -> Option<Entry> {
    let mut in_entry = false;
    let mut name = None;
    let mut wm_class = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim();
        match key.trim() {
            "Name" if name.is_none() && !value.is_empty() => name = Some(value.to_string()),
            "StartupWMClass" if wm_class.is_none() && !value.is_empty() => wm_class = Some(value.to_string()),
            "Hidden" if value.eq_ignore_ascii_case("true") => return None,
            _ => {}
        }
    }
    Some(Entry { name: name?, wm_class })
}

/// The XDG application directories, most important first.
fn dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        Some(d) => out.push(PathBuf::from(d)),
        None => out.extend(home.as_ref().map(|h| h.join(".local/share"))),
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    out.extend(data_dirs.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    // Flatpak and snap export here; usually on XDG_DATA_DIRS already, but
    // not when the app was started outside a full desktop session.
    out.extend(home.as_ref().map(|h| h.join(".local/share/flatpak/exports/share")));
    out.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    out.push(PathBuf::from("/var/lib/snapd/desktop"));

    let mut seen = std::collections::HashSet::new();
    out.into_iter()
        .map(|d| d.join("applications"))
        .filter(|d| seen.insert(d.clone()))
        .collect()
}

struct Cache {
    index: Index,
    built: Instant,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// The display name for `app` (a `WM_CLASS` or Wayland app id), if an
/// installed `.desktop` entry names it.
pub(crate) fn name_for(app: &str) -> Option<String> {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let stale = guard.as_ref().is_none_or(|c| c.built.elapsed() >= REBUILD_AFTER);
    if let Some(name) = guard.as_ref().and_then(|c| c.index.get(app)) {
        return Some(name.to_string());
    }
    if !stale {
        return None;
    }
    let cache = Cache { index: Index::build(&dirs()), built: Instant::now() };
    let name = cache.index.get(app).map(str::to_string);
    *guard = Some(cache);
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory under the system temp dir, removed when dropped.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let dir = std::env::temp_dir().join(format!("lookout-desktop-{tag}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn parses_name_and_class_from_the_desktop_entry_group_only() {
        let e = parse(
            "[Desktop Entry]\nName=Visual Studio Code\nName[de]=Code\nStartupWMClass=Code\n\n[Desktop Action new]\nName=New Window\n",
        )
        .unwrap();
        assert_eq!(e.name, "Visual Studio Code");
        assert_eq!(e.wm_class.as_deref(), Some("Code"));
        assert!(parse("[Desktop Entry]\nName=Gone\nHidden=true\n").is_none());
        assert!(parse("[Desktop Entry]\nExec=foo\n").is_none());
        assert!(parse("[Desktop Action x]\nName=Not me\n").is_none());
    }

    #[test]
    fn finds_by_desktop_id_or_wm_class_case_insensitively() {
        let tmp = TempDir::new("find");
        let apps = tmp.path().join("applications");
        write(&apps, "code.desktop", "[Desktop Entry]\nName=Visual Studio Code\nStartupWMClass=Code\n");
        write(&apps, "org.mozilla.firefox.desktop", "[Desktop Entry]\nName=Firefox\n");
        write(&apps, "code_code.desktop", "[Desktop Entry]\nName=Visual Studio Code (snap)\nStartupWMClass=CodeSnap\n");
        write(&apps, "kde4/kate.desktop", "[Desktop Entry]\nName=Kate\n");
        let index = Index::build(&[apps]);

        assert_eq!(index.get("code"), Some("Visual Studio Code"));
        assert_eq!(index.get("Code"), Some("Visual Studio Code"));
        assert_eq!(index.get("org.mozilla.firefox"), Some("Firefox"));
        assert_eq!(index.get("codesnap"), Some("Visual Studio Code (snap)"));
        assert_eq!(index.get("kde4-kate"), Some("Kate"));
        assert_eq!(index.get("nothing"), None);
        assert_eq!(index.get("  "), None);
    }

    #[test]
    fn the_first_directory_wins() {
        let tmp = TempDir::new("order");
        let user = tmp.path().join("user/applications");
        let system = tmp.path().join("system/applications");
        write(&user, "discord.desktop", "[Desktop Entry]\nName=Discord (mine)\n");
        write(&system, "discord.desktop", "[Desktop Entry]\nName=Discord\n");
        let index = Index::build(&[user, system]);
        assert_eq!(index.get("discord"), Some("Discord (mine)"));
    }
}

