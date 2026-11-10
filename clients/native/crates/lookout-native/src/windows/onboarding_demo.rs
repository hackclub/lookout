//! The onboarding's "how it works" art: Lookout's own UI, cropped to the
//! part each story is about, moving with the app's own curves.
//!
//! - **Start** (two cards, a line under each): a close-up of the + and its
//!   menu over the gallery's loading skeleton. The cursor rests on + until its "Start" tooltip shows, clicks,
//!   the add menu opens (Lapse, then Start from link — `ui/menu.rs`), it
//!   picks Lapse and "Starting Lapse…" shows. Below, your screen — a
//!   capture filling the card, running off its right edge — wakes as a
//!   result: the dim lifts, the recording pill's dot turns red and its
//!   clock runs, and each capture flashes the shutter and rolls the count.
//! - **Sync** (one picture): the finished session's gallery card with
//!   Hackatime beside it. The badge turns Complete, the session's time lifts
//!   off the card as a chip and travels into Hackatime's total for today,
//!   which rolls up while today's bar grows. No cursor: the system does it.
//!
//! Both are pure functions of the milliseconds since their page arrived:
//! reproducible frames, and Reduce Motion shows the last one. Cursor travel
//! is paced like a person (a shallow arc, `420 + 1.3·distance` ms within
//! 500–1000, a skewed minimum-jerk profile, a beat of rest before each click,
//! a 100ms press to 0.9 let go over 260ms);
//! arrivals use `cubic-bezier(.25,.8,.25,1)`, travel `(.45,0,.1,1)`.

use std::sync::Arc;

use gpui_kit::gpui::{
    AnyElement, Hsla, IntoElement, ObjectFit, ParentElement, RenderImage, Styled, StyledImage, div, img,
    prelude::FluentBuilder, px, svg,
};

use crate::motion::{Ease, Spring};
use crate::theme::{self, Theme, black, hex, sh, white};
use crate::ui::icon::Icon;
use crate::ui::feedback::NumberFlow;
use crate::ui::spinner::{Spinner, SpinnerSize};
use crate::ui::text::TextExt;

/// Page geometry (the onboarding window is 420 wide).
const W: f32 = 420.0;
const CARD_X: f32 = 36.0;
const CARD_W: f32 = W - CARD_X * 2.0;
const A_Y: f32 = 44.0;
const A_H: f32 = 160.0;
const CAP1_Y: f32 = A_Y + A_H + 18.0;
const B_Y: f32 = CAP1_Y + 62.0;
const B_H: f32 = 156.0;
const CAP2_Y: f32 = B_Y + B_H + 18.0;
const INSET: f32 = 18.0;

/// stageshow's arrival and travel curves, and the release of a press.
const ARRIVE: Ease = Ease::Bezier(0.25, 0.8, 0.25, 1.0);
const TRAVEL: Ease = Ease::Bezier(0.45, 0.0, 0.1, 1.0);
const LET_GO: Ease = Ease::Bezier(0.22, 1.0, 0.36, 1.0);

/// Hackatime's red (its logo).
fn hackatime_red() -> Hsla {
    hex(0xf05a4a)
}

pub struct Art {
    pub captures: Vec<Arc<RenderImage>>,
    /// Wide crops of the same screen, for the recording card.
    pub screens: Vec<Arc<RenderImage>>,
    pub lapse: Option<Arc<RenderImage>>,
    pub hackatime: Option<Arc<RenderImage>>,
    /// Captures at filmstrip-frame size.
    pub frames: Vec<Arc<RenderImage>>,
    /// Each capture as a print tilted 0°, −1°, … −THROW_TILT°, for the
    /// snapshot thrown into the count (filled in once prepared).
    pub throws: Vec<Vec<crate::ui::sticker::Sticker>>,
    /// Lapse's icon at each tilt 0°, −2°, … −LAPSE_TILT°, for its throw.
    pub lapse_prints: Vec<crate::ui::sticker::Sticker>,
    /// The first capture, pixelated: what a mask shows.
    pub pixel: Option<Arc<RenderImage>>,
}

// ── Timing helpers ──────────────────────────────────────────────────────

/// 0→1 over `dur` ms from `t0`.
fn tw(ms: f64, t0: f64, dur: f64, ease: Ease) -> f32 {
    if ms <= t0 {
        return 0.0;
    }
    ease.apply(((ms - t0) / dur).min(1.0) as f32)
}

/// A spring from 0 to 1 started at `t0`.
fn sp(ms: f64, t0: f64, s: Spring) -> f32 {
    if ms <= t0 {
        return 0.0;
    }
    let secs = ((ms - t0) / 1000.0) as f32;
    let (k, c, m) = (s.stiffness, s.damping, s.mass);
    let omega = (k / m).sqrt();
    let zeta = c / (2.0 * (k * m).sqrt());
    if zeta >= 1.0 {
        return 1.0 - (-omega * secs).exp() * (1.0 + omega * secs);
    }
    let wd = omega * (1.0 - zeta * zeta).sqrt();
    let env = (-zeta * omega * secs).exp();
    1.0 - env * ((wd * secs).cos() + zeta * omega / wd * (wd * secs).sin())
}

#[derive(Clone, Copy)]
struct Move {
    t0: f64,
    from: (f32, f32),
    to: (f32, f32),
}

impl Move {
    fn dur(&self) -> f64 {
        let d = (self.to.0 - self.from.0).hypot(self.to.1 - self.from.1) as f64;
        // Unhurried, like a person: about half a second for a short hop,
        // up to a second across the card.
        (420.0 + d * 1.3).clamp(500.0, 1000.0)
    }
    fn end(&self) -> f64 {
        self.t0 + self.dur()
    }
}

/// How a hand travels: a minimum-jerk profile (the smoothest motion between
/// two rests, which is what people's reaches approximate), skewed so it
/// gets going quickly and spends longer settling onto the target.
fn hand_ease(t: f32) -> f32 {
    let u = t.clamp(0.0, 1.0).powf(0.78);
    u * u * u * (10.0 - 15.0 * u + 6.0 * u * u)
}

/// A point on a shallow arc from `from` to `to` at eased progress `e`.
fn arc(from: (f32, f32), to: (f32, f32), e: f32, max_bow: f32) -> (f32, f32) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let dist = dx.hypot(dy).max(0.001);
    let bow = (dist * 0.11).min(max_bow);
    let lift = (std::f32::consts::PI * e).sin() * bow;
    let (nx, ny) = (-dy / dist, dx / dist);
    (from.0 + dx * e + nx * lift, from.1 + dy * e + ny * lift)
}

/// Where the hand is, like the kit's `move`.
fn cursor_at(moves: &[Move], ms: f64) -> (f32, f32) {
    let mut at = moves.first().map(|m| m.from).unwrap_or((0.0, 0.0));
    for m in moves {
        if ms < m.t0 {
            break;
        }
        let p = (((ms - m.t0) / m.dur()).min(1.0)) as f32;
        at = arc(m.from, m.to, hand_ease(p), 32.0);
    }
    at
}

/// How far pressed (0 up, 1 fully down) for clicks at `clicks`.
fn pressed(ms: f64, clicks: &[f64]) -> f32 {
    clicks
        .iter()
        .map(|&c| tw(ms, c, 80.0, Ease::EASE_OUT) - tw(ms, c + 100.0, 260.0, LET_GO))
        .fold(0.0, f32::max)
}

// ── Start ───────────────────────────────────────────────────────────────

/// The top card shows the gallery at the app's true sizes (no zoom: one
/// scale for everything in it).
const Z: f32 = 1.0;
/// The header's round + (`HeaderBtn` Action: 30pt, 17pt glyph).
const BTN: f32 = 30.0;
/// The + in its place at the top right.
const PLUS: (f32, f32) = (CARD_W - 16.0 - BTN / 2.0, 16.0 + BTN / 2.0);
/// The + in its place at the top right; the add menu right-aligned under it.
/// The add menu as `ui/menu.rs` draws it on this platform.
#[derive(Clone, Copy, PartialEq)]
enum MenuLook {
    /// GTK-style popover: a tail pointing at the +, gradient, 12pt radius.
    Linux,
    /// macOS and Windows: a plain panel under the +, 14pt radius.
    Panel,
}

