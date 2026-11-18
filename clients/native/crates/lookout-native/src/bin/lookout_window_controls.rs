//! Draws libadwaita's header-bar close button to PNGs, for Lookout's GNOME
//! header bar (see `platform/gtk_window_controls.rs`, which runs this and
//! reads the result).
//!
//! A binary of its own because GTK 3 and GTK 4 can't share a process and
//! `lookout` links GTK 3 (WebKitGTK). This one links nothing but libc and
//! `dlopen`s libadwaita, so a system without it just exits non-zero.
//!
//! It builds a header bar in a window that is never shown, realizes the
//! window and maps the bar by hand (enough for GTK to style and snapshot
//! it), then renders the close button through GSK's cairo path once per
//! state: normal, hover, pressed and backdrop, in light and dark. Output:
//! `close-{light,dark}-{state}.png` at 2x, plus `meta` holding the images'
//! logical size.

#[cfg(not(target_os = "linux"))]
fn main() {
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
fn main() {
    imp::main()
}

#[cfg(target_os = "linux")]
mod imp {
use std::path::Path;

/// Device pixels per logical pixel in the PNGs: exact at 1x and 2x.
const SCALE: f64 = 2.0;
/// Logical padding around the button in each image (a theme's shadow or glow).
const MARGIN: f64 = 6.0;
const STATES: [&str; 4] = ["normal", "hover", "pressed", "backdrop"];

pub fn main() {
    let Some(out) = std::env::args_os().nth(1) else {
        eprintln!("usage: lookout-window-controls <out dir>");
        std::process::exit(2);
    };
    // GTK rasterises icons at the widget's scale factor, which a window
    // that's never shown can't learn from a monitor. GDK_SCALE sets it, but
    // only on X11, so prefer X11 (XWayland on a Wayland session) when there
    // is one. Nothing reaches the screen either way.
    // SAFETY: still single-threaded; GTK isn't loaded yet.
    unsafe {
        if std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty()) {
            std::env::set_var("GDK_BACKEND", "x11");
        }
        std::env::set_var("GDK_SCALE", format!("{}", SCALE as i32));
        std::env::remove_var("GDK_DPI_SCALE");
    }
    let code = match unsafe { render(Path::new(&out)) } {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("lookout-window-controls: {e}");
            1
        }
    };
    std::process::exit(code)
}

type P = *mut std::ffi::c_void;
type CStr = *const std::ffi::c_char;

#[repr(C)]
#[derive(Default)]
struct GrapheneRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

const STATE_ACTIVE: i32 = 1 << 0;
const STATE_PRELIGHT: i32 = 1 << 1;
const STATE_BACKDROP: i32 = 1 << 6;
const ADW_COLOR_SCHEME_FORCE_LIGHT: i32 = 1;
const ADW_COLOR_SCHEME_FORCE_DARK: i32 = 4;

