//! Persisted state — what the Tauri app kept in the webview's localStorage
//! (see ui-spec §6.5), now one JSON file in the app config dir, written
//! atomically (temp file + rename) off the UI thread. Device credentials for
//! linked programs live in a separate `secrets.json` (0600), the same file
//! and format the Tauri shell used, so links survive the switch.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const APP_ID: &str = "com.hackclub.lookout";
pub const DEFAULT_API_BASE: &str = "https://lookout.hackclub.com";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TokenEntry {
    pub token: String,
    pub added_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub archived: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub token: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub tracked_seconds: f64,
    #[serde(default)]
    pub screenshot_count: u64,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub total_active_seconds: f64,
    #[serde(default)]
    pub thumbnail_url: Option<String>,
    #[serde(default)]
    pub video_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedSummary {
    pub summary: SessionSummary,
    pub fetched_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProgramLink {
    pub program: String,
    pub origin: String,
    pub pair_url: String,
    pub start_url: String,
    pub paired_at: String,
    pub device_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingPairing {
    pub program: String,
    pub state: String,
    pub verifier: String,
    pub origin: String,
    pub pair_url: String,
    pub start_url: String,
    pub created_at: i64,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub icon_url: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TipMomentRecord {
    pub token: String,
    pub moments: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PendingUpdate {
    pub version: String,
    /// Auto-install attempts already spent on this version.
    pub attempts: u32,
    /// Whether the user was already told this version won't install.
    pub notified: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Persisted {
    /// `lookout-tokens`
    pub tokens: Vec<TokenEntry>,
    /// `lookout:gallery-cache:v2`
    pub gallery_cache: HashMap<String, CachedSummary>,
    /// `lookout-api-base` (None = default)
    pub api_base: Option<String>,
    /// `lookout-blacklisted-apps`
    pub blacklisted_apps: Vec<String>,
    /// `lookout-last-monitor-selection`
    pub last_monitor_selection: Vec<u32>,
    /// `lookout-perm-screen` / `lookout-perm-camera`
    pub perm_screen: bool,
    pub perm_camera: bool,
    /// The setup page was completed with Accessibility required. Native
    /// only (no Tauri key): false for everyone who passed the older page,
    /// so they see it once more.
    pub perm_accessibility: bool,
    /// `onboardingShown`: the onboarding was finished (or skipped through
    /// to the end). Until it is, it shows at launch — for new and existing
    /// users alike. Native only.
    pub onboarding_shown: bool,
    /// `lookout-program-links`
    pub program_links: HashMap<String, ProgramLink>,
    /// `lookout-pending-pairing`
    pub pending_pairing: Option<PendingPairing>,
    /// `lookout-panel-state`
    pub panel_state: HashMap<String, String>,
    /// `lookout-deeplink-sessions`
    pub deeplink_sessions: HashMap<String, String>,
    /// `lookout-tip-moments`
    pub tip_moments: HashMap<String, TipMomentRecord>,
    /// `lookout_update_nag_dismissed`
    pub update_nag_dismissed: Option<String>,
    /// `lookout_last_update_ts` (ms since the epoch): when an update install
    /// last started, so a relaunch into it skips the immediate re-check.
    pub last_update_ts: Option<i64>,
    /// `lookout_pending_update`: an update that downloaded but never installed.
    pub pending_update: Option<PendingUpdate>,
    /// Gallery scroll offset, restored on remount (module-global in the web app).
    #[serde(skip)]
    pub gallery_scroll: f32,
    /// Set once the one-time import from the Tauri app has run.
    pub imported_tauri: bool,
}

pub fn config_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(APP_ID)
}

fn state_path() -> PathBuf {
    config_dir().join("native-state.json")
}

fn secrets_path() -> PathBuf {
    config_dir().join("secrets.json")
}

impl Persisted {
    pub fn load() -> Self {
        let mut p: Persisted = fs::read_to_string(state_path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        if !p.imported_tauri {
            match crate::store::import::from_tauri(&mut p) {
                Ok(true) => {
                    p.imported_tauri = true;
                    p.save();
                }
                // Nothing found (a fresh install, or storage this build can't
                // see yet): look again next launch rather than giving up for good.
                Ok(false) => {}
                Err(e) => log::warn!("[store] legacy import will retry on next launch: {e}"),
            }
        }
        p
    }

    pub fn api_base(&self) -> String {
        // `LOOKOUT_API_BASE` points a dev build at another server without
        // touching the saved choice (e.g. the bundled lookout-dev-server).
        if let Ok(env) = std::env::var("LOOKOUT_API_BASE") {
            if !env.is_empty() {
                return env.trim_end_matches('/').to_string();
            }
        }
        self.api_base.clone().unwrap_or_else(|| DEFAULT_API_BASE.to_string())
    }

    pub fn is_default_api_base(&self) -> bool {
        self.api_base() == DEFAULT_API_BASE
    }

    /// Write atomically on a background thread.
    pub fn save(&self) {
        let raw = match serde_json::to_string_pretty(self) {
            Ok(r) => r,
            Err(e) => {
                log::error!("[store] serialize failed: {e}");
                return;
            }
        };
        std::thread::spawn(move || {
            static LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
            let _g = LOCK.lock();
            if let Err(e) = write_atomic(&state_path(), raw.as_bytes(), false) {
                log::error!("[store] save failed: {e}");
            }
        });
    }

    /// Write now, on this thread — for the moment before the process exits
    /// into an update installer, when a background write could be lost.
    pub fn save_sync(&self) {
        match serde_json::to_string_pretty(self) {
            Ok(raw) => {
                if let Err(e) = write_atomic(&state_path(), raw.as_bytes(), false) {
                    log::error!("[store] save failed: {e}");
                }
            }
            Err(e) => log::error!("[store] serialize failed: {e}"),
        }
    }

    // ── tokens (useTokenStore) ──────────────────────────────────────────

    pub fn active_tokens(&self) -> Vec<String> {
        self.tokens.iter().filter(|t| !t.archived).map(|t| t.token.clone()).collect()
    }

    /// `addToken`: new tokens go first; re-adding an archived token restores it.
    pub fn add_token(&mut self, token: &str) {
        if let Some(e) = self.tokens.iter_mut().find(|t| t.token == token) {
            e.archived = false;
        } else {
            self.tokens.insert(
                0,
                TokenEntry {
                    token: token.to_string(),
                    added_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    label: None,
                    archived: false,
                },
            );
        }
        self.save();
    }

    pub fn archive_token(&mut self, token: &str) {
        if let Some(e) = self.tokens.iter_mut().find(|t| t.token == token) {
            e.archived = true;
        }
        self.save();
    }
}

pub fn write_atomic(path: &PathBuf, bytes: &[u8], private: bool) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let _ = private;
    let mut f = opts.open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path)
}

// ── Secret store (program device tokens) ────────────────────────────────

pub mod secrets {
    use super::*;

    fn read_all() -> HashMap<String, String> {
        fs::read_to_string(secrets_path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn get(key: &str) -> Option<String> {
        read_all().get(key).cloned()
    }

    pub fn set(key: &str, value: &str) -> Result<(), String> {
        let mut m = read_all();
        m.insert(key.to_string(), value.to_string());
        let raw = serde_json::to_vec(&m).map_err(|e| e.to_string())?;
        write_atomic(&secrets_path(), &raw, true).map_err(|e| e.to_string())
    }

    pub fn delete(key: &str) -> Result<(), String> {
        let mut m = read_all();
        m.remove(key);
        let raw = serde_json::to_vec(&m).map_err(|e| e.to_string())?;
        write_atomic(&secrets_path(), &raw, true).map_err(|e| e.to_string())
    }

    pub fn program_key(program: &str) -> String {
        format!("program-device-token:{program}")
    }
}

// ── One-time import from the Tauri app's webview storage ────────────────

mod leveldb;
mod webkit;

pub mod import {
    use super::*;
    use std::path::Path;

    /// Copy the user's sessions and settings out of the Tauri webview's
    /// localStorage, so switching to the native app keeps the gallery,
    /// filtered apps, server choice and program links exactly as they were.
    /// WebKit (macOS, Linux) keeps localStorage in a SQLite `ItemTable` with
    /// UTF-16LE values; WebView2 (Windows) uses LevelDB, which we scan for
    /// the JSON values directly.
    pub fn from_tauri(p: &mut Persisted) -> Result<bool, String> {
        let items = read_webview_local_storage()?;
        if items.is_empty() {
            return Ok(false);
        }
        log::info!("[store] importing {} localStorage keys from the Tauri app", items.len());
        merge(p, &items);
        Ok(true)
    }

    /// Merges the Tauri app's values into `p` without overwriting anything
    /// native already has: sign-ins are added to (never replaced), and the
    /// rest only fills fields that are still empty.
    fn merge(p: &mut Persisted, items: &HashMap<String, String>) {
        let get = |k: &str| items.get(k).cloned();
        if let Some(v) = get("lookout-tokens").and_then(|v| serde_json::from_str::<Vec<TokenEntry>>(&v).ok()) {
            for t in v {
                if !p.tokens.iter().any(|have| have.token == t.token) {
                    p.tokens.push(t);
                }
            }
        }
        if let Some(v) = get("lookout:gallery-cache:v2")
            .and_then(|v| serde_json::from_str::<HashMap<String, CachedSummary>>(&v).ok())
        {
            for (k, s) in v {
                p.gallery_cache.entry(k).or_insert(s);
            }
        }
        if p.api_base.is_none() {
            p.api_base = get("lookout-api-base").filter(|v| !v.is_empty());
        }
        if p.blacklisted_apps.is_empty() {
            if let Some(v) = get("lookout-blacklisted-apps").and_then(|v| serde_json::from_str(&v).ok()) {
                p.blacklisted_apps = v;
            }
        }
        if p.last_monitor_selection.is_empty() {
            if let Some(v) = get("lookout-last-monitor-selection").and_then(|v| serde_json::from_str(&v).ok()) {
                p.last_monitor_selection = v;
            }
        }
        p.perm_screen |= get("lookout-perm-screen").as_deref() == Some("1");
        p.perm_camera |= get("lookout-perm-camera").as_deref() == Some("1");
        if let Some(v) = get("lookout-program-links").and_then(|v| serde_json::from_str::<HashMap<String, ProgramLink>>(&v).ok()) {
            for (k, l) in v {
                p.program_links.entry(k).or_insert(l);
            }
        }
        if p.panel_state.is_empty() {
            if let Some(v) = get("lookout-panel-state").and_then(|v| serde_json::from_str(&v).ok()) {
                p.panel_state = v;
            }
        }
        if let Some(v) = get("lookout-deeplink-sessions").and_then(|v| serde_json::from_str::<HashMap<String, String>>(&v).ok()) {
            for (k, s) in v {
                p.deeplink_sessions.entry(k).or_insert(s);
            }
        }
        if p.tip_moments.is_empty() {
            if let Some(v) = get("lookout-tip-moments").and_then(|v| serde_json::from_str(&v).ok()) {
                p.tip_moments = v;
            }
        }
        if p.update_nag_dismissed.is_none() {
            p.update_nag_dismissed = get("lookout_update_nag_dismissed");
        }
    }

    fn candidate_dirs() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if cfg!(target_os = "linux") {
            if let Some(d) = dirs::data_dir() {
                out.push(d.join(APP_ID).join("localstorage"));
            }
        }
        if cfg!(target_os = "macos") {
            if let Some(h) = dirs::home_dir() {
                out.push(h.join("Library/WebKit").join(APP_ID).join("WebsiteData"));
                out.push(h.join("Library/WebKit").join("lookout").join("WebsiteData"));
            }
        }
        if cfg!(target_os = "windows") {
            if let Some(d) = dirs::data_local_dir() {
                out.push(d.join(APP_ID).join("EBWebView/Default/Local Storage/leveldb"));
            }
        }
        out
    }

    fn read_webview_local_storage() -> Result<HashMap<String, String>, String> {
        let mut out = HashMap::new();
        for dir in candidate_dirs() {
            scan_dir(&dir, 0, &mut out)?;
        }
        Ok(out)
    }

    /// Walks `dir` for localStorage databases. Recursive because WebKit's
    /// layout moved: older versions keep `WebsiteData/LocalStorage/<origin>.localstorage`,
    /// newer macOS keeps `WebsiteData/Default/<salted hash>/<salted hash>/LocalStorage/localstorage.sqlite3`.
    fn scan_dir(dir: &Path, depth: usize, out: &mut HashMap<String, String>) -> Result<(), String> {
        const MAX_DEPTH: usize = 6;
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("{}: {e}", dir.display())),
        };
        if dir.ends_with("leveldb") {
            match leveldb::read(dir, KEYS) {
                Ok(found) => {
                    out.extend(found);
                    return Ok(());
                }
                // Fall back to scanning the files for the values.
                Err(e) => log::warn!("[store] couldn't open {} as LevelDB ({e}); scanning it", dir.display()),
            }
        }
        let mut entries: Vec<_> = entries.collect::<Result<_, _>>().map_err(|e| e.to_string())?;
        // Later reads win, so a dev server's storage (localhost:1420) never
        // shadows the shipped app's (tauri://localhost / tauri.localhost).
        entries.sort_by_key(|e| e.file_name().to_string_lossy().contains("tauri"));
        for e in entries {
            let path = e.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if depth < MAX_DEPTH {
                    scan_dir(&path, depth + 1, out)?;
                }
            } else if name.ends_with(".localstorage") || name == "localstorage.sqlite3" {
                read_sqlite_items(&path, out)?;
            } else if (name.ends_with(".ldb") || name.ends_with(".log")) && dir.ends_with("leveldb") {
                scan_leveldb(&path, out)?;
            }
        }
        Ok(())
    }

    fn read_sqlite_items(path: &PathBuf, out: &mut HashMap<String, String>) -> Result<(), String> {
        let items = super::webkit::read(path, KEYS).map_err(|e| format!("{}: {e}", path.display()))?;
        out.extend(items);
        Ok(())
    }

    fn scan_leveldb(path: &PathBuf, out: &mut HashMap<String, String>) -> Result<(), String> {
        let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        for key in KEYS {
            let needle = key.as_bytes();
            let mut i = 0;
            while let Some(pos) = find(&bytes[i..], needle) {
                let start = i + pos + needle.len();
                // Chromium stores values with a 1-byte encoding prefix (0x01 = latin1).
                if let Some(v) = bytes.get(start..).and_then(|rest| {
                    let rest = rest.strip_prefix(&[1u8]).unwrap_or(rest);
                    let end = rest.iter().position(|b| *b == 0 || *b < 0x09).unwrap_or(rest.len().min(2_000_000));
                    std::str::from_utf8(&rest[..end]).ok().map(|s| s.to_string())
                }) {
                    if !v.is_empty() {
                        out.insert(key.to_string(), v);
                    }
                }
                i = start;
            }
        }
        Ok(())
    }

    fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }

    const KEYS: &[&str] = &[
        "lookout-tokens",
        "lookout:gallery-cache:v2",
        "lookout-api-base",
        "lookout-blacklisted-apps",
        "lookout-last-monitor-selection",
        "lookout-perm-screen",
        "lookout-perm-camera",
        "lookout-program-links",
        "lookout-panel-state",
        "lookout-deeplink-sessions",
        "lookout-tip-moments",
        "lookout_update_nag_dismissed",
    ];

    #[cfg(test)]
    mod tests {
        use super::*;

        fn webkit_db(path: &Path, key: &str, value: &str) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let db = rusqlite::Connection::open(path).unwrap();
            db.execute_batch("CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB NOT NULL ON CONFLICT FAIL);").unwrap();
            let bytes: Vec<u8> = value.encode_utf16().flat_map(u16::to_le_bytes).collect();
            db.execute("INSERT INTO ItemTable VALUES (?1, ?2)", rusqlite::params![key, bytes]).unwrap();
        }

        #[test]
        fn finds_old_and_new_webkit_layouts_and_webview2_leveldb() {
            let root = std::env::temp_dir().join(format!("lookout-import-test-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            // Older WebKit (and WebKitGTK with an explicit localstorage dir).
            webkit_db(&root.join("old/LocalStorage/tauri_localhost_0.localstorage"), "lookout-api-base", "https://old.example");
            // Newer macOS WebKit: salted origin hashes under Default/.
            webkit_db(
                &root.join("new/Default/abc123/abc123/LocalStorage/localstorage.sqlite3"),
                "lookout-tokens",
                r#"[{"token":"t","addedAt":"2026-01-01T00:00:00Z"}]"#,
            );
            // WebView2: LevelDB log with a latin1-prefixed value.
            let ldb = root.join("EBWebView/Default/Local Storage/leveldb");
            fs::create_dir_all(&ldb).unwrap();
            let mut log = b"_tauri://localhost\x00\x01lookout-perm-screen\x011".to_vec();
            log.push(0);
            fs::write(ldb.join("000003.log"), &log).unwrap();
            // A stray .log outside a leveldb folder is ignored.
            fs::write(root.join("new/Default/debug.log"), b"lookout-perm-camera\x011").unwrap();

            let mut out = HashMap::new();
            for dir in ["old", "new", "EBWebView/Default/Local Storage/leveldb"] {
                scan_dir(&root.join(dir), 0, &mut out).unwrap();
            }
            let _ = fs::remove_dir_all(&root);
            assert_eq!(out.get("lookout-api-base").map(String::as_str), Some("https://old.example"));
            assert!(out.get("lookout-tokens").is_some_and(|t| t.contains(r#""token":"t""#)));
            assert_eq!(out.get("lookout-perm-screen").map(String::as_str), Some("1"));
            assert!(!out.contains_key("lookout-perm-camera"));
        }

        #[test]
        fn merge_adds_sign_ins_and_never_overwrites() {
            let mut p = Persisted {
                tokens: vec![TokenEntry { token: "a".into(), added_at: "native".into(), label: None, archived: false }],
                api_base: Some("https://chosen-in-native".into()),
                ..Default::default()
            };
            let items: HashMap<String, String> = [
                ("lookout-tokens", r#"[{"token":"a","addedAt":"tauri"},{"token":"b","addedAt":"tauri"}]"#),
                ("lookout-api-base", "https://from-tauri"),
                ("lookout-blacklisted-apps", r#"["Slack"]"#),
                ("lookout-perm-screen", "1"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
            merge(&mut p, &items);
            let tokens: Vec<_> = p.tokens.iter().map(|t| (t.token.as_str(), t.added_at.as_str())).collect();
            assert_eq!(tokens, [("a", "native"), ("b", "tauri")]);
            assert_eq!(p.api_base.as_deref(), Some("https://chosen-in-native"));
            assert_eq!(p.blacklisted_apps, ["Slack"]);
            assert!(p.perm_screen);
        }
    }
}
