//! Read WebView2's (Chromium's) localStorage, a LevelDB under
//! `EBWebView/Default/Local Storage/leveldb`.
//!
//! A small read-only reader, in safe Rust, rather than a LevelDB library:
//! the one we tried (rusty-leveldb) crashed on Arm64, and all this needs is
//! the newest value of a handful of keys. It reads the two kinds of files
//! that hold data — the write-ahead `.log` and the sorted tables (`.ldb`,
//! `.sst`, whose blocks are usually snappy-compressed) — and keeps, for each
//! key, the write with the highest sequence number (a deletion included), so
//! superseded values in older files never come back. Nothing is written.
//!
//! Chromium's layout inside: data keys are `_` + origin + `\0` + key, and
//! keys and values each start with an encoding byte (0 = UTF-16LE, 1 = Latin-1).
use std::collections::HashMap;
use std::path::Path;

/// The values of `keys`, from the app's own origin when there are several
/// (a dev server's `http://localhost:1420` must not win over `tauri.localhost`).
pub fn read(dir: &Path, keys: &[&str]) -> Result<HashMap<String, String>, String> {
    let mut newest: HashMap<Vec<u8>, (u64, Option<Vec<u8>>)> = HashMap::new();
    let mut put = |k: &[u8], seq: u64, v: Option<&[u8]>| {
        let e = newest.entry(k.to_vec()).or_insert((0, None));
        if seq >= e.0 {
            *e = (seq, v.map(<[u8]>::to_vec));
        }
    };
    let mut read_any = false;
    for e in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = e.map_err(|e| e.to_string())?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let parsed = if name.ends_with(".log") && name != "LOG" {
            std::fs::read(&path).map_err(|e| e.to_string()).and_then(|b| read_log(&b, &mut put))
        } else if name.ends_with(".ldb") || name.ends_with(".sst") {
            std::fs::read(&path).map_err(|e| e.to_string()).and_then(|b| read_table(&b, &mut put))
        } else {
            continue;
        };
        match parsed {
            Ok(()) => read_any = true,
            // One damaged file shouldn't hide what the others hold.
            Err(e) => log::warn!("[store] skipping {}: {e}", path.display()),
        }
    }
    if !read_any {
        return Err(format!("no readable LevelDB files in {}", dir.display()));
    }

    let mut by_origin: HashMap<String, HashMap<String, String>> = HashMap::new();
    for (k, (_, v)) in newest {
        let Some(v) = v else { continue }; // deleted
        let Some(rest) = k.strip_prefix(b"_") else { continue };
        let Some(nul) = rest.iter().position(|b| *b == 0) else { continue };
        let origin = String::from_utf8_lossy(&rest[..nul]).into_owned();
        let (Some(key), Some(value)) = (decode(&rest[nul + 1..]), decode(&v)) else { continue };
        if keys.contains(&key.as_str()) {
            by_origin.entry(origin).or_default().insert(key, value);
        }
    }
    Ok(by_origin
        .into_iter()
        .max_by_key(|(origin, items)| (origin.contains("tauri"), items.len()))
        .map(|(_, items)| items)
        .unwrap_or_default())
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

// ── Format pieces ───────────────────────────────────────────────────────

type Sink<'a> = dyn FnMut(&[u8], u64, Option<&[u8]>) + 'a;

/// A cursor over bytes; every read is bounds-checked.
struct Cur<'a> {
    b: &'a [u8],
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if n > self.b.len() {
            return Err("truncated".into());
        }
        let (h, t) = self.b.split_at(n);
        self.b = t;
        Ok(h)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("bad varint".into())
    }
    fn sized(&mut self) -> Result<&'a [u8], String> {
        let n = usize::try_from(self.varint()?).map_err(|e| e.to_string())?;
        self.take(n)
    }
}

/// The write-ahead log: 32 KiB blocks of records (7-byte header: checksum,
/// length, type), records reassembled from fragments into write batches.
fn read_log(b: &[u8], sink: &mut Sink) -> Result<(), String> {
    const BLOCK: usize = 32 * 1024;
    let mut pos = 0;
    let mut record = Vec::new();
    while pos < b.len() {
        let left_in_block = BLOCK - pos % BLOCK;
        if left_in_block < 7 {
            pos += left_in_block; // trailer padding
            continue;
        }
        let Some(h) = b.get(pos..pos + 7) else { break };
        let len = usize::from(u16::from_le_bytes([h[4], h[5]]));
        let kind = h[6];
        let Some(data) = b.get(pos + 7..pos + 7 + len) else { break }; // torn tail
        pos += 7 + len;
        match kind {
            1 => read_batch(data, sink)?,                    // full
            2 => record = data.to_vec(),                     // first
            3 => record.extend_from_slice(data),             // middle
            4 => {
                record.extend_from_slice(data);              // last
                read_batch(&record, sink)?;
                record.clear();
            }
            _ => pos += BLOCK - pos % BLOCK, // zero padding: rest of the block
        }
    }
    Ok(())
}

