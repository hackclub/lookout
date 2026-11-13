//! First-launch onboarding: a small portrait window on the system material
//! (vibrancy on macOS, Mica on Windows 11), three pages, before the main
//! window opens.
//!
//! 1. Welcome: the app icon with Hack Club stickers (stickers.hackclub.com)
//!    springing out from behind it. Clicking a sticker poofs it and a fresh
//!    one pops into its place; clicking the icon kicks them all outward.
//! 2. Start: a film of Lookout itself (`onboarding_demo`): the cursor picks
//!    Lapse from the + menu and the recorder starts.
//! 3. Finish: the same recorder is stopped; the session drops into the
//!    gallery, turns Complete, and its time is logged to Hackatime.
//!
//! Every movement is a spring from `motion` (interruptible, so mashing
//! Continue or the dots retargets smoothly); with Reduce Motion on the
//! springs snap and only fades remain.

use std::cell::Cell;
use std::f32::consts::TAU;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::gpui::{
    AnyElement, App, AppContext, Bounds, Context, FocusHandle, Hsla, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render, RenderImage, Rgba,
    SharedString, StatefulInteractiveElement, Styled, TitlebarOptions, Window, WindowBackgroundAppearance,
    WindowBounds, WindowControlArea, WindowDecorations, WindowKind, WindowOptions, canvas, div, img, point,
    prelude::FluentBuilder, px, size,
};

use crate::app::{store, store_mut};
use crate::motion::{self, Anim, Curve, Ease, Spring};
use crate::theme::{Theme, black, hex, is_linux, is_mac, is_windows, sh, theme, white};
use crate::ui::button::{BtnSize, Button, Variant};
use crate::ui::header_bar::{HeaderBtn, HeaderBtnKind};
use crate::ui::icon::Icon;
use crate::ui::sticker::{self, Sticker};
use crate::ui::text::TextExt;
use super::onboarding_demo as demo;
use super::main_window::{WINDOW_MARGIN, linux_frame, uses_header_bar, window_corner_radius};

const W: f32 = 420.0;
const H: f32 = 620.0;
/// The hero art's box, under the title bar.
const HERO_TOP: f32 = 34.0;
const HERO_H: f32 = 368.0;
const PAGES: usize = 4;
/// The opening: a small, faint Hack Club flag with a quick shimmer, held
/// while the window settles and the stickers cut out, then the welcome.
const INTRO_MS: f64 = 2500.0;
const FLAG_W: f32 = 84.0;
const FLAG_H: f32 = FLAG_W * 184.0 / 526.0;
/// The welcome icon's centre in the hero.
const WELCOME_CY: f32 = 200.0;

/// The page slide.
const PAGE: Spring = Spring::new(280.0, 31.0);
/// Stickers popping out: bouncy (ζ ≈ 0.55), so they overshoot and settle.
const POP: Spring = Spring::new(240.0, 17.0);
/// A replacement sticker: quicker and springier still.
const REPOP: Spring = Spring::new(340.0, 15.0);
/// Hover / press scale on a sticker.
const NUDGE: Spring = Spring::new(600.0, 24.0);
/// A sticker pressed onto the card: in from big, flat with a little rebound.
const SLAP: Spring = Spring::new(380.0, 20.0);
/// Something arriving whole from just above (stageshow's arrival curve).
const ARRIVE: Ease = Ease::Bezier(0.25, 0.8, 0.25, 1.0);
/// The words rising into place.
const RISE: Ease = Ease::EASE_OUT_QUART;

/// The app icon at 2× (the macOS-shaped one: squircle and shadow baked in).
static ICON_PNG: &[u8] = include_bytes!("../../../../assets/onboarding/app-icon.png");
/// Hackatime's own logo (hackatime.hackclub.com's icon).
static HACKATIME_PNG: &[u8] = include_bytes!("../../../../assets/onboarding/hackatime.png");
/// The icon's box; the squircle inside is about 80% of it, so the icon
/// itself is ~170pt wide, as big in the window as Splice's.
const ICON_BOX: f32 = 212.0;

/// Whether to show onboarding at launch: only while `onboardingShown`
/// isn't set — new users and everyone updating from before it existed.
/// `LOOKOUT_ONBOARDING=1` forces it for development; the CI smoke run skips it.
pub fn wanted(cx: &App) -> bool {
    if std::env::var("LOOKOUT_ONBOARDING").as_deref() == Ok("1") {
        return true;
    }
    if crate::smoke::enabled() {
        return false;
    }
    !store(cx).onboarding_shown
}

/// Open the onboarding at launch, before the main window. `done` runs once
/// it is finished (the main window opens there); closing it instead quits,
/// and it shows again next launch.
pub fn open(cx: &mut App, done: impl FnOnce(&mut App) + 'static) {
    open_window(cx, Box::new(done), true);
}

/// Bring an open onboarding window to the front (a link arrived for it).
pub fn bring_forward(cx: &mut App) {
    for w in cx.windows() {
        if let Some(h) = w.downcast::<Onboarding>() {
            h.update(cx, |_, window, _| window.activate_window()).ok();
            cx.activate(true);
            return;
        }
    }
}

/// Settings → "Show onboarding": the same window over the running app.
/// Closing it just closes it. If one is already open, it comes forward.
pub fn show_again(cx: &mut App) {
    for w in cx.windows() {
        if let Some(h) = w.downcast::<Onboarding>() {
            h.update(cx, |_, window, _| window.activate_window()).ok();
            return;
        }
    }
    open_window(cx, Box::new(|_| {}), false);
}

