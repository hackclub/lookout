//! Read live WebKit localStorage records, including committed WAL contents.
//! Scanning file bytes can resurrect deleted tokens and append unrelated pages.
use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OpenFlags, types::ValueRef};

pub fn read(path: &Path, keys: &[&str]) -> rusqlite::Result<HashMap<String, String>> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    read_items(&db, keys)
}

fn read_items(db: &Connection, keys: &[&str]) -> rusqlite::Result<HashMap<String, String>> {
    let mut stmt = db.prepare("SELECT key, value FROM ItemTable")?;
    let mut rows = stmt.query([])?;
    let mut out = HashMap::new();
    while let Some(row) = rows.next()? {
        let key: String = row.get(0)?;
        if !keys.contains(&key.as_str()) {
            continue;
        }
        let value = match row.get_ref(1)? {
            ValueRef::Text(bytes) => std::str::from_utf8(bytes).ok().map(str::to_owned),
            ValueRef::Blob(bytes) if bytes.len() % 2 == 0 => {
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16(&units).ok()
            }
            _ => None,
        };
        if let Some(value) = value {
            out.insert(key, value);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA secure_delete=OFF; CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB NOT NULL ON CONFLICT FAIL);").unwrap();
        db
    }

    fn insert(db: &Connection, key: &str, value: &str) {
        let bytes: Vec<u8> = value.encode_utf16().flat_map(u16::to_le_bytes).collect();
        db.execute(
            "INSERT OR REPLACE INTO ItemTable VALUES (?1, ?2)",
            rusqlite::params![key, bytes],
        )
        .unwrap();
    }

    #[test]
    fn reads_exact_values_across_sqlite_pages() {
        let db = db();
        let tokens = format!(
            r#"[{{"token":"{}","label":"日本語 🎥"}}]"#,
            "x".repeat(30_000)
        );
        insert(&db, "lookout-tokens", &tokens);
        insert(&db, "lookout-api-base", "https://lookout.hackclub.com");
        let items = read_items(&db, &["lookout-tokens", "lookout-api-base"]).unwrap();
        assert_eq!(items["lookout-tokens"], tokens);
        assert_eq!(items["lookout-api-base"], "https://lookout.hackclub.com");
    }

    #[test]
    fn does_not_resurrect_deleted_or_superseded_tokens() {
        let db = db();
        insert(&db, "lookout-tokens", "old-token");
        insert(&db, "lookout-tokens", "current-token");
        assert_eq!(
            read_items(&db, &["lookout-tokens"]).unwrap()["lookout-tokens"],
            "current-token"
        );
        db.execute("DELETE FROM ItemTable", []).unwrap();
        assert!(read_items(&db, &["lookout-tokens"]).unwrap().is_empty());
    }

    #[test]
    fn ignores_unknown_keys_and_invalid_utf16() {
        let db = db();
        insert(&db, "unrelated", "not imported");
        db.execute(
            "INSERT INTO ItemTable VALUES ('lookout-tokens', X'00d8')",
            [],
        )
        .unwrap();
        assert!(read_items(&db, &["lookout-tokens"]).unwrap().is_empty());
    }

    #[test]
    fn reads_committed_wal_without_modifying_source() {
        let path =
            std::env::temp_dir().join(format!("lookout-migration-{}.sqlite", std::process::id()));
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE ItemTable (key TEXT, value BLOB);")
            .unwrap();
        insert(&db, "lookout-api-base", "https://example.com");
        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            read(&path, &["lookout-api-base"]).unwrap()["lookout-api-base"],
            "https://example.com"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
}