fn menu_look() -> MenuLook {
    if crate::theme::is_linux() { MenuLook::Linux } else { MenuLook::Panel }
}

/// The real menu's minimum width, gap and tail (`ui/menu.rs`).
const MENU_W: f32 = 220.0;
const MENU_GAP: f32 = 6.0;
const TAIL_W: f32 = 20.0;
const TAIL_H: f32 = 10.0;
const TAIL_INSET: f32 = 14.0;
const ROW_H: f32 = 32.0;

/// The menu's resting box `(x, y, w, h)` in card coordinates, and the
/// point it grows from (the tail's tip, or the panel's top-right corner).
fn menu_geom() -> ((f32, f32, f32, f32), (f32, f32)) {
    let below = PLUS.1 + BTN / 2.0;
    match menu_look() {
        MenuLook::Linux => {
            // Shifted right so the tail sits TAIL_INSET from the corner,
            // its tip under the + (menu.rs: `shift`).
            let right = PLUS.0 + TAIL_INSET + TAIL_W / 2.0;
            let top = below + MENU_GAP + TAIL_H;
            let h = 6.0 + ROW_H + 1.0 + 11.0 + 1.0 + ROW_H + 6.0;
            ((right - MENU_W, top, MENU_W, h), (PLUS.0, below + MENU_GAP))
        }
        MenuLook::Panel => {
            let right = PLUS.0 + BTN / 2.0;
            let top = below + MENU_GAP;
            let h = 6.0 + ROW_H + 1.0 + 9.0 + 1.0 + ROW_H + 6.0;
            ((right - MENU_W, top, MENU_W, h), (right, top))
        }
    }
}

/// Card coordinates to page coordinates for the top card.
const fn in_a(p: (f32, f32)) -> (f32, f32) {
    (CARD_X + p.0, A_Y + p.1)
}

const S_REST: (f32, f32) = (W - 26.0, CAP1_Y + 34.0);
const S_AWAY: (f32, f32) = (W - 24.0, CAP2_Y + 56.0);

fn s_moves() -> [Move; 3] {
    let plus = in_a(PLUS);
    let ((mx, my, mw, _), _) = menu_geom();
    let lapse = in_a((mx + mw - 52.0, my + 6.0 + ROW_H / 2.0 + 1.0));
    let m1 = Move { t0: 650.0, from: S_REST, to: plus };
    // It rests on + long enough for the tooltip to say what + is.
    let c1 = m1.end() + 900.0;
    let m2 = Move { t0: c1 + 100.0 + 750.0, from: plus, to: lapse };
    let c2 = m2.end() + 420.0;
    let m3 = Move { t0: c2 + 320.0, from: lapse, to: S_AWAY };
    [m1, m2, m3]
}
fn s_clicks() -> [f64; 2] {
    let m = s_moves();
    [m[0].end() + 900.0, m[1].end() + 420.0]
}
/// When the recorder starts (the session has opened).
fn s_rec() -> f64 {
    s_clicks()[1] + 1300.0
}
pub fn start_end() -> f64 {
    // Four captures in: the clock has visibly slowed by then.
    s_rec() + clock_inverse(40.0) * 1000.0 + 700.0
}

/// How far Lapse's icon tilts mid-throw (degrees; a print every 2°).
pub const LAPSE_TILT: f32 = 24.0;
/// Lapse's icon size in its throw (points).
const LAPSE_SIZE: f32 = 34.0;

/// How far the thrown snapshot tilts (degrees, one print per degree).
pub const THROW_TILT: f32 = 10.0;

/// One capture every 10 seconds of recording.
const CAPTURE_EVERY_S: f64 = 10.0;
/// How fast the shown clock starts (×), and how quickly it eases to real
/// time (seconds).
const CLOCK_BOOST: f64 = 9.0;
const CLOCK_TAU: f64 = 6.0;

/// The recorder's clock (seconds) `t` seconds after it starts: running at
/// `1 + 9·e^(−t/6)` × real time — 10× at first so the captures come
/// quickly, easing down toward real speed, so the 10-second rhythm of the
/// captures shows in how the clock slows between them.
fn clock_at(t: f64) -> f64 {
    let t = t.max(0.0);
    t + CLOCK_BOOST * CLOCK_TAU * (1.0 - (-t / CLOCK_TAU).exp())
}

/// When (seconds after start) the clock reads `c`.
fn clock_inverse(c: f64) -> f64 {
    let (mut lo, mut hi) = (0.0f64, c.max(0.0));
    for _ in 0..50 {
        let mid = (lo + hi) / 2.0;
        if clock_at(mid) < c { lo = mid } else { hi = mid }
    }
    hi
}

/// Captures so far `since` ms after recording started, and when (ms after
/// start) the latest one landed: one at each 10-second mark on the clock.
fn shots_at(since: f64) -> (usize, f64) {
    if since <= 0.0 {
        return (0, 0.0);
    }
    let shots = (clock_at(since / 1000.0) / CAPTURE_EVERY_S).floor() as usize;
    (shots, clock_inverse(shots as f64 * CAPTURE_EVERY_S) * 1000.0)
}