macro_rules! syms {
    ($($name:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        #[allow(non_snake_case)]
        struct Gtk { $($name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)* }
        impl Gtk {
            unsafe fn load(lib: P) -> Result<Self, String> {
                Ok(Self { $($name: {
                    let f = unsafe { libc::dlsym(lib, concat!(stringify!($name), "\0").as_ptr().cast()) };
                    if f.is_null() { return Err(format!("missing {}", stringify!($name))); }
                    unsafe { std::mem::transmute::<P, unsafe extern "C" fn($($arg),*) $(-> $ret)?>(f) }
                },)* })
            }
        }
    };
}

syms! {
    adw_init: fn();
    adw_style_manager_get_default: fn() -> P;
    adw_style_manager_set_color_scheme: fn(P, i32);
    gtk_settings_get_default: fn() -> P;
    gtk_window_new: fn() -> P;
    gtk_window_set_titlebar: fn(P, P);
    gtk_window_destroy: fn(P);
    gtk_header_bar_new: fn() -> P;
    gtk_header_bar_set_decoration_layout: fn(P, CStr);
    gtk_widget_realize: fn(P);
    gtk_widget_map: fn(P);
    gtk_widget_measure: fn(P, i32, i32, *mut i32, *mut i32, *mut i32, *mut i32);
    gtk_widget_allocate: fn(P, i32, i32, i32, P);
    gtk_widget_set_state_flags: fn(P, i32, i32);
    gtk_widget_unset_state_flags: fn(P, i32);
    gtk_widget_get_first_child: fn(P) -> P;
    gtk_widget_get_next_sibling: fn(P) -> P;
    gtk_widget_get_parent: fn(P) -> P;
    gtk_widget_has_css_class: fn(P, CStr) -> i32;
    gtk_widget_compute_bounds: fn(P, P, *mut GrapheneRect) -> i32;
    gtk_widget_snapshot_child: fn(P, P, P);
    gtk_snapshot_new: fn() -> P;
    gtk_snapshot_free_to_node: fn(P) -> P;
    gsk_render_node_draw: fn(P, P);
    gsk_render_node_unref: fn(P);
    cairo_image_surface_create: fn(i32, i32, i32) -> P;
    cairo_create: fn(P) -> P;
    cairo_scale: fn(P, f64, f64);
    cairo_translate: fn(P, f64, f64);
    cairo_destroy: fn(P);
    cairo_surface_flush: fn(P);
    cairo_surface_write_to_png: fn(P, CStr) -> i32;
    cairo_surface_destroy: fn(P);
}

unsafe fn render(out: &Path) -> Result<(), String> {
    let lib = unsafe { libc::dlopen(c"libadwaita-1.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if lib.is_null() {
        return Err("libadwaita-1.so.0 not found".into());
    }
    let g = unsafe { Gtk::load(lib)? };
    let g_object_set = unsafe { libc::dlsym(lib, c"g_object_set".as_ptr()) };
    if g_object_set.is_null() {
        return Err("missing g_object_set".into());
    }
    let g_object_set: unsafe extern "C" fn(P, CStr, ...) = unsafe { std::mem::transmute(g_object_set) };
    unsafe {
        (g.adw_init)();
        // No main loop runs here, so a CSS transition would never get
        // past its first frame: render every state at its end value.
        g_object_set((g.gtk_settings_get_default)(), c"gtk-enable-animations".as_ptr(), 0i32, std::ptr::null::<u8>());
    }
    let manager = unsafe { (g.adw_style_manager_get_default)() };
    let mut size = None;
    for (name, scheme) in [("light", ADW_COLOR_SCHEME_FORCE_LIGHT), ("dark", ADW_COLOR_SCHEME_FORCE_DARK)] {
        unsafe { (g.adw_style_manager_set_color_scheme)(manager, scheme) };
        for (i, state) in STATES.iter().enumerate() {
            let path = out.join(format!("close-{name}-{state}.png"));
            let s = unsafe { render_one(&g, i, &path)? };
            size.get_or_insert(s);
        }
    }
    let (w, h) = size.ok_or("nothing rendered")?;
    std::fs::write(out.join("meta"), format!("{w} {h}\n")).map_err(|e| e.to_string())
}

/// One state of the close button to `path`; returns the image's logical size.
unsafe fn render_one(g: &Gtk, state: usize, path: &Path) -> Result<(f64, f64), String> {
    unsafe {
        let win = (g.gtk_window_new)();
        let bar = (g.gtk_header_bar_new)();
        (g.gtk_header_bar_set_decoration_layout)(bar, c":close".as_ptr());
        (g.gtk_window_set_titlebar)(win, bar);
        let result = (|| {
            let button = find_close(g, bar).ok_or("no close button in the header bar")?;
            // Never shown, never focused: GTK would draw it as backdrop.
            if state == 3 {
                (g.gtk_widget_set_state_flags)(win, STATE_BACKDROP, 0);
            } else {
                (g.gtk_widget_unset_state_flags)(win, STATE_BACKDROP);
            }
            match state {
                1 => (g.gtk_widget_set_state_flags)(button, STATE_PRELIGHT, 0),
                2 => (g.gtk_widget_set_state_flags)(button, STATE_PRELIGHT | STATE_ACTIVE, 0),
                _ => {}
            }
            (g.gtk_widget_realize)(win);
            (g.gtk_widget_map)(bar);
            let (mut min, mut nat, mut b1, mut b2) = (0, 0, 0, 0);
            (g.gtk_widget_measure)(bar, 0, -1, &mut min, &mut nat, &mut b1, &mut b2);
            let width = nat.max(200);
            (g.gtk_widget_measure)(bar, 1, width, &mut min, &mut nat, &mut b1, &mut b2);
            (g.gtk_widget_allocate)(bar, width, nat, -1, std::ptr::null_mut());
            let parent = (g.gtk_widget_get_parent)(button);
            let mut r = GrapheneRect::default();
            if (g.gtk_widget_compute_bounds)(button, parent, &mut r) == 0 || r.w < 1.0 || r.h < 1.0 {
                return Err("close button has no size".to_string());
            }
            let snap = (g.gtk_snapshot_new)();
            (g.gtk_widget_snapshot_child)(parent, button, snap);
            let node = (g.gtk_snapshot_free_to_node)(snap);
            if node.is_null() {
                return Err("close button drew nothing".to_string());
            }
            let (w, h) = (r.w as f64 + 2.0 * MARGIN, r.h as f64 + 2.0 * MARGIN);
            let surface = (g.cairo_image_surface_create)(0, (w * SCALE).ceil() as i32, (h * SCALE).ceil() as i32);
            let cr = (g.cairo_create)(surface);
            (g.cairo_scale)(cr, SCALE, SCALE);
            (g.cairo_translate)(cr, MARGIN - r.x as f64, MARGIN - r.y as f64);
            (g.gsk_render_node_draw)(node, cr);
            (g.cairo_destroy)(cr);
            (g.cairo_surface_flush)(surface);
            let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
            let status = (g.cairo_surface_write_to_png)(surface, c_path.as_ptr());
            (g.cairo_surface_destroy)(surface);
            (g.gsk_render_node_unref)(node);
            if status != 0 {
                return Err(format!("writing {} failed (cairo status {status})", path.display()));
            }
            Ok((w, h))
        })();
        (g.gtk_window_destroy)(win);
        result
    }
}

/// The `button.close` inside the bar's window controls.
unsafe fn find_close(g: &Gtk, w: P) -> Option<P> {
    unsafe {
        let mut c = (g.gtk_widget_get_first_child)(w);
        while !c.is_null() {
            if (g.gtk_widget_has_css_class)(c, c"close".as_ptr()) != 0 {
                return Some(c);
            }
            if let Some(found) = find_close(g, c) {
                return Some(found);
            }
            c = (g.gtk_widget_get_next_sibling)(c);
        }
        None
    }
}
}
