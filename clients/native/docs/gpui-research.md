# GPUI research: how polished GPUI apps are built, and what Lookout should copy

Research date: 2026-11-07. Target stack: `gpui-kit 0.8.1`, which pins `gpui-pre =0.3.10`. Every GPUI signature quoted below was read from the registry source at
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-pre-0.3.10/src`, and the `gpui_base` ones from `.../gpui-base-0.8.1/src`.

Reference apps:
- **Cap**, shallow clone at `/tmp/research/cap`. The GPUI app is `apps/desktop-gpui`, a full rewrite of their Tauri desktop app.
- **Frame** (66HEX), shallow clone at `/tmp/research/frame`. A video/image converter with a polished single-window UI.

Companion doc: `design-guidance.md` in this directory covers the design rules (timings, curves, tactility). This file covers the mechanics of building them.

---

## TL;DR

1. **Both reference apps run patched GPUI forks, and neither runs gpui-pre.**
   - Cap pins `wingleeio/zed@5d1f83d` and applies about 12.5k lines of its own patches: idle frames, streaming image, glass, Windows, Linux and perf.
   - Frame vendors `gpui-ce` with patches for versioned `RenderImage`, transformed image paint, Linux rounded client-side frame and titlebar hit-testing.
   - Copy their **patterns**, not their APIs. Several of their headline features do not exist in gpui-pre 0.3.10:
     - `StreamingImage`
     - `paint_image_transformed`
     - `RenderImage::new_with_id`
     - `use_keyed_transition`
     - `ClientSideFrameOptions`
2. **gpui-kit already ships the motion layer we wanted to write.** `gpui_kit::base` has all of these:
   - `transition()`, CSS-like with `Easing::cubic_bezier`.
   - `spring()`, velocity-preserving and keyed.
   - `Presence`, for enter/exit lifecycle with unmount.
   - `Sequence`, `Stagger` and `Keyframes`.
   - `MotionTokens`.

   All of it is reduced-motion aware, and `gpui_base::init` reads the OS reduce-motion setting. Do **not** hand-roll easing or spring code. Write a thin `motion.rs` with Lookout tokens on top (sketch in §7).
3. **gpui-pre 0.3.10 has no general element transforms.** There is no `scale`, `rotate` or `translate` on `div`/`img`; Cap's README complains about the same gap.
   - Real transforms exist only for `svg().with_transformation(Transformation::scale/rotate/translate)`.
   - So "press scale" on a button has to be faked: inset the background layer, animate the icon SVG scale, or use opacity/colour.
4. **Tray:** GPUI has no tray API, so the recommendation is per platform:
   - macOS: keep Lookout's existing Swift `lookout-tray` (SwiftUI `NSStatusItem` with numericText digits) through `swift-rs`, or use raw `objc2` the way Cap does.
   - Windows: the `tray-icon` crate.
   - Linux: `ksni` (pure-Rust StatusNotifierItem over D-Bus, no GTK loop).
   - Everywhere, OS callbacks only push into a channel, and a `cx.spawn` task drains it. This is Cap's rule; see §6.
5. **Video/preview frames:**
   - Use `Arc<RenderImage>` painted in a `canvas`/custom element.
   - Call `window.drop_image(old)` (or `cx.drop_image(old, Some(window))`) on **every** replace, or the sprite atlas leaks.
   - Keep the preview in its own small `Entity` rendered with `.cached(...)`.
   - Feed it through a latest-frame mailbox or a bounded channel that drops frames.
   - On macOS, `surface()` / `paint_surface` takes a `CVPixelBuffer` with zero copy.
6. **gpui-fast:** an optional feature of gpui-kit (`features = ["gpui-fast"]`), longbridge's retained-mode fork.
   - It cuts idle and animation CPU 75–88% in their benchmarks.
   - It's worth it for an always-on menu-bar recorder, but it is experimental. Build on gpui-pre first, keep render state in entities/globals, and A/B test gpui-fast before release. See §8.

---

## 1. Crate layout in the reference apps

### Cap (`apps/desktop-gpui`)
- `src/` is flat, one module per window: `main_window.rs`, `settings_window.rs`, `editor_window.rs`, `camera_window.rs`, `target_overlay.rs`, `controls_window.rs`, `onboarding_window.rs` and so on.
- Shared infrastructure:
  - `app_windows.rs`: a window-registry `Global` that holds every `open_window` call.
  - `platform.rs` + `platform/windows.rs`: every native hack (objc2, windows-sys, x11rb).
  - `theme.rs`, `tray.rs` (+ `tray/windows.rs`), `hotkeys.rs` (global-hotkey 0.7), `store.rs` (JSON settings), `single_instance.rs`, `assets.rs`.
- `src/ui/` holds the component library. Each component is a `#[derive(IntoElement)] struct` + `RenderOnce`, with one named constructor per surface (`::plain`, `::settings`, `::glass`). See `ui/mod.rs:9-34`.
- `kernels/` (`cap-gpui-kernels`): plain-slice pixel kernels built at `opt-level=3` with SIMD, such as RGBA→BGRA unpadding.
  - Without it, frame conversion in a debug build took 42.7 ms; with it, 1.0 ms.
  - The reason is that generics compile at the instantiating crate's opt level, so hot `image`/resize code sits behind non-generic functions in a separate crate.
- `Cargo.toml` profile:
  - deps `opt-level=2`, app `0`, kernels `3`.
  - release `lto="thin"`, `codegen-units=1`.
- Tokio: a custom runtime (2 workers) is built in `main`, then `gpui_tokio::init_from_handle(cx, handle)` (`main.rs:285-292,382`).

### Frame
- Workspace crates: `frame-core` (no GUI), `frame-updater` and `frame-app`.
- `frame-app/src/app/` holds one root view, `FrameRoot`, split across `render.rs`, `chrome.rs` (titlebar, sheets), `motion.rs`, `primitives.rs`, `components/*`, `preview_panel/*` and `*_actions.rs`.
- Per-widget transient state (pressed, measured width, tooltip warm-up) lives in `window.use_keyed_state(...)`, not in the root.
- **Dev-profile trick** (root `Cargo.toml`): `[profile.dev.package.gpui-ce] opt-level = 3`, and the same for `taffy`. One heavy screen went from 127 ms to 13 ms CPU per frame (`vendor/gpui-ce/FRAME_PATCHES.md`). **Do this for `gpui-pre`, `gpui-base`, `gpui-component` and `taffy`.**

