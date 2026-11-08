// ──────────────────────────────────────────────────────────
// Session statuses
// ──────────────────────────────────────────────────────────

export const SESSION_STATUSES = [
  "pending",
  "active",
  "paused",
  "stopped",
  "compiling",
  "complete",
  "failed",
] as const;

export type SessionStatus = (typeof SESSION_STATUSES)[number];

// ──────────────────────────────────────────────────────────
// Capture & upload timing
// ──────────────────────────────────────────────────────────

/** How often the client should capture a screenshot.
 *  The server returns `nextExpectedAt` after each upload;
 *  this is the default interval if that value is missing.
 *  Default: 60000 (60 seconds) */
export const SCREENSHOT_INTERVAL_MS = 60_000;

/** Max tolerance for clock skew between client and server (ms).
 *  Used when validating timestamps in requests.
 *  Default: 5000 (5 seconds) */
export const MAX_CLOCK_SKEW_MS = 5_000;

// ──────────────────────────────────────────────────────────
// Credit-mode tracking (see plan: server-authoritative wall-clock)
// ──────────────────────────────────────────────────────────

/** Trust envelope: how far in the past `capturedAt` may be relative
 *  to server `now()` before being rejected. Wide enough to absorb
 *  normal client-clock skew and buffered uploads.
 *  Default: 300000 (5 minutes) */
export const CAPTURED_AT_PAST_TOLERANCE_MS = 300_000;

/** Trust envelope: how far in the future `capturedAt` may be relative
 *  to server `now()` before being rejected. Symmetric with the past
 *  bound to handle clients with fast-skewed clocks.
 *  Default: 300000 (5 minutes) */
export const CAPTURED_AT_FUTURE_TOLERANCE_MS = 300_000;

/** Credit-mode streak window: |capturedAt - expectedAt| ≤ this credits
 *  60s; outside resets the streak to a fresh anchor with 0 credit.
 *  Tightly coupled to SCREENSHOT_INTERVAL_MS — keep at half-interval.
 *  Default: 30000 (30 seconds) */
export const STREAK_WINDOW_MS = 30_000;

/** Seconds awarded per in-window capture in credit mode.
 *  Equals SCREENSHOT_INTERVAL_MS / 1000. Don't hardcode 60 in the
 *  credit path — derive from this constant.
 *  Default: 60 */
export const CREDIT_PER_CAPTURE_S = 60;

/** Server-credited seconds below which a session has no timelapse in it.
 *
 *  Under one capture unit there is nothing to compile. The seed capture
 *  opens the recording rather than closing a minute, so the worker drops
 *  it from the video (`dropSeedUnit`), and the only other capture a
 *  sub-minute session holds is the still the client flushes on the way
 *  out. The timelapse that falls out is one frozen frame — or, with the
 *  seed alone, nothing the compile can use at all.
 *
 *  Clients say so in the stop confirmation; they do NOT block the stop.
 *  Someone who opened a session by mistake has to be able to leave, and
 *  the server accepts a stop at any duration regardless — programs and
 *  the internal force-stop must be able to end a session they didn't
 *  record.
 *  Default: 60 (one capture unit) */
export const MIN_STOPPABLE_TRACKED_SECONDS = CREDIT_PER_CAPTURE_S;

// ──────────────────────────────────────────────────────────
// Clips (6 frames/minute via per-minute video uploads)
// ──────────────────────────────────────────────────────────

/** Upload payload formats the server accepts on upload-url.
 *  "jpeg" is the legacy single-screenshot-per-minute payload.
 *  "webm"/"mp4" are per-minute video clips holding ~6 frames captured
 *  seconds apart (webm from Chromium/Firefox MediaRecorder and from the
 *  desktop's Linux VP9 fallback; mp4 from Safari MediaRecorder and the
 *  desktop's H.264 encoders). Neither format is tied to a client — a
 *  client sends whichever container the encoder it found produces. The
 *  per-minute request cadence, credit math, and rate limits are
 *  identical in all formats — a clip is still ONE capture unit.
 *  Clips are gated per session by `sessions.clips_enabled`, which defaults
 *  to TRUE — a program opts OUT with `clips: false` at creation. Immutable
 *  thereafter. */
export const CAPTURE_FORMATS = ["jpeg", "webm", "mp4"] as const;
export type CaptureFormat = (typeof CAPTURE_FORMATS)[number];

/** R2/HTTP content type for each capture format. The presigned PUT is
 *  signed with this content type and confirm re-validates it via
 *  HeadObject, so client and server must agree exactly. */
