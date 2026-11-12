# lookout-core

The capture engine behind Lookout's desktop app, as a plain Rust library with
no UI-toolkit dependency. The Tauri app in `../../src` is one shell over it;
a GTK/libadwaita, Qt, or headless shell links the same crate.

What lives here:

- **Capture sources** (`sources`): monitor and window enumeration, including the
  on-screen window geometry used for redaction.
- **Frame capture** (`capture`): stitched multi-source grabs, Filtered Apps
  redaction, SIMD downscale, JPEG encode. PipeWire/portal capture on Linux.
- **Clips** (`clips`): hardware H.264 per-minute clips (VideoToolbox, Media
  Foundation, GStreamer).
- **Capture loop** (`capture_loop`): the self-scheduling capture tick (on
  the server's credit grid, never delayed by the network), clip cutting,
  live-preview frames, sleep/pause recovery, and the ordered uploader with
  its backlog — captures taken while the link is down are delivered when
  it is back, stamped with when they were taken, so an outage shorter than
  the server's trust envelope costs no credit.
- **Upload pipeline** (`upload`, `transport`): the desktop's reqwest
  transport and `UploadHost` for the pipeline in `lookout-protocol`, plus
  the one-shot capture paths a shell can call.
- **Timing** (`tuning`, an alias of `lookout_session::timing`): the cadence,
  retry, timeout and backlog constants, per `Core`, so the credit
  simulations (`credit_sim`) can run an hour of pause/resume cycling and
  outages against the real crediting rules in seconds.
- **The recording** (`recorder`): the session as a state machine — open,
  pause, resume, stop, source lost, server-ended — and the one
  `RecorderState` a shell draws.
- **Recording clock** (`clock`, `timer`): the single clock every surface
  shows, and the once-a-second menu-bar ticker that reads it.
- **Filtered Apps list** (`apps`) and **capture diagnostics**
  (`capture_diagnostics`).
- **Server API** (`api`): the session lifecycle (status, pause, resume,
  stop, rename, video, cut editing, edit lease), the program registry, the
  announcement banner, and the gallery batch lookup. Responses are the
  server's JSON verbatim; errors carry the HTTP status so UI code can branch
  on it. Shells call these instead of speaking HTTP themselves.

## The layers

The engine is three crates, so the logic every client shares is separate
from the platform it runs on:

- **`lookout-session`** — pure logic, no I/O, no runtime, only `serde`:
  the recording's phases and what the controls should show, the one
  clock every surface ticks from, the clock-offset estimate, the capture
  schedule rules (grid, stale targets, backlog policy), the timing
  constants, and the session/source data types. `#![forbid(unsafe_code)]`.
- **`lookout-protocol`** — the server protocol over a `Transport` trait
  the host supplies: the session API, the presign → PUT → confirm upload
  pipeline with its retry policy and error classification, and the wire
  types with their compatibility tests. `#![forbid(unsafe_code)]`.
- **`lookout-core`** (this crate) — the desktop host: screen and camera
  capture, clips, the portal, the capture loop and its uploader, the
  recorder's orchestration, a reqwest transport. Re-exports the two
  crates below it, so every `lookout_core::` path a shell uses is stable.

A browser or a device host implements `Transport` (and, for the loop,
its own frame source) and gets the same crediting behaviour, with the
same simulations to prove it.

## Writing a shell

Implement `Frontend`, build a `Core`, open a recording and draw its state:

```rust
use std::sync::Arc;
use lookout_core::{Core, CoreEvent, Frontend, RecordingSource, SessionConfig};

struct MyShell;

impl Frontend for MyShell {
    fn emit(&self, event: CoreEvent) {
        // `CoreEvent::RecorderState(state)` is the whole recording UI, every
        // time it changes (None once closed); `RecorderNotice` is an OS
        // notification; `PreviewFrame` is the live preview image. The rest
        // are the raw loop events, for logging.
    }
    fn wants_preview_frames(&self) -> bool { true }
    fn set_tray_title(&self, time_text: &str, paused: bool) { /* "04:59" */ }
    // Camera sessions: implement `camera_start` / `camera_grab_frame` /
    // `camera_stop` to hand the loop frames; leave the defaults otherwise.
}

// Inside a tokio runtime:
let core = Core::new(Arc::new(MyShell), "Lookout GNOME", env!("CARGO_PKG_VERSION"));
let cfg = SessionConfig { token: session_token, api_base_url };
let source = RecordingSource::Screen { sources: picked };
let state = core.recorder_open(cfg, source).await?;   // loads the session, starts capturing
// Buttons:
core.recorder_pause().await?;       // flushes the partial minute, then pauses
core.recorder_resume().await?;
core.recorder_begin_stop().await?;  // pauses into the naming prompt
core.recorder_finish_stop(Some("My cut".into()), /* edit */ false).await?;
core.recorder_close().await;        // on the way out of the screen
```

The recording — the capture loop, the session's status, the single clock
every surface ticks from, the order of operations behind every button —
lives in `recorder`; a shell draws `RecorderState` and relays clicks. See
the module docs there for the rules, each of which was a shipped bug.

Session *creation* happens on the program's website and reaches a shell as
a `lookout://` deep link carrying the token; everything after that goes
through `Core`'s `api` methods (`core.session_status(&cfg)`, `core.session_stop(&cfg, edit)`,
...). Response shapes are documented in `packages/server/API.md` and typed in
`packages/shared/src/types.ts`.

See the crate-level docs in `src/lib.rs` for the full contract.