---

## 2. Windows

### gpui-pre 0.3.10 window API (exact)

```rust
// platform.rs:2579
pub struct WindowOptions {
    pub window_bounds: Option<WindowBounds>,          // Windowed/Maximized/Fullscreen(Bounds<Pixels>)
    pub titlebar: Option<TitlebarOptions>,            // None = no titlebar (macOS borderless)
    pub focus: bool,                                  // default true
    pub show: bool,                                   // default true
    pub kind: WindowKind,
    pub is_movable: bool,                             // default true
    pub app_owns_titlebar_drag: bool,                 // macOS: content view is app-owned titlebar; use with start_window_move
    pub inactive_frame_interval: Option<Duration>,    // default Some(33_333µs) = 30fps when inactive
    pub is_resizable: bool,
    pub is_minimizable: bool,
    pub display_id: Option<DisplayId>,
    pub window_background: WindowBackgroundAppearance,
    pub app_id: Option<String>,
    pub window_min_size: Option<Size<Pixels>>,
    pub window_decorations: Option<WindowDecorations>, // Linux: Server | Client
    pub icon: Option<Arc<image::RgbaImage>>,          // X11 only
    pub tabbing_identifier: Option<String>,
    pub use_display_p3: bool,
}
pub struct TitlebarOptions { pub title: Option<SharedString>, pub appears_transparent: bool, pub traffic_light_position: Option<Point<Pixels>> }
pub enum WindowKind { Normal, PopUp, AnchoredPopup(popup::PopupOptions), Floating,
                      #[cfg(linux+wayland)] LayerShell(layer_shell::LayerShellOptions), Dialog }
pub enum WindowBackgroundAppearance { Opaque /*default*/, Transparent, Blurred, MicaBackdrop, MicaAltBackdrop }
pub enum WindowControlArea { Drag, Close, Max, Min }        // window.rs:759
pub enum Decorations { Server, Client { tiling: Tiling } }  // window.window_decorations()
```

- `WindowKind::PopUp` is a top-level window that sits above all others. Use it for overlays and the recording HUD.
- `AnchoredPopup(PopupOptions { parent, anchor_rect, anchor, gravity, constraint_adjustment, offset, grab… })` is a native, parent-anchored popup (`platform/popup.rs:16`). It's implemented in the macOS, Windows, X11 and Wayland backends. Unsupported platforms return `PopupNotSupportedError`.
- `LayerShell` on Wayland is the only correct way to draw a full-screen area-selection overlay on Wayland.

Useful `Window`/`App` methods, with their line numbers in `window.rs` / `app.rs`:

```rust
window.start_window_move()                    // 3023
window.start_window_resize(edge: ResizeEdge)  // 2518 (CSD resize)
window.titlebar_double_click()                // 6976 (macOS: honours user pref zoom/minimize)
window.zoom_window(); window.minimize_window(); window.remove_window();
window.show_window_menu(position)             // Linux CSD right-click menu
window.set_client_inset(px)                   // 3028, CSD shadow inset
window.window_decorations() -> Decorations    // 3039
window.request_decorations(WindowDecorations) // 2495
window.set_background_appearance(WindowBackgroundAppearance) // 3076
window.appearance() -> WindowAppearance; cx.observe_window_appearance(window, |this, window, cx| ..)
window.set_rem_size(px)                       // global UI zoom (Frame's 80–200% scale trick)
window.on_window_should_close(cx, |window, cx| -> bool)
cx.set_activation_policy(ActivationPolicy::Accessory)  // macOS menu-bar app, no Dock icon
cx.set_quit_mode(QuitMode::Explicit)          // keep running with no windows (tray app)
cx.hide(); cx.displays(); cx.primary_display();
app.on_reopen(..); app.on_open_urls(..)       // on Application, before run()
cx.screen_capture_sources() / is_screen_capture_supported()   // exists in 0.3.10
```

gpui-kit wrappers:
- `gpui_kit::open_window(options, cx, |window, cx| cx.new(..))` wraps your view in `base::Root`, which dialogs, sheets and notifications need.
- `gpui_kit::component::TitleBar::window_options()` returns `titlebar: {appears_transparent: true, traffic_light_position: (9,9)}` plus `app_owns_titlebar_drag: true` (`gpui-component-0.8.1/src/title_bar.rs:60-92`).

### Patterns from Cap

**Main window: transparent, rounded and floating** (`cap/apps/desktop-gpui/src/main.rs:425-473`):
```rust
WindowOptions {
    window_bounds: Some(WindowBounds::Windowed(bounds)),
    #[cfg(target_os = "macos")] titlebar: None,
    #[cfg(target_os = "windows")] titlebar: Some(TitlebarOptions { title: Some("Cap".into()), appears_transparent: true, ..Default::default() }),
    #[cfg(target_os = "linux")] window_decorations: Some(WindowDecorations::Client),
    #[cfg(target_os = "macos")] kind: WindowKind::Floating,
    app_owns_titlebar_drag: true,
    window_background: if cfg!(target_os = "windows") { Opaque } else { Transparent },
    is_resizable: false,
    focus: !first_run, show: !first_run,
    ..Default::default()
}
```
- On Windows, Cap leaves the window opaque. Transparent windows on Windows are fiddly; Mica is better there.
- The rounded corners come from the root `div().rounded(..)` inside a transparent window.
- Cap's comment notes that their fork carries a fix for destination alpha on transparent windows (Porter-Duff OVER, not additive). **Test translucent edges on gpui-pre early**, because this was a real bug.

**One overlay per display** (`src/app_windows.rs:2789-2875`):
- Each display gets a `WindowKind::PopUp` window with `focus: false, show: false, titlebar: None`, a `Transparent` background and `display_id` set. Linux uses `WindowBounds::Fullscreen`.
- On macOS the native frame and level are set **from a spawned task**, not inside `handle.update`. AppKit calls re-enter GPUI and cause "RefCell already borrowed":
```rust
let native = handle.update(cx, |_, window, _| platform::native_window(window)).ok().flatten();
cx.spawn(async move |cx| {
    if let Some(native) = &native { platform::place_overlay_panel(native, x, y, w, h, level); }
    handle.update(cx, |view, window, cx| { window.focus(&view.focus_handle(), cx); window.refresh(); }).ok();
}).detach();
```

