//! WebKitGTK software composition for a GPUI-owned Wayland window.
//!
//! GTK cannot parent a widget to another toolkit's wl_surface. Keep WebKit
//! in a GtkOffscreenWindow, disable accelerated compositing (which needs a
//! native GDK surface), and paint its snapshots into GPUI instead. No X11
//! connection, auxiliary toplevel, or nested compositor is needed.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::gpui::{Bounds, Keystroke, Modifiers, MouseButton, Pixels, Point, Window};
use gtk::gdk;
use gtk::glib::translate::*;
use gtk::prelude::*;
use webkit2gtk::{SettingsExt, WebViewExt};
use wry::WebViewExtUnix;

use super::WebEvent;

pub struct Offscreen {
    window: gtk::OffscreenWindow,
    pub container: gtk::Fixed,
    pending: Rc<Cell<bool>>,
    alive: Rc<Cell<bool>>,
    last_frame: Cell<Instant>,
    size: Cell<(i32, i32, f32)>,
    sender: RefCell<Option<flume::Sender<WebEvent>>>,
}

impl Offscreen {
    pub fn new() -> Self {
        let window = gtk::OffscreenWindow::new();
        let container = gtk::Fixed::new();
        window.add(&container);
        Self {
            window,
            container,
            pending: Rc::new(Cell::new(false)),
            alive: Rc::new(Cell::new(true)),
            last_frame: Cell::new(Instant::now() - Duration::from_secs(1)),
            size: Cell::new((0, 0, 0.0)),
            sender: RefCell::new(None),
        }
    }

    pub fn attach(&self, view: &wry::WebView, sender: flume::Sender<WebEvent>) {
        let web = view.webview();
        if let Some(settings) = WebViewExt::settings(&web) {
            settings.set_hardware_acceleration_policy(webkit2gtk::HardwareAccelerationPolicy::Never);
        }
        self.window.set_default_size(1, 1);
        self.window.show_all();
        self.sender.replace(Some(sender));
    }

    pub fn place(&self, bounds: Option<Bounds<Pixels>>, window: &mut Window, view: &wry::WebView) {
        let Some(bounds) = bounds else { return };
        let web = view.webview();
        let scale = window.scale_factor() / web.scale_factor().max(1) as f32;
        let width = (f32::from(bounds.size.width) * scale).ceil().max(1.0) as i32;
        let height = (f32::from(bounds.size.height) * scale).ceil().max(1.0) as i32;
        if self.size.replace((width, height, scale)) != (width, height, scale) {
            // Render at device resolution; zoom preserves CSS/layout pixels.
            web.set_zoom_level(scale as f64);
            web.set_size_request(width, height);
            self.container.set_size_request(width, height);
            self.window.resize(width, height);
        }
        window.request_animation_frame();
        self.capture(view);
    }

    fn capture(&self, view: &wry::WebView) {
        let web = view.webview();
        if self.pending.get() || self.last_frame.get().elapsed() < Duration::from_millis(33) || web.is_loading() {
            return;
        }
        let Some(sender) = self.sender.borrow().clone() else { return };
        self.pending.set(true);
        self.last_frame.set(Instant::now());
        let pending = self.pending.clone();
        let alive = self.alive.clone();
        web.snapshot(
            webkit2gtk::SnapshotRegion::Visible,
            webkit2gtk::SnapshotOptions::NONE,
            None::<&gtk::gio::Cancellable>,
            move |result| {
                pending.set(false);
                if !alive.get() {
                    return;
                }
                match result.and_then(|surface| {
                    // WebKit returns an image surface for software snapshots.
                    let mut image = gtk::cairo::ImageSurface::try_from(surface)
                        .map_err(|_| gtk::glib::Error::new(gtk::gio::IOErrorEnum::Failed, "WebKit snapshot is not an image"))?;
                    let (width, height, stride) = (image.width() as u32, image.height() as u32, image.stride() as usize);
                    let data = image.data().map_err(|e| gtk::glib::Error::new(gtk::gio::IOErrorEnum::Failed, &e.to_string()))?;
                    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
                    for row in data.chunks(stride).take(height as usize) {
                        pixels.extend_from_slice(&row[..width as usize * 4]);
                    }
                    // Cairo ARGB32 on little-endian Linux is GPUI's BGRA format.
                    let rgba = image::RgbaImage::from_raw(width, height, pixels)
                        .ok_or_else(|| gtk::glib::Error::new(gtk::gio::IOErrorEnum::Failed, "Invalid WebKit snapshot size"))?;
                    Ok(std::sync::Arc::new(gpui_kit::gpui::RenderImage::new(smallvec::smallvec![image::Frame::new(rgba)])))
                }) {
                    Ok(image) => {
                        let _ = sender.send(WebEvent::Frame(image));
                    }
                    Err(e) => log::warn!("[panel] native Wayland snapshot failed: {e}"),
                }
            },
        );
    }

