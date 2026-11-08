//! macOS: a guide that rides along with System Settings while the user turns
//! on a privacy permission (after Codex Computer Use's onboarding; Hex's
//! `permission_guide.m` and riko2chen/AskForPermission, both MIT, showed how).
//!
//! "Allow" on a row of the setup page opens the right Privacy & Security
//! pane. Once the System Settings window has settled, the row itself lifts
//! out of Lookout's window and flies over to it, growing into a card that
//! docks in Settings' bottom-right corner: "Drag Lookout into the list
//! above", with a draggable Lookout tile (the `.app` as a file URL, which the
//! list accepts). Lookout comes to the front for the flight so its start can
//! be seen, and System Settings goes back to the front as the card lands.
//!
//! The flight is one Core Animation layer tree in a click-through overlay
//! window, so it runs on the render server at the display's refresh rate:
//! the row's pixels (captured at the click) cross-fade into the card's
//! around the apex of a quadratic arc while the size grows from row to
//! card, under a single ease that starts gently and spends its back half
//! slowing down, so the card glides into place instead of arriving at speed.
//!
//! The card follows the Settings window (polled at 60 Hz while it moves,
//! 10 Hz otherwise), fades out when neither Settings nor Lookout is in front,
//! and closes once the permission is on or Settings closes. With Reduce
//! Motion on, or without a row to start from, it just fades in.

use super::permissions::Kind;

/// The clicked row, in the key window's content coordinates (logical
/// pixels, origin top-left): where the flight starts.
#[derive(Clone, Copy, Debug)]
pub struct Source {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// What the guide is doing for a permission, for the row that asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// Waiting for System Settings to show its window.
    Opening,
    /// The card has left the row (flying or docked beside Settings).
    InSettings,
}

/// Open the pane for `kind` with the guide. `false` when there's no `.app`
/// bundle to offer (a dev build): the caller opens the pane plainly.
pub fn show(kind: Kind, source: Option<Source>) -> bool {
    imp::show(kind, source)
}

/// Close the guide (no-op when none is up).
pub fn close() {
    imp::close(true);
}