export const CAPTURE_FORMAT_CONTENT_TYPES: Record<CaptureFormat, string> = {
  jpeg: "image/jpeg",
  webm: "video/webm",
  mp4: "video/mp4",
};

/** How often a clip-recording client grabs a frame into the current
 *  clip. 10000ms = 6 frames per SCREENSHOT_INTERVAL_MS. The cadence is
 *  server-authoritative: it's sent to clients as `frameIntervalMs` on
 *  the session GET and upload-url responses, clients capture at exactly
 *  that rate, and no client exposes an override.
 *
 *  Every knob that used to be denominated in "frames" is now derived from
 *  this value (per-frame byte budget, native encoder bitrate, the stall
 *  cap, container size), so changing the cadence is a one-line change and
 *  per-frame QUALITY is held constant — see CLIP_FRAME_BYTE_BUDGET.
 *  Timelapse smoothness scales directly with it: each capture unit becomes
 *  one second of output video, so 6/min renders 6 distinct images per
 *  output second.
 *  Default: 10000 (10 seconds) */
export const CLIP_FRAME_INTERVAL_MS = 10_000;

/** Nominal frames per clip (SCREENSHOT_INTERVAL_MS / CLIP_FRAME_INTERVAL_MS).
 *  Informational — clips are VFR and static screens legitimately emit
 *  fewer encoded frames. The worker derives real counts by demuxing.
 *  Default: 6 */
export const FRAMES_PER_CLIP = Math.round(
  SCREENSHOT_INTERVAL_MS / CLIP_FRAME_INTERVAL_MS,
);

/** Hard cap on frames the client records into a SINGLE clip, as a multiple
 *  of the nominal count.
 *
 *  A clip is cut when its upload tick fires, so a slow uplink stretches the
 *  clip: the recorder keeps grabbing frames at the cadence while the
 *  previous upload drains. Uncapped, a 5-minute network stall produced a
 *  30-frame clip that (a) blew MAX_CLIP_BYTES and was refused server-side,
 *  costing the whole window, and (b) still rendered as ONE second of
 *  output. Capping frames bounds the container instead: the tail of a
 *  stalled window is dropped, the clip still uploads, and the minute still
 *  credits.
 *  Default: 3 */
export const MAX_CLIP_FRAME_OVERRUN = 3;

/** Absolute frame cap for one clip. See MAX_CLIP_FRAME_OVERRUN. */
export const MAX_FRAMES_PER_CLIP = FRAMES_PER_CLIP * MAX_CLIP_FRAME_OVERRUN;

/** Consecutive clip-upload failures a client tolerates before giving up on
 *  clips and recording plain JPEGs for the rest of the session.
 *
 *  Every individual failure is already survivable — the tick retries as a
 *  single JPEG, so the minute still credits. This bound is about not
 *  re-attempting something structurally broken once a minute for hours:
 *  a browser whose encoder emits containers the server rejects, or a session
 *  whose clip support went away underneath the client. Any successful clip
 *  upload resets the count, so a patch of bad network never disables clips.
 *  Matches the desktop client's MAX_CLIP_ENCODER_FAILURES.
 *  Default: 3 */
export const MAX_CLIP_UPLOAD_FAILURES = 3;

/** LEGACY: wall-clock delay from capture start to the FIRST upload tick,
 *  used by older clip-mode clients still in the field.
 *
 *  Current clients fire the seed tick immediately (like JPEG mode always
 *  did): the seed capture credits 0 seconds and the compiler drops it from
 *  the video entirely (see the worker's dropSeedUnit), so delaying it to
 *  give it frames bought nothing — and it shifted the session's streak
 *  anchor (and startedAt) 8s past record-press, permanently losing those
 *  seconds and freezing the display timer at 1:00 until the first credit
 *  landed. Kept because old desktop/web clients still cut their opening
 *  clip on this delay and the worker's seed handling documents itself
 *  against it. Do not add new uses.
 *  Default: 8000 (8 seconds) */
export const CLIP_FIRST_CUT_DELAY_MS = 8_000;

/** Slack added on top of one capture interval when the display timer
 *  interpolates ahead of the last server-credited value.
 *
 *  Credits are earned on a strict 60s grid (the streak anchor), but they
 *  reach the UI a confirm round-trip later — and clips are multi-MB
 *  payloads, so that latency jitters by seconds. With the interpolation
 *  cap at exactly one interval, any confirm-to-confirm gap over 60s froze
 *  the display precisely at a xx:00 boundary (the credited base is always
 *  a multiple of 60) until the next confirm landed — read by users as the
 *  recorder hanging. This slack absorbs normal upload/confirm latency;
 *  a genuine stall still freezes the display, just this much later.
 *  Mirrored by the desktop tray ticker (MAX_TRAY_INTERPOLATION_SECS in
 *  clients/desktop/src-tauri/src/lib.rs) — keep the two in step.
 *  Default: 15 */