pub fn start_page(a: &Art, t: &Theme, ms: f64, captions: (&str, &str), words: f32) -> AnyElement {
    let moves = s_moves();
    let clicks = s_clicks();
    let rec = s_rec();

    // ── The header and the + menu ──
    let on_plus = ms >= moves[0].end() - 40.0 && ms < moves[1].t0 + 40.0;
    let plus_hover = if on_plus { tw(ms, moves[0].end() - 40.0, 100.0, Ease::EASE_OUT) } else { 0.0 };
    let plus_press = pressed(ms, &clicks[..1]);
    // The tooltip shows after the hover delay and goes on the click.
    let tip = if ms < clicks[0] { tw(ms, moves[0].end() + 450.0, 120.0, Ease::EASE_OUT) } else { 0.0 };
    let open_at = clicks[0] + 100.0;
    let close_at = clicks[1] + 220.0;
    // A true scale (text, icons and padding included), from the tail's tip
    // or the panel's corner, on the platform's own spring; on the way out
    // it gives back a little scale as it fades.
    let (spring, open_fade, close_ms) = match menu_look() {
        MenuLook::Linux => (Spring::new(900.0, 44.0), 120.0, 120.0),
        MenuLook::Panel => (Spring::new(800.0, 40.0), 150.0, 170.0),
    };
    let menu_s = (0.82 + 0.18 * sp(ms, open_at, spring)) * (1.0 - 0.04 * tw(ms, close_at, close_ms, Ease::EASE_OUT));
    let menu_o = tw(ms, open_at, open_fade, Ease::EASE_OUT) * (1.0 - tw(ms, close_at, close_ms, Ease::EASE_OUT));
    let lapse_hl = tw(ms, moves[1].end() - 60.0, 120.0, Ease::EASE_OUT) + 0.8 * pressed(ms, &clicks[1..]);
    let pill_in = close_at + 60.0;
    let pill_o = tw(ms, pill_in, 300.0, Ease::MOTION_DEFAULT) * (1.0 - tw(ms, rec + 200.0, 160.0, Ease::EASE_OUT));
    let pill_y = 8.0 * (1.0 - sp(ms, pill_in, Spring::MOTION_XY));

    // The + at rest wears the header's hover fill, so alone it still reads
    // as a button; hover and press step it up.
    let plus = {
        let bg = theme::mix(theme::mix(t.headerbar_control, t.headerbar_control_hover, plus_hover), t.border_hover, plus_press);
        let fg = theme::mix(t.text_secondary, t.text_primary, plus_hover.max(plus_press));
        div()
            .absolute()
            .left(px(PLUS.0 - BTN / 2.0))
            .top(px(PLUS.1 - BTN / 2.0))
            .size(px(BTN))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(bg)
            .child(Icon::PlusBold.el(17.0, fg))
    };
    // The gallery as it loads (`screens/gallery.rs`, `gallery_skeleton`),
    // held still: a session card's thumbnail block, its own corners rounded,
    // running off the card's foot under the +.
    let skeleton = div()
        .absolute()
        .left(px(16.0))
        .top(px(PLUS.1 + BTN / 2.0 + 14.0))
        .w(px(CARD_W - 32.0))
        .h(px((CARD_W - 32.0) * 9.0 / 16.0))
        .rounded(px(12.0))
        .bg(t.skeleton_bg);
    let mut start = plate(t, CARD_X, A_Y, CARD_W, A_H).child(skeleton).child(plus);
    if tip > 0.001 {
        start = start.child(
            div()
                .absolute()
                .left(px(PLUS.0 - 24.0))
                .top(px(PLUS.1 + BTN / 2.0 + 8.0))
                .opacity(tip)
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .bg(t.bg_panel)
                .border_1()
                .border_color(t.border_default)
                .shadow(vec![sh(0.0, 4.0, 10.0, -2.0, black(0.25))])
                .ts(12.0)
                .text_color(t.text_primary)
                .child("Start"),
        );
    }
    if menu_o > 0.001 {
        start = start.child(add_menu(a, t, menu_s, menu_o, lapse_hl));
    }
    if pill_o > 0.001 {
        start = start.child(
            div()
                .absolute()
                .bottom(px(12.0 - pill_y))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .opacity(pill_o.clamp(0.0, 1.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .px(px(14.0))
                        .py(px(8.0))
                        .rounded(px(10.0))
                        .border_1()
                        .border_color(t.border_default)
                        .bg(t.bg_panel)
                        .text_color(t.text_secondary)
                        .ts(12.0)
                        .shadow(vec![sh(0.0, 4.0, 18.0, 0.0, black(0.18))])
                        .child(Spinner::new(SpinnerSize::Sm))
                        .child("Starting Lapse…"),
                ),
        );
    }

    // ── Lookout's recorder, waking ──
    // The real recorder page (`screens/recorder.rs`): the status card —
    // dot, clock, capture count — over the "Latest capture" preview, which
    // runs off the card's foot. Idle it's dimmed at 0:00. Started, the dot
    // turns red and the clock runs; each capture is a camera moment: the
    // shutter flashes white over the preview, the preview dips and springs
    // back, and a snapshot of the frame shrinks away into the count, which
    // rolls (the app's NumberFlow) as it lands.
    let live = tw(ms, rec, 420.0, Ease::EASE_OUT);
    let rec_ms = (ms - rec).max(0.0);
    let (shots, latest_at) = shots_at(ms - rec);
    let latest_at = rec + latest_at;
    let since = ms - latest_at;
    let shot = shots > 0;
    // The shutter: up to white in a few frames, held a beat, then let go.
    let flash = if shot {
        0.85 * tw(ms, latest_at - 40.0, 40.0, Ease::Linear) * (1.0 - tw(ms, latest_at + 70.0, 460.0, Ease::EASE_OUT))
    } else {
        0.0
    };
    let dip = if shot { 1.0 - 0.035 * (1.0 - sp(ms, latest_at, Spring::new(520.0, 18.0))) } else { 1.0 };
    let fly = if shot { tw(ms, latest_at + 60.0, 560.0, TRAVEL) } else { 1.0 };
    // The count changes as the snapshot lands.
    let counted = if shot && since < 520.0 { shots - 1 } else { shots };
    let clock = crate::fmt::format_time(clock_at(rec_ms / 1000.0) as i64);
    let n = a.captures.len().max(1);

    let (sx, sy, sw, sh_) = (16.0, 14.0, CARD_W - 32.0, 52.0);
    let status = div()
        .absolute()
        .left(px(sx))
        .top(px(sy))
        .w(px(sw))
        .h(px(sh_))
        .flex()
        .items_center()
        .justify_between()
        .px(px(16.0))
        .bg(t.bg_surface)
        .rounded(px(8.0))
        .border_1()
        .border_color(t.border_default)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(div().size(px(10.0)).rounded_full().bg(theme::mix(t.status_neutral, theme::status::danger(), live)).flex_none())
                .child(div().ts(26.0).w700().text_color(t.text_primary).child(clock)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .ts(13.0)
                .text_color(t.text_secondary)
                .child(NumberFlow::new("onboarding-captures", counted as i64))
                .child(if counted == 1 { "capture" } else { "captures" }),
        );

    let (px0, py0, pw, ph) = (16.0, sy + sh_ + 12.0, CARD_W - 32.0, (CARD_W - 32.0) * 9.0 / 16.0);
    let (dw, dh) = (pw * dip, ph * dip);
    let mut preview = div()
        .absolute()
        .left(px(px0 + (pw - dw) / 2.0))
        .top(px(py0 + (ph - dh) / 2.0))
        .w(px(dw))
        .h(px(dh))
        .rounded(px(10.0))
        .overflow_hidden()
        .bg(t.bg_sunken)
        .border_1()
        .border_color(t.border_default);
    if let Some(i) = a.captures.get(shots % n) {
        preview = preview.child(img(i.clone()).absolute().inset_0().size_full().object_fit(ObjectFit::Cover).rounded(px(9.0)));
    }
    preview = preview.child(div().absolute().inset_0().bg(white(flash)));

    let mut screen = plate(t, CARD_X, B_Y, CARD_W, B_H)
        .child(div().absolute().inset_0().opacity(0.45 + 0.55 * live).child(status).child(preview));
    // The snapshot thrown into the count: the frame as it was captured,
    // as a print. It rises out of the flash at the preview's own (dipped)
    // size, flicks over to −10° early in the throw, and shrinks along a
    // short arc into the number, dissolving over the last stretch.
    if shot && fly < 1.0 {
        let from = (px0 + pw / 2.0, py0 + ph / 2.0);
        let to = (sx + sw - 16.0 - 40.0, sy + sh_ / 2.0);
        let c = arc(from, to, fly, 18.0);
        let k = 0.965 * (1.0 - 0.88 * fly);
        let lift = tw(ms, latest_at + 60.0, 140.0, Ease::EASE_OUT);
        let fade = 1.0 - ((fly - 0.7).max(0.0) / 0.3);
        let o = (lift * fade).clamp(0.0, 1.0);
        let tilt = THROW_TILT * Ease::EASE_OUT_QUART.apply(fly);
        let prev = (shots + n - 1) % n;
        match a.throws.get(prev).and_then(|v| v.get((tilt.round() as usize).min(v.len().saturating_sub(1)))) {
            Some(print) => {
                // Size by the picture inside the print, as stickers are.
                let long = pw * k / print.content_frac;
                let (fw, fh) = if print.w >= print.h {
                    (long, long * print.h as f32 / print.w as f32)
                } else {
                    (long * print.w as f32 / print.h as f32, long)
                };
                screen = screen.child(
                    div()
                        .absolute()
                        .left(px(c.0 - fw / 2.0))
                        .top(px(c.1 - fh / 2.0))
                        .w(px(fw))
                        .h(px(fh))
                        .opacity(o)
                        .child(img(print.image.clone()).size_full()),
                );
            }
            None => {
                // Not prepared yet: the flat frame.
                if let Some(i) = a.captures.get(prev) {
                    let (fw, fh) = (pw * k, ph * k);
                    screen = screen.child(
                        div()
                            .absolute()
                            .left(px(c.0 - fw / 2.0))
                            .top(px(c.1 - fh / 2.0))
                            .w(px(fw))
                            .h(px(fh))
                            .opacity(o)
                            .rounded(px(10.0 * k.max(0.4)))
                            .overflow_hidden()
                            .child(img(i.clone()).size_full().object_fit(ObjectFit::Cover)),
                    );
                }
            }
        }
    }
    let recorder = screen;

    let hand = cursor_at(&moves, ms);
    // Its job done once Lapse is picked, the hand fades out as it moves
    // off, leaving the recorder to carry the rest.
    let hand_o = tw(ms, 250.0, 250.0, Ease::EASE_OUT) * (1.0 - tw(ms, moves[2].t0, moves[2].dur() * 0.8, Ease::EASE_OUT));
    two_cards(t, start, recorder, captions, words)
        .child(cursor(t, hand, pressed(ms, &clicks), hand_o))
        .into_any_element()
}

/// The + menu (`ui/menu.rs`) in this platform's look: Lapse (from the
/// program registry), then "Start from link". Drawn at scale `s` about its
/// growth point — every size multiplied, so it scales like a real popover
/// rather than a box growing behind fixed text.
fn add_menu(a: &Art, t: &Theme, s: f32, o: f32, lapse_hl: f32) -> AnyElement {
    let look = menu_look();
    let ((bx, by, bw, bh), origin) = menu_geom();
    let at = |x: f32, y: f32| (origin.0 + (x - origin.0) * s, origin.1 + (y - origin.1) * s);
    let (x, y) = at(bx, by);
    let (w, h) = (bw * s, bh * s);
    let z = |v: f32| px(v * s);

    let (radius, border, top_c, bottom_c, hover) = match look {
        MenuLook::Linux => (12.0, t.popover_border, t.popover_bg_top, t.popover_bg, t.popover_hover),
        MenuLook::Panel => (14.0, theme::fade(t.text_primary, 0.12), t.bg_panel, t.bg_panel, theme::fade(t.text_primary, 0.09)),
    };
    let shadow = match look {
        MenuLook::Linux => vec![sh(0.0, 1.0, 3.0, 0.0, black(0.08)), sh(0.0, 4.0, 12.0, 0.0, black(0.1)), sh(0.0, 10.0, 28.0, 0.0, black(0.12))],
        MenuLook::Panel => vec![sh(0.0, 6.0, 32.0, 0.0, black(0.28))],
    };
    let row = |icon: AnyElement, label: &'static str, hl: f32| {
        let base = div().flex().items_center().w_full();
        let base = match look {
            MenuLook::Linux => base.gap(z(12.0)).h(z(ROW_H)).px(z(12.0)).rounded(z(6.0)),
            MenuLook::Panel => base.gap(z(9.0)).h(z(ROW_H)).px(z(9.0)).rounded(z(9.0)),
        };
        let label_el = div().text_color(t.text_primary).whitespace_nowrap().child(label);
        let label_el = match look {
            MenuLook::Linux => label_el.ts(14.5 * s).w400(),
            MenuLook::Panel => label_el.ts(13.5 * s).w500(),
        };
        base.when(hl > 0.0, |d| d.bg(theme::mix(hover, theme::fade(t.text_primary, 0.16), (hl - 1.0).clamp(0.0, 1.0)).opacity(hl.min(1.0))))
            .child(div().size(z(18.0)).flex().items_center().justify_center().flex_none().child(icon))
            .child(label_el)
    };
    let lapse_icon = match &a.lapse {
        Some(i) => img(i.clone()).size(z(18.0)).rounded(z(5.0)).into_any_element(),
        None => Icon::ArrowSquareOutBold.el(15.0 * s, t.text_secondary).into_any_element(),
    };
    let separator = match look {
        MenuLook::Linux => div().h(px(1.0)).my(z(5.0)).bg(t.popover_separator),
        MenuLook::Panel => div().h(px(1.0)).my(z(4.0)).mx(z(10.0)).bg(t.border_default),
    };
    let panel = div()
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(w))
        .h(px(h))
        .rounded(z(radius))
        .bg(gpui_kit::gpui::linear_gradient(
            180.0,
            gpui_kit::gpui::linear_color_stop(top_c, 0.0),
            gpui_kit::gpui::linear_color_stop(bottom_c, 1.0),
        ))
        .border_1()
        .border_color(border)
        .shadow(shadow)
        .child(
            div()
                .size_full()
                .flex()
                .flex_col()
                .gap(z(1.0))
                .p(z(6.0))
                .child(row(lapse_icon, "Lapse", lapse_hl))
                .child(separator)
                .child(row(Icon::PlusBold.el(15.0 * s, t.text_secondary).into_any_element(), "Start from link", 0.0)),
        );
    let mut layer = div().absolute().inset_0().opacity(o.clamp(0.0, 1.0)).child(panel);
    if look == MenuLook::Linux {
        // The tail: filled with the panel's top colour over the panel's
        // border, then its two slanted edges stroked (as menu.rs paints it).
        let (tip_x, tip_y) = origin;
        let (half, th) = (TAIL_W / 2.0 * s, TAIL_H * s);
        let base_y = tip_y + th;
        layer = layer.child(
            gpui_kit::gpui::canvas(|_, _, _| (), move |b, _, window, _| {
                let ox = f32::from(b.origin.x);
                let oy = f32::from(b.origin.y);
                let p = |x: f32, y: f32| gpui_kit::gpui::point(px(ox + x), px(oy + y));
                let mut f = gpui_kit::gpui::PathBuilder::fill();
                f.move_to(p(tip_x - half, base_y + 0.5));
                f.line_to(p(tip_x, tip_y));
                f.line_to(p(tip_x + half, base_y + 0.5));
                f.close();
                if let Ok(path) = f.build() {
                    window.paint_path(path, top_c);
                }
                window.paint_quad(gpui_kit::gpui::fill(
                    gpui_kit::gpui::Bounds { origin: p(tip_x - half + 1.0, base_y - 0.5), size: gpui_kit::gpui::size(px(half * 2.0 - 2.0), px(2.0)) },
                    top_c,
                ));
                let mut st = gpui_kit::gpui::PathBuilder::stroke(px(1.0));
                st.move_to(p(tip_x - half + 0.5, base_y + 0.5));
                st.line_to(p(tip_x, tip_y + 0.5));
                st.line_to(p(tip_x + half - 0.5, base_y + 0.5));
                if let Ok(path) = st.build() {
                    window.paint_path(path, border);
                }
            })
            .absolute()
            .inset_0(),
        );
    }
    layer.into_any_element()
}