fn open_window(cx: &mut App, done: Box<dyn FnOnce(&mut App)>, at_launch: bool) {
    // Linux with a compositor: our own frame (shadow, rounding, close
    // button) in a transparent margin, like the main window.
    let csd = is_linux() && crate::platform::desktop_appearance::client_frames();
    let pad = if csd { 2.0 * WINDOW_MARGIN } else { 0.0 };
    let bounds = Bounds::centered(None, size(px(W + pad), px(H + pad)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Welcome to Lookout".into()),
            appears_transparent: is_mac() || is_windows(),
            traffic_light_position: is_mac().then(|| point(px(14.0), px(14.0))),
        }),
        window_decorations: is_linux().then_some(if csd { WindowDecorations::Client } else { WindowDecorations::Server }),
        window_background: if is_mac() || csd {
            WindowBackgroundAppearance::Transparent
        } else if is_windows() {
            WindowBackgroundAppearance::MicaBackdrop
        } else {
            WindowBackgroundAppearance::Opaque
        },
        is_resizable: false,
        is_minimizable: true,
        kind: WindowKind::Normal,
        app_id: Some(crate::store::APP_ID.into()),
        icon: crate::platform::window_icon(),
        window_min_size: Some(size(px(W + pad), px(H + pad))),
        ..Default::default()
    };
    let done: Rc<Cell<Option<Box<dyn FnOnce(&mut App)>>>> = Rc::new(Cell::new(Some(done)));
    let fallback = done.clone();
    let opened = cx.open_window(options, move |window, cx| {
        let t = Theme::for_appearance(window.appearance()).with_accent(crate::platform::accent());
        cx.set_global(t);
        cx.new(|cx| Onboarding::new(done, at_launch, window, cx))
    });
    if let Err(e) = opened {
        // No GPU can draw a window here: the main window would fail the same
        // way, so explain and exit now (LOOKOUT-NATIVE-1C).
        if crate::platform::startup_failure::is_gpu_error(&format!("{e:#}")) {
            super::window_open_failed("welcome window", &e);
        }
        // Never strand someone without a window: go straight to the app.
        log::error!("[onboarding] couldn't open the window: {e:#}");
        if let Some(done) = fallback.take() {
            done(cx);
        }
        return;
    }
    cx.activate(true);
}

// ── Welcome: the stickers around the icon ───────────────────────────────

#[derive(Clone, Copy)]
struct Slot {
    /// Offset from the icon's centre (points).
    x: f32,
    y: f32,
    /// The sticker's visible size (points, longest side).
    size: f32,
    /// Drawn over the icon rather than tucked behind it.
    front: bool,
}

/// Hugging the icon, overlapping its edges, sizes deliberately uneven.
const SLOTS: [Slot; 7] = [
    Slot { x: -106.0, y: 2.0, size: 112.0, front: false },
    Slot { x: -48.0, y: -102.0, size: 68.0, front: false },
    Slot { x: 90.0, y: -78.0, size: 72.0, front: true },
    Slot { x: 104.0, y: 28.0, size: 106.0, front: true },
    Slot { x: -76.0, y: 86.0, size: 68.0, front: true },
    Slot { x: -128.0, y: 78.0, size: 40.0, front: true },
    Slot { x: 74.0, y: 100.0, size: 70.0, front: true },
];

/// Each sticker's fixed tilt (degrees), baked into its bitmap.
const TILTS: [f32; 15] = [-8.0, 6.0, -4.0, 9.0, -11.0, 5.0, -6.0, 10.0, -3.0, 7.0, -9.0, 4.0, -5.0, 8.0, -7.0];

struct Placed {
    art: usize,
    /// 0→1 as it springs out (overshoots past 1).
    t: Anim,
    /// It travels out from behind the icon (first appearance) rather than
    /// popping in place (a replacement).
    travels: bool,
    hover: Anim,
    press: Anim,
    /// 1 while poofing away (scales to nothing).
    gone: Anim,
    /// Idle bob phase.
    phase: f32,
}

struct Poof {
    puff: Hsla,
    x: f32,
    y: f32,
    size: f32,
    tint: Hsla,
    born: f64,
}

const POOF_MS: f64 = 560.0;

/// Real screenshots (stickers.hackclub.com's catalog, scrolled): the
/// recorder's "Latest capture" and the sessions' thumbnails.
static CAPTURES: [&[u8]; 3] = [
    include_bytes!("../../../../assets/onboarding/capture-1.jpg"),
    include_bytes!("../../../../assets/onboarding/capture-2.jpg"),
    include_bytes!("../../../../assets/onboarding/capture-3.jpg"),
];
static SCREENS: [&[u8]; 4] = [
    include_bytes!("../../../../assets/onboarding/screen-1.jpg"),
    include_bytes!("../../../../assets/onboarding/screen-2.jpg"),
    include_bytes!("../../../../assets/onboarding/screen-3.jpg"),
    include_bytes!("../../../../assets/onboarding/screen-4.jpg"),
];
static PIXEL_PNG: &[u8] = include_bytes!("../../../../assets/onboarding/capture-1-pixel.png");
static LAPSE_PNG: &[u8] = include_bytes!("../../../../assets/onboarding/lapse.png");
/// A sticker never used as a replacement (kept for the pool's variety).
const REWARD_ART: usize = 14;

pub struct Onboarding {
    focus: FocusHandle,
    /// Shown at launch (closing quits) rather than from Settings.
    at_launch: bool,
    /// Shown at launch to someone who already used Lookout before this
    /// update: the first page welcomes them to the new app instead.
    returning: bool,
    vibrant: bool,
    done: Rc<Cell<Option<Box<dyn FnOnce(&mut App)>>>>,
    page: usize,
    /// The page position (fractional while sliding).
    pos: Anim,
    /// Per page: 0→1 each time it is arrived on (the words rise in).
    arrive: [Anim; PAGES],
    arrived_at: [f64; PAGES],
    dots: [Anim; PAGES],