export const TIMER_INTERPOLATION_SLACK_S = 15;

/** Per-frame byte budget for a natively-encoded clip frame — the ACTUAL
 *  quality dial, and the reason the native bitrate is derived rather than
 *  hardcoded.
 *
 *  Sized for TEXT LEGIBILITY: ~400 KB buys a JPEG-q85-class keyframe at
 *  1080p, the bar the legacy single-screenshot pipeline set. Native
 *  encoders receive each frame's real presentation timestamp, so their
 *  bitrate is denominated in bits per second of MEDIA time — meaning the
 *  same bitrate buys 2.5x the bytes per frame when frames sit 10s apart
 *  instead of 4s. Expressing the tuned number per-frame keeps quality
 *  invariant when the cadence changes, in both directions.
 *  Default: 400000 (400 KB) */
export const CLIP_FRAME_BYTE_BUDGET = 400_000;

/** Bitrate (bits/second of media time) a native encoder should be given to
 *  land CLIP_FRAME_BYTE_BUDGET per frame at the supplied cadence.
 *
 *  At the historical 4s cadence this returns exactly 800 kbps — the value
 *  that was measured and tuned by hand (133k and 400k were tried first and
 *  produced visibly soft H.264). A VBR ceiling, not a floor: static screen
 *  content undershoots it heavily. The desktop encoders mirror this
 *  formula in Rust; keep the two in step. */
export function nativeClipBitsPerSecond(frameIntervalMs: number): number {
  const intervalS = Math.max(frameIntervalMs, 1) / 1000;
  return Math.round((CLIP_FRAME_BYTE_BUDGET * 8) / intervalS);
}

/** Native encoder bitrate at the default cadence. Prefer
 *  `nativeClipBitsPerSecond(frameIntervalMs)` wherever the real
 *  server-supplied cadence is in hand. */
export const CLIP_VIDEO_BITS_PER_SECOND = nativeClipBitsPerSecond(
  CLIP_FRAME_INTERVAL_MS,
);

/** H.264 quantizer (QP, 0–51) for browser clips encoded through WebCodecs
 *  in per-frame-quantizer mode — the browser path that actually matches the
 *  desktop's quality, where the engine supports it (Chromium).
 *
 *  Constant-quality is the right dial for this workload: clips are a handful
 *  of near-still 1080p screen frames, so "spend whatever these frames need"
 *  beats any bitrate target. QP 24 lands dense-text 1080p keyframes in the
 *  ~200–400 KB range — the same class as CLIP_FRAME_BYTE_BUDGET — and delta
 *  frames on a static screen cost almost nothing.
 *  Default: 24 */
export const CLIP_WEBCODECS_QP = 24;

/** How much the WebCodecs quantizer coarsens when a finished clip overruns
 *  MAX_CLIP_BYTES (+6 QP roughly halves H.264 output), and the coarsest QP
 *  worth uploading at all. Mirrors the MediaRecorder path's halve-with-floor
 *  backoff. */
export const CLIP_WEBCODECS_QP_STEP = 6;
export const CLIP_WEBCODECS_QP_MAX = 44;

/** Floor for the browser recorder's adaptive bitrate backoff. NOT derived
 *  from the native figure: the two are denominated in different things (see
 *  CLIP_WEB_VIDEO_BITS_PER_SECOND), this is just the coarsest setting worth
 *  uploading at all.
 *  Default: 800000 */
export const CLIP_WEB_MIN_BITS_PER_SECOND = 800_000;