// ── Sync ────────────────────────────────────────────────────────────────

/// The sync picture's box (the welcome hero's size).
pub const SYNC_W: f32 = 420.0;
pub const SYNC_H: f32 = 368.0;
/// The session's gallery card.
const SC_X: f32 = 38.0;
const SC_Y: f32 = 36.0;
const SC_W: f32 = 272.0;
const SC_THUMB: f32 = SC_W * 9.0 / 16.0;
const SC_H: f32 = SC_THUMB + 57.0;
/// Hackatime, straddling the card's lower-right corner.
const HT_X: f32 = 206.0;
const HT_Y: f32 = SC_Y + SC_H - 46.0;
const HT_W: f32 = 182.0;
const HT_H: f32 = 132.0;
/// "When you're done": the session's time, and Hackatime's day before it.
const TRACKED_MIN: i64 = 72;
const BEFORE_MIN: i64 = 3 * 60 + 5;

/// The beats.
const SY_COMPLETE: f64 = 650.0;
const SY_BUD: f64 = 1350.0;
const SY_GO: f64 = 1650.0;
const SY_FLIGHT: f64 = 950.0;
const SY_LAND: f64 = SY_GO + SY_FLIGHT;
pub fn sync_end() -> f64 {
    SY_LAND + 1100.0
}