**macOS native helpers worth copying** (`src/platform.rs`):
- Get the `NSWindow` via `HasWindowHandle` → `RawWindowHandle::AppKit` → `ns_view.window()` (around line 1747).
- `place_overlay_panel` (lines 1568-1599):
  - Flips CG→AppKit y and calls `setFrame_display`.
  - Calls `setCollectionBehavior(CanJoinAllSpaces | FullScreenAuxiliary)`, `setLevel`, `setHasShadow(false)`, `setAnimationBehavior(None)` and `orderFrontRegardless()`.
- **`setSharingType(0)`** (`set_window_capture_hidden`) keeps our own HUD out of the recording. **Lookout needs this.** The Windows equivalent is `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` (`platform/windows.rs`).
- `setIgnoresMouseEvents` makes a window click-through. The Windows equivalent is `WS_EX_LAYERED | WS_EX_TRANSPARENT`.
- `lock_borderless_style` (lines 1446-1513) swizzles `setStyleMask:`, because ScreenCaptureKit re-adds titlebar buttons to borderless windows. Copy it if our capture path uses SCK.
- `kick_display_link` (line 1329) works around the display link not starting on macOS 26 for windows that were shown hidden.
- Levels: controls `CGWindowLevelForKey(10)`, overlay = that minus 1. The main window is level 100 with `join_all_spaces`.
- Always-on-top elsewhere (`platform.rs:3118-3189`):
  - Windows: `SetWindowPos(hwnd, HWND_TOPMOST, …, SWP_NOMOVE|SWP_NOSIZE)`.
  - X11: a `_NET_WM_STATE_ABOVE` client message via `x11rb`.
- Native vibrancy: an `NSVisualEffectView` (or `NSGlassEffectView` on macOS 26) is inserted *below* GPUI's Metal view, and the content layer is clipped with `cornerRadius` (`platform::install_window_material`). Combine this with a `Transparent` window background and translucent fills.

**Frame:** a single normal window. Popovers, selects and tooltips are **in-window** `deferred(div().absolute()…occlude()).priority(n)`, with `cx.stop_propagation()` on mouse-down (`frame-app/src/app/components/select.rs:206-235`, `tooltip.rs:170`). Prefer this to OS popup windows for anything inside a window.

---

## 3. Window chrome per platform

| Platform | Pattern | Source |
|---|---|---|
| macOS | Native traffic lights through `TitlebarOptions { appears_transparent: true, traffic_light_position: Some(point(px(x), px(y))) }`, plus a spacer div of about 70–82px in the header. `traffic_light_position` is the literal top-left of the buttons, not an inset like Tauri's (`cap/.../editor_window.rs:100-109`). | Cap `app_windows.rs:1354`, Frame `runtime.rs:82-111` |
| Windows | Custom 46px min/max/close buttons tagged `.window_control_area(WindowControlArea::Min/Max/Close)`, which gives **native Snap Layouts hover** on Max. Close hover colour is `#c42b1c`. | Frame `chrome.rs:2240-2310`, Cap `ui/windows_caption.rs` |
| Linux | `window_decorations: Some(WindowDecorations::Client)`. When `matches!(window.window_decorations(), Decorations::Client{tiling} if !tiled)`, wrap the root in padding + rounded corners + border + shadow, and call `window.set_client_inset(px)`. For resize, use `start_window_resize(ResizeEdge::…)` on edge hit-areas; neither app does this, both use fixed-size or zero-resize windows. Right-click calls `window.show_window_menu(pos)`. | Frame `render.rs:858-916`, gpui-kit `TitleBar` |

**Drag handling: the important pitfall.**
- Starting a native drag on a button's mouse-down swallows its click. Both apps keep drag handlers off the header root and only start the move **after the pointer moves**.
- This is the gpui-kit `TitleBar` pattern (`gpui-component-0.8.1/src/title_bar.rs:355-376`) and the Frame `chrome.rs:167-217` pattern:
```rust
let state = window.use_state(cx, |_, _| DragState { should_move: false });
div().id("drag").flex_1().window_control_area(WindowControlArea::Drag)
  .on_mouse_down(MouseButton::Left, window.listener_for(&state, |s, ev: &MouseDownEvent, window, _| {
      if window.default_prevented() { return; }               // buttons call window.prevent_default()
      if cfg!(target_os = "macos") && ev.click_count == 2 { window.titlebar_double_click(); } else { s.should_move = true; }
  }))
  .on_mouse_up(MouseButton::Left, window.listener_for(&state, |s, _, _, _| s.should_move = false))
  .on_mouse_move(window.listener_for(&state, |s, _, window, _| if s.should_move { s.should_move = false; window.start_window_move(); }))
```
- Set `app_owns_titlebar_drag: true` on macOS for any window that draws its own titlebar. Otherwise AppKit also drags and delays clicks.

---

## 4. Animation

### 4.1 Raw GPUI (gpui-pre 0.3.10, `elements/animation.rs`, `spring.rs`)