/** Encoder bitrate cap for clips recorded by a BROWSER (MediaRecorder's
 *  `videoBitsPerSecond`). Deliberately ~50x the native constant, because
 *  the two numbers are denominated in different things.
 *
 *  A native encoder gets each frame's true presentation time (4s apart)
 *  plus an explicit ~1fps rate-control hint, so 800 kbps really does
 *  buy ~400 KB per frame. MediaRecorder's rate control ignores wall-clock
 *  frame spacing entirely — measured on Chromium 148 at 1080p, recording
 *  the same frames 4000ms apart and 125ms apart produces BYTE-IDENTICAL
 *  output. There is no "× 60 seconds" budget to spend; the encoder just
 *  allocates a per-frame quantizer from a nominal cadence.
 *
 *  At 800 kbps the browser encoder is therefore pinned at its maximum
 *  quantizer — as coarse as it is allowed to be — and still overshoots
 *  the request. The whole 0.8–2 Mbps range is byte-identical, which is
 *  why raising the shared constant 400k → 800k sharpened the desktop and
 *  did nothing whatsoever for the web.
 *
 *  Measured sweep (1080p, worst-case dense-text content, PSNR vs source):
 *
 *      bitrate   KB/frame   PSNR
 *        0.8M       53.7    25.8 dB   <- previous setting
 *          5M      114.6    30.9 dB
 *         20M      192.0    34.3 dB
 *         40M      335.4    38.6 dB   <- knee; ~parity with native
 *         80M      582.3    43.7 dB   worst case exceeds MAX_CLIP_BYTES
 *
 *  40 Mbps lands at ~335 KB/frame — the same order as the native encoder's
 *  CLIP_FRAME_BYTE_BUDGET. Because the allocation is per-frame and NOT
 *  per-second, this constant is cadence-independent: it needs no change
 *  when CLIP_FRAME_INTERVAL_MS moves, and the clip simply carries fewer
 *  frames. Measured over a full 15-frame clip (the 4s-cadence shape),
 *  against the 8 MB MAX_CLIP_BYTES cap:
 *
 *                      before (vp9 @ 800k)   after (h264 @ 40M)
 *      busy screen        0.89 MB  23.8 dB     3.24 MB  43.3 dB
 *      typical screen     0.37 MB  23.8 dB     2.46 MB  43.3 dB
 *
 *  so even incompressible content sat at 40% of the cap; at 6 frames/min
 *  the same content is under half of that. (0.37 MB matches the ~400 KB/min
 *  these clips were measured at in the field, which is what makes the rest
 *  of the table trustworthy.)
 *  ClipRecorder additionally backs the rate off if a clip ever does
 *  exceed the cap, so a browser with different rate-control semantics
 *  self-corrects instead of failing every upload.
 *  Default: 40000000 */
export const CLIP_WEB_VIDEO_BITS_PER_SECOND = 40_000_000;

/** Max clip file size in bytes, validated server-side via HeadObject
 *  after upload. Sized above the bitrate budget (800 kbps × 60s ≈ 6 MB)
 *  to absorb encoder overshoot and container overhead.
 *  Default: 8388608 (8 MB) */
export const MAX_CLIP_BYTES = 8 * 1024 * 1024;

// ──────────────────────────────────────────────────────────
// Auto-timeout thresholds
// ──────────────────────────────────────────────────────────

/** Auto-pause a session after this many minutes without a
 *  screenshot upload. The session moves to "paused" status.
 *  Default: 10 minutes */
export const AUTO_PAUSE_AFTER_MINUTES = 10;

/** Auto-stop (and trigger compilation) after this many minutes
 *  without a screenshot upload. Applies to both "active" and
 *  "paused" sessions.
 *  Default: 24 hours */
export const AUTO_STOP_AFTER_MINUTES = 1440;

/** Sessions stuck in "compiling" for longer than this are
 *  assumed crashed and reset to "stopped" for re-enqueue.
 *  Default: 60 (minutes) */
export const STUCK_COMPILING_TIMEOUT_MINUTES = 60;

/** Max times the stuck-compiling timeout will re-enqueue a
 *  compilation before giving up and marking the session failed.
 *  Prevents infinite recompile loops from deeper corruption.
 *  Default: 3 */
export const MAX_COMPILE_ATTEMPTS = 3;

/** How long a session a program asked to delete stays recoverable before
 *  its captures, video and rows are purged for good. The session is hidden
 *  from every token and media endpoint for the whole window; only the
 *  owning program can still see it, and restore it.
 *  Default: 60 (minutes) */
export const SESSION_DELETION_GRACE_MINUTES = 60;

// ──────────────────────────────────────────────────────────
// Rate limiting & abuse prevention
// ──────────────────────────────────────────────────────────

/** Max upload-url requests per 60-second window per session.
 *  Sized for: 1 nominal capture/min + occasional burst (race in the
 *  client's fire-and-forget scheduling chain) + up to 3 client-side
 *  retries on transient network errors. 3 was too tight: any hiccup
 *  blew the budget and the chain stalled.
 *  Default: 10 */
export const RATE_LIMIT_PER_MINUTE = 10;

/** Max confirmed screenshots per session.
 *  At 1/min this equals 12 hours of recording.
 *  Default: 720 */
export const MAX_SCREENSHOTS_PER_SESSION = 720;

/** Max total upload-url requests per session (confirmed + unconfirmed).
 *  Sized at ~6x the screenshot cap to absorb client retries and burst
 *  races without truncating long sessions.
 *  Default: 4320 */
export const MAX_UPLOAD_REQUESTS_PER_SESSION = 4320;