pub fn sync_hero(a: &Art, t: &Theme, ms: f64) -> AnyElement {
    let complete = tw(ms, SY_COMPLETE, 400.0, Ease::Linear);
    let bud = tw(ms, SY_BUD, 240.0, Ease::EASE_OUT);
    let go = tw(ms, SY_GO, SY_FLIGHT, TRAVEL);
    let landed = ms >= SY_LAND;
    let roll = tw(ms, SY_LAND - 80.0, 400.0, Ease::Linear);
    let grow = tw(ms, SY_LAND - 60.0, 900.0, ARRIVE);

    // ── The session ──
    let tracked = crate::fmt::format_tracked((TRACKED_MIN * 60) as f64);
    // GPUI clips children to a rectangle, not the card's rounded shape, so
    // the thumbnail rounds its own top corners (the card's 10 less its border).
    let mut thumb = div().relative().w_full().h(px(SC_THUMB)).bg(t.bg_sunken).rounded_t(px(9.0)).overflow_hidden();
    if let Some(i) = a.captures.first() {
        thumb = thumb.child(img(i.clone()).size_full().object_fit(ObjectFit::Cover).rounded_t(px(9.0)));
    }
    thumb = thumb
        .child(
            div()
                .absolute()
                .top(px(8.0))
                .left(px(8.0))
                .size(px(24.0))
                .rounded_full()
                .bg(t.archive_bg)
                .border_1()
                .border_color(t.archive_border)
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::LkArchiveX.el(14.0, t.archive_icon)),
        )
        .child(div().absolute().top(px(12.0)).right(px(8.0)).child(status_badge(t, complete)));
    let session = div()
        .absolute()
        .left(px(SC_X))
        .top(px(SC_Y))
        .w(px(SC_W))
        .h(px(SC_H))
        .rounded(px(10.0))
        .bg(t.bg_body)
        .shadow(raised(t))
        .child(
            div()
                .size_full()
                .flex()
                .flex_col()
                .bg(t.bg_surface)
                .border_1()
                .border_color(t.border_default)
                .rounded(px(10.0))
                .overflow_hidden()
                .child(thumb)
                .child(
                    div()
                        .p(px(12.0))
                        .child(div().ts(13.0).w600().text_color(t.text_primary).mb(px(2.0)).child("Sticker catalog"))
                        .child(div().ts(11.0).text_color(t.text_tertiary).child(format!("{tracked} · {}", today()))),
                ),
        );

    // ── Hackatime ──
    let fmt_hm = |m: i64| format!("{}h {:02}m", m / 60, m % 60);
    // Place by place, as torph rolls numbers: the hours and both minute
    // digits change, "h" and "m" hold still.
    let total = div()
        .absolute()
        .left(px(14.0))
        .top(px(50.0))
        .ts(22.0)
        .w700()
        .text_color(t.text_primary)
        .child(crate::ui::text_morph::TextMorph::new(fmt_hm(BEFORE_MIN), fmt_hm(BEFORE_MIN + TRACKED_MIN), roll));
    let week = [2.1f32, 3.4, 1.2, 4.0, 2.8, 0.0, BEFORE_MIN as f32 / 60.0];
    let mut bars = div().absolute().left(px(14.0)).bottom(px(14.0)).flex().items_end().gap(px(7.0));
    for (i, hours) in week.iter().enumerate() {
        let today = i == week.len() - 1;
        let hrs = if today { hours + TRACKED_MIN as f32 / 60.0 * grow } else { *hours };
        bars = bars.child(
            div()
                .w(px(16.0))
                .h(px((hrs / 5.0 * 34.0).max(2.0)))
                .rounded(px(3.0))
                .bg(if today { hackatime_red() } else if t.dark { white(0.16) } else { black(0.1) }),
        );
    }
    let hackatime = div()
        .absolute()
        .left(px(HT_X))
        .top(px(HT_Y))
        .w(px(HT_W))
        .h(px(HT_H))
        .rounded(px(14.0))
        .bg(if t.dark { hex(0x2c2c2c) } else { hex(0xffffff) })
        .border_1()
        .border_color(t.border_default)
        .shadow(raised(t))
        .child(
            div()
                .absolute()
                .left(px(14.0))
                .top(px(14.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .when_some(a.hackatime.clone(), |d, i| d.child(img(i).size(px(24.0)).flex_none()))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(div().ts(12.0).w600().text_color(t.text_primary).child("Hackatime"))
                        .child(div().ts(10.5).text_color(t.text_tertiary).child("Today")),
                ),
        )
        .child(total)
        .child(bars);

    let mut hero = div().relative().w(px(SYNC_W)).h(px(SYNC_H)).child(session).child(hackatime);

    // ── Lapse, thrown from the session into Hackatime ──
    // Lapse's icon springs up out of the finished session, is tossed in a
    // high lob over to the total — tilting through the throw and levelling
    // as it comes down — and shrinks into the number as it rolls.
    if bud > 0.001 && !landed {
        // From the card's foot, under Hackatime's header, into the number.
        let from = (SC_X + 112.0, SC_Y + SC_H + 4.0);
        let to = (HT_X + 70.0, HT_Y + 64.0);
        // A lob: straight-line travel plus a lift that peaks mid-flight.
        let lob = 10.0 * (std::f32::consts::PI * go).sin();
        let at = (from.0 + (to.0 - from.0) * go, from.1 + (to.1 - from.1) * go - lob);
        // Popping out of the card on a bouncy spring, then shrinking on landing.
        let pop = sp(ms, SY_BUD, Spring::new(420.0, 16.0));
        let land = Ease::EASE_IN_OUT.apply(((go - 0.86) / 0.14).clamp(0.0, 1.0));
        let size = LAPSE_SIZE * (0.4 + 0.6 * pop) * (1.0 - 0.55 * land);
        let o = (bud * (1.0 - land * land)).clamp(0.0, 1.0);
        let tilt = LAPSE_TILT * (std::f32::consts::PI * go).sin();
        let print = a.lapse_prints.get(((tilt / 2.0).round() as usize).min(a.lapse_prints.len().saturating_sub(1)));
        let el = match print {
            Some(pr) => {
                let long = size / pr.content_frac;
                let (w, h) = if pr.w >= pr.h { (long, long * pr.h as f32 / pr.w as f32) } else { (long * pr.w as f32 / pr.h as f32, long) };
                div().absolute().left(px(at.0 - w / 2.0)).top(px(at.1 - h / 2.0)).w(px(w)).h(px(h)).opacity(o).child(img(pr.image.clone()).size_full())
            }
            None => div()
                .absolute()
                .left(px(at.0 - size / 2.0))
                .top(px(at.1 - size / 2.0))
                .size(px(size))
                .opacity(o)
                .when_some(a.lapse.clone(), |d, i| d.child(img(i).size_full().rounded(px(size * 0.22)))),
        };
        hero = hero.child(el);
    }
    hero.into_any_element()
}

// ── Edit ────────────────────────────────────────────────────────────────

/// The edit picture's box.
pub const EDIT_W: f32 = 420.0;
pub const EDIT_H: f32 = 368.0;
/// The frame under the playhead.
const PV: (f32, f32, f32, f32) = (60.0, 26.0, 300.0, 169.0);
/// The filmstrip: 12pt per minute, the session's 40th minute at x = 0.
const STRIP_Y: f32 = 232.0;
const STRIP_H: f32 = 48.0;
const PER_MIN: f32 = 12.0;
const FIRST_MIN: f32 = 40.0;
const PLAYHEAD_X: f32 = 172.0;
/// The toolbar.
const TOOL_Y: f32 = 302.0;
const TOOL_H: f32 = 36.0;
const CUT_BTN: (f32, f32) = (24.0, 100.0);
const MASK_BTN: (f32, f32) = (CUT_BTN.0 + CUT_BTN.1 + 8.0, 110.0);
/// The session, and the cut the hand makes (minutes).
const KEPT_BEFORE: i64 = 72;
const CUT_FROM: f32 = PLAYHEAD_X - PER_MIN / 2.0;
const CUT_TO: f32 = CUT_FROM + PER_MIN * 8.0;
/// The mask: added in the middle of the frame, then dragged over the
/// stickers on the page (frame coordinates).
const MASK_W: f32 = 112.0;
const MASK_H: f32 = 64.0;
const MASK_FROM: (f32, f32) = ((PV.2 - MASK_W) / 2.0, (PV.3 - MASK_H) / 2.0 - 12.0);
const MASK_TO: (f32, f32) = (152.0, 72.0);
/// How far its bottom-right grip is then pulled (it grows to 140×82).
const MASK_GROW: (f32, f32) = (28.0, 18.0);

