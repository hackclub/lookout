//! Vector painting helpers: the iOS-style squircle the Tauri Button clipped
//! its background to (`@squircle-js/react` → `figma-squircle`, smoothing
//! 0.7), circular arcs, and capsule-ended arc strokes for spinners/rings.
//!
//! GPUI 0.3.10 has no transforms on ordinary elements, so anything that
//! *scales* (the button press, popovers) is painted here with the scale
//! baked into the geometry.

use gpui_kit::gpui::{Bounds, Path, PathBuilder, Pixels, Point, point, px};

#[derive(Clone, Copy)]
struct Seg {
    c1: Point<f32>,
    c2: Point<f32>,
    to: Point<f32>,
}

fn p(x: f32, y: f32) -> Point<f32> {
    Point { x, y }
}

/// figma-squircle `getPathParamsForCorner` (no `preserveSmoothing`).
struct CornerParams {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    p: f32,
    arc: f32,
    r: f32,
}

fn corner_params(radius: f32, smoothing: f32, budget: f32) -> CornerParams {
    let r = radius.min(budget).max(0.0);
    let mut s = smoothing;
    let mut pp = (1.0 + s) * r;
    if r > 0.0 {
        let max_s = budget / r - 1.0;
        s = s.min(max_s).max(0.0);
        pp = pp.min(budget);
    }
    let arc_measure = 90.0 * (1.0 - s);
    let arc = (arc_measure / 2.0).to_radians().sin() * r * 2f32.sqrt();
    let angle_alpha = (90.0 - arc_measure) / 2.0;
    let p3_to_p4 = r * (angle_alpha / 2.0).to_radians().tan();
    let angle_beta = 45.0 * s;
    let c = p3_to_p4 * angle_beta.to_radians().cos();
    let d = c * angle_beta.to_radians().tan();
    let b = (pp - arc - c - d) / 3.0;
    let a = 2.0 * b;
    CornerParams { a, b, c, d, p: pp, arc, r }
}

/// Approximate the circle arc centred `c` radius `r` from angle `a0` to `a1`
/// (radians, screen coordinates: y down) with one cubic.
fn arc_seg(c: Point<f32>, r: f32, a0: f32, a1: f32) -> Seg {
    let k = 4.0 / 3.0 * ((a1 - a0) / 4.0).tan();
    let (s0, c0) = a0.sin_cos();
    let (s1, c1) = a1.sin_cos();
    let p0 = p(c.x + r * c0, c.y + r * s0);
    let p3 = p(c.x + r * c1, c.y + r * s1);
    let _ = p0;
    Seg {
        c1: p(c.x + r * c0 - k * r * s0, c.y + r * s0 + k * r * c0),
        c2: p(p3.x + k * r * s1, p3.y - k * r * c1),
        to: p3,
    }
}

/// The top-right corner of a `w`×`h` squircle as cubic segments from
/// `(w - p, 0)` to `(w, p)`.
fn top_right(w: f32, k: &CornerParams) -> (Point<f32>, Vec<Seg>) {
    let start = p(w - k.p, 0.0);
    if k.r <= 0.0 {
        return (start, vec![Seg { c1: start, c2: p(w, 0.0), to: p(w, k.p) }]);
    }
    let e1 = p(start.x + k.a + k.b + k.c, k.d);
    let s1 = Seg { c1: p(start.x + k.a, 0.0), c2: p(start.x + k.a + k.b, 0.0), to: e1 };
    // The arc lies on the circle of radius r centred at (w - r, r).
    let centre = p(w - k.r, k.r);
    let a0 = (e1.y - centre.y).atan2(e1.x - centre.x);
    let e2 = p(e1.x + k.arc, e1.y + k.arc);
    let a1 = (e2.y - centre.y).atan2(e2.x - centre.x);
    let s2 = arc_seg(centre, k.r, a0, a1);
    let e3 = p(w, k.p);
    let s3 = Seg { c1: p(e2.x + k.d, e2.y + k.c), c2: p(e2.x + k.d, e2.y + k.b + k.c), to: e3 };
    (start, vec![s1, s2, s3])
}

/// Reverse a run of segments that starts at `start`; returns the new start.
fn reverse(start: Point<f32>, segs: &[Seg]) -> (Point<f32>, Vec<Seg>) {
    let mut points = vec![start];
    for s in segs {
        points.push(s.to);
    }
    let new_start = *points.last().unwrap();
    let mut out = Vec::with_capacity(segs.len());
    for (i, s) in segs.iter().enumerate().rev() {
        out.push(Seg { c1: s.c2, c2: s.c1, to: points[i] });
    }
    (new_start, out)
}

/// The squircle outline (`getSvgPath({width, height, cornerRadius,
/// cornerSmoothing})`) inside `bounds`, scaled by `scale` about the centre.
pub fn squircle(bounds: Bounds<Pixels>, radius: f32, smoothing: f32, scale: f32) -> Option<Path<Pixels>> {
    let mut b = PathBuilder::fill();
    add_squircle(&mut b, bounds, radius, smoothing, scale);
    b.build().ok()
}

