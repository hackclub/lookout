//! Motion — the timing primitives every animation in the app is built from.
//!
//! The Tauri app animates with three libraries (framer-motion 14, vaul and
//! NumberFlow) plus CSS transitions and keyframes. To make the native app
//! move *identically* this module reimplements their exact maths rather than
//! approximating with GPUI's stock easings:
//!
//! - [`Spring`] is framer-motion's analytic spring (`motion-dom`
//!   `generators/spring.mjs`): stiffness / damping / mass, velocity carried
//!   across retargets, and the same rest thresholds (granular when the
//!   travel is under 5 units).
//! - [`Ease`] is a true CSS `cubic-bezier()` solver, plus CSS `linear()`
//!   stop lists (NumberFlow's digit curve is one).
//! - [`Anim`] is one animated scalar: a tween or a spring that can be
//!   retargeted mid-flight from its current value (and, for springs, its
//!   current velocity) — the "interruptible" behaviour framer-motion has.
//!
//! Views keep `Anim`s in their state, sample them in `render` with
//! [`now`], and call `window.request_animation_frame()` while any is
//! [`Anim::is_animating`]. Nothing here allocates per frame.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

// ── Global knobs ────────────────────────────────────────────────────────

static SLOWMO_BITS: AtomicU32 = AtomicU32::new(0x3f80_0000); // 1.0f32
static REDUCE_MOTION: AtomicBool = AtomicBool::new(false);

/// Debug multiplier: `LOOKOUT_SLOWMO=5` plays every animation 5× slower so
/// each one can be checked frame by frame (design-guidance §11).
pub fn set_slowmo(factor: f32) {
    SLOWMO_BITS.store(factor.max(0.01).to_bits(), Ordering::Relaxed);
}

pub fn slowmo() -> f32 {
    f32::from_bits(SLOWMO_BITS.load(Ordering::Relaxed))
}

/// Whether the OS asks for reduced motion. NumberFlow honoured
/// `prefers-reduced-motion`; we extend that to every spatial animation
/// (opacity and colour still fade — "fewer and gentler, not none").
pub fn reduce_motion() -> bool {
    REDUCE_MOTION.load(Ordering::Relaxed)
}

pub fn set_reduce_motion(on: bool) {
    REDUCE_MOTION.store(on, Ordering::Relaxed);
}

/// A process-wide monotonic clock origin so animation times are plain f64
/// milliseconds (cheap to store, easy to compare).
fn origin() -> Instant {
    use std::sync::OnceLock;
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

/// Milliseconds since the process started, scaled by the slow-motion knob.
pub fn now() -> f64 {
    origin().elapsed().as_secs_f64() * 1000.0 / slowmo() as f64
}

// ── Easing ──────────────────────────────────────────────────────────────

/// A CSS timing function.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ease {
    Linear,
    /// `cubic-bezier(x1, y1, x2, y2)`.
    Bezier(f32, f32, f32, f32),
    /// A CSS `linear(...)` function with evenly spaced stops.
    Stops(&'static [f32]),
}

impl Ease {
    /// CSS `ease` — `cubic-bezier(0.25, 0.1, 0.25, 1)`.
    pub const CSS_EASE: Ease = Ease::Bezier(0.25, 0.1, 0.25, 1.0);
    /// CSS `ease-out` and motion's `"easeOut"` — `cubic-bezier(0, 0, 0.58, 1)`.
    pub const EASE_OUT: Ease = Ease::Bezier(0.0, 0.0, 0.58, 1.0);
    /// CSS `ease-in-out` and motion's `"easeInOut"`.
    pub const EASE_IN_OUT: Ease = Ease::Bezier(0.42, 0.0, 0.58, 1.0);
    /// framer-motion's default tween for non-transform values.
    pub const MOTION_DEFAULT: Ease = Ease::Bezier(0.25, 0.1, 0.35, 1.0);
    /// vaul's drawer curve (from Ionic) — every sheet in the app.
    pub const VAUL: Ease = Ease::Bezier(0.32, 0.72, 0.0, 1.0);
    /// The editor's `EASE_OUT_QUART`.
    pub const EASE_OUT_QUART: Ease = Ease::Bezier(0.25, 1.0, 0.5, 1.0);
    /// NumberFlow 0.6.2's default `transformTiming` easing (a spring sampled
    /// into a CSS `linear()` list).
    pub const NUMBER_FLOW: Ease = Ease::Stops(&NUMBER_FLOW_STOPS);

    /// Map linear progress `t` in [0, 1] to eased progress.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Ease::Linear => t,
            Ease::Bezier(x1, y1, x2, y2) => cubic_bezier(x1, y1, x2, y2, t),
            Ease::Stops(stops) => {
                let n = stops.len();
                if n < 2 {
                    return t;
                }
                let pos = t * (n - 1) as f32;
                let i = (pos.floor() as usize).min(n - 2);
                let f = pos - i as f32;
                stops[i] + (stops[i + 1] - stops[i]) * f
            }
        }
    }
}