const E_REST: (f32, f32) = (EDIT_W - 26.0, 150.0);

struct EditBeats {
    m_cut: Move,
    c_cut: f64,
    m_grip: Move,
    drag0: f64,
    drag_ms: f64,
    m_mask: Move,
    c_mask: f64,
    m_box: Move,
    box0: f64,
    box_ms: f64,
    m_corner: Move,
    size0: f64,
    size_ms: f64,
    m_away: Move,
}

fn edit_beats() -> EditBeats {
    let grip_y = STRIP_Y + STRIP_H / 2.0;
    let cut_btn = (CUT_BTN.0 + CUT_BTN.1 * 0.8, TOOL_Y + TOOL_H * 0.62);
    let m_cut = Move { t0: 650.0, from: E_REST, to: cut_btn };
    let c_cut = m_cut.end() + 280.0;
    let m_grip = Move { t0: c_cut + 700.0, from: cut_btn, to: (CUT_FROM + PER_MIN, grip_y) };
    let drag0 = m_grip.end() + 380.0;
    let drag_ms = 1300.0;
    let mask_btn = (MASK_BTN.0 + MASK_BTN.1 * 0.8, TOOL_Y + TOOL_H * 0.62);
    let m_mask = Move { t0: drag0 + drag_ms + 500.0, from: (CUT_TO, grip_y), to: mask_btn };
    let c_mask = m_mask.end() + 280.0;
    let grab = (PV.0 + MASK_FROM.0 + MASK_W * 0.55, PV.1 + MASK_FROM.1 + MASK_H * 0.5);
    let m_box = Move { t0: c_mask + 600.0, from: mask_btn, to: grab };
    let box0 = m_box.end() + 380.0;
    let box_ms = 1100.0;
    let drop = (grab.0 + MASK_TO.0 - MASK_FROM.0, grab.1 + MASK_TO.1 - MASK_FROM.1);
    // Then to the bottom-right grip, and pull it out to cover more.
    let corner = (PV.0 + MASK_TO.0 + MASK_W, PV.1 + MASK_TO.1 + MASK_H);
    let m_corner = Move { t0: box0 + box_ms + 380.0, from: drop, to: corner };
    let size0 = m_corner.end() + 380.0;
    let size_ms = 1000.0;
    let pulled = (corner.0 + MASK_GROW.0, corner.1 + MASK_GROW.1);
    let m_away = Move { t0: size0 + size_ms + 450.0, from: pulled, to: E_REST };
    EditBeats { m_cut, c_cut, m_grip, drag0, drag_ms, m_mask, c_mask, m_box, box0, box_ms, m_corner, size0, size_ms, m_away }
}

pub fn edit_end() -> f64 {
    edit_beats().m_away.end() + 400.0
}