    fn event(&self, web: &webkit2gtk::WebView, kind: gdk::EventType, fill: impl FnOnce(*mut gdk::ffi::GdkEvent)) {
        let Some(window) = web.window() else { return };
        let mut event = gdk::Event::new(kind);
        // SAFETY: GDK allocated the union for `kind`; each caller fills only
        // that variant. The window reference is transferred to the event,
        // whose destructor releases it after synchronous GTK dispatch.
        unsafe {
            let raw: *mut gdk::ffi::GdkEvent = event.to_glib_none_mut().0;
            (*raw).any.window = window.into_glib_ptr();
            (*raw).any.send_event = 1;
            fill(raw);
        }
        if let Some(seat) = gdk::Display::default().and_then(|d| d.default_seat()) {
            let keyboard = matches!(kind, gdk::EventType::KeyPress | gdk::EventType::KeyRelease);
            event.set_device(if keyboard { seat.keyboard() } else { seat.pointer() }.as_ref());
        }
        web.event(&event);
    }

    pub fn pointer(
        &self,
        view: &wry::WebView,
        position: Point<Pixels>,
        origin: Point<Pixels>,
        button: Option<MouseButton>,
        down: Option<bool>,
        modifiers: Modifiers,
    ) {
        let scale = self.size.get().2 as f64;
        let (x, y) = (f64::from(position.x - origin.x) * scale, f64::from(position.y - origin.y) * scale);
        let kind = match down {
            Some(true) => gdk::EventType::ButtonPress,
            Some(false) => gdk::EventType::ButtonRelease,
            None => gdk::EventType::MotionNotify,
        };
        let web = view.webview();
        if down == Some(true) {
            web.grab_focus();
        }
        self.event(&web, kind, |raw| unsafe {
            if down.is_some() {
                (*raw).button.time = (gtk::glib::ffi::g_get_monotonic_time() / 1000) as u32;
                (*raw).button.x = x;
                (*raw).button.y = y;
                (*raw).button.button = match button {
                    Some(MouseButton::Right) => 3,
                    Some(MouseButton::Middle) => 2,
                    _ => 1,
                };
                (*raw).button.state = modifier_bits(modifiers);
            } else {
                (*raw).motion.time = (gtk::glib::ffi::g_get_monotonic_time() / 1000) as u32;
                (*raw).motion.x = x;
                (*raw).motion.y = y;
                (*raw).motion.state =
                    modifier_bits(modifiers) | if button == Some(MouseButton::Left) { gdk::ffi::GDK_BUTTON1_MASK } else { 0 };
            }
        });
    }

    pub fn scroll(&self, view: &wry::WebView, position: Point<Pixels>, origin: Point<Pixels>, delta: Point<Pixels>, modifiers: Modifiers) {
        // GtkOffscreenWindow has no compositor input surface. Route wheel
        // dispatch/default scrolling through the page, as we do IME editing.
        let script = format!(
            "window.__lookoutScroll?.({})",
            serde_json::json!({
                "x": f32::from(position.x - origin.x), "y": f32::from(position.y - origin.y),
                "dx": -f32::from(delta.x), "dy": -f32::from(delta.y),
                "shiftKey": modifiers.shift, "ctrlKey": modifiers.control,
                "altKey": modifiers.alt, "metaKey": modifiers.platform,
            })
        );
        if let Err(e) = view.evaluate_script(&script) {
            log::warn!("[panel] webview scrolling failed: {e}");
        }
    }

    pub fn key(&self, view: &wry::WebView, stroke: &Keystroke, down: bool) {
        let keyval = keyval(stroke);
        if keyval == 0 {
            return;
        }
        let entry = gdk::Display::default()
            .and_then(|display| gdk::Keymap::for_display(&display).and_then(|map| map.entries_for_keyval(keyval).into_iter().next()));
        self.event(&view.webview(), if down { gdk::EventType::KeyPress } else { gdk::EventType::KeyRelease }, |raw| unsafe {
            (*raw).key.time = (gtk::glib::ffi::g_get_monotonic_time() / 1000) as u32;
            (*raw).key.keyval = keyval;
            if let Some(entry) = &entry {
                (*raw).key.hardware_keycode = entry.keycode() as u16;
                (*raw).key.group = entry.group() as u8;
            }
            (*raw).key.state = modifier_bits(stroke.modifiers);
        });
    }
}