    icon: Option<Arc<RenderImage>>,
    /// The films' art.
    demo: demo::Art,
    stickers: Vec<Option<Sticker>>,
    icon_in: Anim,
    icon_press: Anim,
    /// Outward kick when the icon is clicked.
    kick: Anim,
    placed: Vec<Placed>,
    next_art: usize,
    hovered: Option<usize>,
    pressed: Option<Target>,
    poofs: Vec<Poof>,
    /// The welcome page's art box, as last painted (window coordinates).
    hero: Rc<Cell<Option<Bounds<Pixels>>>>,
    started: bool,
    /// When the window opened, and when the flag intro hands over.
    opened_at: f64,
    intro_end: f64,
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Sticker(usize),
    Icon,
}

impl Onboarding {
    fn new(done: Rc<Cell<Option<Box<dyn FnOnce(&mut App)>>>>, at_launch: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let vibrant = crate::platform::vibrancy::apply_onboarding(window);
        cx.observe_window_appearance(window, |_, window, cx| {
            let t = Theme::for_appearance(window.appearance()).with_accent(crate::platform::accent());
            cx.set_global(t);
            cx.notify();
        })
        .detach();
        // The Linux frame's shadow dims when the window loses focus.
        cx.observe_window_activation(window, |_, _, cx| cx.notify()).detach();
        // At launch, closing it before finishing leaves nothing open: quit.
        // Reopened from Settings, it just closes.
        window.on_window_should_close(cx, move |_, cx| {
            if at_launch {
                crate::crash::note_quit("onboarding closed");
                cx.quit();
            }
            true
        });

        // Bitmaps at the display's own pixel size: GPUI samples images
        // without mipmaps, so anything drawn much smaller than it was
        // decoded shimmers and goes jagged.
        let sf = window.scale_factor().max(1.0);
        let sticker_px = (112.0 * sf).round() as u32;
        // The stickers take a moment to cut out; the icon is up meanwhile.
        let prep = cx.background_executor().spawn(async move {
            sticker::ALL
                .iter()
                .enumerate()
                .map(|(i, art)| match sticker::prepare(art, sticker_px, TILTS[i % TILTS.len()]) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        log::warn!("[onboarding] sticker {}: {e:#}", art.name);
                        None
                    }
                })
                .collect::<Vec<_>>()
        });
        // The captures as prints at each tilt, for the thrown snapshot.
        let throw_px = (300.0 * sf).round() as u32;
        let lapse_px = (40.0 * sf).round() as u32;
        let throws = cx.background_executor().spawn(async move {
            let lapse = image::load_from_memory(LAPSE_PNG)
                .map(|i| {
                    let i = i.into_rgba8();
                    (0..=(demo::LAPSE_TILT / 2.0) as i32).map(|k| sticker::photo(&i, lapse_px, -(k as f32) * 2.0)).collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let caps = CAPTURES
                .iter()
                .map(|b| match image::load_from_memory(b) {
                    Ok(i) => {
                        let i = i.into_rgba8();
                        (0..=demo::THROW_TILT as i32).map(|d| sticker::photo(&i, throw_px, -(d as f32))).collect()
                    }
                    Err(_) => Vec::new(),
                })
                .collect::<Vec<_>>();
            (caps, lapse)
        });
        cx.spawn(async move |this, cx| {
            let (throws, lapse) = throws.await;
            this.update(cx, |o, cx| {
                o.demo.throws = throws;
                o.demo.lapse_prints = lapse;
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let stickers = prep.await;
            this.update(cx, |o, cx| {
                o.stickers = stickers;
                o.start_welcome();
                cx.notify();
            })
            .ok();
        })
        .detach();

        let mut arrive: [Anim; PAGES] = std::array::from_fn(|_| Anim::new(0.0));
        let intro = INTRO_MS as f32;
        arrive[0].set_delayed(1.0, Curve::tween(620.0, RISE), intro + 120.0);
        let now = motion::now();
        let mut icon_in = Anim::new(0.0);
        icon_in.set_delayed(1.0, Curve::Spring(Spring::new(300.0, 19.0)), intro + 60.0);
        let mut me = Self {
            focus,
            at_launch,
            // Anyone with sessions or linked programs used the old app.
            returning: at_launch && {
                let st = store(cx);
                !st.tokens.is_empty() || !st.program_links.is_empty()
            },
            vibrant,
            done,
            page: 0,
            pos: Anim::new(0.0),
            arrive,
            arrived_at: [now; PAGES],
            dots: std::array::from_fn(|i| Anim::new(if i == 0 { 1.0 } else { 0.0 })),
            icon: {
                let px_ = (ICON_BOX * sf).round() as u32;
                crate::images::decode_fit_lanczos(ICON_PNG, px_, px_).ok()
            },
            demo: {
                let fit = |b: &[u8], w: f32, h: f32| crate::images::decode_fit_lanczos(b, (w * sf) as u32, (h * sf) as u32).ok();
                demo::Art {
                    captures: CAPTURES.iter().filter_map(|b| fit(b, 312.0, 176.0)).collect(),
                    screens: SCREENS.iter().filter_map(|b| fit(b, 440.0, 149.0)).collect(),
                    lapse: fit(LAPSE_PNG, 18.0, 18.0),
                    hackatime: fit(HACKATIME_PNG, 24.0, 24.0),
                    // Nearest-neighbour blocks stay crisp: no filtering.
                    throws: Vec::new(),
                    lapse_prints: Vec::new(),
                    frames: CAPTURES.iter().filter_map(|b| fit(b, 86.0, 48.0)).collect(),
                    pixel: crate::images::decode_fit(PIXEL_PNG, 4096, 4096).ok(),
                }
            },
            stickers: Vec::new(),
            icon_in,
            icon_press: Anim::new(0.0),
            kick: Anim::new(0.0),
            placed: Vec::new(),
            next_art: SLOTS.len(),
            hovered: None,
            pressed: None,
            poofs: Vec::new(),
            hero: Rc::new(Cell::new(None)),
            started: false,
            opened_at: now,
            intro_end: now + INTRO_MS,
        };
        // Development: `LOOKOUT_ONBOARDING_PAGE=<n>` opens on page n (0-based).
        if let Some(p) = std::env::var("LOOKOUT_ONBOARDING_PAGE").ok().and_then(|v| v.parse::<usize>().ok()).filter(|p| *p < PAGES) {
            me.page = p;
            me.pos = Anim::new(p as f32);
            me.dots = std::array::from_fn(|i| Anim::new(if i == p { 1.0 } else { 0.0 }));
            me.arrive[p] = Anim::new(1.0);
            me.arrived_at[p] = now;
            me.intro_end = now;
            me.icon_in = Anim::new(1.0);
        }
        me
    }

    fn start_welcome(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        // Still on the flag: the stickers come out after it.
        let wait = (self.intro_end - motion::now()).max(0.0) as f32;
        self.placed = (0..SLOTS.len())
            .map(|i| {
                let mut t = Anim::new(0.0);
                t.set_delayed(1.0, Curve::Spring(POP), wait + 140.0 + i as f32 * 60.0);
                Placed {
                    art: i,
                    t,
                    travels: true,
                    hover: Anim::new(0.0),
                    press: Anim::new(0.0),
                    gone: Anim::new(0.0),
                    phase: i as f32 * 1.7,
                }
            })
            .collect();
    }

    /// Back on the welcome page: the icon and its stickers come out again,
    /// as on first arrival (the stickers already swapped in keep their art).
    fn replay_welcome(&mut self) {
        self.icon_in.jump(0.0);
        self.icon_in.set_delayed(1.0, Curve::Spring(Spring::new(300.0, 19.0)), 160.0);
        self.poofs.clear();
        self.hovered = None;
        for (i, p) in self.placed.iter_mut().enumerate() {
            p.travels = true;
            p.gone.jump(0.0);
            p.press.jump(0.0);
            p.hover.jump(0.0);
            p.t.jump(0.0);
            p.t.set_delayed(1.0, Curve::Spring(POP), 240.0 + i as f32 * 60.0);
        }
    }

    // ── navigation ──

    fn go(&mut self, page: usize, cx: &mut Context<Self>) {
        let page = page.min(PAGES - 1);
        if page == self.page {
            return;
        }
        self.page = page;
        self.pos.set(page as f32, Curve::Spring(PAGE));
        for (i, d) in self.dots.iter_mut().enumerate() {
            d.set(if i == page { 1.0 } else { 0.0 }, Curve::Spring(Spring::new(520.0, 34.0)));
        }
        let now = motion::now();
        self.arrive[page].jump(0.0);
        self.arrive[page].set_delayed(1.0, Curve::tween(560.0, RISE), 90.0);
        self.arrived_at[page] = now;
        if page == 0 {
            self.replay_welcome();
        }
        cx.notify();
    }

    fn next(&mut self, cx: &mut Context<Self>) {
        if motion::now() < self.intro_end {
            return;
        }
        if self.page + 1 < PAGES {
            self.go(self.page + 1, cx);
        } else {
            self.finish(cx);
        }
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        if self.page > 0 {
            self.go(self.page - 1, cx);
        }
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        let Some(done) = self.done.take() else { return };
        store_mut(cx).onboarding_shown = true;
        store(cx).save();
        // The main window first, so there's never a moment with no window.
        done(cx);
        cx.defer(|cx| {
            for w in cx.windows() {
                if let Some(h) = w.downcast::<Onboarding>() {
                    h.update(cx, |_, window, _| window.remove_window()).ok();
                }
            }
        });
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "enter" | "right" | "space" => self.next(cx),
            "left" => self.back(cx),
            _ => {}
        }
    }

    // ── welcome page: sticker hit testing and poofs ──

    /// Centre (relative to the hero box) and visible size of placed sticker `i`.
    fn sticker_geom(&self, i: usize, now: f64) -> (f32, f32, f32) {
        let p = &self.placed[i];
        let slot = SLOTS[i];
        let (cx0, cy0) = (W / 2.0, WELCOME_CY);
        let t = p.t.get();
        let travel = if p.travels { t } else { 1.0 };
        let kick = self.kick.get();
        let len = (slot.x * slot.x + slot.y * slot.y).sqrt().max(1.0);
        let (kx, ky) = (slot.x / len * kick * 16.0, slot.y / len * kick * 16.0);
        let _ = now;
        let x = cx0 + slot.x * travel + kx;
        let y = cy0 + slot.y * travel + ky;
        let scale = (if p.travels { 0.25 + 0.75 * t } else { t }).max(0.0)
            * (1.0 + 0.08 * p.hover.get() - 0.1 * p.press.get())
            * (1.0 - p.gone.get());
        (x, y, slot.size * scale)
    }

    fn hit(&self, pos: gpui_kit::gpui::Point<Pixels>) -> Option<Target> {
        let hero = self.hero.get()?;
        let x = f32::from(pos.x - hero.origin.x);
        let y = f32::from(pos.y - hero.origin.y);
        match self.page {
            0 => {
                let now = motion::now();
                let hit_sticker = |front: bool| {
                    (0..self.placed.len()).rev().find(|&i| {
                        if SLOTS[i].front != front || self.placed[i].gone.target() > 0.5 {
                            return false;
                        }
                        let (sx, sy, s) = self.sticker_geom(i, now);
                        (x - sx).hypot(y - sy) < s * 0.48
                    })
                };
                if let Some(i) = hit_sticker(true) {
                    return Some(Target::Sticker(i));
                }
                let (ix, iy) = (W / 2.0, WELCOME_CY);
                if (x - ix).abs() < ICON_BOX * 0.4 && (y - iy).abs() < ICON_BOX * 0.4 {
                    return Some(Target::Icon);
                }
                hit_sticker(false).map(Target::Sticker)
            }
            _ => None,
        }
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let hit = self.hit(ev.position);
        let hovered = match hit {
            Some(Target::Sticker(i)) => Some(i),
            _ => None,
        };
        if hovered != self.hovered {
            if let Some(old) = self.hovered.and_then(|i| self.placed.get_mut(i)) {
                old.hover.set(0.0, Curve::Spring(NUDGE));
            }
            if let Some(new) = hovered.and_then(|i| self.placed.get_mut(i)) {
                new.hover.set(1.0, Curve::Spring(NUDGE));
            }
            self.hovered = hovered;
            cx.notify();
        }
    }

    fn on_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.hit(ev.position) else { return };
        window.prevent_default();
        self.pressed = Some(target);
        match target {
            Target::Sticker(i) => self.placed[i].press.set(1.0, Curve::Spring(Spring::PRESS)),
            Target::Icon => self.icon_press.set(1.0, Curve::Spring(Spring::PRESS)),
        }
        cx.notify();
    }

    fn on_up(&mut self, ev: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(pressed) = self.pressed.take() else { return };
        let released_on = self.hit(ev.position);
        match pressed {
            Target::Sticker(i) => {
                self.placed[i].press.set(0.0, Curve::Spring(Spring::PRESS));
                if released_on == Some(pressed) {
                    self.poof(i, cx);
                }
            }
            Target::Icon => {
                self.icon_press.set(0.0, Curve::Spring(Spring::new(500.0, 14.0)));
                if released_on == Some(pressed) {
                    self.kick.set(1.0, Curve::Spring(Spring::new(900.0, 30.0)));
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_millis(90)).await;
                        this.update(cx, |o, cx| {
                            o.kick.set(0.0, Curve::Spring(Spring::new(180.0, 7.0)));
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                }
            }
        }
        cx.notify();
    }

    fn poof(&mut self, i: usize, cx: &mut Context<Self>) {
        let now = motion::now();
        let (x, y, s) = self.sticker_geom(i, now);
        let tint = self.stickers.get(self.placed[i].art).and_then(|s| s.as_ref()).map(|s| s.tint).unwrap_or([1.0; 3]);
        let tint: Hsla = Rgba { r: tint[0], g: tint[1], b: tint[2], a: 1.0 }.into();
        let puff = if theme(cx).dark { white(0.92) } else { crate::theme::mix(hex(0xd9dde4), tint, 0.15) };
        self.poofs.push(Poof { puff, x, y, size: s, tint, born: now });
        self.placed[i].gone.set(1.0, Curve::tween(130.0, Ease::Bezier(0.4, 0.0, 1.0, 1.0)));
        // The next unused sticker takes the spot (never the reward).
        let shown: Vec<usize> = self.placed.iter().map(|p| p.art).collect();
        let n = sticker::ALL.len();
        let mut art = self.next_art % n;
        for _ in 0..n {
            if !shown.contains(&art) && art != REWARD_ART && self.stickers.get(art).is_some_and(Option::is_some) {
                break;
            }
            art = (art + 1) % n;
        }
        self.next_art = art + 1;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(300)).await;
            this.update(cx, |o, cx| {
                let p = &mut o.placed[i];
                p.art = art;
                p.travels = false;
                p.gone.jump(0.0);
                p.t.jump(0.0);
                p.t.set(1.0, Curve::Spring(REPOP));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ── heroes ──

    fn sticker_el(&self, art: usize, cx_: f32, cy: f32, visible: f32, opacity: f32) -> Option<AnyElement> {
        let s = self.stickers.get(art)?.as_ref()?;
        if visible <= 0.5 {
            return None;
        }
        let long = visible / s.content_frac;
        let (w, h) = if s.w >= s.h { (long, long * s.h as f32 / s.w as f32) } else { (long * s.w as f32 / s.h as f32, long) };
        Some(
            div()
                .absolute()
                .left(px(cx_ - w / 2.0))
                .top(px(cy - h / 2.0))
                .w(px(w))
                .h(px(h))
                .opacity(opacity)
                .child(img(s.image.clone()).size_full())
                .into_any_element(),
        )
    }

    fn welcome_hero(&self, now: f64) -> AnyElement {
        let mut hero = div().relative().w(px(W)).h(px(HERO_H));
        let geoms: Vec<(f32, f32, f32)> = (0..self.placed.len()).map(|i| self.sticker_geom(i, now)).collect();
        for front in [false, true] {
            for (i, p) in self.placed.iter().enumerate() {
                if SLOTS[i].front != front {
                    continue;
                }
                let (x, y, s) = geoms[i];
                let o = p.t.get().clamp(0.0, 1.0).powf(0.5);
                if let Some(el) = self.sticker_el(p.art, x, y, s, o) {
                    hero = hero.child(el);
                }
            }
            if !front {
                // The icon, between the two layers.
                let k = self.icon_in.get();
                let scale = (0.55 + 0.45 * k) * (1.0 - 0.07 * self.icon_press.get()) * (1.0 + 0.04 * self.kick.get());
                let s = ICON_BOX * scale;
                if let Some(icon) = &self.icon {
                    hero = hero.child(
                        div()
                            .absolute()
                            .left(px(W / 2.0 - s / 2.0))
                            .top(px(WELCOME_CY - s / 2.0))
                            .size(px(s))
                            .opacity(k.clamp(0.0, 1.0))
                            .child(img(icon.clone()).size_full()),
                    );
                }
            }
        }
        for p in &self.poofs {
            hero = hero.child(poof_el(p, now));
        }
        hero.into_any_element()
    }

    fn page_el(&self, i: usize, t: &Theme, now: f64) -> AnyElement {
        if i == 1 {
            return self.how_page(i, t, now);
        }
        let (title, body): (&str, &str) = if i == 0 {
            if self.returning {
                ("Welcome to the new Lookout", "Go check out what's new.")
            } else {
                ("Welcome to Lookout", "Timelapse anything and everything.")
            }
        } else if i == 2 {
            (
                "Edit out anything",
                "Before your timelapse is published, cut out any stretch and pixelate anything\u{a0}private.",
            )
        } else if is_mac() {
            (
                "Synced to Hackatime",
                "Stop when you're done. Lapse publishes your timelapse and logs the time to Hackatime. Next, Lookout will ask to see your\u{a0}screen.",
            )
        } else {
            ("Synced to Hackatime", "Stop when you're done. Lapse publishes your timelapse and logs the time to\u{a0}Hackatime.")
        };
        let hero = if i == 0 {
            self.welcome_hero(now)
        } else if i == 2 {
            let ms = if motion::reduce_motion() { demo::edit_end() } else { self.film_ms(i, now) };
            div()
                .w(px(W))
                .h(px(HERO_H))
                .flex()
                .items_center()
                .justify_center()
                .child(demo::edit_hero(&self.demo, t, ms))
                .into_any_element()
        } else {
            let ms = self.film_ms(i, now);
            let ms = if motion::reduce_motion() { demo::sync_end() } else { ms };
            div()
                .w(px(W))
                .h(px(HERO_H))
                .flex()
                .items_center()
                .justify_center()
                .child(demo::sync_hero(&self.demo, t, ms))
                .into_any_element()
        };
        let a = self.arrive[i].get();
        let a2 = ((a - 0.12) / 0.88).clamp(0.0, 1.0);
        let rise = if motion::reduce_motion() { 0.0 } else { 1.0 };
        let probe = (i == 0).then(|| self.hero.clone());
        div()
            .absolute()
            .top_0()
            .left_0()
            .w(px(W))
            .h(px(H))
            .child(
                div()
                    .absolute()
                    .top(px(HERO_TOP))
                    .left_0()
                    .child(hero)
                    .when_some(probe, |d, probe| {
                        d.child(canvas(move |b, _, _| probe.set(Some(b)), |_, _, _, _| {}).absolute().inset_0())
                    }),
            )
            .child(
                div()
                    .absolute()
                    .top(px(HERO_TOP + HERO_H + 22.0))
                    .left_0()
                    .w(px(W))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(52.0))
                    .child(
                        div().mt(px((1.0 - a) * 10.0 * rise)).opacity(a).ts(21.0).w700().text_color(t.text_primary).child(title),
                    )
                    .child(
                        div()
                            .mt(px((1.0 - a2) * 8.0 * rise))
                            .opacity(a2)
                            .ts_lh(13.0, 1.4)
                            .text_center()
                            .text_color(t.text_secondary)
                            .child(body),
                    )
                    // The welcome page carries Hack Club's terms and code of
                    // conduct, quietly: plain links, no underline.
                    .when(i == 0, |d| {
                        let link = |id: &'static str, label: &'static str, url: &'static str| {
                            div()
                                .id(id)
                                .cursor_pointer()
                                .text_color(t.text_tertiary)
                                .hover(|s| s.text_color(t.text_secondary))
                                .child(label)
                                .on_click(move |_, _, cx| cx.open_url(url))
                        };
                        d.child(
                            div()
                                .mt(px(4.0 + (1.0 - a2) * 8.0 * rise))
                                .opacity(a2)
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .ts(12.0)
                                .child(link("terms", "Privacy & Terms", "https://hackclub.com/privacy-and-terms"))
                                .child(div().text_color(t.text_quaternary).child("·"))
                                .child(link("conduct", "Code of Conduct", "https://hackclub.com/conduct")),
                        )
                    }),
            )
            .into_any_element()
    }

    /// Milliseconds into page `i`'s film. Development:
    /// `LOOKOUT_FILM_FILE=<path>` holding a number pins it there, to step
    /// through the beats as stills.
    fn film_ms(&self, i: usize, now: f64) -> f64 {
        std::env::var("LOOKOUT_FILM_FILE")
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(now - self.arrived_at[i])
    }

    /// Page 2: how a timelapse starts — two cards, a line under each.
    fn how_page(&self, i: usize, t: &Theme, now: f64) -> AnyElement {
        let ms = if motion::reduce_motion() { demo::start_end() } else { self.film_ms(i, now) };
        let el = demo::start_page(
            &self.demo,
            t,
            ms,
            (
                "Click + and pick a program to start a\u{a0}timelapse.",
                "Lookout takes a screenshot every so often while you work. Pause any\u{a0}time.",
            ),
            self.arrive[i].get(),
        );
        div().absolute().top_0().left_0().w(px(W)).h(px(H)).child(el).into_any_element()
    }

    /// The Hack Club flag at 20%, centred, with a soft, slanted shine
    /// sweeping across it every 0.7s. GPUI can't mask a gradient to an SVG, so the
    /// shine is thin slices of a brighter copy, each clipped to its strip.
    fn flag_el(&self, t: &Theme, now: f64, reveal: f32) -> AnyElement {
        let since = (now - self.opened_at) as f32;
        let fade_in = crate::motion::Ease::EASE_OUT.apply((since / 300.0).clamp(0.0, 1.0));
        let o = fade_in * (1.0 - reveal);
        let ink = t.text_primary;
        let (x0, y0) = ((W - FLAG_W) / 2.0, (H - FLAG_H) / 2.0 - 10.0);
        let flag_svg = |c: Hsla| gpui_kit::gpui::svg().path(Icon::HackClubFlag.path()).w(px(FLAG_W)).h(px(FLAG_H)).text_color(c);
        let mut el = div()
            .absolute()
            .left(px(x0))
            .top(px(y0))
            .w(px(FLAG_W))
            .h(px(FLAG_H))
            .opacity(o)
            .child(div().absolute().inset_0().child(flag_svg(ink.opacity(0.2))));
        if !motion::reduce_motion() {
            // A soft, slanted band (leaning ~20°), sweeping every 0.7s. Drawn
            // as small cells of a brighter copy, each clipped to its cell, so
            // the band can lean across the flag's height.
            let band = 34.0;
            let period = 700.0;
            let lean = 0.36; // tan 20°
            let p = (since % period) / period;
            let span = FLAG_W + FLAG_H * lean + 2.0 * band;
            let center = -band - FLAG_H * lean / 2.0 + span * crate::motion::Ease::EASE_IN_OUT.apply(p);
            let (cw, ch) = (3.0f32, 4.0f32);
            let mut y = 0.0;
            while y < FLAG_H {
                // Higher rows sit further right: the band leans.
                let shift = (FLAG_H / 2.0 - (y + ch / 2.0)) * lean;
                let c = center + shift;
                let mut x = ((c - band) / cw).floor() * cw;
                while x < c + band {
                    if x + cw > 0.0 && x < FLAG_W {
                        let d = ((x + cw / 2.0 - c) / band).abs();
                        // A smooth bell, gentle at its edges.
                        let glow = (1.0 - d * d).max(0.0).powi(2) * 0.32;
                        if glow > 0.01 {
                            el = el.child(
                                div()
                                    .absolute()
                                    .left(px(x))
                                    .top(px(y))
                                    .w(px(cw))
                                    .h(px(ch.min(FLAG_H - y)))
                                    .overflow_hidden()
                                    .child(div().absolute().left(px(-x)).top(px(-y)).child(flag_svg(ink.opacity(glow)))),
                            );
                        }
                    }
                    x += cw;
                }
                y += ch;
            }
        }
        el.into_any_element()
    }

    fn any_animating(&mut self) -> bool {
        let mut any = motion::now() < self.intro_end + 500.0;
        any |= self.pos.tick();
        for a in self.arrive.iter_mut().chain(self.dots.iter_mut()) {
            any |= a.tick();
        }
        any |= self.icon_in.tick() | self.icon_press.tick() | self.kick.tick();
        // The films run while their page shows (the recorder's clock keeps
        // going after they settle).
        any |= self.pos.get().round() as usize != 0;
        for p in &mut self.placed {
            any |= p.t.tick() | p.hover.tick() | p.press.tick() | p.gone.tick();
        }
        let now = motion::now();
        self.poofs.retain(|p| now - p.born < POOF_MS);
        any |= !self.poofs.is_empty();
        any
    }
}

/// The app window's own drop shadow, for the miniatures standing in for it.
fn panel_shadow(t: &Theme) -> Vec<gpui_kit::gpui::BoxShadow> {
    if t.dark {
        vec![sh(0.0, 18.0, 40.0, -10.0, black(0.55)), sh(0.0, 0.0, 0.0, 1.0, black(0.4))]
    } else {
        vec![sh(0.0, 18.0, 40.0, -12.0, black(0.22)), sh(0.0, 1.0, 3.0, 0.0, black(0.06))]
    }
}

/// How far (0→1, overshooting) a spring from 0 to 1 has got after `secs`.
fn spring_progress(sp: Spring, secs: f32) -> f32 {
    let k = sp.stiffness;
    let c = sp.damping;
    let m = sp.mass;
    let omega = (k / m).sqrt();
    let zeta = c / (2.0 * (k * m).sqrt());
    if zeta >= 1.0 {
        return 1.0 - (-omega * secs).exp() * (1.0 + omega * secs);
    }
    let wd = omega * (1.0 - zeta * zeta).sqrt();
    let env = (-zeta * omega * secs).exp();
    1.0 - env * ((wd * secs).cos() + zeta * omega / wd * (wd * secs).sin())
}

/// A sticker's poof: a soft puff cloud and confetti in its colour.
fn poof_el(p: &Poof, now: f64) -> AnyElement {
    let t = (((now - p.born) / POOF_MS) as f32).clamp(0.0, 1.0);
    let out = Ease::EASE_OUT_QUART.apply(t);
    let mut layer = div().absolute().inset_0();
    for k in 0..5 {
        let a = k as f32 / 5.0 * TAU + 0.6;
        let r = p.size * (0.12 + 0.2 * out);
        let d = p.size * 0.22 * out;
        let (x, y) = (p.x + a.cos() * d, p.y + a.sin() * d - 6.0 * out);
        layer = layer.child(
            div()
                .absolute()
                .left(px(x - r))
                .top(px(y - r))
                .size(px(r * 2.0))
                .rounded_full()
                .bg(p.puff)
                .opacity((1.0 - t).powf(1.6) * 0.95),
        );
    }
    for k in 0..12 {
        let seed = (k as f32 * 2.399).sin() * 0.5 + 0.5;
        let a = k as f32 / 12.0 * TAU + seed * 0.5;
        let d = p.size * (0.45 + 0.35 * seed) * out;
        let s = 3.0 + 4.0 * seed * (1.0 - t);
        let (x, y) = (p.x + a.cos() * d, p.y + a.sin() * d + 18.0 * t * t);
        let colour = if k % 3 == 0 { p.puff } else { p.tint };
        layer = layer.child(
            div()
                .absolute()
                .left(px(x - s / 2.0))
                .top(px(y - s / 2.0))
                .size(px(s))
                .rounded_full()
                .bg(colour)
                .opacity(1.0 - t * t),
        );
    }
    layer.into_any_element()
}

impl Render for Onboarding {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.any_animating() {
            window.request_animation_frame();
        }
        let t = theme(cx).clone();
        let now = motion::now();
        let pos = self.pos.get();

        let mut pages = div().absolute().inset_0();
        for i in 0..PAGES {
            let d = i as f32 - pos;
            if d.abs() > 1.05 {
                continue;
            }
            let fade = 1.0 - (d.abs() * 0.9).min(1.0);
            pages = pages.child(div().absolute().top_0().left(px(d * W)).w(px(W)).h(px(H)).overflow_hidden().opacity(fade).child(self.page_el(i, &t, now)));
        }

        // Dots: the current one brightens.
        let mut dots = div().flex().items_center().gap(px(9.0));
        for i in 0..PAGES {
            let k = self.dots[i].get();
            let me = cx.entity();
            dots = dots.child(
                div()
                    .id(SharedString::from(format!("dot-{i}")))
                    .size(px(6.0))
                    .rounded_full()
                    .bg(if t.dark { white(0.22 + 0.63 * k) } else { black(0.14 + 0.62 * k) })
                    .on_click(move |_, _, cx| me.update(cx, |o, cx| o.go(i, cx))),
            );
        }
        let last = self.page + 1 == PAGES;
        let me = cx.entity();
        let button = Button::new("continue", if last { "Get Started" } else { "Continue" })
            .variant(Variant::Primary)
            .size(BtnSize::Md)
            .radius(16.0)
            .fixed(148.0, 32.0)
            .font_size(13.0)
            .on_click(move |_, _, cx| me.update(cx, |o, cx| o.next(cx)));

        // The intro: the UI fades up as the flag fades out.
        let reveal = if now >= self.intro_end + 400.0 {
            1.0
        } else {
            let k = ((now - (self.intro_end - 120.0)) / 400.0).clamp(0.0, 1.0) as f32;
            crate::motion::Ease::EASE_OUT.apply(k)
        };
        let flag = (reveal < 1.0).then(|| self.flag_el(&t, now, reveal));
        let caption = is_windows();
        let csd = uses_header_bar(window);
        let radius = if csd { window_corner_radius(window) } else { 0.0 };
        let at_launch = self.at_launch;
        let close = move |window: &mut Window, cx: &mut App| {
            if at_launch {
                crate::crash::note_quit("onboarding closed");
                cx.quit()
            } else {
                window.remove_window()
            }
        };
        let gnome_close = csd.then(|| {
            let right = crate::platform::desktop_appearance::desktop_appearance().controls_on_right;
            div().absolute().top(px(8.0)).when(right, |d| d.right(px(8.0))).when(!right, |d| d.left(px(8.0))).child(
                HeaderBtn::new("close", HeaderBtnKind::Control, Icon::LkHeaderClose.el(14.0, t.text_primary), move |_, window, cx| {
                    close(window, cx)
                })
                .tooltip("Close"),
            )
        });
        let content = div()
            .id("onboarding")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_up))
            .relative()
            .size_full()
            .overflow_hidden()
            // GPUI's overflow mask is rectangular, so the fill must round
            // itself or it squares off the Linux frame's corners.
            .rounded(px(radius))
            .font_family(crate::ui::text::family())
            .when(!self.vibrant, |d| d.bg(t.bg_body))
            .child(pages.opacity(reveal))
            .children(flag)
            .child(
                div()
                    .id("drag")
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(32.0))
                    .window_control_area(WindowControlArea::Drag)
                    // Linux has no OS-hit-tested caption: move the window ourselves.
                    .when(csd, |d| {
                        d.on_mouse_down(MouseButton::Left, |_, window, cx| {
                            cx.stop_propagation();
                            window.start_window_move();
                        })
                        .on_mouse_down(MouseButton::Right, |ev, window, _| window.show_window_menu(ev.position))
                    }),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(px(40.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(24.0))
                    .opacity(reveal)
                    .child(dots)
                    .child(button),
            )
            .when(caption, |d| {
                d.child(
                    div().absolute().top_0().right_0().flex().child(
                        HeaderBtn::icon("close", HeaderBtnKind::Action, Icon::LkWinClose, 10.0, move |_, window, cx| close(window, cx))
                            .tooltip("Close")
                            .caption()
                            .danger(),
                    ),
                )
            })
            .children(gnome_close);
        if csd {
            let active = if window.is_window_active() { 1.0 } else { 0.0 };
            linux_frame(content.into_any_element(), &t, active, window)
        } else {
            content.into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_progress_settles_and_overshoots() {
        assert!(spring_progress(POP, 0.0).abs() < 1e-6);
        assert!((spring_progress(POP, 3.0) - 1.0).abs() < 1e-3);
        let peak = (1..200).map(|i| spring_progress(POP, i as f32 / 200.0)).fold(0.0f32, f32::max);
        assert!(peak > 1.05, "{peak}");
    }

    #[test]
    fn assets_fit() {
        assert!(SLOTS.len() < sticker::ALL.len());
        assert!(REWARD_ART < sticker::ALL.len() && REWARD_ART >= SLOTS.len());
        assert_eq!(TILTS.len(), sticker::ALL.len());
        for b in CAPTURES {
            assert!(crate::images::decode_fit(b, 640, 360).is_ok());
        }
    }
}