/// A write batch: sequence (u64), count (u32), then put/delete records.
fn read_batch(b: &[u8], sink: &mut Sink) -> Result<(), String> {
    let mut c = Cur { b };
    let seq = u64::from_le_bytes(c.take(8)?.try_into().unwrap());
    let count = u32::from_le_bytes(c.take(4)?.try_into().unwrap());
    for i in 0..u64::from(count) {
        match c.u8()? {
            1 => {
                let k = c.sized()?;
                let v = c.sized()?;
                sink(k, seq + i, Some(v));
            }
            0 => sink(c.sized()?, seq + i, None),
            t => return Err(format!("unknown batch record {t}")),
        }
    }
    Ok(())
}

/// A sorted table: footer → index block → data blocks of internal keys
/// (user key + 8 bytes of sequence << 8 | type).
fn read_table(b: &[u8], sink: &mut Sink) -> Result<(), String> {
    const MAGIC: u64 = 0xdb47_7524_8b80_fb57;
    if b.len() < 48 || u64::from_le_bytes(b[b.len() - 8..].try_into().unwrap()) != MAGIC {
        return Err("not a LevelDB table".into());
    }
    let mut footer = Cur { b: &b[b.len() - 48..] };
    let _metaindex = (footer.varint()?, footer.varint()?);
    let index = (footer.varint()?, footer.varint()?);
    let index = block(b, index)?;
    for (_, handle) in entries(&index)? {
        let mut h = Cur { b: &handle };
        let data = block(b, (h.varint()?, h.varint()?))?;
        for (ikey, value) in entries(&data)? {
            if ikey.len() < 8 {
                continue;
            }
            let (user, tag) = ikey.split_at(ikey.len() - 8);
            let tag = u64::from_le_bytes(tag.try_into().unwrap());
            sink(user, tag >> 8, (tag & 0xff == 1).then_some(&value[..]));
        }
    }
    Ok(())
}

/// A block's bytes, decompressed (trailer: 1 type byte + 4 checksum bytes).
fn block(file: &[u8], (offset, size): (u64, u64)) -> Result<Vec<u8>, String> {
    let start = usize::try_from(offset).map_err(|e| e.to_string())?;
    let end = start.checked_add(usize::try_from(size).map_err(|e| e.to_string())?).ok_or("bad block handle")?;
    let data = file.get(start..end).ok_or("block out of range")?;
    match file.get(end) {
        Some(0) => Ok(data.to_vec()),
        Some(1) => snap::raw::Decoder::new().decompress_vec(data).map_err(|e| format!("snappy: {e}")),
        Some(t) => Err(format!("unknown block compression {t}")),
        None => Err("block trailer out of range".into()),
    }
}

/// A block's key/value entries (keys prefix-compressed against the previous one).
fn entries(block: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, String> {
    if block.len() < 4 {
        return Err("short block".into());
    }
    let restarts = u32::from_le_bytes(block[block.len() - 4..].try_into().unwrap()) as usize;
    let end = block.len().checked_sub(4 + restarts.checked_mul(4).ok_or("bad restarts")?).ok_or("bad restarts")?;
    let mut c = Cur { b: &block[..end] };
    let mut out = Vec::new();
    let mut key: Vec<u8> = Vec::new();
    while !c.b.is_empty() {
        let shared = usize::try_from(c.varint()?).map_err(|e| e.to_string())?;
        let unshared = usize::try_from(c.varint()?).map_err(|e| e.to_string())?;
        let vlen = usize::try_from(c.varint()?).map_err(|e| e.to_string())?;
        if shared > key.len() {
            return Err("bad shared key prefix".into());
        }
        key.truncate(shared);
        key.extend_from_slice(c.take(unshared)?);
        out.push((key.clone(), c.take(vlen)?.to_vec()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// testdata/leveldb was written by a real LevelDB (rusty-leveldb, snappy
    /// on): an old token list and a camera flag compacted into a table, then
    /// superseded and deleted, a 60-entry cache that compresses, a dev-origin
    /// key, and two writes of the same key left in the log.
    #[test]
    fn reads_compressed_tables_and_the_log_newest_write_wins() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/store/testdata/leveldb");
        let got = read(
            &dir,
            &["lookout-tokens", "lookout:gallery-cache:v2", "lookout-api-base", "lookout-perm-screen", "lookout-perm-camera"],
        )
        .unwrap();
        assert_eq!(
            got.get("lookout-tokens").map(String::as_str),
            Some(r#"[{"token":"tok-1","addedAt":"2026-01-01T00:00:00Z","label":"Résumé"}]"#)
        );
        assert!(got.get("lookout:gallery-cache:v2").is_some_and(|c| c.contains(r#""s59""#)));
        assert_eq!(got.get("lookout-perm-screen").map(String::as_str), Some("1"));
        assert!(!got.contains_key("lookout-perm-camera"), "deleted");
        assert!(!got.contains_key("lookout-api-base"), "dev origin");
    }

    #[test]
    fn garbage_is_an_error_not_a_crash() {
        let mut sink = |_: &[u8], _: u64, _: Option<&[u8]>| {};
        assert!(read_table(&[0u8; 100], &mut sink).is_err());
        let _ = read_log(&[0xff; 50], &mut sink);
        let mut b = vec![0u8; 64];
        b[56..].copy_from_slice(&0xdb47_7524_8b80_fb57u64.to_le_bytes());
        b[8] = 0xff; // a varint that never ends
        let _ = read_table(&b, &mut sink);
    }
}