/// CSS `cubic-bezier()` — solve x(s) = t for the curve parameter with
/// Newton-Raphson, falling back to bisection, then return y(s). This is the
/// algorithm WebKit and Chromium use, so curves match the web exactly.
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let (x1, y1, x2, y2, t) = (x1 as f64, y1 as f64, x2 as f64, y2 as f64, t as f64);
    let cx = 3.0 * x1;
    let bx = 3.0 * (x2 - x1) - cx;
    let ax = 1.0 - cx - bx;
    let cy = 3.0 * y1;
    let by = 3.0 * (y2 - y1) - cy;
    let ay = 1.0 - cy - by;
    let sample_x = |s: f64| ((ax * s + bx) * s + cx) * s;
    let sample_y = |s: f64| ((ay * s + by) * s + cy) * s;
    let slope_x = |s: f64| (3.0 * ax * s + 2.0 * bx) * s + cx;

    let mut s = t;
    for _ in 0..8 {
        let x = sample_x(s) - t;
        if x.abs() < 1e-7 {
            return sample_y(s) as f32;
        }
        let d = slope_x(s);
        if d.abs() < 1e-6 {
            break;
        }
        s -= x / d;
    }
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    s = t;
    for _ in 0..40 {
        let x = sample_x(s);
        if (x - t).abs() < 1e-7 {
            break;
        }
        if t > x {
            lo = s;
        } else {
            hi = s;
        }
        s = (lo + hi) / 2.0;
    }
    sample_y(s) as f32
}

/// NumberFlow 0.6.2 `transformTiming.easing`, copied verbatim
/// (`node_modules/number-flow/dist/lite.mjs:130-138`).
pub static NUMBER_FLOW_STOPS: [f32; 90] = [
    0.0, 0.005, 0.019, 0.039, 0.066, 0.096, 0.129, 0.165, 0.202, 0.24, 0.278, 0.316, 0.354, 0.39,
    0.426, 0.461, 0.494, 0.526, 0.557, 0.586, 0.614, 0.64, 0.665, 0.689, 0.711, 0.731, 0.751, 0.769,
    0.786, 0.802, 0.817, 0.831, 0.844, 0.856, 0.867, 0.877, 0.887, 0.896, 0.904, 0.912, 0.919,
    0.925, 0.931, 0.937, 0.942, 0.947, 0.951, 0.955, 0.959, 0.962, 0.965, 0.968, 0.971, 0.973,
    0.976, 0.978, 0.98, 0.981, 0.983, 0.984, 0.986, 0.987, 0.988, 0.989, 0.99, 0.991, 0.992, 0.992,
    0.993, 0.994, 0.994, 0.995, 0.995, 0.996, 0.996, 0.9963, 0.9967, 0.9969, 0.9972, 0.9975,
    0.9977, 0.9979, 0.9981, 0.9982, 0.9984, 0.9985, 0.9987, 0.9988, 0.9989, 1.0,
];

// ── Springs ─────────────────────────────────────────────────────────────

/// framer-motion spring physics. `damping` is the coefficient c (not a
/// ratio): ζ = c / (2√(k·m)).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub stiffness: f32,
    pub damping: f32,
    pub mass: f32,
    /// Overrides for the rest thresholds (`restSpeed`, units/s).
    pub rest_speed: Option<f32>,
}

impl Spring {
    pub const fn new(stiffness: f32, damping: f32) -> Self {
        Self { stiffness, damping, mass: 1.0, rest_speed: None }
    }
    pub const fn with_mass(mut self, mass: f32) -> Self {
        self.mass = mass;
        self
    }
    pub const fn with_rest_speed(mut self, rest_speed: f32) -> Self {
        self.rest_speed = Some(rest_speed);
        self
    }

