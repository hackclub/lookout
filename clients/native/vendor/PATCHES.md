# Vendored GPUI crates

These are the crates.io releases of `gpui-pre`, `gpui-pre-wgpu`,
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