fn modifier_bits(m: Modifiers) -> u32 {
    (if m.shift { gdk::ffi::GDK_SHIFT_MASK } else { 0 })
        | (if m.control { gdk::ffi::GDK_CONTROL_MASK } else { 0 })
        | (if m.alt { gdk::ffi::GDK_MOD1_MASK } else { 0 })
        | (if m.platform { gdk::ffi::GDK_SUPER_MASK } else { 0 })
}

fn keyval(stroke: &Keystroke) -> u32 {
    let name = match stroke.key.as_str() {
        "backspace" => "BackSpace",
        "delete" => "Delete",
        "enter" => "Return",
        "escape" => "Escape",
        "tab" => "Tab",
        "space" => "space",
        "left" => "Left",
        "right" => "Right",
        "up" => "Up",
        "down" => "Down",
        "home" => "Home",
        "end" => "End",
        "pageup" => "Page_Up",
        "pagedown" => "Page_Down",
        other => other,
    };
    if let Some(text) = stroke
        .key_char
        .as_deref()
        .filter(|text| !stroke.modifiers.control && !stroke.modifiers.platform && !text.chars().any(char::is_control))
    {
        if let Some(ch) = text.chars().next() {
            // SAFETY: gdk_unicode_to_keyval accepts any Unicode scalar value.
            return unsafe { gdk::ffi::gdk_unicode_to_keyval(ch as u32) };
        }
    }
    let Ok(name) = std::ffi::CString::new(name) else { return 0 };
    unsafe { gdk::ffi::gdk_keyval_from_name(name.as_ptr()) }
}

