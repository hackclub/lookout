# Vendored GPUI crates

These are the crates.io releases of `gpui-pre`, `gpui-pre-wgpu`, `gpui-pre-macos`,
`gpui-pre-windows` and `gpui-pre-apple` **0.3.10**, wired in with
`[patch.crates-io]` in `../Cargo.toml`. They're patched so video frames can be
drawn as GPU surfaces on every platform (upstream only implements
`PaintSurface` on macOS, with square corners and a BT.601 matrix). The video
player (`crates/lookout-native/src/player`, `ui/video.rs`) depends on this.

Every change is marked with a `Lookout patch` comment. `examples/` was dropped
from `gpui-pre` (5 MB, not needed), and so were its `[[example]]` entries in
`Cargo.toml`.

## gpui-pre

- `scene.rs`: `PaintSurface` gains `corner_radii` (all platforms) and, off
  Apple platforms, `frame: Arc<SurfaceFrame>`. New types `SurfaceFrame`,
  `SurfaceFormat` (NV12 / BGRA), `YuvColor`, `SurfaceSourceId`.
- `window.rs`: `Window::paint_surface_frame(bounds, corner_radii, frame)`
  (non-Apple) and `Window::paint_surface_rounded(bounds, corner_radii,
  buffer)` (Apple). The existing `paint_surface` sets zero radii.
- `app.rs`: `App::hide_cursor_until_mouse_moves()` exposes the platform call
  GPUI already uses on typing (for the idle cursor over full-screen video).

## gpui-pre-wgpu (Linux)

- `shaders.wgsl`: the dormant surface pipeline gets CPU-supplied YCbCr→RGB
  rows (any matrix/range), a BGRA mode, and quad-SDF rounded corners.
- `wgpu_renderer.rs`: `prepare_surfaces` keeps one texture set per
  `SurfaceSourceId` and re-uploads only when the frame's `generation`
  changes; `PrimitiveBatch::Surfaces` draws them. Unused sources are freed
  each frame.

## gpui-pre-windows

- `directx_renderer.rs`: `draw_surfaces` was a no-op. Surfaces now get a BGRA
  texture per source (updated in place with `UpdateSubresource`) and draw
  through the existing polychrome-sprite pipeline: one sprite instance per
  surface is appended to the sprite buffer, so no new HLSL is needed and
  rounded corners come for free. Textures are dropped on device loss.

## gpui-pre-apple (macOS)

- `metal_renderer.rs`: `SurfaceBounds` gains `corner_radii`; the surfaces
  buffer is also bound to the fragment stage.
- `shaders.metal`: `surface_fragment` applies the quad SDF for rounded
  corners, and the colour matrix is BT.709 (was BT.601) since Lookout's only
  surface source is H.264 video via `AVPlayerItemVideoOutput` (`420f`).
  Only compiled on a Mac (the cross-check stubs it), so CI is the first
  compile.

## gpui-pre-macos

The crates.io 0.3.10 release, vendored only for this patch:

- `display_link.rs`: when `CVDisplayLinkCreateWithCGDisplay` fails for the
  window's display (-6661 while displays reconfigure, LOOKOUT-NATIVE-17),
  `WindowFrameSource::start` paces the window with a 60 Hz main-queue
  dispatch timer instead of leaving it with no frames (frozen until its next
  screen or occlusion change), and warns once instead of logging an error.
  The next `start` (resize, activation, screen change) tries the link again;
  `stop` cancels the timer.

## Upgrading GPUI

Re-copy the new release into each directory, then re-apply the hunks above
(`grep -rn "Lookout patch" vendor/`). If upstream implements surfaces itself,
drop the patch and switch `ui/video.rs` to its API.

## Edge fades (scroll masks)

- `gpui-pre/src/scene.rs`: `EdgeFade` and `Scene::push_edge_fade` /
  `pop_edge_fade`. While a fade is active, `insert_primitive` emits any
  primitive that overlaps a fade zone as one-device-pixel strips, each clipped
  to its band with the ordinary rectangular content mask and its opacity scaled
  (`Primitive::scale_opacity`), plus the unfaded remainder. `replay` inserts
  recorded primitives unfaded (they were split when first painted). Surfaces
  (video) are never faded. No GPU struct or shader changes.
- `gpui-pre/src/window.rs`: `Window::with_edge_fade(bounds, top, bottom, f)`,
  and `with_edge_fade_x(bounds, left, right, f)` for horizontal fades
  (`EdgeFade` gains `left`/`right`; `Scene::insert_faded_x` splits into
  one-device-pixel columns). Used by the editor timeline when zoomed.
