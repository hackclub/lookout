use image::{DynamicImage, GenericImageView};

pub fn auto_crop_black_borders(mut img: DynamicImage) -> DynamicImage {
    let (width, height) = img.dimensions();

    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0;
    let mut max_y = 0;

    let threshold = 5u8;

    // Try to get a reference to avoid cloning the image buffer.
    // PipeWire always passes RGBA8 natively so this avoids allocations.
    let rgba_fallback;
    let rgba = if let Some(rgba) = img.as_rgba8() {
        rgba
    } else {
        rgba_fallback = img.to_rgba8();
        &rgba_fallback
    };

    let raw = rgba.as_raw();

    // Iterate over raw bytes in chunks of 4 (R, G, B, A).
    // This is orders of magnitude faster than `get_pixel(x, y)` because it avoids bounds checking.
    for (i, pixel) in raw.chunks_exact(4).enumerate() {
        if pixel[3] > threshold
            && (pixel[0] > threshold || pixel[1] > threshold || pixel[2] > threshold)
        {
            let x = (i as u32) % width;
            let y = (i as u32) / width;

            if x < min_x {
                min_x = x;
            }
            if x > max_x {
                max_x = x;
            }
            if y < min_y {
                min_y = y;
            }
            if y > max_y {
                max_y = y;
            }
        }
    }

    if min_x > max_x || min_y > max_y {
        return img;
    }

    let mut crop_width = max_x - min_x + 1;
    let mut crop_height = max_y - min_y + 1;

    if crop_width == width && crop_height == height {
        return img;
    }

    // Grow the box back out to even dimensions where there is room.
    //
    // This feeds the clip encoder, which needs even dimensions for H.264
    // 4:2:0 and therefore rounds whatever it is handed DOWN to even. An odd
    // result here means every frame of every clip misses the encoder's
    // exact-size path by one pixel; `clips::frame_to_bgra` handles that
    // losslessly now, but it is still a whole session recorded one pixel
    // short of its own capture for no reason. Only the PipeWire path calls
    // this, so it is also the only path that can produce an arbitrary size
    // at all.
    if crop_width % 2 == 1 {
        if min_x > 0 {
            min_x -= 1;
        }
        crop_width = max_x - min_x + 1;
        if crop_width % 2 == 1 && max_x + 1 < width {
            crop_width += 1;
        }
    }
    if crop_height % 2 == 1 {
        if min_y > 0 {
            min_y -= 1;
        }
        crop_height = max_y - min_y + 1;
        if crop_height % 2 == 1 && max_y + 1 < height {
            crop_height += 1;
        }
    }

    img.crop(min_x, min_y, crop_width, crop_height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    /// A frame with content in the middle of a black surround crops to the
    /// content — and to EVEN dimensions, because the crop output is what
    /// sizes the clip encoder.
    #[test]
    fn crops_to_content_with_even_dimensions() {
        // Content occupies an odd-sized box (x 10..=20, y 5..=13): 11x9.
        let mut img = RgbaImage::from_pixel(64, 48, Rgba([0, 0, 0, 255]));
        for x in 10..=20u32 {
            for y in 5..=13u32 {
                img.put_pixel(x, y, Rgba([200, 180, 40, 255]));
            }
        }
        let out = auto_crop_black_borders(DynamicImage::ImageRgba8(img));
        let (w, h) = out.dimensions();
        assert_eq!(w % 2, 0, "crop width {w} is odd");
        assert_eq!(h % 2, 0, "crop height {h} is odd");
        // Never smaller than the content it found.
        assert!(w >= 11 && h >= 9, "crop {w}x{h} lost content");
        assert!(w <= 12 && h <= 10, "crop {w}x{h} kept more than one pad pixel");
    }

    /// A full-bleed frame is returned untouched — no crop, no rounding.
    #[test]
    fn leaves_a_borderless_frame_alone() {
        let img = RgbaImage::from_pixel(65, 33, Rgba([90, 90, 90, 255]));
        let out = auto_crop_black_borders(DynamicImage::ImageRgba8(img));
        assert_eq!(out.dimensions(), (65, 33));
    }

    /// An all-black frame has no content box at all; it must come back whole
    /// rather than collapsing to nothing.
    #[test]
    fn leaves_an_all_black_frame_alone() {
        let img = RgbaImage::from_pixel(32, 16, Rgba([0, 0, 0, 255]));
        let out = auto_crop_black_borders(DynamicImage::ImageRgba8(img));
        assert_eq!(out.dimensions(), (32, 16));
    }
}
