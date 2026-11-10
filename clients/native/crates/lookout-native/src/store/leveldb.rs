//! Read WebView2's (Chromium's) localStorage, a LevelDB under
//! `EBWebView/Default/Local Storage/leveldb`. Opened as a database rather
//! than scanned as bytes: every launch folds the log into snappy-compressed
//! tables, where the values aren't readable as plain text, and old versions
//! of a key can linger in older tables until compaction.
//!
//! Chromium's layout: data keys are `_` + origin + `\0` + key, and keys and
//! values each start with an encoding byte (0 = UTF-16LE, 1 = Latin-1).
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusty_leveldb::{DB, LdbIterator, Options};

/// The values of `keys`, from the app's own origin when there are several
/// (a dev server's `http://localhost:1420` must not win over `tauri.localhost`).
pub fn read(dir: &Path, keys: &[&str]) -> Result<HashMap<String, String>, String> {
    // Open a copy: opening recovers the log into a new table and takes a
    // LOCK, and the Tauri app's files are left exactly as they were.
    let copy = Copy::of(dir)?;
    let mut db = DB::open(&copy.0, Options { create_if_missing: false, ..Options::default() }).map_err(|e| e.to_string())?;
    let mut it = db.new_iter().map_err(|e| e.to_string())?;
    let mut by_origin: HashMap<String, HashMap<String, String>> = HashMap::new();
    while let Some((k, v)) = it.next() {
        let Some(rest) = k.strip_prefix(b"_") else { continue };
        let Some(nul) = rest.iter().position(|b| *b == 0) else { continue };
        let origin = String::from_utf8_lossy(&rest[..nul]).into_owned();
        let (Some(key), Some(value)) = (decode(&rest[nul + 1..]), decode(&v)) else { continue };
        if keys.contains(&key.as_str()) {
            by_origin.entry(origin).or_default().insert(key, value);
        }
    }
    let best = by_origin
        .into_iter()
        .max_by_key(|(origin, items)| (origin.contains("tauri"), items.len()))
        .map(|(_, items)| items)
        .unwrap_or_default();
    Ok(best)
}

fn decode(b: &[u8]) -> Option<String> {
    match b.split_first()? {
        (0, rest) if rest.len() % 2 == 0 => {
            String::from_utf16(&rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()).ok()
        }
        (1, rest) => Some(rest.iter().map(|&c| c as char).collect()),
        _ => None,
    }
}

/// A throwaway copy of a LevelDB directory, removed on drop.
struct Copy(PathBuf);

impl Copy {
    fn of(dir: &Path) -> Result<Self, String> {
        let to = std::env::temp_dir().join(format!(
            "lookout-import-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        ));
        std::fs::create_dir_all(&to).map_err(|e| e.to_string())?;
        let copy = Copy(to);
        for e in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let e = e.map_err(|e| e.to_string())?;
            let name = e.file_name();
            if name == "LOCK" || !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            std::fs::copy(e.path(), copy.0.join(&name)).map_err(|err| format!("{}: {err}", e.path().display()))?;
        }
        Ok(copy)
    }
}

impl Drop for Copy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_leveldb::compressor::{CompressorId, SnappyCompressor};

    fn key(origin: &str, k: &str) -> Vec<u8> {
        let mut out = format!("_{origin}").into_bytes();
        out.push(0);
        out.push(1);
        out.extend_from_slice(k.as_bytes());
        out
    }

    fn latin1(v: &str) -> Vec<u8> {
        let mut out = vec![1];
        out.extend_from_slice(v.as_bytes());
        out
    }

    fn utf16(v: &str) -> Vec<u8> {
        let mut out = vec![0];
        out.extend(v.encode_utf16().flat_map(u16::to_le_bytes));
        out
    }

    #[test]
    fn reads_compressed_tables_latest_values_and_prefers_the_app_origin() {
        let dir = std::env::temp_dir().join(format!("lookout-ldb-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let tokens = r#"[{"token":"tok-1","addedAt":"2026-01-01T00:00:00Z","label":"Résumé"}]"#;
        {
            let opt = Options { compressor: SnappyCompressor::ID, ..Options::default() };
            let mut db = DB::open(&dir, opt).unwrap();
            db.put(b"VERSION", b"1").unwrap();
            // An old value, overwritten later: only the newest may come back.
            db.put(&key("http://tauri.localhost", "lookout-tokens"), &latin1("[]")).unwrap();
            db.flush().unwrap();
            db.compact_range(b"\x00", b"\xff").unwrap();
            // Repetitive JSON so snappy really back-references it.
            let cache = format!("{{{}}}", (0..50).map(|i| format!(r#""s{i}":{{"title":"session","units":1}}"#)).collect::<Vec<_>>().join(","));
            db.put(&key("http://tauri.localhost", "lookout-tokens"), &utf16(tokens)).unwrap();
            db.put(&key("http://tauri.localhost", "lookout:gallery-cache:v2"), &latin1(&cache)).unwrap();
            db.put(&key("http://localhost:1420", "lookout-tokens"), &latin1("[]")).unwrap();
            db.put(&key("http://localhost:1420", "lookout-api-base"), &latin1("http://dev")).unwrap();
            db.put(&key("http://tauri.localhost", "unrelated"), &latin1("x")).unwrap();
            db.flush().unwrap();
            db.compact_range(b"\x00", b"\xff").unwrap();
        }
        let got = read(&dir, &["lookout-tokens", "lookout:gallery-cache:v2", "lookout-api-base"]).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(got.get("lookout-tokens").map(String::as_str), Some(tokens));
        assert!(got.get("lookout:gallery-cache:v2").is_some_and(|c| c.contains(r#""s49""#)));
        // From the dev origin only, so it isn't mixed in.
        assert!(!got.contains_key("lookout-api-base"));
        assert!(!got.contains_key("unrelated"));
    }
}
