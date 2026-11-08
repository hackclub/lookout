//! macOS: a guide that rides along with System Settings while the user turns
//! on a privacy permission (the idea is from Hex's `permission_guide.m`, MIT).
//!
//! "Grant Permission" opens the right Privacy & Security pane and docks a
//! small floating panel in the corner of the System Settings window: "Drag
//! Lookout into the list above", with a draggable Lookout tile (the `.app`
//! as a file URL, which the list accepts). The tile first flies there from
//! the click in Lookout's window so the eye can follow it across.
//!
//! The flight is one Core Animation layer in a click-through overlay window,
//! so it runs on the render server at the display's refresh rate. Hex moves
//! a whole window from a 120 Hz timer and eases in and out, which arrives at
//! speed and snaps; here the tile lifts with a spring, travels a paced arc on
//! a long ease-out, scales up and drops its shadow as it lands, and hands off
//! to the identical tile in the panel without a visible seam.
//!
//! The panel follows the Settings window (polled at 60 Hz while it moves,
//! 10 Hz otherwise), fades out when neither Settings nor Lookout is in front,
//! and closes once the permission is on or Settings closes. With Reduce
//! Motion on it just fades in.

use super::permissions::Kind;

/// Open the pane for `kind` with the guide. `false` when there's no `.app`
/// bundle to offer (a dev build): the caller opens the pane plainly.
pub fn show(kind: Kind) -> bool {
    imp::show(kind)
}