- Used by `crates/lookout-native/src/ui/primitives.rs` (`edge_faded`) for the
  gallery and source-list fades, the Tauri app's CSS `mask-image`: content goes
  transparent, so vibrancy and Mica show through.

## Deferred window callbacks after close

- `gpui-pre/src/window.rs`: the `on_active_status_change` and
  `on_visibility_change` handlers ignore a missing window (`.ok()` instead of
  `.log_err()`). On Windows both are delivered from spawned tasks
  (`gpui-pre-windows/src/events.rs`), and closing an active window sends a
  final `WM_ACTIVATE`, so the task can run after `remove_window`. Upstream
  logs "window not found" at error level, which Sentry reported as an issue.
- Same for every other platform callback `Window::new` installs (frame
  requests, input, resize/move, appearance, hover, tab and a11y callbacks):
  `.ok()` instead of `.log_err()` on `handle.update`. macOS and Windows deliver
  frame requests and input after the window closed, or while it's on the
  update stack (nested event loops), which was LOOKOUT-NATIVE-19/-Y/-D.

## Windows robustness (LOOKOUT-NATIVE-9 / -A, testers' freezes and exits)

- `gpui-pre-windows/src/dispatcher.rs`: a failed `PostMessageW` wake-up left
  `wake_posted` set, so nothing ever woke the main thread again and every
  foreground task stalled (frozen app). Now it's cleared so the next dispatch
  retries; the failure is logged once, and not at all after shutdown begins
  (`SHUTTING_DOWN`).
- `gpui-pre-windows/src/platform.rs`: the hidden platform window ignores
  `WM_CLOSE` (DefWindowProc would destroy it, killing the task queue) and
  logs an unexpected `WM_DESTROY`; the vsync thread skips windows that are
  already destroyed but still tracked, and is stopped as soon as the message
  loop ends; `quit()`, the end of the loop and `WM_ENDSESSION` are logged.
- `gpui-pre-windows/src/events.rs`: logs `WM_CLOSE` on a window.
- `gpui-pre/src/app.rs`: logs "last window closed, quitting".
- `gpui-pre-windows/src/window.rs`: `cursor_screen_position()` replaces the
  bare `GetCursorPos` in `mouse_position` (and in
  `direct_manipulation.rs`). It fails with ACCESS_DENIED while another
  desktop has input (UAC, lock screen, Ctrl+Alt+Del); upstream logged that at
  error level (Sentry LOOKOUT-NATIVE-9) and returned (0, 0). Now it returns
  the last position read and logs once at debug.

## Re-entrant callbacks while the App is borrowed (LOOKOUT-NATIVE-Z, -10, -1E/F/G, -7, -6/-8)

On Windows, a synchronous Win32 call made from inside an update delivers
window messages right there: `SetFocus` sends `WM_ACTIVATE`, `ShowWindow*`
and `SetWindowPos` send `WM_SIZE`/`WM_PAINT` (shell hooks such as Windhawk's
window-animation mods even call `PrintWindow`, which paints synchronously),
and nested message pumps (WebView2 creation's `wait_with_pump`, modal
dialogs) also dispatch the main-thread task queue. Upstream then calls into
the App that is still borrowed further up the stack: window callbacks fail
and log "RefCell already borrowed" at error level, and a foreground task
that updates an entity panics.

- `gpui-pre/src/app.rs`: a thread-local count of live `AppRef`/`AppRefMut`
  guards and `app_borrowed_on_this_thread()` (doc-hidden, re-exported).
- `gpui-pre/src/window.rs`: `on_request_frame` defers (returns early, like
  the existing `draw_in_progress()` check) when the App is borrowed; Windows
  re-invalidates every window on the next vsync. Input, resize, move,
  activation, visibility, hover, insets, viewport, button-layout and
  display-change callbacks go through `update_window_or_defer`, which runs
  them on a foreground task once the borrow is released instead of losing
  them. A re-entrant input event reports "unhandled" (what the failed update
  returned before); a re-entrant window-control hit test answers `None`.
- `gpui-pre-windows/src/platform.rs`: `run_foreground_task` leaves the queue
  alone while the App is borrowed and retries from a `USER_TIMER_MINIMUM`
  timer on the platform window (`DEFERRED_TASKS_TIMER_ID`), keeping
  `wake_posted` set so dispatches meanwhile don't spin the nested pump.
- `gpui-pre-windows/src/events.rs`: the size/move-loop timer doesn't drain
  the task queue while the App is borrowed either.