/** Max screenshot file size in bytes.
 *  Validated server-side via HeadObject after upload.
 *  Default: 2097152 (2 MB) */
export const MAX_SCREENSHOT_BYTES = 2 * 1024 * 1024;

// ──────────────────────────────────────────────────────────
// Presigned URL settings
// ──────────────────────────────────────────────────────────

/** How long a presigned PUT URL remains valid (seconds).
 *  Keep short to limit replay/leak window.
 *  Default: 120 (2 minutes) */
export const PRESIGNED_URL_EXPIRY_SECONDS = 120;

// ──────────────────────────────────────────────────────────
// Cleanup
// ──────────────────────────────────────────────────────────

/** Delete unconfirmed screenshot records after this many minutes.
 *  Their presigned URLs have long expired by this point.
 *  Default: 10 (minutes) */
export const UNCONFIRMED_CLEANUP_AFTER_MINUTES = 10;

/** Delete screenshot R2 image objects for successfully compiled sessions
 *  (status=complete with a video) after this many days. The screenshot DB
 *  rows are KEPT (so capture timings stay queryable); only the JPEGs in R2
 *  are removed. Only applies to sessions that have a videoR2Key set.
 *  Default: 7 (days) */
export const SCREENSHOT_RETENTION_DAYS = 7;

/** Max byte length of the client-supplied `clientInfo` query param on
 *  upload-url. Anything larger is ignored (stored as null). Bounds a hostile
 *  client; real payloads are a few hundred bytes. */
export const CLIENT_INFO_MAX_BYTES = 1024;

/** Max byte length of a JA4 TLS fingerprint read from the edge-set request
 *  header (JA4_HEADER). A canonical JA4 is ~36 chars (e.g.
 *  "t13d1516h2_8daaf6152771_b186095e22b6"); the cap leaves headroom for
 *  variants while bounding a hostile/misconfigured edge. Longer values are
 *  ignored (stored as null). */
export const JA4_MAX_BYTES = 128;

// ──────────────────────────────────────────────────────────
// Screenshot capture settings
// ──────────────────────────────────────────────────────────

/** JPEG quality for canvas -> blob conversion (0-1).
 *  0.85 balances quality (~100-300 KB at 1080p) and file size.
 *  Default: 0.85 */
export const JPEG_QUALITY = 0.85;

/** Max capture resolution (width). Screenshots are scaled down
 *  to fit within these bounds while preserving aspect ratio.
 *  Default: 1920 */
export const MAX_WIDTH = 1920;

/** Max capture resolution (height).
 *  Default: 1080 */
export const MAX_HEIGHT = 1080;

// ──────────────────────────────────────────────────────────
// Client upload resilience
// ──────────────────────────────────────────────────────────

/** Max retry attempts for each upload step
 *  (presigned URL request, R2 PUT, confirmation POST).
 *  Default: 3 */
export const MAX_UPLOAD_RETRIES = 3;

/** Per-step deadline for one upload attempt: the presigned-URL request, the
 *  R2 PUT, or the confirm POST. Matches the desktop client's STEP_TIMEOUT.
 *
 *  `fetch` has no default timeout, so without this a half-open socket or a
 *  trickling uplink parks an upload attempt indefinitely, and everything
 *  downstream of it stalls with no error to retry on. A bounded step turns
 *  a dead connection into a normal retryable failure. Generous enough for a
 *  multi-megabyte clip on a weak link — this is a stall detector, not a
 *  bandwidth requirement.
 *  Default: 30000 (30 seconds) */
export const UPLOAD_STEP_TIMEOUT_MS = 30_000;

/** Retry delays in ms (exponential backoff).
 *  Default: [2000, 4000, 8000] */
export const UPLOAD_RETRY_DELAYS_MS = [2_000, 4_000, 8_000];

/** Max screenshots buffered in memory when uploads are slow.
 *  Oldest are dropped if the buffer overflows.
 *  Default: 5 */
export const MAX_PENDING_BUFFER = 5;

// ──────────────────────────────────────────────────────────
// Capture robustness
// ──────────────────────────────────────────────────────────

/** Timeout for canvas.toBlob() before giving up on a frame (ms).
 *  Prevents the capture pipeline from stalling permanently.
 *  Default: 10000 (10 seconds) */
export const CANVAS_TO_BLOB_TIMEOUT_MS = 10_000;

/** Timeout waiting for video element to have decoded dimensions
 *  after play() resolves (ms). Safari may resolve play() before
 *  the first frame is available.
 *  Default: 5000 (5 seconds) */
export const VIDEO_READY_TIMEOUT_MS = 5_000;