pub fn edit_hero(a: &Art, t: &Theme, ms: f64) -> AnyElement {
    let b = edit_beats();
    let drag_p = hand_ease((((ms - b.drag0) / b.drag_ms).clamp(0.0, 1.0)) as f32);
    let box_p = hand_ease((((ms - b.box0) / b.box_ms).clamp(0.0, 1.0)) as f32);
    let dragging_grip = ms >= b.drag0 - 100.0 && ms < b.drag0 + b.drag_ms + 100.0;
    let dragging_box = ms >= b.box0 - 100.0 && ms < b.box0 + b.box_ms + 100.0;
    let size_p = hand_ease((((ms - b.size0) / b.size_ms).clamp(0.0, 1.0)) as f32);
    let resizing = ms >= b.size0 - 100.0 && ms < b.size0 + b.size_ms + 100.0;

    // ── Where the hand is: moves between beats; the grip and the box carry it ──
    let grip_y = STRIP_Y + STRIP_H / 2.0;
    let mut hand = cursor_at(&[b.m_cut, b.m_grip], ms);
    if ms >= b.drag0 {
        hand = (CUT_FROM + PER_MIN + (CUT_TO - CUT_FROM - PER_MIN) * drag_p, grip_y);
    }
    if ms >= b.m_mask.t0 {
        hand = cursor_at(&[b.m_mask, b.m_box], ms);
    }
    let grab = b.m_box.to;
    if ms >= b.box0 {
        hand = (grab.0 + (MASK_TO.0 - MASK_FROM.0) * box_p, grab.1 + (MASK_TO.1 - MASK_FROM.1) * box_p);
    }
    if ms >= b.m_corner.t0 {
        hand = cursor_at(&[b.m_corner], ms);
    }
    if ms >= b.size0 {
        let c = b.m_corner.to;
        hand = (c.0 + MASK_GROW.0 * size_p, c.1 + MASK_GROW.1 * size_p);
    }
    if ms >= b.m_away.t0 {
        hand = cursor_at(&[b.m_away], ms);
    }
    // Held down through each drag: pressed in over the 100ms before it
    // starts, let go over the 100ms after it ends.
    let held = |t0: f64, dur: f64| tw(ms, t0 - 100.0, 80.0, Ease::EASE_OUT) * (1.0 - tw(ms, t0 + dur, 160.0, Ease::EASE_OUT));
    let press = pressed(ms, &[b.c_cut, b.c_mask])
        .max(held(b.drag0, b.drag_ms))
        .max(held(b.box0, b.box_ms))
        .max(held(b.size0, b.size_ms));
    let _ = (dragging_grip, dragging_box, resizing);
    let hand_o = tw(ms, 250.0, 250.0, Ease::EASE_OUT) * (1.0 - tw(ms, b.m_away.end() + 1800.0, 600.0, Ease::EASE_OUT));

    // ── The cut ──
    let cut_in = sp(ms, b.c_cut + 100.0, Spring::new(500.0, 30.0));
    let cut_o = tw(ms, b.c_cut + 100.0, 160.0, Ease::EASE_OUT);
    let cut_end = if ms < b.drag0 { CUT_FROM + PER_MIN * cut_in.max(0.0) } else { hand.0.max(CUT_FROM + PER_MIN) };
    let cut_end = if ms >= b.drag0 + b.drag_ms { CUT_TO } else { cut_end };
    let removed = (((cut_end - CUT_FROM) / PER_MIN).round() as i64).max(if cut_o > 0.0 { 1 } else { 0 });
    let grip_hover = if ms >= b.m_grip.end() - 60.0 && ms < b.m_mask.t0 { tw(ms, b.m_grip.end() - 60.0, 140.0, ARRIVE) } else { 0.0 };
    let selected = ms >= b.c_cut + 100.0 && ms < b.c_mask;

    // ── The mask ──
    let mask_in = sp(ms, b.c_mask + 100.0, Spring::new(520.0, 30.0));
    let mask_o = tw(ms, b.c_mask + 100.0, 160.0, Ease::EASE_OUT);
    let mask_at = (
        MASK_FROM.0 + (MASK_TO.0 - MASK_FROM.0) * box_p,
        MASK_FROM.1 + (MASK_TO.1 - MASK_FROM.1) * box_p,
    );
    let mask_sel = ms >= b.c_mask + 100.0 && ms < b.m_away.end() + 300.0;

    // The frame.
    let mut preview = div()
        .absolute()
        .left(px(PV.0))
        .top(px(PV.1))
        .w(px(PV.2))
        .h(px(PV.3))
        .rounded(px(12.0))
        .overflow_hidden()
        .bg(t.bg_sunken)
        .shadow(vec![sh(0.0, 14.0, 32.0, -12.0, black(if t.dark { 0.6 } else { 0.25 }))])
        .when_some(a.captures.first().cloned(), |d, i| d.child(img(i).size_full().object_fit(ObjectFit::Cover).rounded(px(12.0))));
    if mask_o > 0.001 {
        let s = 0.6 + 0.4 * mask_in;
        // Pulled by its bottom-right grip: the top-left corner stays put.
        let (mw, mh) = (MASK_W + MASK_GROW.0 * size_p, MASK_H + MASK_GROW.1 * size_p);
        let (w, h) = (mw * s, mh * s);
        let (x, y) = (mask_at.0 + (mw - w) / 2.0, mask_at.1 + (mh - h) / 2.0);
        let lane = hex(0x3b82f6);
        let mut boxed = div()
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(w))
            .h(px(h))
            .opacity(mask_o)
            .rounded(px(3.0))
            .overflow_hidden()
            // What the published video will show here: the frame, pixelated.
            .when_some(a.pixel.clone(), |d, i| {
                d.child(img(i).absolute().left(px(-x)).top(px(-y)).w(px(PV.2)).h(px(PV.3)))
            })
            .border(px(if mask_sel { 2.0 } else { 1.5 }))
            .border_color(lane);
        boxed = boxed.child(div());
        preview = preview.child(boxed);
        if mask_sel && mask_in > 0.6 {
            // The grip in hand grows a touch while it's held.
            let held_grip = if ms >= b.m_corner.end() - 60.0 && ms < b.size0 + b.size_ms + 200.0 {
                tw(ms, b.m_corner.end() - 60.0, 140.0, ARRIVE)
            } else {
                0.0
            };
            for (fx, fy) in [(0.0, 0.0), (0.5, 0.0), (1.0, 0.0), (1.0, 0.5), (1.0, 1.0), (0.5, 1.0), (0.0, 1.0), (0.0, 0.5)] {
                let g = 10.0 * if fx == 1.0 && fy == 1.0 { 1.0 + 0.3 * held_grip } else { 1.0 };
                preview = preview.child(
                    div()
                        .absolute()
                        .left(px(x + fx * w - g / 2.0))
                        .top(px(y + fy * h - g / 2.0))
                        .size(px(g))
                        .rounded(px(3.0))
                        .bg(hex(0xffffff))
                        .border(px(1.5))
                        .border_color(lane)
                        .shadow(vec![sh(0.0, 1.0, 3.0, 0.0, black(0.4))])
                        .opacity(mask_o),
                );
            }
        }
    }

    // The ruler and the filmstrip, running off both sides.
    let mut ruler = div().absolute().left_0().top(px(STRIP_Y - 22.0)).w(px(EDIT_W)).h(px(16.0));
    for k in 0..5 {
        let min = FIRST_MIN + 10.0 * k as f32;
        let x = (min - FIRST_MIN) * PER_MIN + 52.0;
        // Words are never cut by the edge: only whole labels.
        if x - 30.0 < 0.0 || x + 30.0 > EDIT_W {
            continue;
        }
        ruler = ruler
            .child(div().absolute().left(px(x)).top(px(13.0)).w(px(1.0)).h(px(5.0)).bg(t.border_hover))
            .child(
                div()
                    .absolute()
                    .left(px(x - 30.0))
                    .w(px(60.0))
                    .top_0()
                    .flex()
                    .justify_center()
                    .ts(10.0)
                    .text_color(t.text_tertiary)
                    .child({
                        let m = min as i64 + 4;
                        format!("{:02}:{:02}:00", m / 60, m % 60)
                    }),
            );
    }
    let frame_w = STRIP_H * 16.0 / 9.0;
    let mut strip = div().absolute().left(px(-14.0)).top(px(STRIP_Y)).w(px(EDIT_W + 28.0)).h(px(STRIP_H)).rounded(px(10.0)).overflow_hidden().flex();
    let n = a.frames.len().max(1);
    for k in 0..((EDIT_W + 28.0) / frame_w).ceil() as usize + 1 {
        if let Some(i) = a.frames.get((k * 2 + 1) % n) {
            strip = strip.child(
                div()
                    .w(px(frame_w))
                    .h_full()
                    .flex_none()
                    .border_r_1()
                    .border_color(black(0.35))
                    .child(img(i.clone()).size_full().object_fit(ObjectFit::Cover)),
            );
        }
    }
    let mut timeline = div().absolute().inset_0().child(ruler).child(strip);
    if cut_o > 0.001 {
        let (left, width) = (CUT_FROM, (cut_end - CUT_FROM).max(2.0));
        let (fill, stripe, border) = (t.cut_fill, t.cut_stripe, t.cut_border);
        let ring = if selected { 4.0 } else { 3.0 };
        let label = (width >= 52.0).then(|| crate::editor_math::span_label((removed) as f64));
        timeline = timeline.child(
            div()
                .absolute()
                .left(px(left))
                .top(px(STRIP_Y))
                .w(px(width))
                .h(px(STRIP_H))
                .opacity(cut_o)
                .child(
                    gpui_kit::gpui::canvas(|_, _, _| (), move |bounds, _, window, _| {
                        let rr = px(8.0);
                        window.paint_quad(gpui_kit::gpui::fill(bounds, black(0.45)).corner_radii(rr));
                        window.paint_quad(gpui_kit::gpui::fill(bounds, fill).corner_radii(rr));
                        crate::screens::editor::paint_hatch(bounds, crate::ui::paint::inset(bounds, 1.0), stripe, window);
                        window.paint_quad(gpui_kit::gpui::quad(bounds, rr, gpui_kit::gpui::transparent_black(), px(ring), border, Default::default()));
                    })
                    .absolute()
                    .inset_0(),
                )
                .when_some(label, |d, l| {
                    d.child(div().absolute().inset_0().flex().items_center().justify_center().ts(14.0).w700().text_color(hex(0xffffff)).child(l))
                }),
        );
        // The white grips on its ends; the one in hand grows a touch.
        for (x, active) in [(left, false), (left + width, true)] {
            let gs = if active { 1.0 + 0.15 * grip_hover } else { 1.0 };
            timeline = timeline.child(
                div()
                    .absolute()
                    .left(px(x - 5.0 * gs.min(1.2)))
                    .top(px(STRIP_Y + STRIP_H / 2.0 - 15.0 * gs))
                    .w(px(10.0 * gs.min(1.2)))
                    .h(px(30.0 * gs))
                    .rounded(px(5.0))
                    .opacity(cut_o)
                    .bg(hex(0xffffff))
                    .shadow(vec![sh(0.0, 1.0, 4.0, 0.0, black(0.4))])
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().w(px(2.0)).h(px(12.0)).rounded(px(1.0)).bg(theme::hexa(0x1f2937, 0.75))),
            );
        }
    }
    // The playhead.
    timeline = timeline.child(
        div()
            .absolute()
            .left(px(PLAYHEAD_X - 1.0))
            .top(px(STRIP_Y - 6.0))
            .w(px(2.0))
            .h(px(STRIP_H + 12.0))
            .rounded(px(1.0))
            .bg(hex(0xffffff))
            .shadow(vec![sh(0.0, 0.0, 3.0, 0.0, black(0.5))]),
    );

    // The toolbar: Add cut, Add mask (the editor's Secondary buttons) and
    // what's kept after cuts.
    let tool = |x: f32, w: f32, icon: Icon, label: &'static str, press: f32, hover: f32| {
        let s = 1.0 - 0.04 * press;
        let (bw, bh) = (w * s, TOOL_H * s);
        div()
            .absolute()
            .left(px(x + (w - bw) / 2.0))
            .top(px(TOOL_Y + (TOOL_H - bh) / 2.0))
            .w(px(bw))
            .h(px(bh))
            .rounded(px(10.0))
            .border_1()
            .border_color(t.border_hover)
            .bg(theme::mix(theme::transparent(), t.bg_surface, hover))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(7.0))
            .ts(13.0)
            .w600()
            .text_color(t.text_primary)
            .child(icon.el(15.0, t.text_primary))
            .child(label)
    };
    let on = |m: &Move, c: f64| if ms >= m.end() - 40.0 && ms < c + 360.0 { tw(ms, m.end() - 40.0, 100.0, Ease::EASE_OUT) } else { 0.0 };
    let kept = KEPT_BEFORE - removed;
    let minus_o = tw(ms, b.c_cut + 100.0, 220.0, Ease::EASE_OUT);
    let counter = div()
        .absolute()
        .right(px(24.0))
        .top(px(TOOL_Y))
        .h(px(TOOL_H))
        .flex()
        .items_center()
        .gap(px(5.0))
        .ts(13.0)
        .text_color(t.text_secondary)
        .whitespace_nowrap()
        .child(div().w700().text_color(t.text_primary).child(crate::editor_math::span_label(kept as f64)))
        .child("after cuts")
        .when(removed > 0, |d| {
            d.child(div().w600().text_color(t.cut_border).opacity(minus_o).child(format!("−{}", crate::editor_math::span_label(removed as f64))))
        });

    div()
        .relative()
        .w(px(EDIT_W))
        .h(px(EDIT_H))
        .child(preview)
        .child(timeline)
        .child(tool(CUT_BTN.0, CUT_BTN.1, Icon::Scissors, "Add cut", pressed(ms, &[b.c_cut]), on(&b.m_cut, b.c_cut)))
        .child(tool(MASK_BTN.0, MASK_BTN.1, Icon::Selection, "Add mask", pressed(ms, &[b.c_mask]), on(&b.m_mask, b.c_mask)))
        .child(counter)
        .child(cursor(t, hand, press, hand_o))
        .into_any_element()
}