    /// The press spring every Button, Card and row uses: `{stiffness: 1500, damping: 60}`.
    pub const PRESS: Spring = Spring::new(1500.0, 60.0);
    /// Route / subpage / picker slide: `{stiffness: 460, damping: 36, mass: 0.7}`.
    pub const ROUTE: Spring = Spring::new(460.0, 36.0).with_mass(0.7);
    /// Overlay / NamingModal entry and ProgramPanel height: `420 / 34 / 0.7`.
    pub const OVERLAY: Spring = Spring::new(420.0, 34.0).with_mass(0.7);
    /// motion's default for an unspecified `scale`: `550 / 30, restSpeed 10`.
    pub const MOTION_SCALE: Spring = Spring::new(550.0, 30.0).with_rest_speed(10.0);
    /// motion's default for unspecified x / y: `500 / 25, restSpeed 10`.
    pub const MOTION_XY: Spring = Spring::new(500.0, 25.0).with_rest_speed(10.0);
}

/// Position and velocity (units per second) of a spring travelling from
/// `from` to `to`, `t_ms` after it started with velocity `v0` (units/s).
fn spring_at(sp: Spring, from: f64, to: f64, v0: f64, t_ms: f64) -> (f64, f64) {
    let k = sp.stiffness as f64;
    let c = sp.damping as f64;
    let m = sp.mass as f64;
    let t = (t_ms / 1000.0).max(0.0);
    let x0 = from - to; // displacement from target
    let omega = (k / m).sqrt();
    let zeta = c / (2.0 * (k * m).sqrt());
    let (x, v);
    if zeta < 1.0 {
        let wd = omega * (1.0 - zeta * zeta).sqrt();
        let a = x0;
        let b = (v0 + zeta * omega * x0) / wd;
        let env = (-zeta * omega * t).exp();
        let (s, co) = (wd * t).sin_cos();
        x = env * (a * co + b * s);
        v = env * ((b * wd - zeta * omega * a) * co - (a * wd + zeta * omega * b) * s);
    } else if (zeta - 1.0).abs() < 1e-9 {
        let env = (-omega * t).exp();
        let b = v0 + omega * x0;
        x = env * (x0 + b * t);
        v = env * (b - omega * (x0 + b * t));
    } else {
        let r = (zeta * zeta - 1.0).sqrt();
        let s1 = -omega * (zeta - r);
        let s2 = -omega * (zeta + r);
        let c2 = (v0 - s1 * x0) / (s2 - s1);
        let c1 = x0 - c2;
        x = c1 * (s1 * t).exp() + c2 * (s2 * t).exp();
        v = c1 * s1 * (s1 * t).exp() + c2 * s2 * (s2 * t).exp();
    }
    (to + x, v)
}

// ── One animated value ──────────────────────────────────────────────────

/// How an [`Anim`] gets to its target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Curve {
    Spring(Spring),
    Tween { duration_ms: f32, ease: Ease },
}

impl Curve {
    pub const fn tween(duration_ms: f32, ease: Ease) -> Self {
        Curve::Tween { duration_ms, ease }
    }
}

/// One animated scalar. Retargeting starts from wherever the value is *now*
/// (and, for springs, with its current velocity), so rapid toggles reverse
/// smoothly instead of jumping — framer-motion's behaviour.
#[derive(Clone, Copy, Debug)]
pub struct Anim {
    from: f64,
    to: f64,
    v0: f64,
    start_ms: f64,
    delay_ms: f64,
    curve: Curve,
    settled: bool,
    /// CSS-transition bookkeeping (`set_css`): the running transition's
    /// reversing-adjusted start value and reversing shortening factor.
    rev_start: f64,
    rev_factor: f64,
}

impl Anim {
    /// A value at rest.
    pub fn new(value: f32) -> Self {
        Self {
            from: value as f64,
            to: value as f64,
            v0: 0.0,
            start_ms: 0.0,
            delay_ms: 0.0,
            curve: Curve::tween(0.0, Ease::Linear),
            settled: true,
            rev_start: value as f64,
            rev_factor: 1.0,
        }
    }

    /// Start animating `from` → `to` right now (an `initial` → `animate`
    /// mount animation).
    pub fn from_to(from: f32, to: f32, curve: Curve) -> Self {
        let mut a = Self::new(from);
        a.set(to, curve);
        a
    }

    pub fn target(&self) -> f32 {
        self.to as f32
    }

    /// Retarget. A no-op when already heading to `to`.
    pub fn set(&mut self, to: f32, curve: Curve) {
        self.set_delayed(to, curve, 0.0);
    }