```rust
pub struct Animation { pub duration: Duration, pub oneshot: bool, pub synced: bool,
                       pub easing: Rc<dyn Fn(f32) -> f32>, pub max_fps: Option<f32> }
impl Animation {
    pub fn new(duration: Duration) -> Self;                 // linear, oneshot
    pub fn repeat(self) -> Self;
    pub fn repeat_synced(self) -> Self;                     // phase-locked to an app clock (spinners stay in sync)
    pub fn with_easing(self, easing: impl Fn(f32) -> f32 + 'static) -> Self;  // output NOT clamped (overshoot ok)
    pub fn with_max_fps(self, max_fps: f32) -> Self;        // throttle decorative loops (e.g. 30fps pulse)
}
pub trait AnimationExt {   // impl for every IntoElement + 'static
    fn with_animation(self, id: impl Into<ElementId>, animation: Animation, animator: impl Fn(Self, f32) -> Self + 'static) -> AnimationElement<Self>;
    fn with_animations(self, id, animations: Vec<Animation>, animator: impl Fn(Self, usize, f32) -> Self + 'static) -> AnimationElement<Self>;
    fn with_spring<T: SpringTarget>(self, id, animation: SpringAnimation<T>, animator: impl FnOnce(Self, T::Output) -> Self + 'static) -> SpringAnimationElement<Self>;
}
// easing fns (gpui::*): linear, quadratic, ease_in_out (quadratic), ease_out_quint() -> impl Fn, bounce(f), pulsating_between(min,max)
pub struct SpringConfig { pub stiffness: f32, pub damping: f32 /* coefficient c, not ratio */, pub mass: f32 }
SpringConfig::new(k, c, m); .step(state, target, dt) -> SpringState; .is_settled(..); .settle_time(..)
SpringAnimation::new(config).to(target).from(initial).with_epsilon(e).playback(SpringPlayback::{Running,Paused,Stopped,Completed,Cancelled})
SpringTarget is implemented for f32, Pixels, Rems, bool (-> AnimationPhase, use .interpolate(from,to))
pub fn sampled_easing(config, epsilon) -> (Duration, impl Fn(f32)->f32)   // spring as easing curve for with_animation
window.request_animation_frame()   // notifies current view next frame
cx.reduce_motion() / cx.set_reduce_motion(bool)    // with_animation honours it automatically
window.use_keyed_state(key, cx, |window, cx| S) -> Entity<S>; window.use_state(cx, init)  // #[track_caller]
```

**Pitfalls:**
- `with_animation` **restarts when the element ID changes and only then**. To replay an animation, put a generation counter or the state into the ID, for example `("toast-in", gen)` or `if open {"in"} else {"out"}`. Cap does this everywhere (`ui/hint.rs:511-540`).
- `with_animation` is fire-and-forget. A reversal mid-flight jumps. For anything the user can toggle quickly (hover, open/close, selection), use a value transition or spring (§4.2).
- A `with_animation` that runs for a long time keeps requesting frames, which costs idle CPU in a tray app. Use `with_max_fps(30.)` for decorative loops such as the recording dot pulse.
- When you call `request_animation_frame()` yourself, stop calling it once the value has settled, and skip it under `cx.reduce_motion()`.

### 4.2 gpui-kit's motion layer: use this

All of the following are in `gpui_kit::base`, from `gpui-base-0.8.1/src/motion.rs` and `motion/*`. The design doc is `/home/anson/.t3/ref/gpui-kit/docs/STYLING-AND-MOTION.md`.

```rust
pub fn transition<T: Interpolate + PartialEq + 'static>(id: impl Into<TransitionId>, target: T, policy: Transition, window: &mut Window, cx: &mut App) -> T;
pub fn transition_with_status<T>(..same..) -> MotionValue<T>;   // { value, status: Idle|Delayed|Running|Finished }
Transition::new(Duration)   // default easing: ease_out_cubic
    .delay(impl Into<SignedDuration>)
    .ease(impl Fn(f32)->f32 + 'static)
    .easing(Easing)
Easing::{Linear, Ease, EaseIn, EaseOut, EaseInOut, CubicBezier{..}, Steps{..}, LinearStops(..), Custom(Rc<..>)}
Easing::cubic_bezier(x1,y1,x2,y2) -> Result<Easing, EasingError>   // y may overshoot; true CSS solver
gpui_base::animation::cubic_bezier(x1,y1,x2,y2) -> impl Fn(f32)->f32   // clamped, for Animation::with_easing

pub fn spring<T: SpringTarget>(id, target: T, policy: Spring, window, cx) -> T::Output;
Spring::new(response: Duration)        // critically damped (ratio 1.0), no overshoot
    .with_damping(ratio /* zeta, <1 overshoots */)
    .with_epsilon(e /* target units; use ~0.1–0.5 for px */)
    .with_travel(bool /* false while dragging: value follows pointer exactly */)

Presence::new(id, present: bool).transition(Transition).sample(window, cx) -> PresenceSample { phase, progress, status }
    sample.should_render()   // keep mounted during exit, unmount after
Sequence::new(id, from).with_step(target, Transition).sample(window, cx)   // chained steps; put a generation in id to replay
Stagger::new(step: Duration, StaggerOrigin::First).delay(index, count)
Keyframes<T>, animate_keyframes(..), Discrete<T>, MotionTransform { translation, scale, rotation_radians, opacity } (Interpolate)
Interpolate impls: anything Lerp (f32, Pixels, Hsla…), Size<Pixels>, Bounds<Pixels>, MotionTransform
```

What this layer gives us:
- **Smooth reversal.** A retargeted transition restarts from the currently sampled value. A spring also keeps the velocity.
- **Reduced motion.** `gpui_base::init` (called by `gpui_kit::init`) reads the OS reduce-motion preference into `cx.set_reduce_motion`.
- **Allocation-free sampling** (under 0.1 ms per 1,000 samples).
- **Product tokens** in `gpui_kit::component::theme::MotionTokens`, defined in `gpui-component-0.8.1/src/theme/motion.rs`:
  - durations 0 / 120 / 180 / 280 ms
  - `easing_enter = cubic_bezier(0.16,1,0.3,1)`, `easing_exit = (0.4,0,1,1)`, `easing_move = (0.2,0,0,1)`
  - `spring_control = Spring::new(180ms)`, `spring_move = Spring::new(280ms).with_damping(0.85).with_epsilon(0.1)`

These are exactly the "CSS-like transition with cubic-bezier" and "spring helper" we planned. **Do not write our own.**

### 4.3 Animation patterns from the reference apps (all portable to gpui-pre)

- **Cap `ui/glass_switch.rs`.** An `Instant` stored in `use_keyed_state`. Progress is computed in `render`, and the view keeps calling `window.request_animation_frame()` while `progress < 1`. Pressed state is tracked with `on_mouse_down` / `on_mouse_up` / `on_mouse_up_out`. This is what `gpui_base::transition` does internally, so prefer the library.
- **Cap staggered entrances** (`editor_window.rs:1714-1735`, `onboarding_window.rs:1402`):
  - One long `Animation` (for example 620 ms) drives several children.
  - Each child maps the shared delta into its own delayed window: `((delta*total - delay - i*stagger) / dur).clamp(0,1)`.
  - Then it applies `ease_out_back` for overshoot: `1 + c3*x³ + c1*x²` with `c1 = 1.35`, `c3 = c1+1`, `x = t-1`.
  - The base equivalent is `Stagger` + `Sequence`.