// ── Pieces ──────────────────────────────────────────────────────────────

/// Two cards and their captions.
fn two_cards(t: &Theme, top: gpui_kit::gpui::Div, bottom: gpui_kit::gpui::Div, captions: (&str, &str), words: f32) -> gpui_kit::gpui::Div {
    let caption = |y: f32, text: &str, strong: bool, o: f32| {
        div()
            .absolute()
            .left(px(40.0))
            .top(px(y))
            .w(px(W - 80.0))
            .ts_lh(13.5, 1.45)
            .text_center()
            .text_color(if strong { t.text_primary } else { t.text_secondary })
            .opacity(o)
            .child(text.to_string())
    };
    let w2 = ((words - 0.15) / 0.85).clamp(0.0, 1.0);
    div()
        .relative()
        .w(px(W))
        .h(px(CAP2_Y + 60.0))
        .child(top)
        .child(caption(CAP1_Y, captions.0, true, words))
        .child(bottom)
        .child(caption(CAP2_Y, captions.1, false, w2))
}

/// A card on the window's material: a glass step with a hairline.
fn plate(t: &Theme, x: f32, y: f32, w: f32, h: f32) -> gpui_kit::gpui::Div {
    div()
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(w))
        .h(px(h))
        .rounded(px(18.0))
        .bg(if t.dark { black(0.24) } else { white(0.7) })
        .border_1()
        .border_color(if t.dark { white(0.07) } else { black(0.06) })
        .overflow_hidden()
}

/// One raised shadow for the sync picture's two surfaces.
fn raised(t: &Theme) -> Vec<gpui_kit::gpui::BoxShadow> {
    if t.dark {
        vec![sh(0.0, 16.0, 36.0, -10.0, black(0.6)), sh(0.0, 1.0, 3.0, 0.0, black(0.3))]
    } else {
        vec![sh(0.0, 16.0, 36.0, -12.0, black(0.2)), sh(0.0, 1.0, 3.0, 0.0, black(0.06))]
    }
}

fn today() -> String {
    use chrono::Datelike;
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let now = chrono::Local::now();
    format!("{} {}", MONTHS[now.month0() as usize], now.day())
}

/// The gallery's status badge, Compiling → Complete: the label morphs as
/// torph does ("Comp…l" slides into place, "i…ing" leaves, "ete" arrives)
/// while the colour mixes. `k` runs linearly 0→1 over the morph.
fn status_badge(t: &Theme, k: f32) -> AnyElement {
    let (from_label, from_c) = theme::status_config("compiling", t);
    let (to_label, to_c) = theme::status_config("complete", t);
    div()
        .flex()
        .items_center()
        .px(px(8.0))
        .h(px(15.0))
        .rounded(px(999.0))
        .bg(theme::mix(from_c, to_c, Ease::EASE_OUT.apply((k * 1.6).min(1.0))))
        .shadow(vec![sh(0.0, 0.0, 0.0, 1.0, black(0.1))])
        .text_color(hex(0xffffff))
        .ts(10.0)
        .w600()
        .child(crate::ui::text_morph::TextMorph::new(from_label, to_label, k))
        .into_any_element()
}

/// The pointer: the macOS arrow (stageshow kit's `MacPointer`, MIT) — a
/// white edge under a black fill — at 1.75× (a touch above the kit's `large`, for
/// close-ups), with the soft shadow the real one casts. It presses toward
/// its tip.
fn cursor(t: &Theme, at: (f32, f32), press: f32, o: f32) -> AnyElement {
    let _ = t;
    if o <= 0.001 {
        return div().into_any_element();
    }
    let s = 1.75 * (1.0 - 0.1 * press);
    let size = 24.0 * s;
    // The tip sits at (7.5, 5.55) in the 24pt glyph.
    let (hx, hy) = (7.5 * s, 5.55 * s);
    let glyph = |path: &'static str, c: Hsla, dx: f32, dy: f32| {
        svg().path(path).absolute().left(px(dx)).top(px(dy)).size(px(size)).text_color(c)
    };
    div()
        .absolute()
        .left(px(at.0 - hx))
        .top(px(at.1 - hy))
        .size(px(size))
        .opacity(o.clamp(0.0, 1.0))
        // A soft shadow: the silhouette, faint, a little below and around.
        .child(glyph("onboarding/cursor-mac-edge.svg", black(0.10), 0.0, 2.0))
        .child(glyph("onboarding/cursor-mac-edge.svg", black(0.12), 0.0, 1.0))
        .child(glyph("onboarding/cursor-mac-edge.svg", white(1.0), 0.0, 0.0))
        .child(glyph("onboarding/cursor-mac-fill.svg", black(1.0), 0.0, 0.0))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beats_in_order() {
        let m = s_moves();
        let c = s_clicks();
        assert!(m[0].end() < c[0] && c[0] < m[1].t0 && m[1].end() < c[1] && c[1] < s_rec() && s_rec() < start_end());
        assert!(SY_COMPLETE < SY_BUD && SY_BUD < SY_GO && SY_LAND < sync_end());
        assert_eq!(pressed(c[0] + 600.0, &c[..1]), 0.0);
        assert!(pressed(c[0] + 90.0, &c) > 0.9);
        // Captures land on the clock's 10-second marks; the clock starts at
        // 10× and slows toward real time.
        assert_eq!(shots_at(-1.0).0, 0);
        let (k, at) = shots_at(5_000.0);
        assert!(k >= 3 && (clock_at(at / 1000.0) - k as f64 * 10.0).abs() < 1e-3);
        assert!((clock_at(0.001) / 0.001 - 10.0).abs() < 0.05);
        let late = clock_at(60.0) - clock_at(59.0);
        assert!(late > 1.0 && late < 1.1);
    }

    #[test]
    fn the_hand_lands_on_plus() {
        let m = s_moves();
        let (x, y) = cursor_at(&m, m[0].end() + 1.0);
        let p = in_a(PLUS);
        assert!((x - p.0).abs() < 0.01 && (y - p.1).abs() < 0.01);
    }
}
