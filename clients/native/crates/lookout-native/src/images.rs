//! Image decoding and a small URL image cache (thumbnails, program icons,
//! tip heroes). GPUI paints `RenderImage`s, which hold BGRA pixels; every
//! replaced image must be dropped from the sprite atlas or it leaks, so
//! the cache owns that bookkeeping.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::gpui::{App, AppContext, Context, Entity, Global, RenderImage};
use smallvec::smallvec;

pub fn decode_to_render_image(bytes: &[u8]) -> anyhow::Result<Arc<RenderImage>> {
    let img = image::load_from_memory(bytes)?.into_rgba8();
    Ok(rgba_to_render_image(img))
}

pub fn rgba_to_render_image(mut img: image::RgbaImage) -> Arc<RenderImage> {
    for px in img.pixels_mut() {
        px.0.swap(0, 2);
    }
    Arc::new(RenderImage::new(smallvec![image::Frame::new(img)]))
}

/// Decode and downscale to fit `max_w`×`max_h` (thumbnails at display
/// size — never keep full-size frames for a 240px card).
pub fn decode_fit(bytes: &[u8], max_w: u32, max_h: u32) -> anyhow::Result<Arc<RenderImage>> {
    let img = image::load_from_memory(bytes)?;
    let img = if img.width() > max_w || img.height() > max_h {
        img.resize(max_w, max_h, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    Ok(rgba_to_render_image(img.into_rgba8()))
}

/// [`decode_fit`] with a sharp Lanczos filter, for art shown at exactly
/// the decoded size (icons, onboarding art).
pub fn decode_fit_lanczos(bytes: &[u8], max_w: u32, max_h: u32) -> anyhow::Result<Arc<RenderImage>> {
    let img = image::load_from_memory(bytes)?;
    let img = if img.width() > max_w || img.height() > max_h {
        img.resize(max_w, max_h, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    Ok(rgba_to_render_image(img.into_rgba8()))
}

#[derive(Clone)]
pub enum Remote {
    Loading,
    Ready(Arc<RenderImage>, f32),
    Failed,
}

pub struct ImageCache {
    entries: HashMap<String, Remote>,
}

pub struct ImageCacheGlobal(pub Entity<ImageCache>);
impl Global for ImageCacheGlobal {}

pub fn init(cx: &mut App) {
    let e = cx.new(|_| ImageCache { entries: HashMap::new() });
    cx.set_global(ImageCacheGlobal(e));
    let _ = std::thread::Builder::new().name("image-cache-prune".into()).spawn(disk::prune);
}

pub fn cache(cx: &App) -> Entity<ImageCache> {
    cx.global::<ImageCacheGlobal>().0.clone()
}

/// The image at `url` if loaded; starts loading it otherwise. Views that
/// call this should `cx.observe(&images::cache(cx), ..)` to re-render when
/// it lands. The `f32` is the image's aspect ratio (w/h).
///
/// Fetched images are kept on disk ([`disk`]), so a relaunch paints them
/// without waiting on the network. A copy older than the server's
/// `max-age` (a day) is shown at once and refreshed behind it.
pub fn get(url: &str, cx: &mut App) -> Remote {
    let cache = cache(cx);
    if let Some(r) = cache.read(cx).entries.get(url) {
        return r.clone();
    }
    cache.update(cx, |c, _| {
        c.entries.insert(url.to_string(), Remote::Loading);
    });
    let url_owned = url.to_string();
    let http = crate::app::state(cx).http.clone();
    log::debug!("[image] loading {url}");
    let started = std::time::Instant::now();
    let fut = crate::app::tokio(cx, async move {
        if let Some(rest) = url_owned.strip_prefix("data:") {
            // data:image/png;base64,....
            let b64 = rest.split_once(',').map(|(_, b)| b).unwrap_or("");
            let bytes = lookout_core::base64_decode(b64).map_err(|e| anyhow::anyhow!(e))?;
            let (img, aspect) = decode_for_cache(&bytes)?;
            return anyhow::Ok((img, aspect, None));
        }
        let key = url_owned.clone();
        if let Ok(Some((bytes, fresh))) = tokio::task::spawn_blocking(move || disk::read(&key)).await {
            match decode_for_cache(&bytes) {
                Ok((img, aspect)) => {
                    let stale = (!fresh).then_some(bytes);
                    return anyhow::Ok((img, aspect, stale));
                }
                Err(e) => log::debug!("[image] cached copy unreadable, refetching {url_owned}: {e:#}"),
            }
        }
        let bytes = download(&http, &url_owned).await?;
        let (img, aspect) = decode_for_cache(&bytes)?;
        let key = url_owned.clone();
        tokio::task::spawn_blocking(move || disk::write(&key, &bytes));
        anyhow::Ok((img, aspect, None))
    });
    let url_key = url.to_string();
    cx.spawn(async move |cx| {
        let result = fut.await;
        let stale = cache.update(cx, |c, cx| {
            let (entry, stale) = match result {
                Ok((img, aspect, stale)) => {
                    log::debug!("[image] ready in {:?}: {url_key}", started.elapsed());
                    (Remote::Ready(img, aspect), stale)
                }
                Err(e) => {
                    log::warn!("[image] failed after {:?}: {url_key}: {e:#}", started.elapsed());
                    (Remote::Failed, None)
                }
            };
            c.entries.insert(url_key.clone(), entry);
            cx.notify();
            stale
        });
        if let Some(old) = stale {
            revalidate(cache, url_key, old, cx).await;
        }
    })
    .detach();
    Remote::Loading
}

/// Re-download an image whose disk copy has passed `max-age`, and swap it
/// in if it changed. A failure keeps showing the old copy.
async fn revalidate(cache: Entity<ImageCache>, url: String, old: Vec<u8>, cx: &mut gpui_kit::gpui::AsyncApp) {
    let http = cx.update(|cx| crate::app::state(cx).http.clone());
    let key = url.clone();
    let fut = crate::app::tokio_async(cx, async move {
        let bytes = download(&http, &key).await?;
        if bytes == old {
            tokio::task::spawn_blocking(move || disk::touch(&key));
            return anyhow::Ok(None);
        }
        let decoded = decode_for_cache(&bytes)?;
        tokio::task::spawn_blocking(move || disk::write(&key, &bytes));
        anyhow::Ok(Some(decoded))
    });
    match fut.await {
        Ok(Some((img, aspect))) => {
            log::debug!("[image] changed server-side: {url}");
            cache.update(cx, |c, cx| {
                // Only replace what's still on screen; an evicted entry
                // reloads the new copy from disk when it's needed again.
                if let Some(Remote::Ready(..)) = c.entries.get(&url) {
                    if let Some(Remote::Ready(old, _)) = c.entries.insert(url, Remote::Ready(img, aspect)) {
                        cx.drop_image(old, None);
                    }
                    cx.notify();
                }
            });
        }
        Ok(None) => {}
        Err(e) => log::debug!("[image] refresh failed, keeping cached copy of {url}: {e:#}"),
    }
}

/// Decode, capped at 960px a side: the largest any view draws them (a
/// 448pt gallery card on a 2x display).
fn decode_for_cache(bytes: &[u8]) -> anyhow::Result<(Arc<RenderImage>, f32)> {
    let img = image::load_from_memory(bytes)?;
    let aspect = img.width() as f32 / img.height().max(1) as f32;
    let img = if img.width() > 960 || img.height() > 960 {
        img.resize(960, 960, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    Ok((rgba_to_render_image(img.into_rgba8()), aspect))
}

async fn download(http: &reqwest::Client, url: &str) -> anyhow::Result<Vec<u8>> {
    // A handful at a time, so a gallery's worth of thumbnails reuses
    // keep-alive connections instead of opening one each at once
    // (browsers cap per-host connections the same way). On the macOS
    // CI runner six simultaneous connections stalled for 30s.
    let _permit = fetch_slots().acquire().await;
    // Network-level failures (a dropped keep-alive connection, a
    // blip) get two more tries; an HTTP error or a bad image doesn't.
    let mut attempt = 0;
    let res = loop {
        // A stalled connection is retried on a fresh one rather than
        // waited out: 10s per try, timeouts included.
        match http.get(url).timeout(std::time::Duration::from_secs(10)).send().await {
            Ok(res) => break res,
            Err(_) if attempt < 2 => {
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(400 * 3u64.pow(attempt - 1))).await;
            }
            Err(e) => return Err(e.into()),
        }
    };
    if !res.status().is_success() {
        anyhow::bail!("HTTP {}", res.status());
    }
    Ok(res.bytes().await?.to_vec())
}

/// Fetched images on disk, one file per URL (named by its SHA-256) in the
/// user cache folder. A file's mtime is when it was last fetched or
/// confirmed unchanged; files nobody has confirmed for 30 days are pruned
/// at launch.
pub mod disk {
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    /// The server's `max-age` for thumbnails.
    const FRESH_FOR: Duration = Duration::from_secs(24 * 3600);
    const KEEP_FOR: Duration = Duration::from_secs(30 * 24 * 3600);

    fn dir() -> PathBuf {
        dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join(crate::store::APP_ID).join("images")
    }

    fn path(url: &str) -> PathBuf {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(url.as_bytes());
        dir().join(hash.iter().map(|b| format!("{b:02x}")).collect::<String>())
    }

    /// The stored bytes for `url`, and whether they're still fresh.
    pub fn read(url: &str) -> Option<(Vec<u8>, bool)> {
        let p = path(url);
        let modified = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
        let bytes = std::fs::read(&p).ok()?;
        let age = SystemTime::now().duration_since(modified).unwrap_or_default();
        Some((bytes, age < FRESH_FOR))
    }

    /// Store `bytes` for `url` (written aside, then renamed into place, so
    /// a reader never sees half a file).
    pub fn write(url: &str, bytes: &[u8]) {
        let p = path(url);
        let res = std::fs::create_dir_all(dir()).and_then(|_| {
            let tmp = p.with_extension(format!("tmp{}", std::process::id()));
            std::fs::write(&tmp, bytes)?;
            std::fs::rename(&tmp, &p).inspect_err(|_| {
                let _ = std::fs::remove_file(&tmp);
            })
        });
        if let Err(e) = res {
            log::debug!("[image] couldn't cache {url}: {e}");
        }
    }

    /// The stored copy was confirmed unchanged: fresh for another day.
    pub fn touch(url: &str) {
        if let Ok(f) = std::fs::File::options().write(true).open(path(url)) {
            let _ = f.set_modified(SystemTime::now());
        }
    }

    /// Delete copies not confirmed in [`KEEP_FOR`], and stray temp files.
    pub fn prune() {
        let Ok(entries) = std::fs::read_dir(dir()) else { return };
        let now = SystemTime::now();
        let mut removed = 0;
        for e in entries.flatten() {
            let Some(age) = e.metadata().and_then(|m| m.modified()).ok().map(|m| now.duration_since(m).unwrap_or_default())
            else {
                continue;
            };
            // A temp file is a write that never finished (one in flight is
            // seconds old).
            let tmp = e.path().extension().is_some_and(|x| x.to_string_lossy().starts_with("tmp"));
            let expired = if tmp { age > Duration::from_secs(3600) } else { age > KEEP_FOR };
            if expired && std::fs::remove_file(e.path()).is_ok() {
                removed += 1;
            }
        }
        if removed > 0 {
            log::debug!("[image] pruned {removed} cached image(s)");
        }
    }
}

impl ImageCache {
    /// Whether `url` is decoded and held (as a texture).
    pub fn is_ready(&self, url: &str) -> bool {
        matches!(self.entries.get(url), Some(Remote::Ready(..)))
    }

    /// Forget an entry and release its texture (a gallery thumbnail
    /// scrolled far away; it reloads from disk when it's needed again).
    pub fn evict(&mut self, url: &str, cx: &mut Context<Self>) {
        if let Some(Remote::Ready(img, _)) = self.entries.remove(url) {
            cx.drop_image(img, None);
        }
    }
}

/// Concurrent image downloads.
fn fetch_slots() -> &'static tokio::sync::Semaphore {
    static SLOTS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    SLOTS.get_or_init(|| tokio::sync::Semaphore::new(3))
}