- **Cap `ui/collapsible.rs:112-133`.** The natural height is measured with a `canvas(|bounds,..| cell.set(bounds.size.height), |..|{})` probe, then animated from the current height when interrupted. The base equivalent is `MotionReveal` / `Collapsible::motion_id`.
- **Frame `app/motion.rs`.** Copy two ideas from it:
  1. `mix_rgba(from, to, p)` mixes in **premultiplied** space, so a hover from transparent to a colour doesn't flash through black or grey.
  2. Popovers use the enum `PopoverState { Hidden, Closing, Open }`. While `Closing`, the popover stays rendered with `opacity(p)` and `top(54 + (1-p)*4px)`. Once `p≈0`, `cx.defer_in(window, |this,_,cx| this.finish_close(cx))` flips it to `Hidden`. `Presence` implements the same thing.
- **Frame icon swap** (`components/button.rs:354-372`). Two absolutely positioned SVGs crossfade:
  - opacity `p`
  - `svg().with_transformation(Transformation::scale(size(s,s)))` with `s = 0.25+0.75p`
  - Frame also uses `.blur((1-p)*4px)`, but **gpui-pre has no element blur**, so drop that part.
  - This is the single best "premium" micro-interaction that works on gpui-pre, because SVG transforms are supported.
- **Frame frame-rate-independent smoothing** (`preview_actions.rs:2547-2564`), useful for scrub and zoom follow:
  `factor = 1 - (1 - 0.2).powf(elapsed / 16ms); current = (target - current) * factor + current`. A spring with `with_travel(!dragging)` is the base equivalent.
- **Timer loops.** Use `cx.spawn(async move |this, cx| loop { cx.background_executor().timer(16ms).await; this.update(cx, ..)?; })` only for non-visual polling (for example the recording clock at 1 Hz). Give each loop a generation counter so stale loops exit (Cap `editor_window.rs:3497-3519`). Frame's loops stop themselves when idle and `notify()` only on change.

### 4.4 Press-scale without transforms (gpui-pre limitation)
- Neither app has a true press-scale; Cap's README says there are no transforms on its rev either. Options on gpui-pre 0.3.10:
  1. **Inset background layer.** The button is a fixed-size `relative()` box. Its fill and border are an `absolute()` child with `inset(px(k))`, where `k` is a spring from 0 to about 1.5px while pressed. The label stays put. This reads as a ~97% scale for buttons 28–40px tall.
  2. **Icon-only buttons.** Scale the `svg()` with `Transformation::scale`, which is a real transform.
  3. A colour/opacity change via `.active(|s| s.bg(..))` or `group_active`. This is GPUI-native and costs nothing.
- Track the pressed flag in `window.use_keyed_state(format!("{id}-pressed"))`.
- Release on `on_mouse_up`, **`on_mouse_up_out`** and `capture_any_mouse_up`, or the button sticks pressed when the user drags off it (Frame `primitives.rs:100-185`).
- Call `window.prevent_default()` in the button's mouse-down so the titlebar drag logic ignores it.

---

## 5. Theming, fonts, icons

**Theming**
- Cap (`src/theme.rs`): a flat `Copy` struct of Radix steps (`gray_1..12`, `blue_*`, …) plus a `material` sub-palette of translucent fills for vibrancy windows. Shadows are stacked `BoxShadow` presets, for example `0 1px 2px α.12` plus a 0.5px spread hairline.
- Cap follows the system theme via `cx.observe_window_appearance(window, |_,_,cx| cx.notify())` + `window.appearance()` (`Theme::bind_window`).
- Frame (`frame-app/src/theme.rs:66-238`): tokens are written in **OKLCH**, converted once into a `LazyLock` palette. Dark neutrals are alpha tints of the primary text colour. Tests check WCAG contrast for every role on every surface (`theme.rs:533-580`). Radii derive from `RADIUS_BASE = 3.6`.
- For Lookout, use gpui-kit's `Theme` (`cx.theme()`) as the base. Load a custom theme JSON or override tokens instead of building a parallel theme system. Add a small `LookoutTokens` for brand colours and recording-red. Frame's OKLCH-token-plus-contrast-test idea is cheap to copy.

**Fonts.** Both apps embed fonts with `include_bytes!` and load them before opening any window:
```rust
cx.text_system().add_fonts(vec![Cow::Borrowed(INTER_REGULAR), Cow::Borrowed(INTER_MEDIUM)])?;  // text_system.rs:302
```
- Cap uses `".SystemUIFont"` on macOS/Windows and its bundled Geist on Linux, plus `FontFallbacks` per locale for CJK.
- Frame turns on `tnum` (tabular numbers) for timecodes via font features. **Use `tnum` for the recording timer** so the digits don't jitter.

**Icons**
- Use `svg().path("icons/x.svg").size(..).text_color(..)`. GPUI keeps **only the alpha channel** and tints it, so icons must be single-colour with `currentColor` strokes.
- For multi-colour artwork, rasterise with `resvg` into a `RenderImage` (Cap `editor_sidebar/cursor.rs`) or use `img()`.
- gpui-kit-assets ships Lucide. `Assets` embeds 101 icons; `AllAssets` embeds 1,830 and adds +1 MiB. Provide our own `AssetSource` that falls back to `gpui_kit::assets::Assets` for the extra icons.
- Cap has a test that scans the sources so every referenced icon path is guaranteed to exist. Copy it.
- Minimal embedded `AssetSource` (Cap `src/assets.rs`):
```rust
impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ASSETS.iter().find(|(n, _)| *n == path).map(|(_, b)| Cow::Borrowed(*b)))
    }
    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ASSETS.iter().filter(|(n, _)| n.starts_with(path)).map(|(n, _)| (*n).into()).collect())
    }
}
```

---

## 6. Tray / menu bar and global hotkeys

- GPUI (including gpui-pre 0.3.10) has **no status-item or tray API**. The only related calls are `cx.set_menus`, `cx.set_dock_menu`, `set_activation_policy(Accessory)` and `QuitMode::Explicit`.
- Cap's approach:
  - macOS: hand-built `NSStatusItem` + `NSMenu` with `objc2` `define_class!` (`src/tray.rs:1015-1240`), using template PNGs.
  - Windows: `tray-icon 0.21` (`src/tray/windows.rs`).
  - Linux: no tray at all (stub).
