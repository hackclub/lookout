//! From a chosen [`Artifact`] to a verified payload on disk: the patch when
//! this install has the right base, else (or when anything about the patch
//! goes wrong) the full artifact, resumably.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::manifest::{Artifact, Format};
use crate::{download, hash, patch};

#[derive(Debug)]
pub struct Obtained {
    /// The verified payload (`work/payload`).
    pub payload: PathBuf,
    pub via_patch: bool,
}

/// `base` is this install's payload of `installed` (its cached copy, or the
/// AppImage itself). `progress(done, total)` covers whichever download runs.
pub async fn obtain_payload(
    http: &reqwest::Client,
    art: &Artifact,
    installed: &str,
    base: Option<&Path>,
    work: &Path,
    mut progress: impl FnMut(u64, u64) + Send,
) -> Result<Obtained, String> {
    std::fs::create_dir_all(work).map_err(|e| e.to_string())?;
    let out = work.join("payload");
    let _ = std::fs::remove_file(&out);

    if let (Some(base), Some(p)) = (base, art.patch_from(installed)) {
        match try_patch(http, art, p, base, work, &out, &mut progress).await {
            Ok(()) => return Ok(Obtained { payload: out, via_patch: true }),
            Err(e) => {
                log::warn!("[update] patch from v{installed} unusable ({e}) — downloading the full update");
                let _ = std::fs::remove_file(&out);
            }
        }
    }

    let file = work.join(file_name(&art.url).unwrap_or_else(|| "full".into()));
    download::fetch_resumable(http, &art.url, &file, art.size, &art.sha256, &mut progress).await?;
    let (fmt, file2, out2, want, want_len) = (art.format, file.clone(), out.clone(), art.payload_sha256.clone(), art.payload_size);
    blocking(move || payload_from_full(fmt, &file2, &out2, &want, want_len)).await?;
    let _ = std::fs::remove_file(&file);
    Ok(Obtained { payload: out, via_patch: false })
}

async fn try_patch(
    http: &reqwest::Client,
    art: &Artifact,
    p: &crate::manifest::Patch,
    base: &Path,
    work: &Path,
    out: &Path,
    progress: &mut (impl FnMut(u64, u64) + Send),
) -> Result<(), String> {
    let (b, want_base) = (base.to_path_buf(), p.from_sha256.clone());
    let base_ok = blocking(move || Ok(hash::sha256_file(&b).map(|h| hash::same(&h, &want_base)).unwrap_or(false))).await?;
    if !base_ok {
        return Err("this install's files don't match the patch's base".into());
    }
    let pf = work.join(format!("patch-{}.zst", p.from_version.trim_start_matches('v')));
    download::fetch_resumable(http, &p.url, &pf, p.size, &p.sha256, &mut *progress).await?;
    let (b, pf2, o, want, len) = (base.to_path_buf(), pf.clone(), out.to_path_buf(), art.payload_sha256.clone(), art.payload_size);
    let r = blocking(move || {
        let base = std::fs::read(&b).map_err(|e| e.to_string())?;
        let patch = std::fs::read(&pf2).map_err(|e| e.to_string())?;
        let new = patch::apply(&base, &patch, len).map_err(|e| format!("apply: {e}"))?;
        let h = hash::sha256_hex(&new);
        if new.len() as u64 != len || !hash::same(&h, &want) {
            return Err(format!("patched result doesn't match (sha256 {h})"));
        }
        std::fs::write(&o, &new).map_err(|e| e.to_string())
    })
    .await;
    let _ = std::fs::remove_file(&pf);
    r
}

/// The payload from a verified full download.
pub fn payload_from_full(fmt: Format, file: &Path, out: &Path, want_sha: &str, want_len: u64) -> Result<(), String> {
    if fmt.payload_is_file() {
        // The download is already verified against the same hash.
        std::fs::rename(file, out).or_else(|_| std::fs::copy(file, out).map(|_| ())).map_err(|e| e.to_string())?;
        return Ok(());
    }
    let mut gz = flate2::read::GzDecoder::new(std::fs::File::open(file).map_err(|e| e.to_string())?).take(want_len + 1);
    let mut tar = Vec::with_capacity(want_len as usize);
    gz.read_to_end(&mut tar).map_err(|e| format!("unpack: {e}"))?;
    let h = hash::sha256_hex(&tar);
    if tar.len() as u64 != want_len || !hash::same(&h, want_sha) {
        return Err(format!("unpacked payload doesn't match the manifest (sha256 {h})"));
    }
    std::fs::write(out, tar).map_err(|e| e.to_string())
}