impl Drop for Offscreen {
    fn drop(&mut self) {
        self.alive.set(false);
        // SAFETY: all GTK access, including teardown, stays on the GPUI/GTK main thread.
        unsafe {
            self.window.destroy();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{PendingWebView, linux};
    use super::*;
    use gpui_kit::gpui::{Keystroke, point, px};
    use std::io::{Read, Write};

    fn wait(rx: &flume::Receiver<WebEvent>, accept: impl Fn(&WebEvent) -> bool) -> WebEvent {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
            while let Ok(event) = rx.try_recv() {
                assert!(!matches!(event, WebEvent::Failed), "WebKit failed to load the fixture");
                if accept(&event) {
                    return event;
                }
            }
            assert!(Instant::now() < deadline, "Timed out waiting for native Wayland webview");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn probe(panel: &super::super::PanelWebView, rx: &flume::Receiver<WebEvent>, expression: &str) -> serde_json::Value {
        panel.view.evaluate_script(&format!("window.parent.postMessage({{type:'probe',value:({expression})}})")).unwrap();
        match wait(rx, |e| matches!(e, WebEvent::Message(v) if v["type"] == "probe")) {
            WebEvent::Message(v) => v["value"].clone(),
            _ => unreachable!(),
        }
    }

    /// Run with a native Wayland compositor, DISPLAY unset, and one test thread:
    /// cargo test -p lookout-native native_wayland_webview -- --ignored --test-threads=1
    #[test]
    #[ignore = "requires a running native Wayland compositor"]
    fn native_wayland_webview() {
        assert!(std::env::var_os("WAYLAND_DISPLAY").is_some());
        assert!(std::env::var_os("DISPLAY").is_none(), "Test must not be able to use X11");
        gdk::set_allowed_backends("wayland");
        gtk::init().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let html = r#"<!doctype html><style>body{margin:0;background:rgb(255,0,0)}button,input{position:absolute;left:0;width:200px;height:30px}button{top:0}input{top:40px}</style>
              <button onclick="window.clicks=(window.clicks||0)+1;window.parent.postMessage({type:'clicked'})">Continue</button><input id="text" oninput="window.lastInput=this.value"><div style="height:1500px"></div>
              <script>document.addEventListener('wheel',e=>window.parent.postMessage({type:'wheel',dy:e.deltaY,height:document.documentElement.scrollHeight}),{passive:true});document.addEventListener('keydown',e=>{if(e.key==='Enter')window.parent.postMessage({type:'enter'})});window.parent.postMessage({type:'lookout:ready'});window.parent.postMessage({type:'lookout:resize',height:300});</script>"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                html.len(),
                html
            )
            .unwrap();
        });
        let pending = PendingWebView {
            origin: super::super::origin_of(&url).unwrap(),
            url,
            dark: false,
            background: (255, 255, 255),
            parent: linux::Parent::Wayland,
            scale: 1.0,
        };
        let (panel, rx) = pending.build().unwrap();
        assert!(panel.composited());
        let host = panel.offscreen.as_ref().unwrap();
        let web = panel.view.webview();
        web.set_size_request(400, 250);
        host.container.set_size_request(400, 250);
        host.window.resize(400, 250);
        host.size.set((400, 250, 1.0));
        wait(&rx, |e| matches!(e, WebEvent::Message(v) if v["type"] == "lookout:resize" && v["height"] == 300));
        wait(&rx, |e| matches!(e, WebEvent::Loaded));
        host.capture(&panel.view);
        let WebEvent::Frame(frame) = wait(&rx, |e| matches!(e, WebEvent::Frame(_))) else { unreachable!() };
        let pixels = frame.as_bytes(0).unwrap();
        assert!(pixels.chunks_exact(4).any(|p| p == [0, 0, 255, 255]), "Page background was not rendered");
        assert!(pixels.chunks_exact(4).any(|p| p != [0, 0, 255, 255]), "Page controls were not rendered");
        assert_eq!(probe(&panel, &rx, "innerWidth"), 400);
        assert_eq!(probe(&panel, &rx, "innerHeight"), 250);
        for down in [true, false] {
            host.pointer(
                &panel.view,
                point(px(20.0), px(15.0)),
                point(px(0.0), px(0.0)),
                Some(MouseButton::Left),
                Some(down),
                Modifiers::default(),
            );
        }
        wait(&rx, |e| matches!(e, WebEvent::Message(v) if v["type"] == "clicked"));
        assert_eq!(probe(&panel, &rx, "window.clicks"), 1);
        for down in [true, false] {
            host.pointer(
                &panel.view,
                point(px(20.0), px(55.0)),
                point(px(0.0), px(0.0)),
                Some(MouseButton::Left),
                Some(down),
                Modifiers::default(),
            );
        }
        wait(&rx, |e| matches!(e, WebEvent::Message(v) if v["type"] == "lookout:input-state" && v["text"].is_string()));
        panel.edit("replace", Some(0..0), "Native 🦀 Wayland", None);
        assert_eq!(probe(&panel, &rx, "document.querySelector('input').value"), "Native 🦀 Wayland");
        assert_eq!(probe(&panel, &rx, "window.lastInput"), "Native 🦀 Wayland");
        // IME replacement/selection uses UTF-16 DOM offsets, including astral characters.
        panel.edit("replace", Some(7..9), "日本語", Some(7..10));
        assert_eq!(probe(&panel, &rx, "document.querySelector('input').value"), "Native 日本語 Wayland");
        let key = Keystroke::parse("backspace").unwrap();
        host.key(&panel.view, &key, true);
        host.key(&panel.view, &key, false);
        wait(&rx, |e| matches!(e, WebEvent::Message(v) if v["type"] == "lookout:input-state" && v["text"] == "Native  Wayland"));
        assert_eq!(probe(&panel, &rx, "document.querySelector('input').value"), "Native  Wayland");
        let mut enter = Keystroke::parse("enter").unwrap();
        enter.key_char = Some("\r".to_string());
        host.key(&panel.view, &enter, true);
        host.key(&panel.view, &enter, false);
        wait(&rx, |e| matches!(e, WebEvent::Message(v) if v["type"] == "enter"));
        host.scroll(&panel.view, point(px(100.0), px(100.0)), point(px(0.0), px(0.0)), point(px(0.0), px(-120.0)), Modifiers::default());
        let wheel = wait(&rx, |e| matches!(e, WebEvent::Message(v) if v["type"] == "wheel"));
        if let WebEvent::Message(value) = wheel {
            assert!(value["dy"].as_f64().unwrap() > 0.0);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while probe(&panel, &rx, "scrollY").as_f64().unwrap_or(0.0) <= 0.0 {
            host.capture(&panel.view);
            assert!(Instant::now() < deadline, "WebKit did not scroll");
            std::thread::sleep(Duration::from_millis(10));
        }
        web.set_size_request(320, 200);
        host.container.set_size_request(320, 200);
        host.window.resize(320, 200);
        // A subsequent GTK allocation and WebKit layout must reach the new viewport.
        let deadline = Instant::now() + Duration::from_secs(5);
        while probe(&panel, &rx, "innerWidth") != 320 {
            assert!(Instant::now() < deadline, "WebKit did not resize");
            std::thread::sleep(Duration::from_millis(10));
        }
        host.last_frame.set(Instant::now() - Duration::from_secs(1));
        host.capture(&panel.view);
        wait(&rx, |e| matches!(e, WebEvent::Frame(_)));
        panel.view.evaluate_script("window.parent.postMessage({type:'lookout:done'})").unwrap();
        wait(&rx, |e| matches!(e, WebEvent::Message(v) if v["type"] == "lookout:done"));
    }
}