- Frame has no tray.

**The threading rule** (Cap `tray.rs:8-14`):
- NSMenu actions fire inside AppKit's nested menu-tracking run loop, where GPUI's `App` RefCell may already be borrowed.
- So OS callbacks **only send to a channel**, and a GPUI foreground task drains it:
```rust
let (tx, rx) = flume::unbounded::<TrayEvent>();
install_native_tray(tx);   // objc target / tray-icon MenuEvent::set_event_handler / ksni callback → tx.send(..)
cx.spawn(async move |cx| {
    while let Ok(ev) = rx.recv_async().await { cx.update(|cx| handle_tray_event(ev, cx)).ok(); }
}).detach();
```
Cap's `global-hotkey 0.7` setup (`hotkeys.rs:175-202`) follows the same pattern.

**Recommendation for Lookout:**

| OS | Crate / approach | Notes |
|---|---|---|
| macOS | Reuse `clients/desktop/src-tauri/swift/lookout-tray` (SwiftUI status item with `numericText` digit animation) via `swift-rs`. Fallback: `objc2-app-kit` `NSStatusItem` as Cap does. | GPUI owns `NSApp` on the main thread, so the status item just works. `cx.set_activation_policy(ActivationPolicy::Accessory)` + `QuitMode::Explicit` gives a menu-bar-only app. |
| Windows | `tray-icon` (+ `muda` menus) | GPUI's Win32 loop pumps messages on the main thread, which is all tray-icon needs. Create it after `run()` starts. |
| Linux | `ksni` (StatusNotifierItem over D-Bus, pure Rust, own thread) | `tray-icon` on Linux needs a running GTK main loop, and GPUI doesn't run one. Avoid it there. GNOME needs the AppIndicator extension; show the main window when no tray host exists. |

- The tray popover window can be `WindowKind::PopUp` (or `Floating`), borderless, `Transparent`, positioned under the status item rect.
- It should hide on `observe_window_activation` loss.

**Other app-shell items from Cap:**
- `single_instance.rs`: a pidfile/lock on macOS and Linux, a named mutex on Windows, forwarding over a localhost port.
- `on_reopen` handles Dock clicks and `on_open_urls` handles deep links.
- `store.rs`: atomic JSON settings, written as temp file + rename on a background thread. Frame also fsyncs the directory and uses `MoveFileExW` with `WRITE_THROUGH`.

---

## 7. Rendering capture previews and timelapse frames

**APIs (gpui-pre 0.3.10)**
```rust
pub struct RenderImage { pub id: ImageId, .. }     // BGRA data despite wrapping image::RgbaImage frames
RenderImage::new(data: impl Into<SmallVec<[image::Frame; 1]>>) -> Self   // fresh ImageId every call
img(source: impl Into<ImageSource>) -> Img
enum ImageSource { Resource(Resource), Render(Arc<RenderImage>), Image(Arc<Image>), Custom(Arc<dyn Fn(&mut Window,&mut App)->Option<Result<Arc<RenderImage>,ImageCacheError>>>) }
window.paint_image(bounds, image_bounds, corner_radii: Corners<Pixels>, data: Arc<RenderImage>, frame_index: usize, grayscale: bool) -> Result<()>  // paint phase only
window.drop_image(data: Arc<RenderImage>) -> Result<()>          // evict from THIS window's atlas
cx.drop_image(image: Arc<RenderImage>, current_window: Option<&mut Window>)  // all windows
canvas(prepaint: FnOnce(Bounds,&mut Window,&mut App)->T, paint: FnOnce(Bounds,T,&mut Window,&mut App))
#[cfg(macos)] surface(source: impl Into<SurfaceSource>)   // From<CFRetained<CVPixelBuffer>>
#[cfg(macos)] window.paint_surface(bounds, image_buffer: CFRetained<CVPixelBuffer>)
entity.clone().cached(StyleRefinement::default().size_full())   // view caching: only re-renders on notify
```

**The recipe**, combining Cap `editor_window.rs:609-643, 7098-7104, 11090, 13272-13291` and Frame `preview_engine/frame_store.rs`, `preview_actions.rs:656-684`:
1. **Producer thread** (capture or decoder) converts to tight BGRA in a non-generic `opt-level=3` kernel crate. Cap's `kernels::frame::unpad_rgba_to_bgra` uses AVX2/SSSE3.
2. It hands off through a **latest-frame mailbox**: `Arc<Mutex<{generation, latest: Option<Arc<Frame>>}>>` (Frame), or a bounded `flume` channel with `try_send` that drops when full (Cap). Count the frames that get overwritten.
3. Build `Arc<RenderImage>` once per frame: `RenderImage::new(smallvec![image::Frame::new(RgbaImage::from_raw(w, h, bgra)?)])`. Pin the `image` crate version to the one GPUI uses, or `image::Frame` will be a different type.
4. **Swap and drop.** Every replaced image must be evicted. This is the #1 leak, and Cap does it in about 25 places:
```rust
if let Some(prev) = self.frame.replace(new_image) { let _ = window.drop_image(prev); }   // or cx.drop_image(prev, None)
```
   If the same image appears in several windows, use `cx.drop_image`. `window.drop_image` only clears the calling window.
5. **Isolate repaints.** The preview is its own `Entity<PreviewView>`, rendered as `self.preview.clone().cached(StyleRefinement::default().size_full())`. A new frame calls `preview.update(cx, |p, cx| { p.set(..); cx.notify() })`, so only the small view re-renders, not the whole window.
6. Paint inside `canvas(..)` with `window.paint_image(bounds, fitted_bounds, corners, image, 0, false)`. Compute the aspect fit yourself; use `img(...).object_fit(ObjectFit::Contain)` for simple cases.
7. **macOS zero-copy.** If the capture produces IOSurface-backed `CVPixelBuffer`s (ScreenCaptureKit does), `surface(pixel_buffer)` skips both the CPU conversion and the atlas.
   - Cap needed a fork patch to add 32BGRA to `paint_surface`. **Verify which pixel formats gpui-pre 0.3.10's Metal surface path accepts** (likely only 420v/YUV biplanar). If BGRA is rejected, ask SCK for `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`.