pub fn file_name(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let name = path.rsplit('/').next()?;
    (!name.is_empty() && !name.contains("..")).then(|| name.to_string())
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Patch;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn serve(files: HashMap<String, Vec<u8>>) -> (String, Arc<Mutex<Vec<String>>>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", server.server_addr().to_ip().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let path = req.url().to_string();
                l2.lock().unwrap().push(path.clone());
                let resp = match files.get(&path) {
                    Some(b) => tiny_http::Response::from_data(b.clone()),
                    None => tiny_http::Response::from_data(b"nope".to_vec()).with_status_code(404),
                };
                let _ = req.respond(resp);
            }
        });
        (addr, log)
    }

    /// Incompressible bytes, so a patch only works against its real base.
    fn bytes(n: usize, seed: u32) -> Vec<u8> {
        let mut x = (seed as u64).wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    struct Fixture {
        old: Vec<u8>,
        new: Vec<u8>,
        art: Artifact,
        files: HashMap<String, Vec<u8>>,
    }

    fn fixture(bad_patch: bool) -> Fixture {
        let old = bytes(400_000, 1);
        // A "bad" patch is a valid zstd patch made against a slightly
        // different base than the one its from_sha256 names: it applies, but
        // the bytes it copies from the base come out wrong.
        let mut patch_base = old.clone();
        if bad_patch {
            for b in &mut patch_base[100_000..200_000] {
                *b ^= 0x5a;
            }
        }
        let mut new = patch_base.clone();
        new[5000..5100].fill(9);
        new.extend_from_slice(b"version 2");
        let p = patch::create(&patch_base, &new, 9).unwrap();
        let mut files = HashMap::new();
        files.insert("/full.AppImage".to_string(), new.clone());
        files.insert("/p.zst".to_string(), p.clone());
        let art = Artifact {
            url: String::new(),
            size: new.len() as u64,
            sha256: hash::sha256_hex(&new),
            format: Format::Appimage,
            payload_size: new.len() as u64,
            payload_sha256: hash::sha256_hex(&new),
            patches: vec![Patch {
                from_version: "0.4.0".into(),
                from_sha256: hash::sha256_hex(&old),
                url: String::new(),
                size: p.len() as u64,
                sha256: hash::sha256_hex(&p),
            }],
        };
        Fixture { old, new, art, files }
    }

    fn wire(f: &mut Fixture) -> Arc<Mutex<Vec<String>>> {
        let (addr, log) = serve(f.files.clone());
        f.art.url = format!("{addr}/full.AppImage");
        f.art.patches[0].url = format!("{addr}/p.zst");
        log
    }

    #[tokio::test]
    async fn applies_patch() {
        let mut f = fixture(false);
        let log = wire(&mut f);
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("base");
        std::fs::write(&base, &f.old).unwrap();
        let o = obtain_payload(&reqwest::Client::new(), &f.art, "0.4.0", Some(&base), &d.path().join("w"), |_, _| {}).await.unwrap();
        assert!(o.via_patch);
        assert_eq!(std::fs::read(&o.payload).unwrap(), f.new);
        assert_eq!(*log.lock().unwrap(), vec!["/p.zst".to_string()]);
    }

    #[tokio::test]
    async fn patch_result_mismatch_falls_back_to_full() {
        let mut f = fixture(true);
        let log = wire(&mut f);
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("base");
        std::fs::write(&base, &f.old).unwrap();
        let o = obtain_payload(&reqwest::Client::new(), &f.art, "0.4.0", Some(&base), &d.path().join("w"), |_, _| {}).await.unwrap();
        assert!(!o.via_patch);
        assert_eq!(std::fs::read(&o.payload).unwrap(), f.new);
        assert_eq!(*log.lock().unwrap(), vec!["/p.zst".to_string(), "/full.AppImage".to_string()]);
    }

    #[tokio::test]
    async fn wrong_base_skips_patch_download() {
        let mut f = fixture(false);
        let log = wire(&mut f);
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("base");
        std::fs::write(&base, b"some other build").unwrap();
        let o = obtain_payload(&reqwest::Client::new(), &f.art, "0.4.0", Some(&base), &d.path().join("w"), |_, _| {}).await.unwrap();
        assert!(!o.via_patch);
        assert_eq!(*log.lock().unwrap(), vec!["/full.AppImage".to_string()]);
    }

    #[tokio::test]
    async fn corrupted_patch_download_falls_back() {
        let mut f = fixture(false);
        f.files.get_mut("/p.zst").unwrap()[20] ^= 0xff; // sha256 no longer matches the manifest
        wire(&mut f);
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("base");
        std::fs::write(&base, &f.old).unwrap();
        let o = obtain_payload(&reqwest::Client::new(), &f.art, "0.4.0", Some(&base), &d.path().join("w"), |_, _| {}).await.unwrap();
        assert!(!o.via_patch);
        assert_eq!(std::fs::read(&o.payload).unwrap(), f.new);
    }

    #[tokio::test]
    async fn no_patch_for_other_versions() {
        let mut f = fixture(false);
        let log = wire(&mut f);
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("base");
        std::fs::write(&base, &f.old).unwrap();
        let o = obtain_payload(&reqwest::Client::new(), &f.art, "0.3.0", Some(&base), &d.path().join("w"), |_, _| {}).await.unwrap();
        assert!(!o.via_patch);
        assert_eq!(*log.lock().unwrap(), vec!["/full.AppImage".to_string()]);
    }

    #[test]
    fn tar_gz_payload() {
        use std::io::Write;
        let d = tempfile::tempdir().unwrap();
        let tar = bytes(50_000, 3);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar).unwrap();
        let f = d.path().join("a.app.tar.gz");
        std::fs::write(&f, gz.finish().unwrap()).unwrap();
        let out = d.path().join("payload");
        payload_from_full(Format::AppTarGz, &f, &out, &hash::sha256_hex(&tar), tar.len() as u64).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), tar);
        assert!(payload_from_full(Format::AppTarGz, &f, &out, &"0".repeat(64), tar.len() as u64).is_err());
    }

    #[test]
    fn names() {
        assert_eq!(file_name("https://x/y/Lookout.AppImage?x=1").as_deref(), Some("Lookout.AppImage"));
        assert_eq!(file_name("https://x/y/"), None);
    }
}
