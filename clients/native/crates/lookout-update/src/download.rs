//! Resumable downloads. Bytes land in `<dest>.part`; an interrupted
//! download picks up where it stopped with an HTTP `Range` request (a
//! server that ignores it and answers 200 just restarts the file). Only a
//! complete file whose size and SHA-256 match the signed manifest is
//! renamed to `dest`; a hash mismatch deletes the `.part`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::hash;

pub fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// A small file (manifest, signature) into memory, capped at `max` bytes.
pub async fn fetch_small(http: &reqwest::Client, url: &str, max: usize) -> Result<Vec<u8>, String> {
    let mut res = http.get(url).timeout(Duration::from_secs(30)).send().await.map_err(|e| format!("{url}: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("{url}: HTTP {}", res.status()));
    }
    let mut out = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| e.to_string())? {
        out.extend_from_slice(&chunk);
        if out.len() > max {
            return Err(format!("{url}: larger than {max} bytes"));
        }
    }
    Ok(out)
}

/// Download `url` to `dest`, resuming `<dest>.part` if present.
/// `progress(done, total)` is called as bytes arrive.
pub async fn fetch_resumable(
    http: &reqwest::Client,
    url: &str,
    dest: &Path,
    size: u64,
    sha256: &str,
    mut progress: impl FnMut(u64, u64) + Send,
) -> Result<(), String> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    if dest.exists() && hash::sha256_file(dest).is_ok_and(|h| hash::same(&h, sha256)) {
        progress(size, size);
        return Ok(());
    }
    let part = part_path(dest);
    let mut have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if have > size {
        have = 0;
    }
    // Two rounds at most: a resume the server refuses (416) restarts once.
    for _ in 0..2 {
        if have < size {
            match fetch_range(http, url, &part, have, size, &mut progress).await? {
                Fetched::Ok => {}
                Fetched::RangeRefused => {
                    have = 0;
                    let _ = std::fs::remove_file(&part);
                    continue;
                }
            }
        }
        let got = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if got != size {
            // Keep the .part: the next attempt resumes from here.
            return Err(format!("download ended early ({got} of {size} bytes)"));
        }
        progress(size, size);
        let h = hash::sha256_file(&part).map_err(|e| e.to_string())?;
        if !hash::same(&h, sha256) {
            let _ = std::fs::remove_file(&part);
            return Err(format!("download hash mismatch (got {h})"));
        }
        std::fs::rename(&part, dest).map_err(|e| e.to_string())?;
        return Ok(());
    }
    Err("server refused to resume the download".into())
}

enum Fetched {
    Ok,
    RangeRefused,
}

