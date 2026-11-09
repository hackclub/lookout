//! Live source previews — `useScreenPreview` without the custom URI
//! scheme. One process-wide cache per source, shared by every view that
//! shows it; frames are grabbed through the engine (blacklist redaction and
//! PipeWire fds included) at `854×480`, JPEG quality 65, on the blocking
//! pool, then swapped in without a blink. A source is only polled while a
//! view keeps asking for it, at that view's cadence.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::gpui::{App, AppContext, Context, Entity, Global, RenderImage};
use lookout_core::CaptureSource;

pub fn key(src: &CaptureSource) -> String {
    match src {
        CaptureSource::Monitor { id } => format!("monitor/{id}"),
        CaptureSource::Window { id } => format!("window/{id}"),
        CaptureSource::PipeWire { id } => format!("pipewire/{id}"),
    }
}

struct Entry {
    image: Option<(Arc<RenderImage>, f32)>,
    fetching: bool,
    requested_at: Instant,
    last_fetch: Option<Instant>,
    /// Wanted again at this time (the next poll).
    wanted_interval: Duration,
    error: Option<String>,
}

pub struct Previews {
    entries: HashMap<String, Entry>,
}

pub struct PreviewsGlobal(pub Entity<Previews>);
impl Global for PreviewsGlobal {}

pub fn init(cx: &mut App) {
    let e = cx.new(|_| Previews { entries: HashMap::new() });
    cx.set_global(PreviewsGlobal(e));
}

pub fn entity(cx: &App) -> Entity<Previews> {
    cx.global::<PreviewsGlobal>().0.clone()
}

/// The latest frame of `src` (image, aspect w/h). When `live`, keeps it
/// refreshing every `interval`; otherwise fetches at most once.
pub fn get(src: &CaptureSource, interval: Duration, live: bool, cx: &mut App) -> Option<(Arc<RenderImage>, f32)> {
    let k = key(src);
    let previews = entity(cx);
    let (image, due) = {
        let p = previews.read(cx);
        match p.entries.get(&k) {
            Some(e) => {
                let due = !e.fetching
                    && (e.image.is_none() && e.error.is_none()
                        || (live && e.last_fetch.map(|t| t.elapsed() >= interval).unwrap_or(true)));
                (e.image.clone(), due)
            }
            None => (None, true),
        }
    };
    if due {
        start_fetch(previews, k, src.clone(), interval, cx);
    }
    image
}

/// Last failed grab, for views that would otherwise show a spinner forever.
pub fn error(src: &CaptureSource, cx: &App) -> Option<String> {
    entity(cx).read(cx).entries.get(&key(src)).and_then(|e| e.error.clone())
}

fn start_fetch(previews: Entity<Previews>, k: String, src: CaptureSource, interval: Duration, cx: &mut App) {
    let started = Instant::now();
    previews.update(cx, |p, _| {
        let e = p.entries.entry(k.clone()).or_insert(Entry {
            image: None,
            fetching: false,
            requested_at: started,
            last_fetch: None,
            wanted_interval: interval,
            error: None,
        });
        e.fetching = true;
        e.requested_at = started;
        e.wanted_interval = interval;
    });
    let core = crate::app::state(cx).core.clone();
    let fut = crate::app::tokio(cx, async move {
        tokio::task::spawn_blocking(move || {
            let shot = core.take_screenshot(src, 854, 480, 65)?;
            let bytes = lookout_core::base64_decode(&shot.base64)?;
            let img = crate::images::decode_to_render_image(&bytes).map_err(|e| e.to_string())?;
            Ok::<_, String>((img, shot.width as f32 / shot.height.max(1) as f32))
        })
        .await
        .map_err(|e| e.to_string())?
    });
    cx.spawn(async move |cx| {
        let result = fut.await;
        let interval = previews.update(cx, |p, cx| {
            // Refresh may have cleared the cache while this grab was running.
            let Some(e) = p.entries.get_mut(&k) else { return interval };
            if e.requested_at != started {
                return e.wanted_interval;
            }
            e.fetching = false;
            e.last_fetch = Some(Instant::now());
            match result {
                Ok(img) => {
                    if let Some((old, _)) = e.image.replace(img) {
                        cx.drop_image(old, None);
                    }
                    e.error = None;
                }
                Err(err) => {
                    log::warn!("[preview] {k}: {err}");
                    e.error = Some(err);
                }
            }
            cx.notify();
            e.wanted_interval
        });
        // Schedule the next poll: the fetch time counts toward the interval.
        let spent = started.elapsed();
        let wait = interval.saturating_sub(spent).max(Duration::from_millis(16));
        cx.background_executor().timer(wait).await;
        previews.update(cx, |_, cx| cx.notify());
    })
    .detach();
}

impl Previews {
    /// Forget every cached frame (the picker refreshed its source list).
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        for (_, e) in self.entries.drain() {
            if let Some((img, _)) = e.image {
                cx.drop_image(img, None);
            }
        }
    }
}
