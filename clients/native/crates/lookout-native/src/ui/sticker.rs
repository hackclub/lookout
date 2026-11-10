//! Hack Club stickers (from stickers.hackclub.com, credits in
//! `assets/stickers/CREDITS.md`) made to look like real ones at load time:
//! a die-cut white edge traced around the art, a slight tilt, and a soft
//! drop shadow baked into the bitmap. GPUI can't rotate or shadow an image,
//! so all of it happens once, here, on a background thread.

use std::sync::Arc;

use gpui_kit::gpui::RenderImage;
use image::{Rgba, RgbaImage};

/// One embedded sticker: its file name (for logs) and PNG bytes.
pub struct Art {
    pub name: &'static str,
    pub png: &'static [u8],
}

macro_rules! stickers {
    ($($name:literal),* $(,)?) => {
        &[$(Art { name: $name, png: include_bytes!(concat!("../../../../assets/stickers/", $name, ".png")) }),*]
    };
}

/// The pool the onboarding draws from.
pub static ALL: &[Art] = stickers!(
    // The first seven ring the icon on the welcome page: bold, chunky art
    // that still reads at 40pt. The rest replace them as they're poofed.
    "hackhackclub",
    "midnight-kitten",
    "i-heart-hc",
    "skullpup",
    "boba-orpheus",
    "pixel-orange",
    "sprig",
    "juice-box",
    "hack-ramen",
    "lifering-orpheus",
    "journpheus",
    "construct-orpheus",
    "orpheus-skateboard",
    "hackaccino",
    "kawaii-hack-club",
);

/// A sticker ready to paint.
#[derive(Clone)]
pub struct Sticker {
    pub image: Arc<RenderImage>,
    /// Bitmap size in pixels (shadow and edge included).
    pub w: u32,
    pub h: u32,
    /// How much of the bitmap's width/height the sticker itself spans
    /// (the rest is shadow room), so callers can size by the visible art.
    pub content_frac: f32,
    /// The art's average colour (sRGB, 0..1): the poof's confetti.
    pub tint: [f32; 3],
}

/// The die-cut edge, the shadow's blur and drop, and its strength, as
/// fractions of the art's longest side (so a sticker looks the same at 1×
/// and 2×).
const EDGE: f32 = 0.028;
const SHADOW_BLUR: f32 = 0.03;
const SHADOW_DY: f32 = 0.022;
const SHADOW_ALPHA: f32 = 0.26;
/// Everything is drawn at this multiple and filtered down, so the cut edge
/// is antialiased rather than traced from a hard alpha threshold.
const SUPERSAMPLE: u32 = 2;

/// Build a sticker whose art fits `max_px` (device pixels), tilted by
/// `tilt_deg`.
pub fn prepare(art: &Art, max_px: u32, tilt_deg: f32) -> anyhow::Result<Sticker> {
    let src = image::load_from_memory(art.png)?.into_rgba8();
    Ok(prepare_image(src, max_px, tilt_deg, EDGE))
}

/// A photo of `src` as a print: a thin white border (`edge` of its longest
/// side), tilted by `tilt_deg`, with the same soft baked shadow — for
/// pictures that need to be thrown about (GPUI can't rotate an element).
pub fn photo(src: &RgbaImage, max_px: u32, tilt_deg: f32) -> Sticker {
    prepare_image(src.clone(), max_px, tilt_deg, 0.016)
}