/// The guide's phase for `kind` (`Idle` when it's up for another one).
pub fn phase(kind: Kind) -> Phase {
    imp::phase(kind)
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;
    pub fn show(_kind: Kind, _source: Option<Source>) -> bool {
        false
    }
    pub fn close(_animated: bool) {}
    pub fn phase(_kind: Kind) -> Phase {
        Phase::Idle
    }
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
        NSApplication, NSApplicationActivationOptions, NSBackingStoreType, NSBezierPath, NSBitmapImageRep, NSColor,
        NSCursor, NSDraggingContext, NSDraggingFormation, NSDraggingItem, NSDraggingSession, NSDraggingSource,
        NSDragOperation, NSEvent, NSFont, NSFontWeightMedium, NSFontWeightSemibold, NSImage, NSImageView, NSPanel,
        NSResponder, NSRunningApplication, NSScreen, NSTextField, NSView, NSWindow, NSWindowCollectionBehavior,
        NSWindowStyleMask, NSWorkspace,
    };
    use objc2_core_graphics::{
        CGMutablePath, CGWindowImageOption, CGWindowListCopyWindowInfo, CGWindowListOption, kCGNullWindowID,
    };
    use objc2_foundation::{
        NSArray, NSBundle, NSDictionary, NSFileManager, NSNumber, NSObject, NSPoint, NSRect,
        NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSTimer, NSURL, NSValue, ns_string,
    };
    use objc2_quartz_core::{
        CAKeyframeAnimation, CALayer, CAMediaTiming, CAMediaTimingFunction, CABasicAnimation, CACurrentMediaTime,
        CATransaction, kCAAnimationPaced, kCAFillModeBoth, kCAGravityResize,
    };

    const SETTINGS_ID: &str = "com.apple.systempreferences";

    // Card layout (AppKit points, origin bottom-left).
    const PANEL_W: f64 = 380.0;
    const PANEL_H_DRAG: f64 = 116.0;
    const PANEL_H_SWITCH: f64 = 60.0;
    const PAD: f64 = 16.0;
    const TILE_H: f64 = 48.0;
    const CARD_RADIUS: f64 = 18.0;
    /// The setup page's rows (`screens/permission.rs`).
    const ROW_RADIUS: f64 = 14.0;
    /// Inset from the Settings window's bottom-right corner.
    const DOCK_INSET: f64 = 16.0;

    // Flight.
    const FLIGHT_SECS: f64 = 0.74;
    /// Peak of the arc above the higher end (points), at most.
    const ARC_MAX: f64 = 150.0;
    /// Motion blur at the fastest point (points).
    const BLUR_PEAK: f64 = 5.0;
    /// A warm Settings window counts as settled once its frame has held
    /// still this long; a cold launch also re-sends the pane URL (see
    /// `open_pane`), so that waits until after the last re-send.
    const SETTLE_WARM: f64 = 0.2;
    const SETTLE_COLD: f64 = 1.45;
    /// Settings that hasn't settled by then gets the card without a flight.
    const FLIGHT_PATIENCE: f64 = 6.0;
    /// ...and no guide at all after this.
    const SETTINGS_PATIENCE: f64 = 12.0;

    // ---------------------------------------------------------------- card

    define_class!(
        // SAFETY: NSView subclass with no Drop and no state.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LookoutPermissionCard"]
        struct Card;

        impl Card {
            #[unsafe(method(drawRect:))]
            fn draw_rect(&self, _dirty: NSRect) {
                let b = self.bounds();
                let r = NSRect::new(NSPoint::new(b.origin.x + 0.5, b.origin.y + 0.5), NSSize::new(b.size.width - 1.0, b.size.height - 1.0));
                let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, CARD_RADIUS - 0.5, CARD_RADIUS - 0.5);
                NSColor::windowBackgroundColor().setFill();
                path.fill();
                NSColor::separatorColor().setStroke();
                path.setLineWidth(1.0);
                path.stroke();
            }
        }

        unsafe impl NSObjectProtocol for Card {}
    );

    impl Card {
        fn new(frame: NSRect, mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(());
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }
    }

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

    /// The clicked row, captured at the click.
    struct Shot {
        /// Screen coordinates.
        rect: NSRect,
        /// Its pixels; `None` falls back to the card's own image.
        image: Option<Retained<NSImage>>,
        window: Retained<NSWindow>,
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Stage {
        /// Waiting for the Settings window to settle.
        Waiting { first_seen: Option<f64>, last: Option<(f64, f64, f64, f64)>, still_since: f64 },
        Flying,
        Docked,
    }

    struct Guide {
        /// Tells a stale landing timer from this guide's own.
        id: u64,
        kind: Kind,
        panel: Retained<NSPanel>,
        card: Retained<Card>,
        /// System Settings wasn't running at the click.
        cold: bool,
        source: Option<Shot>,
        stage: Stage,
        overlay: Option<Retained<NSPanel>>,
        timer: Retained<NSTimer>,
        opened_at: f64,
        saw_settings: bool,
        last_origin: Option<NSPoint>,
        last_move: f64,
        ticks: u64,
        /// Card opacity target; `alpha` eases toward it each tick (docked).
        target: f64,
        alpha: f64,
        closing: bool,
    }

    thread_local! {
        static GUIDE: RefCell<Option<Guide>> = const { RefCell::new(None) };
        /// Mirrors the guide's kind and phase for `phase()`, which the UI
        /// polls; kept apart from `GUIDE` so a query never meets a borrow.
        static PHASE: Cell<(Option<Kind>, Phase)> = const { Cell::new((None, Phase::Idle)) };
        /// Bumped per pane request, so an older cold-launch retry stands down.
        static PANE_REQUEST: Cell<u64> = const { Cell::new(0) };
        static GUIDE_ID: Cell<u64> = const { Cell::new(0) };
    }

    fn now() -> f64 {
        CACurrentMediaTime()
    }

    fn set_phase(kind: Option<Kind>, phase: Phase) {
        PHASE.with(|p| p.set((kind, phase)));
    }

    pub fn phase(kind: Kind) -> Phase {
        match PHASE.with(Cell::get) {
            (Some(k), p) if k == kind => p,
            _ => Phase::Idle,
        }
    }

    /// Run `f` on the main run loop after `secs` (outside any borrow).
    fn later(secs: f64, f: impl FnOnce() + 'static) {
        let f = Cell::new(Some(f));
        let block = RcBlock::new(move |_t: std::ptr::NonNull<NSTimer>| {
            if let Some(f) = f.take() {
                f()
            }
        });
        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(secs, false, &block) };
    }

    /// The flight's time scale (`LOOKOUT_SLOWMO`).
    fn secs(s: f64) -> f64 {
        s * crate::motion::slowmo() as f64
    }

    // ---------------------------------------------------------------- show

    pub fn show(kind: Kind, source: Option<Source>) -> bool {
        let Some(mtm) = MainThreadMarker::new() else { return false };
        let bundle = NSBundle::mainBundle().bundleURL();
        let is_app = bundle.pathExtension().is_some_and(|e| e.to_string().eq_ignore_ascii_case("app"));
        if !is_app {
            log::info!("[permission-guide] not running from a .app; opening the pane plainly");
            return false;
        }
        close(false);
        // Before Settings comes up over the row.
        let shot = if crate::motion::reduce_motion() { None } else { source.and_then(|s| capture(s, mtm)) };
        log::info!(
            "[permission-guide] {kind:?}: row {}, reduce motion {}",
            match &shot {
                Some(s) if s.image.is_some() => "captured",
                Some(_) => "found (no pixels)",
                None => "not found",
            },
            crate::motion::reduce_motion()
        );
        let cold = settings_pid().is_none();
        open_pane(kind);

        // Camera's list only holds apps that asked, so it's switched on, not dragged in.
        let draggable = kind != Kind::Camera;
        let name = app_name(&bundle);
        let (panel, card) = build_panel(kind, draggable, &bundle, &name, mtm);

        let timer = unsafe {
            NSTimer::timerWithTimeInterval_repeats_block(
                1.0 / 60.0,
                true,
                &RcBlock::new(|_t: std::ptr::NonNull<NSTimer>| tick()),
            )
        };
        unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };

        let t = now();
        GUIDE.with_borrow_mut(|g| {
            *g = Some(Guide {
                id: GUIDE_ID.with(|i| {
                    i.set(i.get() + 1);
                    i.get()
                }),
                kind,
                panel,
                card,
                cold,
                source: shot,
                stage: Stage::Waiting { first_seen: None, last: None, still_since: t },
                overlay: None,
                timer,
                opened_at: t,
                saw_settings: false,
                last_origin: None,
                last_move: t,
                ticks: 0,
                target: 0.0,
                alpha: 0.0,
                closing: false,
            })
        });
        set_phase(Some(kind), Phase::Opening);
        tick();
        true
    }

    /// Find the row on screen and capture its pixels.
    fn capture(s: Source, mtm: MainThreadMarker) -> Option<Shot> {
        let app = NSApplication::sharedApplication(mtm);
        let window = app.keyWindow().or_else(|| app.mainWindow())?;
        let content = window.contentView()?.frame();
        let in_window = NSRect::new(
            NSPoint::new(content.origin.x + s.x, content.origin.y + content.size.height - s.y - s.h),
            NSSize::new(s.w, s.h),
        );
        let rect = window.convertRectToScreen(in_window);
        let image = window_image(&window, rect, mtm);
        Some(Shot { rect, image, window })
    }

    /// `rect` (screen coordinates) of our own `window`, as the window server
    /// has it. An app may always read its own windows, so this needs no
    /// Screen Recording permission.
    #[allow(deprecated)]
    fn window_image(window: &NSWindow, rect: NSRect, mtm: MainThreadMarker) -> Option<Retained<NSImage>> {
        let primary_h = NSScreen::screens(mtm).firstObject()?.frame().size.height;
        let quartz = NSRect::new(NSPoint::new(rect.origin.x, primary_h - rect.origin.y - rect.size.height), rect.size);
        let image = objc2_core_graphics::CGWindowListCreateImage(
            quartz,
            CGWindowListOption::OptionIncludingWindow,
            window.windowNumber() as u32,
            CGWindowImageOption::BoundsIgnoreFraming | CGWindowImageOption::BestResolution,
        )?;
        Some(NSImage::initWithCGImage_size(NSImage::alloc(), &image, rect.size))
    }

    fn build_panel(
        kind: Kind,
        draggable: bool,
        bundle: &NSURL,
        name: &NSString,
        mtm: MainThreadMarker,
    ) -> (Retained<NSPanel>, Retained<Card>) {
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
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(true);
        panel.setHidesOnDeactivate(false);
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setAlphaValue(0.0);

        // Drawn rather than a material, so the flight can show its exact pixels.
        let card = Card::new(frame, mtm);

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
            card.addSubview(&v);
        }
        let label = NSTextField::labelWithString(&NSString::from_str(&text), mtm);
        label.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, unsafe { NSFontWeightMedium })));
        label.setTextColor(Some(&NSColor::labelColor()));
        label.setFrame(NSRect::new(NSPoint::new(PAD + 22.0, text_y), NSSize::new(PANEL_W - 2.0 * PAD - 22.0, 20.0)));
        card.addSubview(&label);

        if draggable {
            let path = bundle.path().unwrap_or_else(|| NSString::from_str(""));
            let icon = NSWorkspace::sharedWorkspace().iconForFile(&path);
            icon.setSize(NSSize::new(32.0, 32.0));
            let tile_frame = NSRect::new(NSPoint::new(PAD, PAD), NSSize::new(PANEL_W - 2.0 * PAD, TILE_H));
            let tile = Tile::new(tile_frame, bundle.retain(), &icon, name, mtm);
            card.addSubview(&tile);
        }
        panel.setContentView(Some(&card));
        (panel, card)
    }

    // ---------------------------------------------------------------- pane

    fn pane_url(kind: Kind) -> &'static str {
        match kind {
            Kind::Screen => "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ScreenCapture",
            Kind::Camera => "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Camera",
            Kind::Accessibility => "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Accessibility",
        }
    }

    /// The list's name as Privacy & Security shows it.
    fn list_name(kind: Kind) -> &'static str {
        match kind {
            Kind::Screen => "Screen & System Audio Recording",
            Kind::Camera => "Camera",
            // Renamed in macOS 27.
            Kind::Accessibility if os_major() >= 27 => "Device Control and Data Access",
            Kind::Accessibility => "Accessibility",
        }
    }

    /// Open the pane. A System Settings that wasn't running restores the
    /// pane it showed last and drops the URL's, so after a cold launch the
    /// URL goes again once its window is up and has settled.
    fn open_pane(kind: Kind) {
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
            done = g.step(mtm);
        });
        if done {
            if let Some(g) = GUIDE.with_borrow_mut(Option::take) {
                g.teardown();
            }
        }
    }

    impl Guide {
        /// One frame. `true` when the guide is finished.
        fn step(&mut self, mtm: MainThreadMarker) -> bool {
            let t = now();
            self.ticks += 1;
            if !self.closing && super::super::permissions::check(self.kind) {
                log::info!("[permission-guide] {:?} granted", self.kind);
                self.closing = true;
            }
            let animating = (self.alpha - self.target).abs() > 0.004 || self.stage != Stage::Docked || t - self.last_move < 0.5;
            // Idle: ~10 Hz is plenty to notice the Settings window starting to move.
            if !animating && !self.closing && self.ticks % 6 != 0 {
                return false;
            }

            if !self.closing {
                match settings_frame() {
                    Some(settings) => {
                        self.saw_settings = true;
                        let origin = dock_origin(settings, self.panel.frame().size, mtm);
                        if let Stage::Waiting { first_seen, last, still_since } = &mut self.stage {
                            let key = (settings.origin.x, settings.origin.y, settings.size.width, settings.size.height);
                            let first = *first_seen.get_or_insert(t);
                            if *last != Some(key) {
                                *last = Some(key);
                                *still_since = t;
                            }
                            let settled = t - *still_since >= SETTLE_WARM && (!self.cold || t - first >= SETTLE_COLD);
                            if settled || t - self.opened_at > FLIGHT_PATIENCE {
                                self.enter(origin, mtm);
                            }
                        } else if self.last_origin.is_none_or(|o| o.x != origin.x || o.y != origin.y) {
                            self.panel.setFrameOrigin(origin);
                            self.last_origin = Some(origin);
                            self.last_move = t;
                        }
                        if self.stage == Stage::Docked {
                            let front = frontmost_is_settings() || NSApplication::sharedApplication(mtm).isActive();
                            self.target = if front { 1.0 } else { 0.0 };
                        }
                    }
                    None => {
                        if self.saw_settings || t - self.opened_at > SETTINGS_PATIENCE {
                            self.closing = true;
                        }
                    }
                }
            }
            if self.closing {
                self.target = 0.0;
                self.drop_overlay();
                if self.stage == Stage::Flying {
                    self.stage = Stage::Docked;
                }
            }
            if self.stage != Stage::Docked {
                return false;
            }

            // Ease the card's opacity (~150 ms either way at 60 Hz).
            let k = if self.target > self.alpha { 0.2 } else { 0.25 };
            self.alpha += (self.target - self.alpha) * k;
            if (self.alpha - self.target).abs() < 0.004 {
                self.alpha = self.target;
            }
            self.panel.setAlphaValue(self.alpha);
            self.closing && self.alpha == 0.0
        }

        /// Settings has settled: put the card in place and fly it there.
        fn enter(&mut self, origin: NSPoint, mtm: MainThreadMarker) {
            self.panel.setFrameOrigin(origin);
            self.last_origin = Some(origin);
            self.last_move = now();
            self.panel.orderFrontRegardless();
            set_phase(Some(self.kind), Phase::InSettings);

            let source = self.source.take();
            let overlay = source.as_ref().and_then(|s| fly(s, self.panel.frame(), &self.card, mtm));
            match overlay {
                Some(overlay) => {
                    log::info!("[permission-guide] flying the card in");
                    self.overlay = Some(overlay);
                    self.stage = Stage::Flying;
                    // Lookout comes forward so the flight starts from a row
                    // that can be seen; Settings returns as the card lands.
                    let window = source.map(|s| s.window);
                    later(0.0, move || {
                        #[allow(deprecated)]
                        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
                        if let Some(w) = window {
                            w.orderFrontRegardless();
                        }
                    });
                    let id = self.id;
                    later(secs(FLIGHT_SECS), move || land(id));
                }
                None => {
                    log::info!("[permission-guide] showing the card (no flight)");
                    self.stage = Stage::Docked;
                }
            }
        }

        /// The flight arrived: the real card takes over from its image.
        fn land(&mut self) {
            if self.stage != Stage::Flying {
                return;
            }
            self.stage = Stage::Docked;
            self.alpha = 1.0;
            self.target = 1.0;
            self.panel.setAlphaValue(1.0);
            self.panel.orderFrontRegardless();
            // A frame of overlap (identical pixels) hides any skew.
            if let Some(overlay) = self.overlay.take() {
                later(0.05, move || overlay.orderOut(None));
            }
            later(0.0, activate_settings);
        }

        fn drop_overlay(&mut self) {
            if let Some(overlay) = self.overlay.take() {
                overlay.orderOut(None);
            }
        }

        fn teardown(mut self) {
            self.timer.invalidate();
            self.drop_overlay();
            self.panel.orderOut(None);
            set_phase(None, Phase::Idle);
        }
    }

    fn land(id: u64) {
        GUIDE.with_borrow_mut(|g| {
            if let Some(g) = g.as_mut().filter(|g| g.id == id) {
                g.land();
            }
        });
    }

    fn activate_settings() {
        let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(SETTINGS_ID));
        if let Some(app) = apps.firstObject() {
            #[allow(deprecated)]
            app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
        }
    }

    pub fn close(animated: bool) {
        if animated {
            GUIDE.with_borrow_mut(|g| {
                if let Some(g) = g.as_mut() {
                    g.closing = true;
                }
            });
        } else if let Some(g) = GUIDE.with_borrow_mut(Option::take) {
            g.teardown();
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

    // ---------------------------------------------------------------- flight

    /// Fly the row's image from `src` to the card at `target` (screen
    /// coordinates), morphing into the card's image on the way. Returns the
    /// overlay window it runs in; `None` (nothing shown) when the two ends
    /// are on different screens.
    fn fly(src: &Shot, target: NSRect, card: &Card, mtm: MainThreadMarker) -> Option<Retained<NSPanel>> {
        let center = |r: NSRect| NSPoint::new(r.origin.x + r.size.width / 2.0, r.origin.y + r.size.height / 2.0);
        let (sc, tc) = (center(src.rect), center(target));
        let screens = NSScreen::screens(mtm);
        let Some(screen) = screens.iter().find(|s| contains(s.frame(), sc)) else {
            log::info!("[permission-guide] the row isn't on a screen; no flight");
            return None;
        };
        let sf = screen.frame();
        if !contains(sf, tc) {
            log::info!("[permission-guide] System Settings is on another screen; no flight");
            return None;
        }
        let Some(card_img) = snapshot(card) else {
            log::warn!("[permission-guide] couldn't render the card; no flight");
            return None;
        };
        let scale = screen.backingScaleFactor();

        let overlay = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            sf,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        overlay.setLevel(101); // NSPopUpMenuWindowLevel: above the card's panel
        overlay.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        overlay.setOpaque(false);
        overlay.setBackgroundColor(Some(&NSColor::clearColor()));
        overlay.setHasShadow(false);
        overlay.setIgnoresMouseEvents(true);
        overlay.setHidesOnDeactivate(false);
        unsafe { overlay.setReleasedWhenClosed(false) };
        overlay.setFrame_display(sf, false);

        // A layer-hosting view: our own root layer, which AppKit leaves alone.
        let host = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), sf.size));
        let root = CALayer::new();
        host.setLayer(Some(&root));
        host.setWantsLayer(true);
        host.setLayerUsesCoreImageFilters(true);

        let local = |p: NSPoint| NSPoint::new((p.x - sf.origin.x).round(), (p.y - sf.origin.y).round());
        let (from, to) = (local(sc), local(tc));
        let (from_size, to_size) = (src.rect.size, target.size);
        let mid = |s: NSSize| NSPoint::new(s.width / 2.0, s.height / 2.0);

        // The container travels and carries the shadow and blur; its two
        // faces (row, card) are clipped to the morphing rounded rect.
        let container = CALayer::new();
        let face = |image: &NSImage| {
            let l = CALayer::new();
            unsafe { l.setContents(Some(image)) };
            l.setContentsGravity(unsafe { kCAGravityResize });
            l.setContentsScale(scale);
            l.setMasksToBounds(true);
            l.setCornerCurve(unsafe { objc2_quartz_core::kCACornerCurveContinuous });
            l
        };
        let row_face = face(src.image.as_deref().unwrap_or(&card_img));
        let card_face = face(&card_img);
        without_actions(|| {
            container.setBounds(NSRect::new(NSPoint::new(0.0, 0.0), to_size));
            container.setPosition(to);
            container.setShadowOpacity(0.0);
            container.setShadowRadius(18.0);
            container.setShadowOffset(NSSize::new(0.0, -6.0));
            for (l, opacity) in [(&row_face, 0.0), (&card_face, 1.0)] {
                l.setBounds(NSRect::new(NSPoint::new(0.0, 0.0), to_size));
                l.setPosition(mid(to_size));
                l.setCornerRadius(CARD_RADIUS);
                l.setOpacity(opacity);
                container.addSublayer(l);
            }
            if let Some(blur) = blur_filter() {
                unsafe { container.setFilters(Some(&NSArray::from_retained_slice(&[blur]))) };
            }
            root.addSublayer(&container);
        });
        overlay.setContentView(Some(&host));

        let d = secs(FLIGHT_SECS);
        // Gentle out of the row, then a long, soft arrival.
        let ease = CAMediaTimingFunction::functionWithControlPoints(0.36, 0.0, 0.1, 1.0);
        let basic = |key: &NSString, a: Retained<AnyObject>, b: Retained<AnyObject>| {
            let x = CABasicAnimation::animationWithKeyPath(Some(key));
            unsafe {
                x.setFromValue(Some(&a));
                x.setToValue(Some(&b));
            }
            x.setDuration(d);
            x.setTimingFunction(Some(&ease));
            x.setFillMode(unsafe { kCAFillModeBoth });
            Retained::into_super(x)
        };
        // Keyframes in progress space: the same ease maps time to progress.
        let frames = |key: &NSString, values: &[f64], times: &[f64]| {
            let x = CAKeyframeAnimation::animationWithKeyPath(Some(key));
            let values: Vec<Retained<AnyObject>> = values.iter().map(|v| number(*v)).collect();
            let times: Vec<Retained<NSNumber>> = times.iter().map(|t| NSNumber::new_f64(*t)).collect();
            unsafe {
                x.setValues(Some(&NSArray::from_retained_slice(&values)));
                x.setKeyTimes(Some(&NSArray::from_retained_slice(&times)));
            }
            x.setDuration(d);
            x.setTimingFunction(Some(&ease));
            x.setFillMode(unsafe { kCAFillModeBoth });
            Retained::into_super(x)
        };

        let dist = (to.x - from.x).hypot(to.y - from.y);
        let control = NSPoint::new((from.x + to.x) / 2.0, from.y.max(to.y) + (dist * 0.3).clamp(40.0, ARC_MAX));
        let path = CGMutablePath::new();
        unsafe {
            CGMutablePath::move_to_point(Some(&path), std::ptr::null(), from.x, from.y);
            CGMutablePath::add_quad_curve_to_point(Some(&path), std::ptr::null(), control.x, control.y, to.x, to.y);
        }
        let travel = CAKeyframeAnimation::animationWithKeyPath(Some(ns_string!("position")));
        travel.setPath(Some(&path));
        travel.setCalculationMode(unsafe { kCAAnimationPaced });
        travel.setDuration(d);
        travel.setTimingFunction(Some(&ease));
        travel.setFillMode(unsafe { kCAFillModeBoth });
        container.addAnimation_forKey(&travel, Some(ns_string!("travel")));

        container.addAnimation_forKey(&basic(ns_string!("bounds.size"), size_value(from_size), size_value(to_size)), Some(ns_string!("grow")));
        container.addAnimation_forKey(&frames(ns_string!("shadowOpacity"), &[0.0, 0.3, 0.0], &[0.0, 0.45, 1.0]), Some(ns_string!("lift")));
        if blur_filter_class().is_some() {
            container.addAnimation_forKey(
                &frames(ns_string!("filters.blur.inputRadius"), &[0.0, BLUR_PEAK, 0.0], &[0.0, 0.45, 0.9]),
                Some(ns_string!("blur")),
            );
        }
        for (l, fade) in [(&row_face, [1.0, 1.0, 0.0, 0.0]), (&card_face, [0.0, 0.0, 1.0, 1.0])] {
            l.addAnimation_forKey(&basic(ns_string!("bounds.size"), size_value(from_size), size_value(to_size)), Some(ns_string!("grow")));
            l.addAnimation_forKey(&basic(ns_string!("position"), point_value(mid(from_size)), point_value(mid(to_size))), Some(ns_string!("recenter")));
            l.addAnimation_forKey(&basic(ns_string!("cornerRadius"), number(ROW_RADIUS), number(CARD_RADIUS)), Some(ns_string!("round")));
            // The swap happens around the apex, where the eye is on the motion.
            l.addAnimation_forKey(&frames(ns_string!("opacity"), &fade, &[0.0, 0.3, 0.62, 1.0]), Some(ns_string!("swap")));
        }
        overlay.orderFrontRegardless();
        Some(overlay)
    }

    // ---------------------------------------------------------------- helpers

    fn without_actions(f: impl FnOnce()) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        f();
        CATransaction::commit();
    }

    fn number(v: f64) -> Retained<AnyObject> {
        Retained::into_super(Retained::into_super(NSNumber::new_f64(v))).into()
    }

    fn size_value(s: NSSize) -> Retained<AnyObject> {
        Retained::into_super(unsafe { NSValue::valueWithSize(s) }).into()
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

    fn os_major() -> isize {
        objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion().majorVersion
    }

    /// An SF Symbol (macOS 11+; the app still runs on 10.15).
    fn symbol(name: &NSString) -> Option<Retained<NSImage>> {
        if os_major() < 11 {
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
