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
}

pub fn cache(cx: &App) -> Entity<ImageCache> {
    cx.global::<ImageCacheGlobal>().0.clone()
}

/// The image at `url` if loaded; starts fetching it otherwise. Views that
/// call this should `cx.observe(&images::cache(cx), ..)` to re-render when
/// it lands. The `f32` is the image's aspect ratio (w/h).
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
    log::debug!("[image] fetching {url}");
    let started = std::time::Instant::now();
    let fut = crate::app::tokio(cx, async move {
        let bytes = if let Some(rest) = url_owned.strip_prefix("data:") {
            // data:image/png;base64,....
            let b64 = rest.split_once(',').map(|(_, b)| b).unwrap_or("");
            lookout_core::base64_decode(b64).map_err(|e| anyhow::anyhow!(e))?
        } else {
            // Network-level failures (a dropped keep-alive connection, a
            // blip) get two more tries; an HTTP error or a bad image doesn't.
            let mut attempt = 0;
            let res = loop {
                match http.get(&url_owned).timeout(std::time::Duration::from_secs(30)).send().await {
                    Ok(res) => break res,
                    Err(e) if attempt < 2 && !e.is_timeout() => {
                        attempt += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(400 * 3u64.pow(attempt - 1))).await;
                    }
                    Err(e) => return Err(e.into()),
                }
            };
            if !res.status().is_success() {
                anyhow::bail!("HTTP {}", res.status());
            }
            res.bytes().await?.to_vec()
        };
        let img = image::load_from_memory(&bytes)?;
        let aspect = img.width() as f32 / img.height().max(1) as f32;
        let img = if img.width() > 960 || img.height() > 960 {
            img.resize(960, 960, image::imageops::FilterType::Triangle)
        } else {
            img
        };
        anyhow::Ok((rgba_to_render_image(img.into_rgba8()), aspect))
    });
    let url_key = url.to_string();
    cx.spawn(async move |cx| {
        let result = fut.await;
        cache.update(cx, |c, cx| {
            let entry = match result {
                Ok((img, aspect)) => {
                    log::debug!("[image] ready in {:?}: {url_key}", started.elapsed());
                    Remote::Ready(img, aspect)
                }
                Err(e) => {
                    log::warn!("[image] failed after {:?}: {url_key}: {e:#}", started.elapsed());
                    Remote::Failed
                }
            };
            c.entries.insert(url_key, entry);
            cx.notify();
        });
    })
    .detach();
    Remote::Loading
}

impl ImageCache {
    /// Forget an entry (e.g. a thumbnail that changed server-side) and
    /// release its texture.
    pub fn evict(&mut self, url: &str, cx: &mut Context<Self>) {
        if let Some(Remote::Ready(img, _)) = self.entries.remove(url) {
            cx.drop_image(img, None);
        }
    }
}