fn prepare_image(src: RgbaImage, max_px: u32, tilt_deg: f32, edge: f32) -> Sticker {
    // Trim the file's transparent margin so sizes mean the art itself.
    let src = image::DynamicImage::ImageRgba8(crop_to_alpha(src));
    let tint = average_colour(&src.to_rgba8());
    let work = max_px * SUPERSAMPLE;
    let src = src.resize(work, work, image::imageops::FilterType::Lanczos3).into_rgba8();
    let long = work as f32;
    let edge_px = (edge * long).max(1.5);
    let blur = ((SHADOW_BLUR * long) as usize).max(1);
    let dy = (SHADOW_DY * long).round() as i32;

    // Room for the tilt, the edge and the shadow on every side.
    let pad = edge_px.ceil() as u32 + blur as u32 * 2 + dy.unsigned_abs();
    let pad = pad.div_ceil(SUPERSAMPLE) * SUPERSAMPLE;
    let rotated = rotate(&src, tilt_deg.to_radians(), pad);
    let (w, h) = rotated.dimensions();

    // Distance from every pixel to the art (sub-pixel: the threshold
    // crossing is offset by the art's own alpha), then the edge's coverage.
    let solid: Vec<bool> = rotated.pixels().map(|p| p.0[3] >= 128).collect();
    let dist = distance_field(&solid, w as usize, h as usize);
    let edge: Vec<f32> = dist.iter().map(|d| (edge_px + 0.5 - d).clamp(0.0, 1.0)).collect();

    // The shadow: the cut-out silhouette, blurred and dropped a little.
    let mut shadow = vec![0.0f32; edge.len()];
    for y in 0..h as i32 {
        let sy = y - dy;
        if sy < 0 || sy >= h as i32 {
            continue;
        }
        for x in 0..w as usize {
            shadow[y as usize * w as usize + x] = edge[sy as usize * w as usize + x];
        }
    }
    for _ in 0..3 {
        box_blur(&mut shadow, w as usize, h as usize, blur / 2);
    }

    // Composite back to front: shadow, white edge, art (premultiplied maths).
    let mut out = RgbaImage::new(w, h);
    for (i, px) in out.pixels_mut().enumerate() {
        let x = i % w as usize;
        let y = i / w as usize;
        let a = rotated.get_pixel(x as u32, y as u32).0;
        let art_a = a[3] as f32 / 255.0;
        let mut alpha = shadow[i] * SHADOW_ALPHA;
        let (mut r, mut g, mut b) = (0.0f32, 0.0f32, 0.0f32);
        let e = edge[i];
        r = r * (1.0 - e) + e;
        g = g * (1.0 - e) + e;
        b = b * (1.0 - e) + e;
        alpha = alpha * (1.0 - e) + e;
        let (ar, ag, ab) = (a[0] as f32 / 255.0, a[1] as f32 / 255.0, a[2] as f32 / 255.0);
        r = r * (1.0 - art_a) + ar * art_a;
        g = g * (1.0 - art_a) + ag * art_a;
        b = b * (1.0 - art_a) + ab * art_a;
        alpha = alpha * (1.0 - art_a) + art_a;
        let un = |c: f32| if alpha > 0.0 { (c / alpha).clamp(0.0, 1.0) } else { 0.0 };
        *px = Rgba([(un(r) * 255.0).round() as u8, (un(g) * 255.0).round() as u8, (un(b) * 255.0).round() as u8, (alpha * 255.0).round() as u8]);
    }
    // Filter down to the real size, in premultiplied space so the edge
    // doesn't pick up a dark fringe.
    let out = downsample_premultiplied(out, SUPERSAMPLE);
    let (w, h) = out.dimensions();
    let content_frac = 1.0 - 2.0 * ((pad as f32 - edge_px) / SUPERSAMPLE as f32) / w.max(h) as f32;
    Sticker { image: crate::images::rgba_to_render_image(out), w, h, content_frac, tint }
}