pub fn add_squircle(b: &mut PathBuilder, bounds: Bounds<Pixels>, radius: f32, smoothing: f32, scale: f32) {
    let w = f32::from(bounds.size.width);
    let h = f32::from(bounds.size.height);
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let budget = w.min(h) / 2.0;
    let k = corner_params(radius, smoothing, budget);
    let (tr_start, tr) = top_right(w, &k);
    // Other corners by reflection, reversed so the outline stays continuous.
    let reflect = |segs: &[Seg], start: Point<f32>, f: &dyn Fn(Point<f32>) -> Point<f32>| {
        let mapped: Vec<Seg> = segs.iter().map(|s| Seg { c1: f(s.c1), c2: f(s.c2), to: f(s.to) }).collect();
        reverse(f(start), &mapped)
    };
    let (br_start, br) = reflect(&tr, tr_start, &|q: Point<f32>| p(q.x, h - q.y));
    let (bl_start, bl) = reflect(&br, br_start, &|q: Point<f32>| p(w - q.x, q.y));
    let (tl_start, tl) = reflect(&tr, tr_start, &|q: Point<f32>| p(w - q.x, q.y));

    let ox = f32::from(bounds.origin.x);
    let oy = f32::from(bounds.origin.y);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let xf = |q: Point<f32>| -> Point<Pixels> {
        point(px(ox + cx + (q.x - cx) * scale), px(oy + cy + (q.y - cy) * scale))
    };

    b.move_to(xf(tr_start));
    for run in [(&tr, tr_start), (&br, br_start), (&bl, bl_start), (&tl, tl_start)] {
        b.line_to(xf(run.1));
        for s in run.0.iter() {
            b.cubic_bezier_to(xf(s.to), xf(s.c1), xf(s.c2));
        }
    }
    b.close();
}

/// A plain rounded rectangle outline (circular corners) — used for shapes
/// the Tauri app drew with CSS `border-radius`, when they need scaling.
pub fn add_rounded_rect(b: &mut PathBuilder, bounds: Bounds<Pixels>, radius: f32) {
    let x = f32::from(bounds.origin.x);
    let y = f32::from(bounds.origin.y);
    let w = f32::from(bounds.size.width);
    let h = f32::from(bounds.size.height);
    let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    let pt = |a: f32, b: f32| point(px(a), px(b));
    let k = 0.552_284_8 * r;
    b.move_to(pt(x + r, y));
    b.line_to(pt(x + w - r, y));
    b.cubic_bezier_to(pt(x + w, y + r), pt(x + w - r + k, y), pt(x + w, y + r - k));
    b.line_to(pt(x + w, y + h - r));
    b.cubic_bezier_to(pt(x + w - r, y + h), pt(x + w, y + h - r + k), pt(x + w - r + k, y + h));
    b.line_to(pt(x + r, y + h));
    b.cubic_bezier_to(pt(x, y + h - r), pt(x + r - k, y + h), pt(x, y + h - r + k));
    b.line_to(pt(x, y + r));
    b.cubic_bezier_to(pt(x + r, y), pt(x, y + r - k), pt(x + r - k, y));
    b.close();
}

/// Append a circular arc (as cubics, ≤90° each) to `b`, from `a0` to `a1`
/// radians around `c`. Starts a new sub-path when `move_first`.
pub fn add_arc(b: &mut PathBuilder, c: Point<f32>, r: f32, a0: f32, a1: f32, move_first: bool) {
    let total = a1 - a0;
    let n = ((total.abs() / std::f32::consts::FRAC_PI_2).ceil() as usize).max(1);
    let step = total / n as f32;
    let start = p(c.x + r * a0.cos(), c.y + r * a0.sin());
    if move_first {
        b.move_to(point(px(start.x), px(start.y)));
    } else {
        b.line_to(point(px(start.x), px(start.y)));
    }
    for i in 0..n {
        let s = arc_seg(c, r, a0 + step * i as f32, a0 + step * (i + 1) as f32);
        b.cubic_bezier_to(
            point(px(s.to.x), px(s.to.y)),
            point(px(s.c1.x), px(s.c1.y)),
            point(px(s.c2.x), px(s.c2.y)),
        );
    }
}

/// A stroked arc with round caps, built as a filled outline (so it does not
/// depend on the tessellator's cap support): the band between radii
/// `r ± w/2` from `a0` to `a1`, closed with semicircular caps.
pub fn round_capped_arc(c: Point<f32>, r: f32, w: f32, a0: f32, a1: f32) -> Option<Path<Pixels>> {
    let mut b = PathBuilder::fill();
    if (a1 - a0).abs() >= std::f32::consts::TAU - 1e-3 {
        add_arc(&mut b, c, r + w / 2.0, 0.0, std::f32::consts::TAU, true);
        b.close();
        add_arc(&mut b, c, r - w / 2.0, std::f32::consts::TAU, 0.0, true);
        b.close();
        return b.build().ok();
    }
    let ro = r + w / 2.0;
    let ri = (r - w / 2.0).max(0.0);
    add_arc(&mut b, c, ro, a0, a1, true);
    // end cap: semicircle around the arc end point, outer → inner
    let end = p(c.x + r * a1.cos(), c.y + r * a1.sin());
    let dir = if a1 >= a0 { 1.0 } else { -1.0 };
    add_arc(&mut b, end, w / 2.0, a1, a1 + std::f32::consts::PI * dir, false);
    add_arc(&mut b, c, ri, a1, a0, false);
    let start = p(c.x + r * a0.cos(), c.y + r * a0.sin());
    add_arc(&mut b, start, w / 2.0, a0 + std::f32::consts::PI * dir, a0 + std::f32::consts::TAU * dir, false);
    b.close();
    b.build().ok()
}