/// Close the guide (no-op when none is up).
pub fn close() {
    imp::close(true);
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;
    pub fn show(_kind: Kind) -> bool {
        false
    }
    pub fn close(_animated: bool) {}
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol, ProtocolObject};
    use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
    use objc2_app_kit::{
        NSApplication, NSBackingStoreType, NSBezierPath, NSBitmapImageRep, NSColor, NSCursor, NSDraggingContext,
        NSDraggingFormation, NSDraggingItem, NSDraggingSession, NSDraggingSource, NSDragOperation, NSEvent, NSFont,
        NSFontWeightMedium, NSFontWeightSemibold, NSImage, NSImageView, NSPanel, NSResponder, NSRunningApplication,
        NSScreen, NSTextField, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
        NSVisualEffectView, NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
    };
    use objc2_core_graphics::{CGMutablePath, CGWindowListCopyWindowInfo, CGWindowListOption, kCGNullWindowID};
    use objc2_foundation::{
        NSArray, NSBundle, NSDictionary, NSFileManager, NSNumber, NSObject, NSPoint, NSRect, NSRunLoop,
        NSObjectNSKeyValueCoding, NSRunLoopCommonModes, NSSize, NSString, NSTimer, NSURL, NSValue, ns_string,
    };
    use objc2_quartz_core::{
        CABasicAnimation, CACurrentMediaTime, CAKeyframeAnimation, CALayer, CAMediaTiming, CAMediaTimingFunction,
        CASpringAnimation, CATransaction, kCAAnimationPaced, kCAFillModeBoth,
    };

    const SETTINGS_ID: &str = "com.apple.systempreferences";

    // Panel layout (AppKit points, origin bottom-left).
    const PANEL_W: f64 = 380.0;
    const PANEL_H_DRAG: f64 = 116.0;
    const PANEL_H_SWITCH: f64 = 60.0;
    const PAD: f64 = 16.0;
    const TILE_H: f64 = 48.0;
    /// Inset from the Settings window's bottom-right corner.
    const DOCK_INSET: f64 = 16.0;

    // Flight.
    const LIFT_PX: f64 = 14.0;
    const LIFT_SCALE: f64 = 0.86;
    const START_SCALE: f64 = 0.72;
    const FLIGHT_SECS: f64 = 0.78;
    /// The panel starts fading in this far into the flight.
    const PANEL_IN_AT: f64 = 0.4;
    /// System Settings that hasn't shown a window by then gets no flight.
    const FLIGHT_PATIENCE: f64 = 4.0;
    /// ...and no guide at all after this.
    const SETTINGS_PATIENCE: f64 = 12.0;

    // ---------------------------------------------------------------- tile

    pub struct TileIvars {
        url: Retained<NSURL>,
        down: RefCell<Option<Retained<NSEvent>>>,
    }

    define_class!(
        // SAFETY: NSView subclass with no Drop; all state lives in ivars.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LookoutPermissionTile"]
        #[ivars = TileIvars]
        struct Tile;

        impl Tile {
            #[unsafe(method(acceptsFirstMouse:))]
            fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
                true
            }

            #[unsafe(method(resetCursorRects))]
            fn reset_cursor_rects(&self) {
                self.addCursorRect_cursor(self.bounds(), &NSCursor::openHandCursor());
            }

            #[unsafe(method(drawRect:))]
            fn draw_rect(&self, _dirty: NSRect) {
                let b = self.bounds();
                let r = NSRect::new(NSPoint::new(b.origin.x + 0.5, b.origin.y + 0.5), NSSize::new(b.size.width - 1.0, b.size.height - 1.0));
                let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, 10.0, 10.0);
                NSColor::controlBackgroundColor().setFill();
                path.fill();
                NSColor::separatorColor().setStroke();
                path.setLineWidth(1.0);
                path.stroke();
            }

            #[unsafe(method(mouseDown:))]
            fn mouse_down(&self, event: &NSEvent) {
                *self.ivars().down.borrow_mut() = Some(event.retain());
            }

            #[unsafe(method(mouseUp:))]
            fn mouse_up(&self, _event: &NSEvent) {
                self.ivars().down.borrow_mut().take();
            }

            #[unsafe(method(mouseDragged:))]
            fn mouse_dragged(&self, event: &NSEvent) {
                let Some(down) = self.ivars().down.borrow().clone() else { return };
                let (a, b) = (down.locationInWindow(), event.locationInWindow());
                if (b.x - a.x).hypot(b.y - a.y) < 3.0 {
                    return;
                }
                self.ivars().down.borrow_mut().take();
                let writer = ProtocolObject::from_ref(&*self.ivars().url);
                let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), writer);
                if let Some(image) = snapshot(self) {
                    // SAFETY: an NSImage is valid dragging contents.
                    unsafe { item.setDraggingFrame_contents(self.bounds(), Some(&image)) };
                }
                let items = NSArray::from_retained_slice(&[item]);
                let session = self.beginDraggingSessionWithItems_event_source(&items, &down, ProtocolObject::from_ref(self));
                session.setAnimatesToStartingPositionsOnCancelOrFail(true);
                session.setDraggingFormation(NSDraggingFormation::None);
            }
        }

        unsafe impl NSObjectProtocol for Tile {}

        unsafe impl NSDraggingSource for Tile {
            #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
            fn source_operation_mask(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
                NSDragOperation::Copy
            }
        }
    );

    impl Tile {
        fn new(frame: NSRect, url: Retained<NSURL>, icon: &NSImage, name: &NSString, mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(TileIvars { url, down: RefCell::new(None) });
            let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };

            let icon_view = NSImageView::imageViewWithImage(icon, mtm);
            icon_view.setFrame(NSRect::new(NSPoint::new(10.0, 8.0), NSSize::new(32.0, 32.0)));
            this.addSubview(&icon_view);

            let label = NSTextField::labelWithString(name, mtm);
            label.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, unsafe { NSFontWeightSemibold })));
            label.setTextColor(Some(&NSColor::labelColor()));
            label.setFrame(NSRect::new(NSPoint::new(52.0, 15.0), NSSize::new(frame.size.width - 96.0, 18.0)));
            this.addSubview(&label);

            // A grip on the right says "this can be dragged".
            if let Some(grip) = symbol(ns_string!("line.3.horizontal")) {
                let grip_view = NSImageView::imageViewWithImage(&grip, mtm);
                grip_view.setContentTintColor(Some(&NSColor::tertiaryLabelColor()));
                grip_view.setFrame(NSRect::new(NSPoint::new(frame.size.width - 34.0, 16.0), NSSize::new(18.0, 16.0)));
                this.addSubview(&grip_view);
            }
            this
        }
    }

    /// The view rendered to an image (the drag image and the flying tile).
    fn snapshot(view: &NSView) -> Option<Retained<NSImage>> {
        let bounds = view.bounds();
        let rep: Retained<NSBitmapImageRep> = view.bitmapImageRepForCachingDisplayInRect(bounds)?;
        view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
        let image = NSImage::initWithSize(NSImage::alloc(), bounds.size);
        image.addRepresentation(&rep);
        Some(image)
    }

    // ---------------------------------------------------------------- state

    struct Flight {
        overlay: Retained<NSPanel>,
        layer: Retained<CALayer>,
        /// The overlay's screen frame (the flight stays on one screen).
        screen: NSRect,
        shown_at: f64,
        launched: bool,
    }

    struct Guide {
        /// Tells a stale landing timer from this guide's own.
        id: u64,
        kind: Kind,
        panel: Retained<NSPanel>,
        tile: Option<Retained<Tile>>,
        timer: Retained<NSTimer>,
        opened_at: f64,
        saw_settings: bool,
        last_origin: Option<NSPoint>,
        last_move: f64,
        ticks: u64,
        /// Panel opacity target; `alpha` eases toward it each tick.
        target: f64,
        alpha: f64,
        /// No fading in before this time (the flight's hand-off).
        reveal_at: f64,
        closing: bool,
        flight: Option<Flight>,
    }

    thread_local! {
        static GUIDE: RefCell<Option<Guide>> = const { RefCell::new(None) };
        /// Bumped per pane request, so an older cold-launch retry stands down.
        static PANE_REQUEST: Cell<u64> = const { Cell::new(0) };
        static GUIDE_ID: Cell<u64> = const { Cell::new(0) };
    }

    fn now() -> f64 {
        CACurrentMediaTime()
    }

    // ---------------------------------------------------------------- show

    pub fn show(kind: Kind) -> bool {
        let Some(mtm) = MainThreadMarker::new() else { return false };
        let bundle = NSBundle::mainBundle().bundleURL();
        let is_app = bundle.pathExtension().is_some_and(|e| e.to_string().eq_ignore_ascii_case("app"));
        if !is_app {
            return false;
        }
        close(false);
        open_pane(kind, mtm);

        let click = NSEvent::mouseLocation();
        let click_in_app = NSApplication::sharedApplication(mtm)
            .windows()
            .iter()
            .any(|w| w.isVisible() && contains(w.frame(), click));
        let fly = click_in_app && !crate::motion::reduce_motion();

        // Camera's list only holds apps that asked, so it's switched on, not dragged in.
        let draggable = kind != Kind::Camera;
        let name = app_name(&bundle);
        let (panel, tile) = build_panel(kind, draggable, &bundle, &name, mtm);

        let timer = unsafe {
            NSTimer::timerWithTimeInterval_repeats_block(
                1.0 / 60.0,
                true,
                &RcBlock::new(|_t: std::ptr::NonNull<NSTimer>| tick()),
            )
        };
        unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };

        let t = now();
        let flight = match (&tile, fly) {
            (Some(tile), true) => {
                tile.setAlphaValue(0.0);
                snapshot(tile).and_then(|img| begin_flight(click, &img, tile.frame().size, mtm))
            }
            _ => None,
        };
        GUIDE.with_borrow_mut(|g| {
            *g = Some(Guide {
                id: GUIDE_ID.with(|i| {
                    i.set(i.get() + 1);
                    i.get()
                }),
                kind,
                panel,
                tile,
                timer,
                opened_at: t,
                saw_settings: false,
                last_origin: None,
                last_move: t,
                ticks: 0,
                target: 0.0,
                alpha: 0.0,
                reveal_at: if flight.is_some() { f64::INFINITY } else { 0.0 },
                closing: false,
                flight,
            })
        });
        tick();
        true
    }

    fn build_panel(
        kind: Kind,
        draggable: bool,
        bundle: &NSURL,
        name: &NSString,
        mtm: MainThreadMarker,
    ) -> (Retained<NSPanel>, Option<Retained<Tile>>) {
        let h = if draggable { PANEL_H_DRAG } else { PANEL_H_SWITCH };
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PANEL_W, h));
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            frame,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel | NSWindowStyleMask::FullSizeContentView,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setFloatingPanel(true);
        panel.setLevel(3); // NSFloatingWindowLevel
        panel.setCollectionBehavior(NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary);
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(true);
        panel.setHidesOnDeactivate(false);
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setAlphaValue(0.0);

        let material = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
        material.setMaterial(NSVisualEffectMaterial::Popover);
        material.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        material.setState(NSVisualEffectState::Active);
        material.setWantsLayer(true);
        if let Some(layer) = material.layer() {
            layer.setCornerRadius(18.0);
            layer.setMasksToBounds(true);
            layer.setCornerCurve(unsafe { objc2_quartz_core::kCACornerCurveContinuous });
        }

        let list = list_name(kind);
        let text = if draggable {
            format!("Drag {name} into the {list} list above")
        } else {
            format!("Switch on {name} in the {list} list above")
        };
        let text_y = if draggable { h - PAD - 22.0 } else { (h - 20.0) / 2.0 };
        if let Some(arrow) = symbol(ns_string!("arrow.up")) {
            let v = NSImageView::imageViewWithImage(&arrow, mtm);
            v.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
            v.setFrame(NSRect::new(NSPoint::new(PAD, text_y + 2.0), NSSize::new(14.0, 16.0)));
            material.addSubview(&v);
        }
        let label = NSTextField::labelWithString(&NSString::from_str(&text), mtm);
        label.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, unsafe { NSFontWeightMedium })));
        label.setTextColor(Some(&NSColor::labelColor()));
        label.setFrame(NSRect::new(NSPoint::new(PAD + 22.0, text_y), NSSize::new(PANEL_W - 2.0 * PAD - 22.0, 20.0)));
        material.addSubview(&label);

        let tile = draggable.then(|| {
            let path = bundle.path().unwrap_or_else(|| NSString::from_str(""));
            let icon = NSWorkspace::sharedWorkspace().iconForFile(&path);
            icon.setSize(NSSize::new(32.0, 32.0));
            let tile_frame = NSRect::new(NSPoint::new(PAD, PAD), NSSize::new(PANEL_W - 2.0 * PAD, TILE_H));
            let tile = Tile::new(tile_frame, bundle.retain(), &icon, name, mtm);
            material.addSubview(&tile);
            tile
        });
        panel.setContentView(Some(&material));
        (panel, tile)
    }

    // ---------------------------------------------------------------- pane

    fn pane_url(kind: Kind) -> &'static str {
        match kind {
            Kind::Screen => "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ScreenCapture",
            Kind::Camera => "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Camera",
        }
    }

    /// The list's name as Privacy & Security shows it.
    fn list_name(kind: Kind) -> &'static str {
        match kind {
            Kind::Screen => "Screen & System Audio Recording",
            Kind::Camera => "Camera",
        }
    }

    /// Open the pane. A System Settings that wasn't running restores the
    /// pane it showed last and drops the URL's, so after a cold launch the
    /// URL goes again once its window is up and has settled.
    fn open_pane(kind: Kind, _mtm: MainThreadMarker) {
        let Some(url) = NSURL::URLWithString(&NSString::from_str(pane_url(kind))) else { return };
        let cold = settings_pid().is_none();
        let request = PANE_REQUEST.with(|r| {
            r.set(r.get() + 1);
            r.get()
        });
        NSWorkspace::sharedWorkspace().openURL(&url);
        if !cold {
            return;
        }
        let polls = Cell::new(0u32);
        let block = RcBlock::new(move |t: std::ptr::NonNull<NSTimer>| {
            polls.set(polls.get() + 1);
            let stale = PANE_REQUEST.with(|r| r.get()) != request;
            let up = !stale && settings_frame().is_some();
            if stale || up || polls.get() > 100 {
                unsafe { t.as_ref().invalidate() };
            }
            if up {
                for delay in [0.35, 1.2] {
                    let url = url.clone();
                    let again = RcBlock::new(move |_t: std::ptr::NonNull<NSTimer>| {
                        if PANE_REQUEST.with(|r| r.get()) == request {
                            NSWorkspace::sharedWorkspace().openURL(&url);
                        }
                    });
                    unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(delay, false, &again) };
                }
            }
        });
        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.1, true, &block) };
    }

    // ---------------------------------------------------------------- tick

    fn tick() {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let mut done = false;
        GUIDE.with_borrow_mut(|slot| {
            let Some(g) = slot.as_mut() else { return };
            done = step(g, mtm);
        });
        if done {
            if let Some(g) = GUIDE.with_borrow_mut(Option::take) {
                teardown(g);
            }
        }
    }

    /// One frame. `true` when the guide is finished.
    fn step(g: &mut Guide, mtm: MainThreadMarker) -> bool {
        let t = now();
        g.ticks += 1;
        if !g.closing && super::super::permissions::check(g.kind) {
            log::info!("[permission-guide] {:?} granted", g.kind);
            g.closing = true;
        }
        let animating = (g.alpha - g.target).abs() > 0.004 || g.flight.is_some() || t - g.last_move < 0.5;
        // Idle: ~10 Hz is plenty to notice the Settings window starting to move.
        if !animating && !g.closing && g.ticks % 6 != 0 {
            return false;
        }

        if !g.closing {
            match settings_frame() {
                Some(settings) => {
                    g.saw_settings = true;
                    let size = g.panel.frame().size;
                    let origin = dock_origin(settings, size, mtm);
                    if g.last_origin.is_none_or(|o| o.x != origin.x || o.y != origin.y) {
                        g.panel.setFrameOrigin(origin);
                        if g.last_origin.is_none() {
                            g.panel.orderFrontRegardless();
                        }
                        g.last_origin = Some(origin);
                        g.last_move = t;
                    }
                    let target = tile_center(g);
                    if let Some(f) = g.flight.as_mut().filter(|f| !f.launched) {
                        if launch_flight(f, target) {
                            g.reveal_at = t + FLIGHT_SECS * PANEL_IN_AT;
                            schedule_landing(g.id, FLIGHT_SECS);
                        } else {
                            // Different screen: no flight, just the panel.
                            g.abandon_flight();
                        }
                    }
                    let front = frontmost_is_settings() || NSApplication::sharedApplication(mtm).isActive();
                    g.target = if front && t >= g.reveal_at { 1.0 } else { 0.0 };
                }
                None => {
                    if g.saw_settings || t - g.opened_at > SETTINGS_PATIENCE {
                        g.closing = true;
                    }
                    if g.flight.as_ref().is_some_and(|f| !f.launched && t - f.shown_at > FLIGHT_PATIENCE) {
                        g.abandon_flight();
                    }
                }
            }
        }
        if g.closing {
            g.target = 0.0;
            g.abandon_flight();
        }

        // Ease the panel's opacity (~150 ms either way at 60 Hz).
        let k = if g.target > g.alpha { 0.2 } else { 0.25 };
        g.alpha += (g.target - g.alpha) * k;
        if (g.alpha - g.target).abs() < 0.004 {
            g.alpha = g.target;
        }
        g.panel.setAlphaValue(g.alpha);
        g.closing && g.alpha == 0.0
    }

    fn teardown(g: Guide) {
        g.timer.invalidate();
        if let Some(f) = g.flight {
            f.overlay.orderOut(None);
        }
        g.panel.orderOut(None);
    }

    pub fn close(animated: bool) {
        if animated {
            GUIDE.with_borrow_mut(|g| {
                if let Some(g) = g.as_mut() {
                    g.closing = true;
                }
            });
        } else if let Some(g) = GUIDE.with_borrow_mut(Option::take) {
            teardown(g);
        }
    }

    /// Bottom-right inside the Settings window, kept on its screen.
    fn dock_origin(settings: NSRect, size: NSSize, mtm: MainThreadMarker) -> NSPoint {
        let screens = NSScreen::screens(mtm);
        let visible = screens
            .iter()
            .find(|s| intersects(s.frame(), settings))
            .or_else(|| NSScreen::mainScreen(mtm))
            .map(|s| s.visibleFrame())
            .unwrap_or(settings);
        let x = settings.origin.x + settings.size.width - size.width - DOCK_INSET;
        let y = settings.origin.y + DOCK_INSET;
        let x = x.min(visible.origin.x + visible.size.width - size.width - 8.0).max(visible.origin.x + 8.0);
        let y = y.min(visible.origin.y + visible.size.height - size.height - 8.0).max(visible.origin.y + 8.0);
        NSPoint::new(x.round(), y.round())
    }

    /// Where the tile sits, in screen coordinates (centre).
    fn tile_center(g: &Guide) -> NSPoint {
        let origin = g.last_origin.unwrap_or(NSPoint::new(0.0, 0.0));
        let tf = g.tile.as_ref().map(|t| t.frame()).unwrap_or_default();
        NSPoint::new(origin.x + tf.origin.x + tf.size.width / 2.0, origin.y + tf.origin.y + tf.size.height / 2.0)
    }

    // ---------------------------------------------------------------- flight

    /// Lift a copy of the tile out of Lookout's window at `click`.
    fn begin_flight(click: NSPoint, image: &NSImage, size: NSSize, mtm: MainThreadMarker) -> Option<Flight> {
        let screens = NSScreen::screens(mtm);
        let screen = screens.iter().find(|s| contains(s.frame(), click))?;
        let sf = screen.frame();
        let overlay = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            sf,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        overlay.setLevel(101); // NSPopUpMenuWindowLevel: above the guide panel
        overlay.setCollectionBehavior(NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary);
        overlay.setOpaque(false);
        overlay.setBackgroundColor(Some(&NSColor::clearColor()));
        overlay.setHasShadow(false);
        overlay.setIgnoresMouseEvents(true);
        overlay.setHidesOnDeactivate(false);
        unsafe { overlay.setReleasedWhenClosed(false) };
        overlay.setFrame_display(sf, false);

        let host = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), sf.size));
        host.setWantsLayer(true);
        host.setLayerUsesCoreImageFilters(true);
        let root = host.layer()?;

        let layer = CALayer::new();
        layer.setBounds(NSRect::new(NSPoint::new(0.0, 0.0), size));
        unsafe { layer.setContents(Some(image)) };
        layer.setContentsScale(screen.backingScaleFactor());
        layer.setShadowOpacity(0.22);
        layer.setShadowRadius(16.0);
        layer.setShadowOffset(NSSize::new(0.0, -8.0));
        if let Some(blur) = blur_filter() {
            unsafe { layer.setFilters(Some(&NSArray::from_retained_slice(&[blur]))) };
        }
        let start = NSPoint::new(click.x - sf.origin.x, click.y - sf.origin.y);
        let lifted = NSPoint::new(start.x, start.y + LIFT_PX);
        without_actions(|| {
            layer.setPosition(lifted);
            set_scale(&layer, LIFT_SCALE);
            layer.setOpacity(1.0);
        });
        root.addSublayer(&layer);
        overlay.setContentView(Some(&host));
        overlay.orderFrontRegardless();

        // The lift: a soft spring up and out of the window, fading in.
        let spring = |key: &NSString, from: Retained<AnyObject>, to: Retained<AnyObject>| {
            // Just under critical damping (2·√stiffness ≈ 32.2): no bounce,
            // a soft settle. (`perceptualDuration:bounce:` is macOS 14+.)
            let a = CASpringAnimation::animationWithKeyPath(Some(key));
            a.setMass(1.0);
            a.setStiffness(260.0);
            a.setDamping(31.0);
            unsafe {
                a.setFromValue(Some(&from));
                a.setToValue(Some(&to));
            }
            a.setDuration(a.settlingDuration());
            a.setFillMode(unsafe { kCAFillModeBoth });
            a
        };
        layer.addAnimation_forKey(&spring(ns_string!("position"), point_value(start), point_value(lifted)), Some(ns_string!("lift")));
        layer.addAnimation_forKey(
            &spring(ns_string!("transform.scale"), number(START_SCALE), number(LIFT_SCALE)),
            Some(ns_string!("lift-scale")),
        );
        let fade = CABasicAnimation::animationWithKeyPath(Some(ns_string!("opacity")));
        unsafe {
            fade.setFromValue(Some(&number(0.0)));
            fade.setToValue(Some(&number(1.0)));
        }
        fade.setDuration(0.16);
        fade.setTimingFunction(Some(&CAMediaTimingFunction::functionWithControlPoints(0.0, 0.0, 0.2, 1.0)));
        layer.addAnimation_forKey(&fade, Some(ns_string!("lift-fade")));

        Some(Flight { overlay, layer, screen: sf, shown_at: now(), launched: false })
    }

    /// Fly from wherever the lift is now to `target` (screen coordinates).
    /// `false` (nothing started) when the target is on another screen.
    fn launch_flight(f: &mut Flight, target: NSPoint) -> bool {
        if !contains(f.screen, target) {
            return false;
        }
        f.launched = true;
        let layer = &f.layer;
        let presented = unsafe { layer.presentationLayer() };
        let from = presented.as_ref().map(|p| p.position()).unwrap_or_else(|| layer.position());
        let from_scale = presented.as_ref().map(|p| current_scale(p)).unwrap_or(LIFT_SCALE);
        let from_opacity = presented.as_ref().map(|p| p.opacity()).unwrap_or(1.0);
        layer.removeAllAnimations();

        let end = NSPoint::new((target.x - f.screen.origin.x).round(), (target.y - f.screen.origin.y).round());
        let dist = (end.x - from.x).hypot(end.y - from.y);
        let control = NSPoint::new((from.x + end.x) / 2.0, from.y.max(end.y) + (dist * 0.22).min(140.0));
        let path = CGMutablePath::new();
        unsafe {
            CGMutablePath::move_to_point(Some(&path), std::ptr::null(), from.x, from.y);
            CGMutablePath::add_quad_curve_to_point(Some(&path), std::ptr::null(), control.x, control.y, end.x, end.y);
        }

        // Starts briskly (it's already moving out of the lift) and spends
        // the back half settling, so it glides into place.
        let timing = CAMediaTimingFunction::functionWithControlPoints(0.3, 0.1, 0.0, 1.0);

        without_actions(|| {
            layer.setPosition(end);
            set_scale(layer, 1.0);
            layer.setOpacity(1.0);
            layer.setShadowOpacity(0.0);
        });

        let travel = CAKeyframeAnimation::animationWithKeyPath(Some(ns_string!("position")));
        travel.setPath(Some(&path));
        travel.setCalculationMode(unsafe { kCAAnimationPaced });
        travel.setDuration(FLIGHT_SECS);
        travel.setTimingFunction(Some(&timing));
        layer.addAnimation_forKey(&travel, Some(ns_string!("travel")));

        let basic = |key: &NSString, from: f64, to: f64| {
            let a = CABasicAnimation::animationWithKeyPath(Some(key));
            unsafe {
                a.setFromValue(Some(&number(from)));
                a.setToValue(Some(&number(to)));
            }
            a.setDuration(FLIGHT_SECS);
            a.setTimingFunction(Some(&timing));
            a
        };
        layer.addAnimation_forKey(&basic(ns_string!("transform.scale"), from_scale, 1.0), Some(ns_string!("grow")));
        layer.addAnimation_forKey(&basic(ns_string!("shadowOpacity"), 0.22, 0.0), Some(ns_string!("settle")));
        if (from_opacity - 1.0).abs() > 0.01 {
            layer.addAnimation_forKey(&basic(ns_string!("opacity"), from_opacity as f64, 1.0), Some(ns_string!("fade")));
        }
        if blur_filter_class().is_some() {
            // A touch of motion blur through the fast part only.
            let blur = CAKeyframeAnimation::animationWithKeyPath(Some(ns_string!("filters.blur.inputRadius")));
            unsafe {
                blur.setValues(Some(&NSArray::from_retained_slice(&[number(0.0), number(2.5), number(0.0)])));
                blur.setKeyTimes(Some(&NSArray::from_retained_slice(&[
                    NSNumber::new_f64(0.0),
                    NSNumber::new_f64(0.3),
                    NSNumber::new_f64(0.7),
                ])));
            }
            blur.setDuration(FLIGHT_SECS);
            layer.addAnimation_forKey(&blur, Some(ns_string!("blur")));
        }
        true
    }

    /// Show the real tile once the flight has landed, then drop the overlay
    /// a beat later (an overlap of identical pixels hides any frame skew).
    fn schedule_landing(id: u64, after: f64) {
        let block = RcBlock::new(move |_t: std::ptr::NonNull<NSTimer>| land(id));
        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(after, false, &block) };
    }

    fn land(id: u64) {
        GUIDE.with_borrow_mut(|g| {
            if let Some(g) = g.as_mut().filter(|g| g.id == id) {
                g.land();
            }
        });
    }

    impl Guide {
        /// The flight arrived: the panel's own tile takes over.
        fn land(&mut self) {
            if let Some(tile) = &self.tile {
                tile.setAlphaValue(1.0);
            }
            self.reveal_at = self.reveal_at.min(now());
            if let Some(f) = self.flight.take() {
                let overlay = f.overlay;
                let block = RcBlock::new(move |_t: std::ptr::NonNull<NSTimer>| overlay.orderOut(None));
                unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.06, false, &block) };
            }
        }

        /// Give up on the flight: fade the flying tile out and show the
        /// panel's own (no-op without one).
        fn abandon_flight(&mut self) {
            let Some(f) = self.flight.take() else { return };
            let fade = CABasicAnimation::animationWithKeyPath(Some(ns_string!("opacity")));
            unsafe {
                fade.setFromValue(Some(&number(1.0)));
                fade.setToValue(Some(&number(0.0)));
            }
            fade.setDuration(0.15);
            without_actions(|| f.layer.setOpacity(0.0));
            f.layer.addAnimation_forKey(&fade, Some(ns_string!("drop")));
            let overlay = f.overlay;
            let block = RcBlock::new(move |_t: std::ptr::NonNull<NSTimer>| overlay.orderOut(None));
            unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.2, false, &block) };
            if let Some(tile) = &self.tile {
                tile.setAlphaValue(1.0);
            }
            self.reveal_at = 0.0;
        }
    }

    // ---------------------------------------------------------------- helpers

    fn without_actions(f: impl FnOnce()) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        f();
        CATransaction::commit();
    }

    fn set_scale(layer: &CALayer, s: f64) {
        unsafe { layer.setValue_forKeyPath(Some(&number(s)), ns_string!("transform.scale")) };
    }

    fn current_scale(layer: &CALayer) -> f64 {
        let v = layer.valueForKeyPath(ns_string!("transform.scale"));
        v.and_then(|v| v.downcast::<NSNumber>().ok()).map(|n| n.doubleValue()).unwrap_or(1.0)
    }

    fn number(v: f64) -> Retained<AnyObject> {
        Retained::into_super(Retained::into_super(NSNumber::new_f64(v))).into()
    }

    fn point_value(p: NSPoint) -> Retained<AnyObject> {
        Retained::into_super(unsafe { NSValue::valueWithPoint(p) }).into()
    }

    fn blur_filter_class() -> Option<&'static AnyClass> {
        AnyClass::get(c"CIFilter")
    }

    /// A zero-radius Gaussian blur named `blur` (animated during the flight).
    fn blur_filter() -> Option<Retained<AnyObject>> {
        let cls = blur_filter_class()?;
        unsafe {
            let f: Option<Retained<AnyObject>> = msg_send![cls, filterWithName: ns_string!("CIGaussianBlur")];
            let f = f?;
            let _: () = msg_send![&*f, setName: ns_string!("blur")];
            let _: () = msg_send![&*f, setValue: &*number(0.0), forKey: ns_string!("inputRadius")];
            Some(f)
        }
    }

    /// An SF Symbol (macOS 11+; the app still runs on 10.15).
    fn symbol(name: &NSString) -> Option<Retained<NSImage>> {
        let v = objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion();
        if v.majorVersion < 11 {
            return None;
        }
        NSImage::imageWithSystemSymbolName_accessibilityDescription(name, None)
    }

    fn contains(r: NSRect, p: NSPoint) -> bool {
        p.x >= r.origin.x && p.x <= r.origin.x + r.size.width && p.y >= r.origin.y && p.y <= r.origin.y + r.size.height
    }

    fn intersects(a: NSRect, b: NSRect) -> bool {
        a.origin.x < b.origin.x + b.size.width
            && b.origin.x < a.origin.x + a.size.width
            && a.origin.y < b.origin.y + b.size.height
            && b.origin.y < a.origin.y + a.size.height
    }

    fn app_name(bundle: &NSURL) -> Retained<NSString> {
        let path = bundle.path().unwrap_or_else(|| NSString::from_str("Lookout"));
        let name = NSFileManager::defaultManager().displayNameAtPath(&path).to_string();
        let name = name.strip_suffix(".app").unwrap_or(&name);
        NSString::from_str(if name.is_empty() { "Lookout" } else { name })
    }

    fn settings_pid() -> Option<i32> {
        let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(SETTINGS_ID));
        apps.firstObject().map(|a| a.processIdentifier())
    }

    fn frontmost_is_settings() -> bool {
        NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .and_then(|a| a.bundleIdentifier())
            .is_some_and(|id| id.to_string() == SETTINGS_ID)
    }

    /// System Settings' main window in AppKit screen coordinates.
    fn settings_frame() -> Option<NSRect> {
        let pid = settings_pid()?;
        let list = CGWindowListCopyWindowInfo(
            CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
            kCGNullWindowID,
        )?;
        // SAFETY: CFArray of CFDictionary is toll-free bridged to NSArray of NSDictionary.
        let list: &NSArray<NSDictionary<NSString, AnyObject>> =
            unsafe { &*(&*list as *const _ as *const NSArray<NSDictionary<NSString, AnyObject>>) };
        let num = |d: &NSDictionary<NSString, AnyObject>, k: &NSString| {
            d.objectForKey(k).and_then(|v| v.downcast::<NSNumber>().ok()).map(|n| n.doubleValue())
        };
        let mut best: Option<NSRect> = None;
        for w in list.iter() {
            if num(&w, ns_string!("kCGWindowOwnerPID")) != Some(pid as f64) || num(&w, ns_string!("kCGWindowLayer")) != Some(0.0) {
                continue;
            }
            let Some(b) = w.objectForKey(ns_string!("kCGWindowBounds")).and_then(|v| v.downcast::<NSDictionary>().ok()) else {
                continue;
            };
            // SAFETY: the bounds dictionary is keyed by strings.
            let b: &NSDictionary<NSString, AnyObject> = unsafe { &*(&*b as *const NSDictionary as *const NSDictionary<NSString, AnyObject>) };
            let (Some(x), Some(y), Some(width), Some(height)) =
                (num(b, ns_string!("X")), num(b, ns_string!("Y")), num(b, ns_string!("Width")), num(b, ns_string!("Height")))
            else {
                continue;
            };
            if width < 300.0 || height < 200.0 {
                continue;
            }
            if best.is_none_or(|r| width * height > r.size.width * r.size.height) {
                best = Some(NSRect::new(NSPoint::new(x, y), NSSize::new(width, height)));
            }
        }
        // Quartz's origin is the primary screen's top-left; AppKit's its bottom-left.
        let mtm = MainThreadMarker::new()?;
        let primary_h = NSScreen::screens(mtm).firstObject().map(|s| s.frame().size.height)?;
        best.map(|r| NSRect::new(NSPoint::new(r.origin.x, primary_h - r.origin.y - r.size.height), r.size))
    }
}
