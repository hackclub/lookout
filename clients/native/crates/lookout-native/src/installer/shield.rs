//! The UAC shield for buttons that will raise a UAC prompt. On Windows it's
//! the system's own (`SHGetStockIconInfo(SIID_SHIELD)`), so it matches the
//! running Windows version; elsewhere (the installer demo) a stand-in SVG.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::gpui::{ImageSource, RenderImage};

thread_local! {
    static CACHE: RefCell<HashMap<u32, Option<Arc<RenderImage>>>> = RefCell::new(HashMap::new());
}

/// The shield at `px` device pixels square, as an image source.
pub fn source(px: u32) -> ImageSource {
    let sys = CACHE.with(|c| c.borrow_mut().entry(px).or_insert_with(|| system_shield(px)).clone());
    match sys {
        Some(img) => ImageSource::Render(img),
        None => "installer/uac-shield.svg".into(),
    }
}

/// The 16px shield element for a button, sharp at 1× and 2×.
pub fn shield_img() -> gpui_kit::gpui::Img {
    use gpui_kit::gpui::{Styled, img, px};
    img(source(32)).size(px(16.0)).flex_none()
}

#[cfg(windows)]
fn system_shield(px: u32) -> Option<Arc<RenderImage>> {
    use windows::Win32::UI::Shell::{SHDefExtractIconW, SHGSI_ICONLOCATION, SHGetStockIconInfo, SHSTOCKICONINFO, SIID_SHIELD};
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};
    use windows::core::PCWSTR;

    unsafe {
        let mut info = SHSTOCKICONINFO { cbSize: std::mem::size_of::<SHSTOCKICONINFO>() as u32, ..Default::default() };
        SHGetStockIconInfo(SIID_SHIELD, SHGSI_ICONLOCATION, &mut info).ok()?;
        let mut icon = HICON::default();
        SHDefExtractIconW(PCWSTR(info.szPath.as_ptr()), info.iIcon, 0, Some(&mut icon), None, px).ok().ok()?;
        if icon.is_invalid() {
            return None;
        }
        let rgba = hicon_to_rgba(icon);
        let _ = DestroyIcon(icon);
        rgba.map(crate::images::rgba_to_render_image)
    }
}

/// A 32-bit icon's colour bitmap as RGBA (shell icons carry their alpha).
#[cfg(windows)]
unsafe fn hicon_to_rgba(icon: windows::Win32::UI::WindowsAndMessaging::HICON) -> Option<image::RgbaImage> {
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW, HGDIOBJ,
        ReleaseDC,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};

    unsafe {
        let mut ii = ICONINFO::default();
        GetIconInfo(icon, &mut ii).ok()?;
        let free = |ii: &ICONINFO| {
            let _ = DeleteObject(HGDIOBJ(ii.hbmColor.0));
            let _ = DeleteObject(HGDIOBJ(ii.hbmMask.0));
        };
        if ii.hbmColor.is_invalid() {
            free(&ii);
            return None;
        }
        let mut bm = BITMAP::default();
        if GetObjectW(HGDIOBJ(ii.hbmColor.0), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 {
            free(&ii);
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight);
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let dc = GetDC(None);
        let lines = GetDIBits(dc, ii.hbmColor, 0, h as u32, Some(buf.as_mut_ptr() as *mut _), &mut bmi, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        free(&ii);
        if lines == 0 {
            return None;
        }
        for p in buf.chunks_exact_mut(4) {
            p.swap(0, 2); // BGRA → RGBA
        }
        image::RgbaImage::from_raw(w as u32, h as u32, buf)
    }
}

#[cfg(not(windows))]
fn system_shield(_: u32) -> Option<Arc<RenderImage>> {
    None
}
