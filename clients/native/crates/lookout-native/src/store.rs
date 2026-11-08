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
            crate::store::import::from_tauri(&mut p);
            p.imported_tauri = true;
            p.save();
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

pub mod import {
    use super::*;

    /// Copy the user's sessions and settings out of the Tauri webview's
    /// localStorage, so switching to the native app keeps the gallery,
    /// filtered apps, server choice and program links exactly as they were.
    /// WebKit (macOS, Linux) keeps localStorage in a SQLite `ItemTable` with
    /// UTF-16LE values; WebView2 (Windows) uses LevelDB, which we scan for
    /// the JSON values directly.
    pub fn from_tauri(p: &mut Persisted) {
        let items = read_webview_local_storage();
        if items.is_empty() {
            return;
        }
        log::info!("[store] importing {} localStorage keys from the Tauri app", items.len());
        let get = |k: &str| items.get(k).cloned();
        if let Some(v) = get("lookout-tokens").and_then(|v| serde_json::from_str::<Vec<TokenEntry>>(&v).ok()) {
            p.tokens = v;
        }
        if let Some(v) = get("lookout:gallery-cache:v2")
            .and_then(|v| serde_json::from_str::<HashMap<String, CachedSummary>>(&v).ok())
        {
            p.gallery_cache = v;
        }
        if let Some(v) = get("lookout-api-base") {
            p.api_base = Some(v);
        }
        if let Some(v) = get("lookout-blacklisted-apps").and_then(|v| serde_json::from_str(&v).ok()) {
            p.blacklisted_apps = v;
        }
        if let Some(v) = get("lookout-last-monitor-selection").and_then(|v| serde_json::from_str(&v).ok()) {
            p.last_monitor_selection = v;
        }
        p.perm_screen = get("lookout-perm-screen").as_deref() == Some("1");
        p.perm_camera = get("lookout-perm-camera").as_deref() == Some("1");
        if let Some(v) = get("lookout-program-links").and_then(|v| serde_json::from_str(&v).ok()) {
            p.program_links = v;
        }
        if let Some(v) = get("lookout-panel-state").and_then(|v| serde_json::from_str(&v).ok()) {
            p.panel_state = v;
        }
        if let Some(v) = get("lookout-deeplink-sessions").and_then(|v| serde_json::from_str(&v).ok()) {
            p.deeplink_sessions = v;
        }
        if let Some(v) = get("lookout-tip-moments").and_then(|v| serde_json::from_str(&v).ok()) {
            p.tip_moments = v;
        }
        p.update_nag_dismissed = get("lookout_update_nag_dismissed");
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
                out.push(h.join("Library/WebKit").join(APP_ID).join("WebsiteData/LocalStorage"));
                out.push(h.join("Library/WebKit").join("lookout").join("WebsiteData/LocalStorage"));
            }
        }
        if cfg!(target_os = "windows") {
            if let Some(d) = dirs::data_local_dir() {
                out.push(d.join(APP_ID).join("EBWebView/Default/Local Storage/leveldb"));
            }
        }
        out
    }

    fn read_webview_local_storage() -> HashMap<String, String> {
        let mut out = HashMap::new();
        for dir in candidate_dirs() {
            let Ok(entries) = fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                let path = e.path();
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.ends_with(".localstorage") {
                    read_sqlite_items(&path, &mut out);
                } else if name.ends_with(".ldb") || name.ends_with(".log") {
                    scan_leveldb(&path, &mut out);
                }
            }
        }
        out
    }

    /// Minimal read-only SQLite page walker is overkill; WebKit's
    /// localstorage files are small, so we scan for `key` UTF-16 strings
    /// followed by their value blobs.
    fn read_sqlite_items(path: &PathBuf, out: &mut HashMap<String, String>) {
        let Ok(bytes) = fs::read(path) else { return };
        for key in KEYS {
            if let Some(v) = find_utf16_value(&bytes, key) {
                out.entry(key.to_string()).or_insert(v);
            }
        }
    }

    fn scan_leveldb(path: &PathBuf, out: &mut HashMap<String, String>) {
        let Ok(bytes) = fs::read(path) else { return };
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
    }

    fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }

    /// WebKit `ItemTable(key TEXT, value BLOB)`: keys are stored as text in
    /// the record, values as UTF-16LE blobs right after. Look for the UTF-8
    /// key and decode the UTF-16 run that follows it.
    fn find_utf16_value(bytes: &[u8], key: &str) -> Option<String> {
        let needle = key.as_bytes();
        let mut best: Option<String> = None;
        let mut i = 0;
        while let Some(pos) = find(&bytes[i..], needle) {
            let start = i + pos + needle.len();
            let mut units = Vec::new();
            let mut j = start;
            while j + 1 < bytes.len() {
                let u = u16::from_le_bytes([bytes[j], bytes[j + 1]]);
                if u == 0 || (u < 0x09) {
                    break;
                }
                units.push(u);
                j += 2;
                if units.len() > 4_000_000 {
                    break;
                }
            }
            if let Ok(s) = String::from_utf16(&units) {
                let plausible = s.starts_with('[') || s.starts_with('{') || s.starts_with('"') || s.starts_with("http") || s == "1" || s.chars().all(|c| c.is_ascii_graphic());
                if plausible && !s.is_empty() && best.as_ref().map(|b| s.len() > b.len()).unwrap_or(true) {
                    best = Some(s);
                }
            }
            i = start;
        }
        best
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
}