8. **Thumbnails and timelapse scrubber.** Decode once at thumbnail size into a per-view cache keyed by frame index. Evict with `drop_image` when it scrolls out of the cache window.
   - With stock gpui-pre, every new `RenderImage` gets a new `ImageId` and a new atlas tile. Frame's patch reuses a tile per stable id (`RenderImage::new_with_id(id, version, frames)`), which gpui-pre does **not** have. Don't call it.

Cap's `StreamingImage` patch (`patches/zed-streaming-image.patch:174-540`) shows the ideal design: one dedicated texture per stream, outside the atlas, re-uploaded only when its generation changes. If live-preview CPU or GPU becomes a problem on Windows or Linux, this is the upstream patch to propose or carry.

---

## 8. gpui-fast: what it is and whether to use it

- **What it is:** `longbridge/gpui-fast` (crates `gpui-fast`, `gpui-fast-platform`; 0.1.13 is the latest, gpui-kit 0.8.1 uses `0.1.12`). It's a fork of Zed's GPUI with:
  - **Retained mode:** unchanged views, layout nodes and text measurements are reused between frames.
  - **Scroll layers:** cached GPU tiles.
  - **Native window composition:** GPUI overlays render above embedded native views such as webviews.
- **How to enable it:** `gpui-kit = { version = "0.8.1", features = ["gpui-fast"] }`. Imports, init and test macros stay the same, and no `[patch]` is needed.
- **Benchmarks** (their README, against gpui-pre 0.3.7):
  - spinner animating: 5.91 ms / 85% CPU → 0.73 ms / 11%
  - list scrolling: 2.66 ms / 39% → 0.39 ms / 6%
  - gpui-kit story gallery: 37% less scroll CPU on average
- **Contract:** anything `render` reads must live in entities, globals or list/scroll state, or be `cx.notify()`'d. That rules out `Rc<RefCell>` outside entities and reading `Instant::now()` without a frame request.
  - Writing to an entity or global during prepaint/paint, or on every frame, defeats retention.
  - If something looks stale, debug with `GPUI_VIEW_RETENTION=0`.
- **Verdict:** worth it for Lookout, which spends hours idle in the tray while recording with a pulsing indicator and a ticking clock.
  - Plan for it from day one by following the contract above; it's good GPUI hygiene anyway.
  - Develop on default gpui-pre, CI-build both backends, and switch once the HUD and preview have been checked on all three OSes. It's labelled experimental.

---

## 9. Pitfalls checklist

- **Re-entrancy (macOS).** Never call AppKit APIs that may call back into GPUI (`setFrame`, `orderFront`, menus) from inside `handle.update`/`render`. Defer with `cx.spawn` / `cx.defer` (Cap §2).
- **Hidden windows.** The display link may not start on macOS 26 for windows created hidden. Cap's `kick_display_link` works around it.
- **Atlas leaks.** Pair every image replacement with `drop_image`.
- **Unoptimised dev builds.** Set `[profile.dev.package."gpui-pre"] opt-level = 3`, and the same for `taffy`, `gpui-base`, `gpui-component`, `image` and our kernels crate.
- **IDs.** Repeated elements need IDs derived from the domain (session id, frame index), not list position. `with_animation` replays only when its ID changes.
- **Pressed state.** Release it on `on_mouse_up_out` too.
- **Window dragging.** Start the drag on mouse-move, not mouse-down. Buttons call `window.prevent_default()`.
- **Transparent windows.** Check edge alpha on gpui-pre (Cap needed an OVER-blend fix). Windows looks better with `MicaBackdrop`/`Opaque` plus rounded DWM corners than with true transparency.
- **No transforms or blur on divs in gpui-pre 0.3.10.** Only `svg().with_transformation(..)` and box-shadow `blur_radius`. Design around opacity, translation (via `left/top/mt`), size and colour.
- **Idle CPU.** `WindowOptions::inactive_frame_interval` defaults to 30 fps throttling when the window is inactive. Use `Animation::with_max_fps` for decorative loops, and hide or close windows instead of leaving invisible always-on windows animating.
- **Reduce motion.** `gpui_kit::init` reads the OS reduce-motion setting. Custom `request_animation_frame` loops must check `cx.reduce_motion()`.

---

## 10. Recommended structure for `clients/native`

```
clients/native/
  Cargo.toml                 # workspace; gpui-kit = "=0.8.1" (+ optional "gpui-fast" feature passthrough)
  crates/
    lookout-native/          # the app binary
      src/
        main.rs              # runtime (tokio handle -> spawn via cx.background_executor or gpui_tokio-like bridge),
                             #   fonts, assets, gpui_kit::init, single-instance, tray, hotkeys, open main window
        app_state.rs         # Global/Entity: recorder session (wraps lookout-core), settings, auth
        windows/             # one module per window: main.rs, hud.rs (recording pill, PopUp), tray_panel.rs,
                             #   area_select.rs (per-display PopUp / Wayland LayerShell), settings.rs, onboarding.rs
        chrome/              # titlebar.rs (macOS spacer / Windows caption buttons / Linux CSD), drag.rs
        ui/                  # our components on top of gpui_kit::component: button.rs (inset press), icon_swap.rs,
                             #   segmented.rs, timer_text.rs (tnum), preview.rs (cached view + canvas paint_image)
        motion.rs            # Lookout motion tokens + helpers over gpui_kit::base (below)
        theme.rs             # LookoutTokens on top of cx.theme(); OKLCH constants; contrast tests
        platform/            # mod.rs + macos.rs (objc2: levels, sharingType=0, spaces, vibrancy), windows.rs
                             #   (WDA_EXCLUDEFROMCAPTURE, TOPMOST, DWM), linux.rs (x11rb above, LayerShell)
        tray/                # mod.rs (channel + drain task), macos.rs (swift-rs lookout-tray), windows.rs (tray-icon), linux.rs (ksni)
        assets.rs            # include_bytes! table + fallback to gpui_kit::assets::Assets; icon-exists test
    lookout-kernels/         # non-generic pixel kernels (BGRA swizzle, downscale), opt-level=3 even in dev
```