async fn fetch_range(
    http: &reqwest::Client,
    url: &str,
    part: &Path,
    have: u64,
    size: u64,
    progress: &mut (impl FnMut(u64, u64) + Send),
) -> Result<Fetched, String> {
    let mut req = http.get(url).timeout(Duration::from_secs(60 * 60));
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut res = req.send().await.map_err(|e| format!("download: {e}"))?;
    let status = res.status().as_u16();
    let mut start = have;
    match status {
        206 => {
            // `Content-Range: bytes <start>-<end>/<total>` must start where we are.
            let ok = res
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("bytes "))
                .and_then(|v| v.split('-').next())
                .and_then(|v| v.trim().parse::<u64>().ok())
                == Some(have);
            if !ok {
                return Ok(Fetched::RangeRefused);
            }
        }
        200 => start = 0,
        416 => return Ok(Fetched::RangeRefused),
        s => return Err(format!("download: HTTP {s}")),
    }
    let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(start == 0).append(start > 0).open(part).map_err(|e| e.to_string())?;
    if start > 0 {
        log::info!("[update] resuming download at {start} of {size} bytes");
    }
    let mut done = start;
    progress(done, size);
    loop {
        match res.chunk().await {
            Ok(Some(chunk)) => {
                if done + chunk.len() as u64 > size {
                    drop(f);
                    let _ = std::fs::remove_file(part);
                    return Err("download is larger than the manifest says".into());
                }
                f.write_all(&chunk).map_err(|e| e.to_string())?;
                done += chunk.len() as u64;
                progress(done, size);
            }
            Ok(None) => break,
            // A dropped connection keeps what arrived for the next resume.
            Err(e) => {
                let _ = f.flush();
                return Err(format!("download interrupted at {done} of {size} bytes: {e}"));
            }
        }
    }
    f.flush().map_err(|e| e.to_string())?;
    Ok(Fetched::Ok)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A tiny HTTP server for `data` that honours `Range` and, for the first
    /// `drop_first` requests, sends only half the body (with a matching
    /// Content-Length, like a proxy that gave up).
    pub fn serve(data: Vec<u8>, drop_first: usize, honour_range: bool) -> (String, Arc<AtomicUsize>, Arc<parking::Ranges>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", server.server_addr().to_ip().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let ranges = Arc::new(parking::Ranges::default());
        let (h, r) = (hits.clone(), ranges.clone());
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let n = h.fetch_add(1, Ordering::SeqCst);
                let range = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Range"))
                    .and_then(|h| h.value.as_str().strip_prefix("bytes=").map(|s| s.trim_end_matches('-').parse::<usize>().unwrap()));
                r.0.lock().unwrap().push(range);
                let (start, code) = match range {
                    Some(s) if honour_range => (s, 206),
                    _ => (0, 200),
                };
                let mut body = data[start..].to_vec();
                if n < drop_first {
                    body.truncate(body.len() / 2);
                }
                let mut resp = tiny_http::Response::from_data(body).with_status_code(code);
                if code == 206 {
                    resp = resp.with_header(
                        tiny_http::Header::from_bytes("Content-Range", format!("bytes {start}-{}/{}", data.len() - 1, data.len())).unwrap(),
                    );
                }
                let _ = req.respond(resp);
            }
        });
        (addr, hits, ranges)
    }

    pub mod parking {
        #[derive(Default)]
        pub struct Ranges(pub std::sync::Mutex<Vec<Option<usize>>>);
    }

    fn data() -> Vec<u8> {
        (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect()
    }

    #[tokio::test]
    async fn resumes_with_range() {
        let d = data();
        let sha = hash::sha256_hex(&d);
        let (url, hits, ranges) = serve(d.clone(), 1, true);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.bin");
        let http = reqwest::Client::new();
        // First attempt gets half and stops short; the .part is kept.
        let e = fetch_resumable(&http, &format!("{url}/a"), &dest, d.len() as u64, &sha, |_, _| {}).await.unwrap_err();
        assert!(e.contains("ended early"), "{e}");
        let half = std::fs::metadata(part_path(&dest)).unwrap().len();
        assert_eq!(half, d.len() as u64 / 2);
        // Second attempt asks for the rest only.
        fetch_resumable(&http, &format!("{url}/a"), &dest, d.len() as u64, &sha, |_, _| {}).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), d);
        assert!(!part_path(&dest).exists());
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert_eq!(ranges.0.lock().unwrap().clone(), vec![None, Some(half as usize)]);
        // Already complete: no request at all.
        fetch_resumable(&http, &format!("{url}/a"), &dest, d.len() as u64, &sha, |_, _| {}).await.unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn server_without_range_restarts() {
        let d = data();
        let sha = hash::sha256_hex(&d);
        let (url, _, _) = serve(d.clone(), 0, false);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.bin");
        std::fs::write(part_path(&dest), &d[..1000]).unwrap();
        fetch_resumable(&reqwest::Client::new(), &url, &dest, d.len() as u64, &sha, |_, _| {}).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), d);
    }

    #[tokio::test]
    async fn hash_mismatch_discards_part() {
        let d = data();
        let (url, _, _) = serve(d.clone(), 0, true);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.bin");
        let e = fetch_resumable(&reqwest::Client::new(), &url, &dest, d.len() as u64, &"0".repeat(64), |_, _| {}).await.unwrap_err();
        assert!(e.contains("mismatch"), "{e}");
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
    }

    #[tokio::test]
    async fn oversized_body_rejected() {
        let d = data();
        let (url, _, _) = serve(d.clone(), 0, true);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.bin");
        let e = fetch_resumable(&reqwest::Client::new(), &url, &dest, 1000, &"0".repeat(64), |_, _| {}).await.unwrap_err();
        assert!(e.contains("larger"), "{e}");
    }
}