    /// Retarget after `delay_ms` (motion's `transition.delay`).
    pub fn set_delayed(&mut self, to: f32, curve: Curve, delay_ms: f32) {
        let to = to as f64;
        if (to - self.to).abs() < f64::EPSILON && !self.settled {
            return;
        }
        if (to - self.to).abs() < f64::EPSILON && self.settled {
            return;
        }
        let t = now();
        let (value, velocity) = self.sample_at(t);
        self.from = value;
        self.to = to;
        self.v0 = velocity;
        self.start_ms = t;
        self.delay_ms = delay_ms as f64;
        self.curve = curve;
        self.settled = false;
        self.rev_start = value;
        self.rev_factor = 1.0;
        if reduce_motion_applies(curve) {
            self.jump(to as f32);
        }
    }

    /// Retarget the way a CSS `transition` does (CSS Transitions §3): the
    /// new transition starts from the current value with no velocity, and
    /// when it *reverses* a running transition back to that transition's
    /// start value its duration is shortened by how far the old one got
    /// (the "reversing shortening factor"). Use this for everything the
    /// Tauri app animated with a stylesheet/inline `transition:`, so a quick
    /// hover-out takes as long as the hover-in had progressed, exactly like
    /// the browser.
    pub fn set_css(&mut self, to: f32, duration_ms: f32, ease: Ease) {
        let to64 = to as f64;
        if (to64 - self.to).abs() < f64::EPSILON {
            return;
        }
        let t = now();
        let running = self.is_animating();
        let (value, _) = self.sample_at(t);
        let (mut factor, rev_start) = (1.0f64, value);
        let mut new_rev_start = rev_start;
        if running && (to64 - self.rev_start).abs() < 1e-6 {
            if let Curve::Tween { duration_ms: d, ease: e } = self.curve {
                let elapsed = (t - self.start_ms - self.delay_ms).max(0.0);
                let p = if d > 0.0 { (elapsed / d as f64).min(1.0) } else { 1.0 };
                let out = e.apply(p as f32) as f64;
                factor = (out * self.rev_factor + 1.0 - self.rev_factor).abs().clamp(0.0, 1.0);
                new_rev_start = self.to;
            }
        }
        self.from = value;
        self.to = to64;
        self.v0 = 0.0;
        self.start_ms = t;
        self.delay_ms = 0.0;
        self.curve = Curve::tween((duration_ms as f64 * factor) as f32, ease);
        self.settled = false;
        self.rev_start = new_rev_start;
        self.rev_factor = factor;
    }

    /// Jump to `value` with no animation.
    pub fn jump(&mut self, value: f32) {
        *self = Anim::new(value);
    }

    fn sample_at(&self, t: f64) -> (f64, f64) {
        if self.settled {
            return (self.to, 0.0);
        }
        let elapsed = t - self.start_ms - self.delay_ms;
        if elapsed <= 0.0 {
            return (self.from, if self.delay_ms > 0.0 { 0.0 } else { self.v0 });
        }
        match self.curve {
            Curve::Spring(sp) => spring_at(sp, self.from, self.to, self.v0, elapsed),
            Curve::Tween { duration_ms, ease } => {
                let d = duration_ms as f64;
                if d <= 0.0 || elapsed >= d {
                    return (self.to, 0.0);
                }
                let p = (elapsed / d) as f32;
                let e = ease.apply(p) as f64;
                let value = self.from + (self.to - self.from) * e;
                // Numeric velocity for handing over to a spring on retarget.
                let dt = 1.0f64;
                let e2 = ease.apply(((elapsed + dt) / d).min(1.0) as f32) as f64;
                let v2 = self.from + (self.to - self.from) * e2;
                (value, (v2 - value) / dt * 1000.0)
            }
        }
    }

    /// The value now.
    pub fn get(&self) -> f32 {
        self.sample_at(now()).0 as f32
    }

    /// Whether the value is still moving (keep requesting frames).
    pub fn is_animating(&self) -> bool {
        if self.settled {
            return false;
        }
        let t = now();
        let elapsed = t - self.start_ms - self.delay_ms;
        if elapsed < 0.0 {
            return true;
        }
        match self.curve {
            Curve::Tween { duration_ms, .. } => elapsed < duration_ms as f64,
            Curve::Spring(sp) => {
                let (x, v) = spring_at(sp, self.from, self.to, self.v0, elapsed);
                let delta = (self.to - self.from).abs();
                let granular = delta < 5.0;
                let rest_speed = sp
                    .rest_speed
                    .map(|r| r as f64)
                    .unwrap_or(if granular { 0.01 } else { 2.0 });
                let rest_delta = if granular { 0.005 } else { 0.5 };
                // Guard: no spring in this app should run longer than 10s.
                !(v.abs() <= rest_speed && (self.to - x).abs() <= rest_delta) && elapsed < 10_000.0
            }
        }
    }