`motion.rs` sketch. It wraps `gpui_base`, so it inherits reversal, reduced motion and frame requests:
```rust
use std::time::Duration;
use gpui_kit::{App, Window, base::{self, Easing, Spring, Transition, TransitionId}};

pub const FAST: Duration = Duration::from_millis(120);
pub const NORMAL: Duration = Duration::from_millis(180);
pub const SLOW: Duration = Duration::from_millis(280);

pub fn ease_out() -> Easing { Easing::cubic_bezier(0.23, 1.0, 0.32, 1.0).unwrap() }      // strong ease-out (UI enter)
pub fn ease_in_out() -> Easing { Easing::cubic_bezier(0.77, 0.0, 0.175, 1.0).unwrap() }  // on-screen movement
pub fn ease_exit() -> Easing { Easing::cubic_bezier(0.4, 0.0, 1.0, 1.0).unwrap() }

pub const PRESS: Spring = Spring::new(Duration::from_millis(160));                        // no overshoot
pub const BOUNCY: Spring = Spring::new(Duration::from_millis(320)).with_damping(0.75);   // pill/HUD morphs
pub const PX: Spring = Spring::new(Duration::from_millis(280)).with_epsilon(0.25);       // pixel-valued springs

/// CSS `transition: <prop> <dur> cubic-bezier(..)` for one value.
pub fn tween<T: base::Interpolate + PartialEq + 'static>(id: impl Into<TransitionId>, target: T, dur: Duration, ease: Easing, window: &mut Window, cx: &mut App) -> T {
    base::transition(id, target, Transition::new(dur).easing(ease), window, cx)
}
/// 0..1 open progress that keeps rendering through the exit; returns None when fully hidden.
pub fn presence(id: impl Into<TransitionId>, open: bool, window: &mut Window, cx: &mut App) -> Option<f32> {
    let s = base::Presence::new(id, open).transition(Transition::new(if open { NORMAL } else { FAST }).easing(if open { ease_out() } else { ease_exit() })).sample(window, cx);
    s.should_render().then_some(s.progress)
}
```
- The curves follow `design-guidance.md`. The `0.23,1,0.32,1` and `0.77,0,0.175,1` values are Emil Kowalski's.
- Before relying on the sketch, note that it has not been compiled yet. The `Presence` builder (`new(id, present).transition(Transition).sample(window, cx)`) and the `const fn` `Spring` chain (`new`, `with_damping`, `with_epsilon`, `with_travel`) were checked against the gpui-base 0.8.1 source.

Usage example, a hover background on a recording-list row:
```rust
let hovered = *window.use_keyed_state(("row-hover", id), cx, |_, _| false).read(cx);
let bg = motion::tween(("row-bg", id), if hovered { theme.row_hover } else { theme.row }, motion::FAST, motion::ease_out(), window, cx);
div().id(("row", id)).bg(bg).on_hover(/* set keyed state + notify */)
```
For colour transitions, mix in premultiplied space (Frame `mix_rgba`, `frame-app/src/app/motion.rs`). Implement `Interpolate` for a `PremulRgba` newtype to avoid muddy fades through transparent.

---

## 11. File index (where to look for copyable code)

| Topic | File |
|---|---|
| Transparent floating main window | `/tmp/research/cap/apps/desktop-gpui/src/main.rs:425-602` |
| Per-display overlays, native placement | `/tmp/research/cap/apps/desktop-gpui/src/app_windows.rs:2789-2875`, `src/platform.rs:1446-1800, 3118-3189` |
| Capture-exclusion, click-through (Windows) | `/tmp/research/cap/apps/desktop-gpui/src/platform/windows.rs` |
| Tray (macOS objc2 / Windows tray-icon) | `/tmp/research/cap/apps/desktop-gpui/src/tray.rs`, `src/tray/windows.rs` |
| Hotkeys | `/tmp/research/cap/apps/desktop-gpui/src/hotkeys.rs:175-202` |
| Animated switch + press state | `/tmp/research/cap/apps/desktop-gpui/src/ui/glass_switch.rs` |
| Staggered overshoot entrance | `/tmp/research/cap/apps/desktop-gpui/src/editor_window.rs:1714-1735` |
| Measured-height collapse | `/tmp/research/cap/apps/desktop-gpui/src/ui/collapsible.rs:112-133` |
| Frame → RenderImage + drop_image + cached preview | `/tmp/research/cap/apps/desktop-gpui/src/editor_window.rs:609-643, 7098-7104, 11090, 13272-13291` |
| SIMD kernels crate | `/tmp/research/cap/apps/desktop-gpui/kernels/` |
| Windows caption buttons | `/tmp/research/cap/apps/desktop-gpui/src/ui/windows_caption.rs`, `/tmp/research/frame/frame-app/src/app/chrome.rs:2240-2310` |
| Theme (Radix steps, material, shadows) | `/tmp/research/cap/apps/desktop-gpui/src/theme.rs` |
| Motion helpers, premultiplied colour mix | `/tmp/research/frame/frame-app/src/app/motion.rs` |
| Button hover/press motion | `/tmp/research/frame/frame-app/src/app/primitives.rs:37-185`, `components/button.rs:354-372` |
| Titlebar drag-after-move | `/tmp/research/frame/frame-app/src/app/chrome.rs:167-217` |
| Linux CSD wrapper | `/tmp/research/frame/frame-app/src/app/render.rs:858-916` |
| Timeline scrubber (drag, measure) | `/tmp/research/frame/frame-app/src/app/preview_panel/timeline.rs:281-468` |
| Latest-frame mailbox | `/tmp/research/frame/frame-app/src/preview_engine/frame_store.rs` |
| OKLCH theme + contrast tests | `/tmp/research/frame/frame-app/src/theme.rs` |
| OS reduce-motion polling (3 OSes) | `/tmp/research/frame/frame-app/src/app/system_motion.rs` (gpui_base already does this) |
| Atomic settings persistence | `/tmp/research/frame/frame-app/src/app_persistence.rs:394-487` |
| gpui-kit motion design doc | `/home/anson/.t3/ref/gpui-kit/docs/STYLING-AND-MOTION.md` |
| gpui-kit TitleBar | `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-component-0.8.1/src/title_bar.rs` |
| gpui-base motion | `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-base-0.8.1/src/motion.rs`, `motion/` |