fn downsample_premultiplied(img: RgbaImage, by: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let mut pm = img;
    for p in pm.pixels_mut() {
        let a = p.0[3] as u16;
        for c in 0..3 {
            p.0[c] = ((p.0[c] as u16 * a + 127) / 255) as u8;
        }
    }
    let mut small = image::imageops::resize(&pm, w / by, h / by, image::imageops::FilterType::Lanczos3);
    for p in small.pixels_mut() {
        let a = p.0[3] as u16;
        if a > 0 {
            for c in 0..3 {
                p.0[c] = ((p.0[c] as u16 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    small
}

fn crop_to_alpha(img: RgbaImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (x, y, p) in img.enumerate_pixels() {
        if p.0[3] > 8 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x1 < x0 || y1 < y0 {
        return img;
    }
    image::imageops::crop_imm(&img, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image()
}

fn average_colour(img: &RgbaImage) -> [f32; 3] {
    let (mut r, mut g, mut b, mut n) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for p in img.pixels() {
        let a = p.0[3] as f64 / 255.0;
        if a < 0.5 {
            continue;
        }
        // Skip near-white and near-black: confetti should carry the colour.
        let (pr, pg, pb) = (p.0[0] as f64, p.0[1] as f64, p.0[2] as f64);
        let max = pr.max(pg).max(pb);
        let min = pr.min(pg).min(pb);
        if max - min < 40.0 {
            continue;
        }
        r += pr;
        g += pg;
        b += pb;
        n += 1.0;
    }
    if n == 0.0 {
        return [0.95, 0.95, 0.95];
    }
    [(r / n / 255.0) as f32, (g / n / 255.0) as f32, (b / n / 255.0) as f32]
}

/// Rotate about the centre into a canvas big enough for the result plus
/// `pad` on every side (bilinear, premultiplied so edges don't fringe).
fn rotate(src: &RgbaImage, rad: f32, pad: u32) -> RgbaImage {
    let (sw, sh) = (src.width() as f32, src.height() as f32);
    let (s, c) = rad.sin_cos();
    let bw = (sw * c.abs() + sh * s.abs()).ceil() as u32;
    let bh = (sw * s.abs() + sh * c.abs()).ceil() as u32;
    let (w, h) = (bw + pad * 2, bh + pad * 2);
    let mut out = RgbaImage::new(w, h);
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let (scx, scy) = (sw / 2.0, sh / 2.0);
    let fetch = |x: i32, y: i32| -> [f32; 4] {
        if x < 0 || y < 0 || x >= src.width() as i32 || y >= src.height() as i32 {
            return [0.0; 4];
        }
        let p = src.get_pixel(x as u32, y as u32).0;
        let a = p[3] as f32 / 255.0;
        [p[0] as f32 / 255.0 * a, p[1] as f32 / 255.0 * a, p[2] as f32 / 255.0 * a, a]
    };
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            // Inverse rotation back into the source.
            let sx = dx * c + dy * s + scx - 0.5;
            let sy = -dx * s + dy * c + scy - 0.5;
            let (x0, y0) = (sx.floor() as i32, sy.floor() as i32);
            let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
            let p00 = fetch(x0, y0);
            let p10 = fetch(x0 + 1, y0);
            let p01 = fetch(x0, y0 + 1);
            let p11 = fetch(x0 + 1, y0 + 1);
            let mut v = [0.0f32; 4];
            for k in 0..4 {
                let top = p00[k] * (1.0 - fx) + p10[k] * fx;
                let bot = p01[k] * (1.0 - fx) + p11[k] * fx;
                v[k] = top * (1.0 - fy) + bot * fy;
            }
            if v[3] <= 0.0 {
                continue;
            }
            let un = |c: f32| ((c / v[3]).clamp(0.0, 1.0) * 255.0).round() as u8;
            out.put_pixel(x, y, Rgba([un(v[0]), un(v[1]), un(v[2]), (v[3] * 255.0).round() as u8]));
        }
    }
    out
}

/// Approximate Euclidean distance to the nearest `solid` pixel (two-pass
/// chamfer with 1 / √2 steps): plenty for a 7px edge.
fn distance_field(solid: &[bool], w: usize, h: usize) -> Vec<f32> {
    const INF: f32 = 1e9;
    const D: f32 = std::f32::consts::SQRT_2;
    let mut d: Vec<f32> = solid.iter().map(|s| if *s { 0.0 } else { INF }).collect();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut v = d[i];
            if x > 0 {
                v = v.min(d[i - 1] + 1.0);
            }
            if y > 0 {
                v = v.min(d[i - w] + 1.0);
                if x > 0 {
                    v = v.min(d[i - w - 1] + D);
                }
                if x + 1 < w {
                    v = v.min(d[i - w + 1] + D);
                }
            }
            d[i] = v;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            let mut v = d[i];
            if x + 1 < w {
                v = v.min(d[i + 1] + 1.0);
            }
            if y + 1 < h {
                v = v.min(d[i + w] + 1.0);
                if x + 1 < w {
                    v = v.min(d[i + w + 1] + D);
                }
                if x > 0 {
                    v = v.min(d[i + w - 1] + D);
                }
            }
            d[i] = v;
        }
    }
    d
}

/// One separable box-blur pass of radius `r` (three passes ≈ a Gaussian).
fn box_blur(buf: &mut [f32], w: usize, h: usize, r: usize) {
    if r == 0 {
        return;
    }
    let mut tmp = vec![0.0f32; buf.len()];
    let norm = 1.0 / (2 * r + 1) as f32;
    for y in 0..h {
        let row = &buf[y * w..(y + 1) * w];
        let mut acc: f32 = (0..=r).map(|x| row[x.min(w - 1)]).sum::<f32>() + row[0] * r as f32;
        for x in 0..w {
            tmp[y * w + x] = acc * norm;
            let add = row[(x + r + 1).min(w - 1)];
            let sub = row[x.saturating_sub(r)];
            acc += add - sub;
        }
    }
    for x in 0..w {
        let col = |y: usize| tmp[y.min(h - 1) * w + x];
        let mut acc: f32 = (0..=r).map(col).sum::<f32>() + col(0) * r as f32;
        for y in 0..h {
            buf[y * w + x] = acc * norm;
            acc += col(y + r + 1) - col(y.saturating_sub(r));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sticker_prepares() {
        for (i, art) in ALL.iter().enumerate() {
            let s = prepare(art, 120, if i % 2 == 0 { 8.0 } else { -6.0 }).unwrap_or_else(|e| panic!("{}: {e}", art.name));
            // `LOOKOUT_STICKER_DUMP=<dir>` writes them out to look at.
            if let Ok(dir) = std::env::var("LOOKOUT_STICKER_DUMP") {
                let mut bgra = s.image.as_bytes(0).unwrap().to_vec();
                for px in bgra.chunks_mut(4) {
                    px.swap(0, 2);
                }
                image::RgbaImage::from_raw(s.w, s.h, bgra).unwrap().save(format!("{dir}/{}.png", art.name)).unwrap();
            }
            assert!(s.w.max(s.h) > 110 && s.content_frac > 0.5 && s.content_frac < 1.0, "{}", art.name);
        }
    }

    #[test]
    fn distance_field_is_euclidean_enough() {
        let (w, h) = (21, 21);
        let mut solid = vec![false; w * h];
        solid[10 * w + 10] = true;
        let d = distance_field(&solid, w, h);
        assert_eq!(d[10 * w + 15], 5.0);
        assert!((d[15 * w + 15] - 5.0 * std::f32::consts::SQRT_2).abs() < 1e-3);
    }
}
