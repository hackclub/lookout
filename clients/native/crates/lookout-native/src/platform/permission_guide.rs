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
//! The flight moves and resizes the live glass panel along a quadratic arc.
//! The captured row fades out over that material around the apex, revealing
//! the live card content. The same panel remains docked, with no material or
//! geometry handoff at arrival.
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
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol, ProtocolObject};
    use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationOptions, NSBackingStoreType, NSBezierPath, NSBitmapImageRep, NSColor,
        NSCursor, NSDraggingContext, NSDraggingFormation, NSDraggingItem, NSDraggingSession, NSDraggingSource,
        NSDragOperation, NSEvent, NSGraphicsContext, NSLineJoinStyle, NSShadow, NSFont, NSFontWeightMedium, NSFontWeightSemibold, NSImage, NSImageScaling, NSImageView, NSPanel,
        NSResponder, NSRunningApplication, NSScreen, NSTextField, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
        NSVisualEffectView, NSWindow, NSWorkspaceOpenConfiguration, NSWindowCollectionBehavior,
        NSWindowStyleMask, NSWorkspace,
    };
    use objc2_core_graphics::{
        CGWindowImageOption, CGWindowListCopyWindowInfo, CGWindowListOption, kCGNullWindowID,
    };
    use objc2_foundation::{
        NSArray, NSBundle, NSDictionary, NSFileManager, NSNumber, NSObject, NSPoint, NSRect,
        NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSTimer, NSURL, ns_string,
    };
    use objc2_quartz_core::CACurrentMediaTime;

    const SETTINGS_ID: &str = "com.apple.systempreferences";

    // Card layout (AppKit points, origin bottom-left).
    const PANEL_W: f64 = 380.0;
    const PANEL_H_DRAG: f64 = 132.0;
    const PANEL_H_SWITCH: f64 = 84.0;
    const PAD: f64 = 16.0;
    const TILE_H: f64 = 48.0;
    const CARD_RADIUS: f64 = 18.0;
    /// The setup page's rows (`screens/permission.rs`).
    const ROW_RADIUS: f64 = 14.0;
    /// Inset from the Settings window's bottom-right corner.
    const DOCK_INSET: f64 = 16.0;

    // Flight.
    const FLIGHT_SECS: f64 = 0.8;
    /// Gentle out of the row, then a long, soft arrival.
    const FLIGHT_EASE: crate::motion::Ease = crate::motion::Ease::Bezier(0.33, 0.0, 0.12, 1.0);
    /// Peak of the arc above the higher end (points), at most.
    const ARC_MAX: f64 = 150.0;
    /// A warm Settings window counts as settled once its frame has held
    /// still this long; a cold launch also re-sends the pane URL (see
    /// `open_pane`), so that waits until after the last re-send.
    const SETTLE_WARM: f64 = 0.2;
    const SETTLE_COLD: f64 = 1.45;
    /// Settings that hasn't settled by then gets the card without a flight.
    const FLIGHT_PATIENCE: f64 = 6.0;
    /// ...and no guide at all after this.
    const SETTINGS_PATIENCE: f64 = 12.0;

    // ---------------------------------------------------------------- arrow

    define_class!(
        // SAFETY: stateless main-thread NSView subclass.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LookoutPermissionArrow"]
        struct Arrow;

        impl Arrow {
            #[unsafe(method(drawRect:))]
            fn draw_rect(&self, _dirty: NSRect) {
                // A filled arrow with a rounded white rim, matching the guide
                // reference. Draw as a path so it stays crisp while stretching.
                let path = NSBezierPath::bezierPath();
                path.moveToPoint(NSPoint::new(16.0, 6.0));
                for (x, y) in [(26.0, 6.0), (26.0, 27.0), (36.0, 27.0),
                    (21.0, 44.0), (6.0, 27.0), (16.0, 27.0)] {
                    path.lineToPoint(NSPoint::new(x, y));
                }
                path.closePath();
                path.setLineJoinStyle(NSLineJoinStyle::Round);
                path.setLineWidth(3.0);
                NSGraphicsContext::saveGraphicsState_class();
                let shadow = NSShadow::new();
                shadow.setShadowColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.48, 1.0, 0.35)));
                shadow.setShadowBlurRadius(4.0);
                shadow.setShadowOffset(NSSize::new(0.0, -1.0));
                shadow.set();
                NSColor::whiteColor().setStroke();
                path.stroke();
                NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.60, 1.0, 1.0).setFill();
                path.fill();
                NSGraphicsContext::restoreGraphicsState_class();
            }
        }
        unsafe impl NSObjectProtocol for Arrow {}
    );

    struct ArrowMotion {
        view: Retained<Arrow>,
        frame: NSRect,
        started: Option<f64>,
        last_tick: f64,
        last_kick: f64,
        recoil_target: f64,
        scale: f64,
        scale_velocity: f64,
        recoil: f64,
        recoil_velocity: f64,
    }

    impl ArrowMotion {
        fn new(frame: NSRect, mtm: MainThreadMarker) -> Self {
            let view = Arrow::alloc(mtm).set_ivars(());
            let view: Retained<Arrow> = unsafe { msg_send![super(view), initWithFrame: frame] };
            view.setAlphaValue(if crate::motion::reduce_motion() { 1.0 } else { 0.0 });
            Self { view, frame, started: None, last_tick: now(), last_kick: 0.0,
                recoil_target: 1.0, scale: 0.2, scale_velocity: 18.0,
                recoil: 1.0, recoil_velocity: 0.0 }
        }

        fn active(&self, t: f64) -> bool {
            !crate::motion::reduce_motion() && self.started.is_some() &&
                (t - self.last_kick < secs(4.0) || (self.scale - 1.0).abs() > 0.001)
        }

        fn kick(&mut self, dy: f64, dt: f64, t: f64) {
            self.recoil_target = (1.0 - dy / dt.max(1.0 / 120.0) / 2200.0).clamp(0.3, 2.0);
            self.last_kick = t;
        }

        fn step(&mut self, t: f64) {
            if crate::motion::reduce_motion() {
                self.view.setFrame(self.frame);
                self.view.setAlphaValue(1.0);
                return;
            }
            if self.started.is_none() {
                self.started = Some(t);
                self.last_tick = t;
                self.last_kick = t;
            }
            let elapsed = (t - self.last_kick) / secs(1.0);
            let target = if elapsed < 0.15 { self.recoil_target }
                else if (2.15..2.37).contains(&elapsed) { 2.0 } else { 1.0 };
            let dt = ((t - self.last_tick) / secs(1.0)).clamp(0.0, 0.05);
            self.last_tick = t;
            // Small integration steps keep the underdamped spring stable even
            // when a Settings move makes the main run loop miss a frame.
            let steps = (dt * 240.0).ceil().max(1.0) as usize;
            let h = dt / steps as f64;
            for _ in 0..steps {
                self.scale_velocity += (200.0 * (1.0 - self.scale) - 11.0 * self.scale_velocity) * h;
                self.scale += self.scale_velocity * h;
                self.recoil_velocity += (180.0 * (target - self.recoil) - 14.0 * self.recoil_velocity) * h;
                self.recoil += self.recoil_velocity * h;
            }
            let scale = self.scale.max(0.05);
            let width = self.frame.size.width * scale;
            let height = self.frame.size.height * scale * self.recoil.max(0.2);
            let offset = -18.0 * (1.0 - self.scale);
            self.view.setFrame(NSRect::new(NSPoint::new(
                self.frame.origin.x + (self.frame.size.width - width) / 2.0,
                self.frame.origin.y + offset,
            ), NSSize::new(width, height)));
            self.view.setBounds(NSRect::new(NSPoint::new(0.0, 0.0), self.frame.size));
            self.view.setAlphaValue(((t - self.started.unwrap()) / secs(0.15)).clamp(0.0, 1.0));
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
        /// Its pixels; without them the live glass flies on its own.
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
        /// Keeps delayed Settings activation tied to this guide.
        id: u64,
        kind: Kind,
        panel: Retained<NSPanel>,
        content: Retained<NSView>,
        arrow: ArrowMotion,
        /// System Settings wasn't running at the click.
        cold: bool,
        source: Option<Shot>,
        stage: Stage,
        flight: Option<Flight>,
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
        // System Settings open on another desktop (or minimised) would
        // pull the user over there and back; a fresh one opens here.
        let elsewhere = settings_pid().is_some() && settings_frame().is_none();
        let cold = elsewhere || settings_pid().is_none();
        if elsewhere {
            reopen_settings_here(kind);
        } else {
            open_pane(kind);
        }

        // Camera's list only holds apps that asked, so it's switched on, not dragged in.
        let draggable = kind != Kind::Camera;
        let name = app_name(&bundle);
        let (panel, content, arrow) = build_panel(kind, draggable, &bundle, &name, mtm);

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
                content,
                arrow,
                cold,
                source: shot,
                stage: Stage::Waiting { first_seen: None, last: None, still_since: t },
                flight: None,
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

    /// One live card for both flight and docking: Liquid Glass on macOS 26+,
    /// a popover material before.
    fn build_panel(
        kind: Kind,
        draggable: bool,
        bundle: &NSURL,
        name: &NSString,
        mtm: MainThreadMarker,
    ) -> (Retained<NSPanel>, Retained<NSView>, ArrowMotion) {
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

        let (glass, content) = glass_view(frame, mtm);
        let arrow = populate(&content, kind, draggable, bundle, name, mtm);
        panel.setContentView(Some(&glass));

        (panel, content, arrow)
    }

    /// `(outer view, view to put the content in)`.
    fn glass_view(frame: NSRect, mtm: MainThreadMarker) -> (Retained<NSView>, Retained<NSView>) {
        if let Some(cls) = AnyClass::get(c"NSGlassEffectView") {
            // macOS 26: Liquid Glass.
            unsafe {
                let a: Allocated<NSView> = msg_send![cls, alloc];
                let glass: Retained<NSView> = msg_send![a, initWithFrame: frame];
                let _: () = msg_send![&*glass, setCornerRadius: CARD_RADIUS];
                let content = NSView::initWithFrame(NSView::alloc(mtm), frame);
                let _: () = msg_send![&*glass, setContentView: &*content];
                return (glass, content);
            }
        }
        let material = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
        material.setMaterial(NSVisualEffectMaterial::Popover);
        material.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        material.setState(NSVisualEffectState::Active);
        material.setWantsLayer(true);
        if let Some(layer) = material.layer() {
            layer.setCornerRadius(CARD_RADIUS);
            layer.setMasksToBounds(true);
            layer.setCornerCurve(unsafe { objc2_quartz_core::kCACornerCurveContinuous });
        }
        let view: Retained<NSView> = Retained::into_super(material);
        let content = NSView::initWithFrame(NSView::alloc(mtm), frame);
        view.addSubview(&content);
        (view, content)
    }

    fn populate(into: &NSView, kind: Kind, draggable: bool, bundle: &NSURL, name: &NSString, mtm: MainThreadMarker) -> ArrowMotion {
        let h = into.frame().size.height;
        let list = list_name(kind);
        let text = if draggable {
            format!("Drag {name} into the {list} list above")
        } else {
            format!("Switch on {name} in the {list} list above")
        };
        let text_y = if draggable { h - PAD - 30.0 } else { (h - 20.0) / 2.0 };
        let arrow = ArrowMotion::new(NSRect::new(NSPoint::new(PAD - 5.0, text_y - 14.0), NSSize::new(42.0, 50.0)), mtm);
        into.addSubview(&arrow.view);
        let label = NSTextField::labelWithString(&NSString::from_str(&text), mtm);
        label.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, unsafe { NSFontWeightMedium })));
        label.setTextColor(Some(&NSColor::labelColor()));
        label.setFrame(NSRect::new(NSPoint::new(PAD + 42.0, text_y), NSSize::new(PANEL_W - 2.0 * PAD - 42.0, 20.0)));
        into.addSubview(&label);

        if draggable {
            let path = bundle.path().unwrap_or_else(|| NSString::from_str(""));
            let icon = NSWorkspace::sharedWorkspace().iconForFile(&path);
            icon.setSize(NSSize::new(32.0, 32.0));
            let tile_frame = NSRect::new(NSPoint::new(PAD, PAD), NSSize::new(PANEL_W - 2.0 * PAD, TILE_H));
            let tile = Tile::new(tile_frame, bundle.retain(), &icon, name, mtm);
            into.addSubview(&tile);
        }
        arrow
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
        open_quietly(&url);
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
                            open_quietly(&url);
                        }
                    });
                    unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(delay, false, &again) };
                }
            }
        });
        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.1, true, &block) };
    }

    /// Open without bringing System Settings forward: Lookout stays in
    /// front so the flight starts from a row that can be seen, and Settings
    /// comes forward mid-flight.
    fn open_quietly(url: &NSURL) {
        let config = NSWorkspaceOpenConfiguration::configuration();
        config.setActivates(false);
        NSWorkspace::sharedWorkspace().openURL_configuration_completionHandler(url, &config, None);
    }

    /// Quit the System Settings that's on another desktop, then open the
    /// pane in a fresh one (on this desktop).
    fn reopen_settings_here(kind: Kind) {
        let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(SETTINGS_ID));
        if let Some(app) = apps.firstObject() {
            log::info!("[permission-guide] System Settings is on another desktop; reopening it here");
            app.terminate();
        }
        let id = GUIDE_ID.with(Cell::get) + 1; // the guide being set up
        let started = now();
        let block = RcBlock::new(move |t: std::ptr::NonNull<NSTimer>| {
            let current = GUIDE_ID.with(Cell::get) == id && GUIDE.with_borrow(|g| g.is_some());
            let gone = settings_pid().is_none();
            if !current || gone || now() - started > 2.0 {
                unsafe { t.as_ref().invalidate() };
                if current {
                    open_pane(kind);
                }
            }
        });
        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.05, true, &block) };
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
            let animating = (self.alpha - self.target).abs() > 0.004 || self.stage != Stage::Docked || t - self.last_move < 0.5 || self.arrow.active(t);
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
                        } else if self.stage == Stage::Docked && self.last_origin.is_none_or(|o| o.x != origin.x || o.y != origin.y) {
                            if let Some(previous) = self.last_origin {
                                self.arrow.kick(origin.y - previous.y, t - self.last_move, t);
                            }
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
                self.finish_flight();
                if self.stage == Stage::Flying {
                    self.stage = Stage::Docked;
                }
            }
            if self.stage == Stage::Flying {
                self.advance_flight(t);
            }
            if self.stage != Stage::Docked {
                return false;
            }

            self.arrow.step(t);

            // Ease visibility changes and the no-flight entrance (~150 ms).
            let k = if self.target > self.alpha { 0.26 } else { 0.25 };
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
            self.flight = source.as_ref().and_then(|s| Flight::new(s, self.panel.frame(), mtm));
            if self.flight.is_some() {
                log::info!("[permission-guide] flying the live glass card in");
                self.stage = Stage::Flying;
                self.panel.setLevel(101); // above Settings throughout the flight
                self.panel.setIgnoresMouseEvents(true);
                self.alpha = 1.0;
                self.target = 1.0;
                self.panel.setAlphaValue(1.0);
                if let Some(row) = self.flight.as_ref().and_then(|f| f.row.as_ref()) {
                    self.panel.contentView().unwrap().addSubview(row);
                }
                self.advance_flight(now());
                let id = self.id;
                later(secs(FLIGHT_SECS * 0.45), move || {
                    let current = GUIDE.with_borrow(|g| g.as_ref().is_some_and(|g| g.id == id && !g.closing));
                    if current {
                        activate_settings();
                    }
                });
            } else {
                log::info!("[permission-guide] showing the card (no flight)");
                self.stage = Stage::Docked;
                activate_settings();
            }
        }

        fn advance_flight(&mut self, t: f64) {
            let Some(f) = &self.flight else { return };
            let elapsed = ((t - f.started) / secs(FLIGHT_SECS)).clamp(0.0, 1.0);
            let p = FLIGHT_EASE.apply(elapsed as f32) as f64;
            let frame = f.frame(p);
            self.panel.setFrame_display(frame, true);
            // Scale the original content coordinates with the panel, rather
            // than relaying out labels and the draggable tile on every frame.
            self.content.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), frame.size));
            self.content.setBounds(NSRect::new(NSPoint::new(0.0, 0.0), f.target.size));
            let swap = ((p - 0.28) / (0.62 - 0.28)).clamp(0.0, 1.0);
            let swap = swap * swap * (3.0 - 2.0 * swap);
            self.content.setAlphaValue(swap);
            let radius = ROW_RADIUS + (CARD_RADIUS - ROW_RADIUS) * p;
            let glass = self.panel.contentView().unwrap();
            set_glass_radius(&glass, radius);
            if let Some(row) = &f.row {
                row.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), frame.size));
                row.setAlphaValue(1.0 - swap);
                if let Some(layer) = row.layer() {
                    layer.setCornerRadius(radius);
                }
            }
            if elapsed == 1.0 {
                self.finish_flight();
                self.stage = Stage::Docked;
            }
        }

        fn finish_flight(&mut self) {
            if let Some(f) = self.flight.take() {
                if let Some(row) = f.row {
                    row.removeFromSuperview();
                }
                self.panel.setFrame_display(f.target, true);
                let bounds = NSRect::new(NSPoint::new(0.0, 0.0), f.target.size);
                self.content.setFrame(bounds);
                self.content.setBounds(bounds);
                self.content.setAlphaValue(1.0);
                if let Some(glass) = self.panel.contentView() {
                    set_glass_radius(&glass, CARD_RADIUS);
                }
                self.panel.setLevel(3);
                self.panel.setIgnoresMouseEvents(false);
            }
        }

        fn teardown(mut self) {
            self.timer.invalidate();
            self.finish_flight();
            self.panel.orderOut(None);
            set_phase(None, Phase::Idle);
        }
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

    struct Flight {
        source: NSRect,
        target: NSRect,
        control: NSPoint,
        started: f64,
        row: Option<Retained<NSImageView>>,
    }

    impl Flight {
        fn new(src: &Shot, target: NSRect, mtm: MainThreadMarker) -> Option<Self> {
            let from = rect_center(src.rect);
            let to = rect_center(target);
            // Keep the existing no-flight behavior across displays.
            if !NSScreen::screens(mtm).iter().any(|s| contains(s.frame(), from) && contains(s.frame(), to)) {
                return None;
            }
            let dist = (to.x - from.x).hypot(to.y - from.y);
            let row = src.image.as_ref().map(|image| {
                let row = NSImageView::imageViewWithImage(image, mtm);
                row.setImageScaling(NSImageScaling::ScaleAxesIndependently);
                row.setWantsLayer(true);
                if let Some(layer) = row.layer() {
                    layer.setMasksToBounds(true);
                    layer.setCornerCurve(unsafe { objc2_quartz_core::kCACornerCurveContinuous });
                }
                row
            });
            Some(Self {
                source: src.rect,
                target,
                control: NSPoint::new((from.x + to.x) / 2.0, from.y.max(to.y) + (dist * 0.3).clamp(40.0, ARC_MAX)),
                started: now(),
                row,
            })
        }

        fn frame(&self, p: f64) -> NSRect {
            let from = rect_center(self.source);
            let to = rect_center(self.target);
            let u = 1.0 - p;
            let size = NSSize::new(
                self.source.size.width + (self.target.size.width - self.source.size.width) * p,
                self.source.size.height + (self.target.size.height - self.source.size.height) * p,
            );
            NSRect::new(NSPoint::new(
                u * u * from.x + 2.0 * u * p * self.control.x + p * p * to.x - size.width / 2.0,
                u * u * from.y + 2.0 * u * p * self.control.y + p * p * to.y - size.height / 2.0,
            ), size)
        }
    }

    fn rect_center(r: NSRect) -> NSPoint {
        NSPoint::new(r.origin.x + r.size.width / 2.0, r.origin.y + r.size.height / 2.0)
    }

    fn set_glass_radius(glass: &NSView, radius: f64) {
        if AnyClass::get(c"NSGlassEffectView").is_some() {
            unsafe { let _: () = msg_send![glass, setCornerRadius: radius]; }
        } else if let Some(layer) = glass.layer() {
            layer.setCornerRadius(radius);
        }
    }

    // ---------------------------------------------------------------- helpers

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