/// A full ring (annulus) of centre-line radius `r` and width `w`.
pub fn ring(c: Point<f32>, r: f32, w: f32) -> Option<Path<Pixels>> {
    round_capped_arc(c, r, w, 0.0, std::f32::consts::TAU)
}

pub fn bounds_center(b: Bounds<Pixels>) -> Point<f32> {
    p(
        f32::from(b.origin.x) + f32::from(b.size.width) / 2.0,
        f32::from(b.origin.y) + f32::from(b.size.height) / 2.0,
    )
}

/// Shrink `b` about its centre by `scale` (CSS `transform: scale()` on a box).
pub fn scale_bounds(b: Bounds<Pixels>, scale: f32) -> Bounds<Pixels> {
    let w = f32::from(b.size.width);
    let h = f32::from(b.size.height);
    let dx = w * (1.0 - scale) / 2.0;
    let dy = h * (1.0 - scale) / 2.0;
    Bounds {
        origin: point(b.origin.x + px(dx), b.origin.y + px(dy)),
        size: gpui_kit::gpui::size(px(w * scale), px(h * scale)),
    }
}

/// Shrink `b` toward an anchor point (fractions of width/height) by `scale`.
pub fn scale_bounds_from(b: Bounds<Pixels>, scale: f32, ax: f32, ay: f32) -> Bounds<Pixels> {
    let w = f32::from(b.size.width);
    let h = f32::from(b.size.height);
    let dx = w * (1.0 - scale) * ax;
    let dy = h * (1.0 - scale) * ay;
    Bounds {
        origin: point(b.origin.x + px(dx), b.origin.y + px(dy)),
        size: gpui_kit::gpui::size(px(w * scale), px(h * scale)),
    }
}

pub fn inset(b: Bounds<Pixels>, by: f32) -> Bounds<Pixels> {
    Bounds {
        origin: point(b.origin.x + px(by), b.origin.y + px(by)),
        size: gpui_kit::gpui::size(
            px((f32::from(b.size.width) - 2.0 * by).max(0.0)),
            px((f32::from(b.size.height) - 2.0 * by).max(0.0)),
        ),
    }
}

/// A CSS rounded box (`border-radius`, 1px border) scaled about its centre —
/// for surfaces whose *background layer* scaled on press.
pub fn paint_rounded_bg(
    bounds: Bounds<Pixels>,
    radius: f32,
    scale: f32,
    fill: gpui_kit::gpui::Hsla,
    border: gpui_kit::gpui::Hsla,
    window: &mut gpui_kit::gpui::Window,
) {
    let b = scale_bounds(bounds, scale);
    let r = radius * scale;
    if fill.a > 0.001 {
        window.paint_quad(gpui_kit::gpui::fill(b, fill).corner_radii(px(r)));
    }
    if border.a > 0.001 {
        window.paint_quad(
            gpui_kit::gpui::quad(b, px(r), gpui_kit::gpui::transparent_black(), px(1.0), border, Default::default()),
        );
    }
}

/// Paint `img` (aspect `aspect` = w/h) into `bounds` with CSS `object-fit`
/// contain or cover, scaled about the centre and clipped to `bounds`.
pub fn paint_fit_image(
    bounds: Bounds<Pixels>,
    img: std::sync::Arc<gpui_kit::gpui::RenderImage>,
    aspect: f32,
    cover: bool,
    scale: f32,
    radius: f32,
    window: &mut gpui_kit::gpui::Window,
) {
    let bw = f32::from(bounds.size.width);
    let bh = f32::from(bounds.size.height);
    if bw <= 0.0 || bh <= 0.0 {
        return;
    }
    let box_aspect = bw / bh;
    let (w, h) = if (aspect > box_aspect) ^ cover { (bw, bw / aspect) } else { (bh * aspect, bh) };
    let (w, h) = (w * scale, h * scale);
    let x = f32::from(bounds.origin.x) + (bw - w) / 2.0;
    let y = f32::from(bounds.origin.y) + (bh - h) / 2.0;
    let target = Bounds { origin: point(px(x), px(y)), size: gpui_kit::gpui::size(px(w), px(h)) };
    // `bounds` clips (cover crops to the box); `target` is where the whole
    // image is laid out.
    let visible = if cover { scale_bounds(bounds, scale.min(1.0)) } else { target };
    let corner = if cover { px(radius) } else { px(0.0) };
    let _ = window.paint_image(visible, target, gpui_kit::gpui::Corners::all(corner), img, 0, false);
}