    /// Settle the bookkeeping once the animation has finished, so later
    /// `get()` calls are a constant read. Returns whether it is still moving.
    pub fn tick(&mut self) -> bool {
        let moving = self.is_animating();
        if !moving && !self.settled {
            self.settled = true;
            self.from = self.to;
            self.v0 = 0.0;
        }
        moving
    }
}

fn reduce_motion_applies(curve: Curve) -> bool {
    // Springs carry spatial motion in this app (slides, presses, pops); under
    // reduced motion they snap. Tweens are mostly opacity/colour fades and
    // keep playing.
    reduce_motion() && matches!(curve, Curve::Spring(_))
}

/// Convenience: keep `window` animating while `animating` is true.
#[macro_export]
macro_rules! keep_animating {
    ($window:expr, $($anim:expr),+ $(,)?) => {{
        let mut any = false;
        $( any |= $anim.tick(); )+
        if any { $window.request_animation_frame(); }
        any
    }};
}

// ── Repeating keyframe loops (CSS `animation: … infinite`) ──────────────

/// `@keyframes pulse { 0%, 100% { opacity: 1 } 50% { opacity: 0.4 } }`
/// played as `pulse <period> ease-in-out infinite`. CSS applies the timing
/// function per keyframe segment, so each half eases separately.
pub fn pulse_opacity(period_ms: f64) -> f32 {
    let p = ((now() % period_ms) / period_ms) as f32;
    if p < 0.5 {
        1.0 - 0.6 * Ease::EASE_IN_OUT.apply(p * 2.0)
    } else {
        0.4 + 0.6 * Ease::EASE_IN_OUT.apply((p - 0.5) * 2.0)
    }
}

/// `@keyframes spin` at `period_ms` linear: the current angle in radians.
pub fn spin_angle(period_ms: f64) -> f32 {
    let p = (now() % period_ms) / period_ms;
    (p * std::f64::consts::TAU) as f32
}

/// `@keyframes shimmer { 0% { background-position: 200% 0 } 100% { -200% 0 } }`
/// over 1.5s ease-in-out: returns the gradient's offset as a fraction of the
/// element width (the 200%-wide gradient's left edge, in element widths).
pub fn shimmer_phase() -> f32 {
    let p = ((now() % 1500.0) / 1500.0) as f32;
    Ease::EASE_IN_OUT.apply(p)
}

pub fn duration_ms(d: Duration) -> f32 {
    d.as_secs_f32() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bezier_endpoints_and_midpoint() {
        assert_eq!(cubic_bezier(0.25, 0.1, 0.25, 1.0, 0.0), 0.0);
        assert_eq!(cubic_bezier(0.25, 0.1, 0.25, 1.0, 1.0), 1.0);
        // CSS `ease` at 0.5 is ≈ 0.8024.
        let v = cubic_bezier(0.25, 0.1, 0.25, 1.0, 0.5);
        assert!((v - 0.8024).abs() < 0.002, "{v}");
        // linear bezier
        let l = cubic_bezier(0.0, 0.0, 1.0, 1.0, 0.3);
        assert!((l - 0.3).abs() < 1e-4);
    }

    #[test]
    fn spring_settles_at_target() {
        let sp = Spring::PRESS;
        let (x, v) = spring_at(sp, 1.0, 0.96, 0.0, 2000.0);
        assert!((x - 0.96).abs() < 1e-6 && v.abs() < 1e-4);
        // underdamped ζ≈0.775 overshoots slightly
        let mut min = 1.0f64;
        for i in 0..400 {
            let (x, _) = spring_at(sp, 1.0, 0.96, 0.0, i as f64);
            min = min.min(x);
        }
        assert!(min < 0.96);
    }

    #[test]
    fn css_reversal_shortens() {
        let mut a = Anim::new(0.0);
        a.set_css(1.0, 100.0, Ease::Linear);
        // Reverse immediately: the shortened transition is ~0 long.
        a.set_css(0.0, 100.0, Ease::Linear);
        match a.curve {
            Curve::Tween { duration_ms, .. } => assert!(duration_ms < 5.0, "{duration_ms}"),
            _ => panic!(),
        }
        assert!((a.rev_start - 1.0).abs() < 1e-9);
    }

    #[test]
    fn stops_interpolate() {
        assert_eq!(Ease::NUMBER_FLOW.apply(0.0), 0.0);
        assert_eq!(Ease::NUMBER_FLOW.apply(1.0), 1.0);
        assert!(Ease::NUMBER_FLOW.apply(0.5) > 0.9);
    }
}
