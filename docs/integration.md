# Lookout Integration Guide

Lookout is a screen recording timelapse service. It has two distinct API surfaces:

1. **Internal API** — server-to-server, protected by API key. Used by your trusted backend to create/manage sessions.
2. **Client API** — browser-facing, authenticated by session token. Used by the user's browser to record and upload screenshots.

## Architecture Overview

```
┌─────────────────────┐         ┌───────────────────────┐
│  Your Backend       │         │  Lookout Server      │
│  (trusted server)   │────────>│  (internal API)       │
│                     │  POST   │                       │
│  Creates sessions,  │  /api/  │  Creates session,     │
│  manages lifecycle  │  internal│  returns token       │
└─────────┬───────────┘         └───────────────────────┘
          │                               │
          │ Passes token to browser       │
          │ (URL param, redirect, etc.)   │
          v                               │
┌─────────────────────┐         ┌───────────────────────┐
│  User's Browser     │         │  Lookout Server      │
│  (untrusted client) │────────>│  (client API)         │
│                     │  token  │                       │
│  Screen capture,    │  based  │  Presigned URLs,      │
│  upload screenshots │         │  timing validation    │
└─────────┬───────────┘         └───────────────────────┘
          │
          │ Direct upload via presigned URL
          v
┌─────────────────────┐
│  Cloudflare R2      │
│  (screenshot store) │
└─────────────────────┘
```

## Part 1: Server-to-Server (Internal API)

Your trusted backend is the only entity that can create sessions. All internal
API calls require the `X-API-Key` header.

### Create a session

```bash
curl -X POST https://lookout.hackclub.com/api/internal/sessions \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-api-key" \
  -d '{"metadata": {"userId": "user_123", "projectId": "proj_456"}}'
```

Response:
```json
{
  "token": "5b70dd22...64-char-hex-string",
  "sessionId": "137c9b2f-3e74-4c25-a295-b41bd4d2c5d1",
  "sessionUrl": "https://lookout.hackclub.com/session?token=5b70dd22..."
}
```

- `token` — the session credential. Give this to the user's browser, and **store it on your server** associated with the user so you can look up the session later.
- `sessionId` — the server-side ID.
- `sessionUrl` — a convenience URL you can redirect the user to. It serves a recorder Lookout hosts, where the user picks the desktop app, this browser, or a camera; it also takes `?app=your-program` and `?edit=false` (drops "Edit & save" from the stop dialog).
- `metadata` — any JSON you want to associate with the session (user info, project, etc.)
- `clips` — set `false` to opt this session OUT of [clips](#clips-6-frames-per-minute) and back to 1 JPEG/min. Default `true` (~6 frames/min video → 6× smoother timelapses); immutable after creation.
- `redirectUrl` — optional [redirect hook](#redirect-hook): an http(s) URL the recording client sends the user to once their timelapse finishes compiling. Immutable after creation.

### Redirect hook

Pass `redirectUrl` when creating a session to send the user somewhere when
their timelapse is done — e.g. back to your submission form:

```bash
curl -X POST https://lookout.hackclub.com/api/internal/sessions \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-api-key" \
  -d '{"metadata": {"userId": "user_123"}, "redirectUrl": "https://yourprogram.example/submit?step=timelapse-done"}'
```

How it behaves:

- The URL must be `http(s)` (max 2048 chars) — anything else is rejected with
  a 400 at creation time.
- The desktop app opens the URL in the user's default browser the moment it
  sees the session flip to `complete` while the user is watching the compile
  (i.e. right after they stop recording). It fires at most once per session,
  and does **not** fire when someone later re-opens an already-completed
  session from their gallery.
- The URL is surfaced to clients on `GET /api/sessions/:token` and
  `GET /api/sessions/:token/status` as `redirectUrl`, so custom clients can
  implement the same behavior.
- Older desktop clients ignore the field — treat the redirect as a
  convenience, not a guaranteed callback. For server-side certainty, poll
  [session status](#get-session-info) instead.

### Clips (6 frames per minute)

Sessions record **clips** by default: instead of one JPEG per minute, the
recording client uploads one ~60s video file per minute containing ~6 frames
captured 10s apart. The compiled timelapse has the same length but is 6×
smoother, with motion from the very first second. Pass `"clips": false` at
creation to opt out.

What this means for your program:

- **Nothing in your integration changes.** A clip is still one capture unit
  per minute — `trackedSeconds`, `screenshotCount`, `/timings` (still one
  timestamp per minute → Hackatime forwarding unchanged), `videoUrl`, and
  every response shape are identical between clips and non-clips sessions.
- **Clients negotiate automatically.** The hosted web recorder and React SDK
  (≥0.4) detect the flag on the session and record clips; older clients and
  the desktop app keep uploading JPEGs to the same session, which stays fully
  valid (formats can even mix within one session).
- **Network:** a clip is capped at 8 MB/min server-side. At 6 frames/min a
  typical screen measures ~1.1 MB/min and a deliberately incompressible one
  ~1.4 MB/min — under half of what the same content cost at 15 frames/min,
  since bandwidth scales with the frame count and the per-frame quality
  budget is held constant.
- **Frame quality:** clip frames are bitrate-capped rather than encoded
  independently, but the budget is sized per frame to hold q0.85-JPEG-class
  detail at 1080p even on busy screens, and it is rescaled whenever the
  cadence changes — so frames stay legible at any frame rate. For review
  purposes you get 6× more moments per minute.
- The flag is per session, so you can disable it for a fraction of new
  sessions and compare, or turn it off entirely for a program that needs the
  legacy payload.

### Get session info

```bash
curl https://lookout.hackclub.com/api/internal/sessions/SESSION_ID \
  -H "X-API-Key: your-api-key"
```

Response:
```json
{
  "session": {
    "id": "137c9b2f-3e74-4c25-a295-b41bd4d2c5d1",
    "token": "5b70dd22...64-char-hex-string",
    "name": "My timelapse",
    "metadata": {"userId": "user_123", "projectId": "proj_456"},
    "status": "active",
    "startedAt": "2024-01-01T12:00:00.000Z",
    "totalActiveSeconds": 300,
    "videoUrl": null,
    "videoWebmUrl": null,
    "thumbnailUrl": null,
    "createdAt": "2024-01-01T11:50:00.000Z"
  },
  "trackedSeconds": 123,
  "screenshotCount": 45
}
```

- `trackedSeconds` — tamper-proof tracked time. Sessions created post-0.2.1 use **credit mode**: each capture that arrives within ±30s of the streak-anchored expected mark credits 60s; out-of-window captures reset the streak. Pre-0.2.1 sessions remain on **bucket mode** (`distinct confirmed minute buckets × 60`). Mode is sticky per session — clients that send `capturedAt` flip the session to credit on first upload.
- `screenshotCount` — number of confirmed screenshots

### Force-stop a session

```bash
curl -X POST https://lookout.hackclub.com/api/internal/sessions/SESSION_ID/stop \
  -H "X-API-Key: your-api-key"
```

### Recompile a failed session

```bash
curl -X POST https://lookout.hackclub.com/api/internal/sessions/SESSION_ID/recompile \
  -H "X-API-Key: your-api-key"
```

### Delete a session

```bash
curl -X DELETE https://lookout.hackclub.com/api/internal/sessions/SESSION_ID \
  -H "X-API-Key: your-api-key"
```

Use this when a user asks you to delete their data (a GDPR request, or the
Hack Club privacy hub calling your program), or when you discard a recording
for good. Your program must delete its Lookout sessions as part of erasing a
user: Lookout has no accounts, so nobody else can find them.

The session disappears immediately — the recorder, the batch endpoint and the
media URLs all treat it as gone — and is destroyed for good an hour later.
Until then, `POST /api/internal/sessions/SESSION_ID/restore` brings it back,
so a bug or a wrong user id in your program is recoverable. Calling `DELETE`
again is safe and doesn't extend the hour.

You can only delete sessions your own key created. See
[`packages/server/API.md`](../packages/server/API.md#delete-session) for the
full contract.

## Part 2: Client (Browser) Flow

If you're using React, the [`@lookout/react` SDK](../clients/react/API.md) handles
all of this for you with a drop-in `<LookoutRecorder>` component or the `useLookout()` hook.

The browser receives the token and uses it for all operations. **The client is
untrusted** — all timing and time tracking is validated server-side.

### Typical client flow

```
1. Get token from URL:  /session?token=abc123
2. GET /api/sessions/:token          → check session status
3. User clicks "Start Recording"
4. Call navigator.mediaDevices.getDisplayMedia() to share screen
5. Capture loop — each iteration is one full pipeline, awaited end-to-end.
   The cadence (~60s between captures in steady state) emerges from the
   server's nextExpectedAt, NOT a fixed client setInterval.

   a. Stamp capturedAt = client clock at the moment you grab the frame
   b. Capture canvas screenshot (JPEG, max 1080p)
   c. GET /api/sessions/:token/upload-url?capturedAt=<iso8601>
      → { uploadUrl, screenshotId, nextExpectedAt, trackingMode }
      (First call activates the session: pending → active)
      (Presence of capturedAt on the FIRST upload sticks the session to
       credit mode; absence sticks it to bucket mode. Mode is permanent.)
   d. PUT blob to uploadUrl (presigned R2 URL)
   e. POST /api/sessions/:token/screenshots { screenshotId, width, height, fileSize }
      → { confirmed, trackedSeconds, nextExpectedAt }
      Display trackedSeconds (server-authoritative) — do NOT compute
      display time from uploads.completed.
   f. Schedule the next iteration:
      delay = max(0, parse(nextExpectedAt) - Date.now())
      Then setTimeout(loop, delay). Never fire sooner than this — bursts
      cause streak resets in credit mode.
6. User clicks "Pause"  → POST /api/sessions/:token/pause
7. User clicks "Resume" → POST /api/sessions/:token/resume → restart loop
8. User clicks "Stop"   → POST /api/sessions/:token/stop → token becomes read-only
9. Poll GET /api/sessions/:token/status for compilation progress
10. GET /api/sessions/:token/video → presigned URL for the timelapse MP4
```

### Client API reference

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/sessions/:token` | Session status (for recovery after refresh) |
| GET | `/api/sessions/:token/upload-url` | Get presigned PUT URL. Pass `?capturedAt=<iso8601>` to opt the session into credit mode. Activates session on first call. Rate limited: 10/min per session. |
| POST | `/api/sessions/:token/screenshots` | Confirm upload. Body: `{ screenshotId, width, height, fileSize, final? }`. Returns `{ confirmed, trackedSeconds, nextExpectedAt }`. Server verifies R2 object exists. Rate limited: 20/min per token. Set `final: true` only on the one capture a client flushes right before pausing/stopping: credit-mode servers then credit the exact seconds elapsed since the last credited mark (clamped to 0–60) instead of the all-or-nothing streak rule, so a pause at 03:15 resumes at 03:15. Flush **before** the pause POST — a paused session rejects confirms. |
| POST | `/api/sessions/:token/pause` | Pause session |
| POST | `/api/sessions/:token/resume` | Resume session |
| POST | `/api/sessions/:token/stop` | Stop session, trigger compilation. Accepts a stop at any duration — but under a credited minute there's no capture unit to compile, so the official clients warn in the stop confirmation before going through with it. |
| GET | `/api/sessions/:token/status` | Poll compilation status |
| GET | `/api/sessions/:token/video` | Get presigned video URL |

### Upload resilience

The client should handle network failures gracefully:

1. **Run the upload pipeline serially per capture** — take screenshot, await `GET /upload-url`, await R2 PUT, await `POST /screenshots`, then schedule the next capture from the confirm response's `nextExpectedAt`. This is the pattern the desktop Rust loop and the v0.2.4+ React SDK both use. Fire-and-forget queueing produces stale-ref bursts and is no longer recommended.
2. **Retry each leg** — presigned URL request, R2 PUT, confirmation POST — up to 3 times with exponential backoff (2s, 4s, 8s). Treat 409 (session paused/stopped) as terminal, not retriable.
3. **Send `capturedAt` on every upload-url request** (ISO-8601, UTC) — this opts the session into credit-mode tracking. Without it, the session stays on legacy bucket mode for life. Stamp `capturedAt` at the moment the frame is grabbed, not when the request is sent — uploads can be delayed by network without losing credit accuracy.
4. **Schedule the next capture from `nextExpectedAt`** — every confirm response carries the server's authoritative target for the next capture. Compute `delay = max(0, parse(nextExpectedAt) - now)`. If the delay is 0 (server fell behind), fire immediately to catch up — but never fire sooner than this, or you'll cause streak resets.
5. **Idempotent confirmation** — confirming an already-confirmed screenshot is a no-op, so retries on the confirm leg are safe.
6. **Display the server's `trackedSeconds`, not a derived estimate** — do not compute display time from `uploads.completed * intervalSeconds` or similar. In credit mode, not every successful upload credits a minute (out-of-window captures return 200 but `credited_seconds = 0`). Display estimates derived from upload count over-count in those cases; previously this inflated displays by exactly 2× when total round-trip hit ~90s.

### Second-level timer display

The server only updates `trackedSeconds` once per credited capture (~once a minute). For a smoothly-ticking UI, interpolate **locally** between server updates — but cap the interpolation at one capture interval so the display can never overshoot the next credit. This is the same shape `useSessionTimer` ships in the React SDK and the Rust tray ticker uses on desktop.

```ts
const INTERVAL_S = 60; // SCREENSHOT_INTERVAL_MS / 1000 — the cap

let baseSeconds = 0;        // last server-credited value
let lastSyncMs = Date.now(); // when we received it

// Call this from each confirm response and from the periodic
// GET /api/sessions/:token status poll.
function onServerTrackedSeconds(serverTracked: number) {
  // Ratchet forward — never let a stale-read response (e.g. an
  // idempotent retry returning a cached older value) drag the timer back.
  if (serverTracked > baseSeconds) {
    baseSeconds = serverTracked;
    lastSyncMs = Date.now();
  }
}

function getDisplaySeconds(): number {
  const elapsedS = Math.floor((Date.now() - lastSyncMs) / 1000);
  // Cap at one interval. If captures stall, the display freezes at
  // base + 60 instead of running unbounded. When the next credit
  // lands it equals the frozen value — no visible jump.
  return baseSeconds + Math.min(INTERVAL_S, elapsedS);
}

// Tick the UI once per second while recording.
const tickId = setInterval(() => {
  ui.timer.textContent = formatTime(getDisplaySeconds());
}, 1000);

// On pause/stop/compile: stop ticking and snap to the server value.
// Worst-case drop the user sees is one interval, never the full session.
function onSessionInactive() {
  clearInterval(tickId);
  ui.timer.textContent = formatTime(baseSeconds);
}
```

**Why the cap matters:** without it, the display runs at wall-clock rate forever and reveals the true (smaller) `trackedSeconds` only when the user clicks Stop. Users have reported this as "timer ran to 20 min, then dropped to 5 min on compile." With the 60s cap, the maximum visible drop is one capture interval.

**On stop:** read `trackedSeconds` from the `/stop` response and assign it to `baseSeconds` so the final display matches the server's committed value exactly.

### Session recovery after page refresh

On page load, read the token from the URL and call `GET /api/sessions/:token`:

- `pending` → show "Start Recording" button
- `active` → prompt user to re-share screen (the session is still going)
- `paused` → show "Resume" button
- `stopped` / `compiling` → show progress indicator, poll status
- `complete` → show video player
- `failed` → show error message

The `totalActiveSeconds` and `trackedSeconds` fields let you restore the timer display.

### Screen capture implementation

```javascript
// Request screen share (max 1080p, low framerate to save CPU)
const stream = await navigator.mediaDevices.getDisplayMedia({
  video: { width: { max: 1920 }, height: { max: 1080 }, frameRate: { ideal: 1 } },
  audio: false,
});

// Listen for user stopping share via browser UI
stream.getVideoTracks()[0].addEventListener('ended', onShareStopped);

// Create hidden video element
const video = document.createElement('video');
video.srcObject = stream;
video.muted = true;
await video.play();

// Capture a screenshot
function captureScreenshot(): Promise<Blob> {
  const canvas = document.createElement('canvas');
  const scale = Math.min(1920 / video.videoWidth, 1080 / video.videoHeight, 1);
  canvas.width = Math.round(video.videoWidth * scale);
  canvas.height = Math.round(video.videoHeight * scale);
  canvas.getContext('2d').drawImage(video, 0, 0, canvas.width, canvas.height);

  return new Promise(resolve => {
    canvas.toBlob(resolve, 'image/jpeg', 0.85);
  });
}
```

## Part 3: Get Info About a Session (After Recording)

Once a session is complete (or at any point), use the token you stored in Part 1
to fetch session details.

```bash
curl https://lookout.hackclub.com/api/sessions/TOKEN
```

Response:
```json
{
  "status": "complete",
  "trackedSeconds": 3540,
  "screenshotCount": 59,
  "startedAt": "2024-01-01T12:00:00.000Z",
  "totalActiveSeconds": 3600,
  "createdAt": "2024-01-01T11:50:00.000Z",
  "thumbnailUrl": "https://...",
  "videoUrl": "https://...",
  "videoWebmUrl": "https://...",
  "clientInfo": "Lookout Web (Fallout)/0.2.6 (macOS 14.3; Chrome 120.0)",
  "metadata": {"userId": "user_123", "projectId": "proj_456"}
}
```

Key fields for your backend:
- `trackedSeconds` — tamper-proof tracked time. Use this for time verification. Credit-mode sessions (default for clients ≥0.2.1) credit 60s per capture that lands within ±30s of the server-anchored expected mark; bucket-mode sessions use distinct minute-bucket count × 60.
- `screenshotCount` — number of confirmed screenshots
- `videoUrl` — presigned URL to the compiled MP4 timelapse
- `videoWebmUrl` — legacy URL retained for pre-0.2.0 clients; points at a static "please update" video (WebM encoding was dropped in 0.2.0)
- `thumbnailUrl` — presigned URL for the session thumbnail
- `clientInfo` — [client telemetry string](#client-telemetry) (which Lookout client/version/OS/browser recorded the session); `null` if none recorded
- `metadata` — the metadata you attached when creating the session

**Note:** To fetch multiple sessions at once, use `POST /api/sessions/batch` with a `{"tokens": ["token1", "token2", ...]}` body (max 100).

## Timings endpoint

`GET /api/sessions/:token/timings` returns the capture timestamps of every confirmed screenshot in a timelapse — i.e. *when* the session was recorded. It's token-authenticated like the other client endpoints, so the same token you stored in Part 1 works.

```bash
curl https://lookout.hackclub.com/api/sessions/TOKEN/timings
```

Response:
```json
{
  "status": "complete",
  "count": 59,
  "first": "2024-01-01T12:00:00.000Z",
  "last": "2024-01-01T12:59:00.000Z",
  "clientInfo": "Lookout Web (Fallout)/0.2.6 (macOS 14.3; Chrome 120.0)",
  "timestamps": [
    "2024-01-01T12:00:00.000Z",
    "2024-01-01T12:01:00.000Z",
    "2024-01-01T12:02:00.000Z"
  ]
}
```

- `timestamps` — ISO-8601, ascending. One entry per confirmed screenshot (~60s apart in steady state).
- `first` / `last` — convenience accessors (first/last element of the array); `null` for a session with no screenshots.
- `count` — number of timestamps (= confirmed screenshot count). **Not a count of minutes** — more than one capture can land in the same minute (retries, resume, jitter), so `count` can exceed the number of distinct minutes. Use `trackedSeconds` for tracked time.
- `clientInfo` — [client telemetry string](#client-telemetry) from the first screenshot; `null` if none recorded.

**⚠️ `last − first` is not the recorded duration.** Sessions can be paused and resumed, leaving gaps between consecutive timestamps, so that span is wall-clock elapsed time and **overstates** actual capture time. For tamper-proof tracked time use `trackedSeconds` from `GET /api/sessions/:token`.

**Availability:** timestamps are available for timelapses recorded from **~2026-05-26** onward. Older timelapses did not have timestamps collected and return `count: 0` with an empty `timestamps` array (even though the session is `complete` with a playable video).

**Timestamp precision:** for current recordings these are true capture times — the moment each frame was grabbed. Older legacy clients report a server-side receive time instead, which trails the true capture by upload latency.

### Hackatime integration

The `timestamps` array is what you forward to [Hackatime](https://hackatime.hackclub.com) as heartbeats. Your program should:

1. Fetch `GET /api/sessions/:token/timings` for the session.
2. Parse the `timestamps` array and map each ISO-8601 string to a Hackatime heartbeat (`time` = epoch seconds for that timestamp).
3. Set the **editor to `Lookout`** on every heartbeat, so the recorded time is attributed to that editor in Hackatime.
4. Forward the heartbeats to Hackatime.

Because captures are ~60s apart, the heartbeats reconstruct the session's active intervals, and Hackatime's own gap handling collapses pauses — so you don't need to special-case the paused gaps yourself. Send each timelapse's heartbeats once (e.g. after the session is `complete`) to avoid duplicates.

```ts
const LOOKOUT = "https://lookout.hackclub.com";
// Hackatime is Wakatime-compatible; this is its bulk-heartbeat endpoint.
const HACKATIME = "https://hackatime.hackclub.com/api/hackatime/v1";

async function forwardTimelapseToHackatime(token: string, hackatimeApiKey: string) {
  // 1. Pull the capture timestamps for this timelapse.
  const res = await fetch(`${LOOKOUT}/api/sessions/${token}/timings`);
  if (!res.ok) throw new Error(`timings request failed: ${res.status}`);
  const { timestamps } = (await res.json()) as { timestamps: string[] };
  if (timestamps.length === 0) return; // nothing recorded yet

  // 2. Map each capture to a Hackatime heartbeat.
  const heartbeats = timestamps.map((iso) => ({
    type: "file",
    entity: "timelapse",            // what shows up as the "file" in Hackatime
    category: "coding",
    editor: "Lookout",              // attribute the time to the Lookout editor
    time: Date.parse(iso) / 1000,   // epoch SECONDS (float), not millis
  }));

  // 3. Bulk-forward to Hackatime. Use the user's Hackatime API key.
  const post = await fetch(`${HACKATIME}/users/current/heartbeats.bulk`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${hackatimeApiKey}`,
    },
    body: JSON.stringify(heartbeats),
  });
  if (!post.ok) throw new Error(`hackatime push failed: ${post.status}`);
}
```

Notes:
- `time` must be **epoch seconds** (a float), not milliseconds — `Date.parse(iso)` returns millis, so divide by 1000.
- The `editor` field is what drives the "Lookout" attribution; keep `entity`/`project` stable per user or project so the time lands in one bucket.
- Run this once per session (after `complete`). If you must re-run, Hackatime de-dupes identical heartbeats by `time` + `entity`, but don't rely on it — track which sessions you've already forwarded.

> **Note:** The original screenshot images are only retained for 7 days after a session stops, after which the JPEGs are deleted from storage. The capture timestamps (and the compiled video and thumbnail) are kept.

## Edits and cuts

When a user stops a recording, the official clients offer three choices:
keep recording, save as recorded, or **review and cut first**. If they
choose to edit, they mark wall-clock stretches to remove and Lookout drops
those minutes from the video, the `/timings` heartbeats, and
`trackedSeconds` — all from one stored list of `{start, end}` intervals.

**You get this for free.** It ships inside the recorder, so any program
that redirects users to the hosted recorder, or embeds
`<LookoutRecorder>`, already has it: the stop button opens the choice
dialog, and picking "Edit & save" opens the editor as a modal over your
page. No code change, no new version to adopt, nothing to call. Both
dialogs render into `document.body`, so they aren't constrained by the
width of the container you put the recorder in.

The exception is a program driving the headless `useLookout()` hook with
its own recording UI. That UI owns its own stop button, so it opts in by
passing `actions.stop({ edit: true })` and rendering `<TimelapseEditor>`
(see the [SDK reference](../clients/react/API.md)).

What this means for your program:

- **Nothing in your integration changes, and nothing you read ever changes
  underneath you.** Editing happens *before* the session reaches
  `complete`: a session being edited stays `stopped`, and only flips to
  `complete` once the user's cuts are baked in. So the first time you see a
  finished session, its video, `trackedSeconds`, and `/timings` are final.
  There is no post-publication editing.
- **The lifecycle you observe is unchanged.** `stopped → compiling →
  complete` (or `stopped → complete`), exactly as before — an edit just
  means the session sits in `stopped` a little longer. The redirect hook
  still fires when the session completes, which is now also the moment the
  edits are in.
- **An abandoned edit can't strand a timelapse.** The hold is a lease the
  open editor renews, not a fixed deadline: editing takes as long as it
  takes, and once nothing is renewing it (window closed, app quit) the
  session publishes as recorded within about two minutes. It can delay
  publication, never cancel it. If you poll, treat a slightly longer
  `stopped` exactly as you always have.
- **Cuts only ever shrink the numbers.** A user cannot gain time by
  editing — removing footage removes its credit. The pre-edit value is
  available as `uncutTrackedSeconds` and the intervals as `cuts` on
  `GET /api/sessions/:token`; `?includeCut=true` on `/timings` returns the
  removed timestamps, if you want to audit or display them.
- **Cut footage is deleted immediately** once the edited timelapse
  publishes — the point of a cut is usually "I didn't mean to record that."
- **Opting out of the review step:** add `?edit=false` to the hosted
  recorder URL, or pass `<LookoutRecorder editing={false} />` in the React
  SDK. Stopping is then a single click, as before.
- **Matching your brand:** SDK embedders can pass
  `<LookoutProvider accentColor="#16a34a">` to replace Lookout's blue on
  primary buttons, focus rings, and progress. See the
  [SDK reference](../clients/react/API.md).

## Client telemetry

Every recording client reports a free-form **client info** string on each `upload-url` request (query param `clientInfo`). It's like an HTTP User-Agent but with Lookout-specific info — for telemetry and debugging. The server stores it opaquely (never parses it) and surfaces the session's first recorded value as `clientInfo` on `GET /api/sessions/:token`, the timings endpoint, and the internal admin endpoint.

Format (User-Agent–like): `Lookout <Type> [(<EmbeddedApp>)]/<version> (<OS> <version>[; <Browser> <version>])`

```
Lookout Desktop/0.2.6 (macOS 14.3)
Lookout Web (Fallout)/0.2.6 (macOS 14.3; Chrome 120.0)
Lookout Sdk (Stardance)/0.2.6 (Windows 10; Firefox 121.0)
```

How each client populates it:

- **Desktop** — type `Desktop`, app version + OS detected natively. No browser/embedded-app.
- **Web** (`@lookout/web`) — type `Web`, version + browser/OS auto-detected. The embedded host program comes from the `?app=` URL param on the recorder link (e.g. `…/session?token=…&app=Fallout`), or the `VITE_LOOKOUT_EMBEDDED_APP` build env var.
- **React SDK** (`@lookout/react`) — type `Sdk`, version + browser/OS auto-detected. Pass the host program via the `appName` prop on `<LookoutProvider appName="Fallout">`.

It's best-effort: a client omits anything it can't detect, the server truncates over 1024 chars, and a malformed value never fails an upload. `clientInfo` is `null` for sessions recorded before this existed or where no client sent one.

## "Open in <Program>" link

A timelapse recorded through Lookout usually has a home on your site — the
published page, the submission it belongs to. Set `viewUrl` and the desktop app
offers it as an **Open in *Program* ↗** action on the session view, next to the
timelapse's name. Without it there's no way back to your site except finding it
by hand.

Unlike [`redirectUrl`](#redirect-hook) and [`panelUrl`](#program-panels-in-app-forms),
this one is **mutable** — the page you want to link to usually doesn't exist
when the session is created:

```bash
# After the user publishes, point at the real thing:
curl -X POST https://lookout.hackclub.com/api/internal/sessions/SESSION_ID/view-url \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-api-key" \
  -d '{"viewUrl": "https://yourprogram.example/timelapses/8f3a2c"}'
```

Pass `{"viewUrl": null}` to clear it. You can also set it at creation if you
already know the URL. `http(s)`, max 2048 chars.

It's returned on `GET /api/sessions/:token` as `viewUrl`, and it opens in the
user's real browser — this is your whole site, not the single-purpose form a
panel renders in-app. The two pair up nicely: a panel collects the details,
then `view-url` gives the user a way back to what it produced.

The button's label uses your program's **display name** from the registry, so
set that (via the Lookout admin) if you want "Open in Lapse" rather than "Open
in lapse".

## Program panels (in-app forms)

> Optional, and independent of everything else here. A program with no
> `panelUrl` keeps using the [redirect hook](#redirect-hook) exactly as before.

If your program needs information when a timelapse finishes — a title, a
visibility choice, which project to credit the time to — the redirect hook
sends the user to your site in a browser tab to collect it. A **panel** puts
that same page in a sheet inside the desktop app instead, so there's no app
switch. Your page, your design, your validation; Lookout supplies a rectangle.

### When it opens

**As soon as the recording is saved — not when the video is ready.** The sheet
comes up over the compile progress, so the user answers your questions while
the timelapse builds instead of watching a progress bar first. For a session
the user chose to edit, "saved" means the moment they publish from the editor.

This has one consequence you must design for: **when your panel loads, the
video usually does not exist yet.** `GET /api/sessions/:token` will report
`compiling` (sometimes `stopped`) and `videoUrl: null`. So don't preview the
video in a panel, and don't block your form on it — take the answers, save
them, and let your own backend poll for `complete` afterwards if it needs the
file. `trackedSeconds`, `/timings` and the session's name are all available
immediately.

The redirect hook still fires on `complete` as always — but only for sessions
*without* a panel, since a panel is the same handoff done in-app and doing both
would send the user to a browser tab they already dealt with.

### Setting one up

Pass `panelUrl` when you create the session, alongside (not instead of)
`redirectUrl`:

```bash
curl -X POST https://lookout.hackclub.com/api/internal/sessions \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-api-key" \
  -d '{
        "metadata": {"userId": "user_123"},
        "panelUrl": "https://yourprogram.example/publish/8f3a2c9e1b...",
        "redirectUrl": "https://yourprogram.example/timelapses/42"
      }'
```

**The URL is the credential.** Make it unguessable and specific to this one
session — a random token in the path, the way `sessionUrl` works. Two reasons:

1. A framed page is a third-party context, so **your cookies will not reach
   it.** WebKit (macOS/Linux) and WebView2 both partition or block
   third-party cookies, so a cookie-authenticated panel just renders a login
   screen. Authenticate off the URL instead.
2. It means panels need no credential of their own, no pairing, and no
   handshake.

`panelUrl` must be `https`, except on `localhost`/`127.0.0.1` so you can
develop your panel against a local server. Max 2048 chars, immutable after
creation.

Sending it to a Lookout that predates panels is safe: unknown fields on this
endpoint are dropped, not rejected, so the session is created without a panel
and the redirect hook covers the flow as before. You do not need to gate the
field on a version check. It's returned on `GET /api/sessions/:token` and
`GET /api/sessions/:token/status`, both token-authenticated.

### Talking to the app

The panel drives the sheet over `postMessage`. Send to `window.parent`:

```javascript
// You have something worth looking at. The app's spinner comes off.
parent.postMessage({ type: "lookout:ready" }, "*");

// Grow/shrink the sheet to fit your content. No upper bound beyond the app window.
parent.postMessage({ type: "lookout:resize", height: document.body.scrollHeight }, "*");

// You're finished. The sheet closes and never re-offers itself.
parent.postMessage({ type: "lookout:done" }, "*");

// The user backed out inside your UI. Same as them closing the sheet.
parent.postMessage({ type: "lookout:cancel" }, "*");
```

Send `ready` when your panel is *usable*, not when it loads — after the fetch
that fills your form, not on mount. Until it arrives the app covers the frame
with its own spinner, so `load` firing over an empty skeleton shows nobody a
skeleton. If you never send it the cover lifts on a timeout (a few seconds)
and whatever you have is what the user sees.

Your frame is live behind that cover, not hidden, so timers, animation frames
and network requests all run normally while it is up.

Send `resize` whenever your content height changes — a multi-step form should
send it on every step, and the sheet springs between sizes. A panel that never
sends it just gets the 220px minimum.

Measure with `Math.ceil(document.body.getBoundingClientRect().height)`, inside
`requestAnimationFrame` so layout has settled. The two obvious alternatives are
both wrong:

- `document.documentElement.scrollHeight` is floored at the frame's current
  height, because `<html>` fills it. Your sheet would grow and never shrink —
  step 2 of a form could never be shorter than step 1.
- `document.body.scrollHeight` omits the last child's bottom margin, leaving
  the frame a few pixels short of its content and adding a scrollbar you didn't
  ask for.

There is no height cap: the sheet is as tall as you ask for. The app window is
the only bound, and if you exceed it the sheet scrolls — which works, but a
panel taller than someone's window is worth paging instead, the way the app's
own flows do.

Anything else is ignored, and messages are accepted only from `panelUrl`'s
exact origin.

### What the app guarantees

- **The frame is sandboxed** (`allow-scripts allow-forms allow-same-origin
  allow-popups`). Notably absent is `allow-top-navigation`: a panel cannot
  navigate the app window. Use `target="_blank"` (or `lookout:done` plus your
  `redirectUrl`) if you need to send someone to a real browser tab.
- **No access to Lookout internals.** A cross-origin frame has its own JS
  realm and the desktop app's IPC is main-frame only, so there is nothing of
  Lookout's to reach from inside a panel.
- **Your panel is attributed.** The sheet shows your program name and the
  panel's origin above the frame, so it's never ambiguous whose UI it is.
- **There is always a way out.** If your panel fails to load, errors, or
  hasn't loaded within 12 seconds, the app offers `redirectUrl` (or `panelUrl`)
  in the real browser instead. Set `redirectUrl` too — it's the fallback.
- **Dismissal is not loss.** If the user closes the sheet without your page
  sending `lookout:done`, the ask persists as a card on the session's page
  ("*Program* needs a few details") that reopens the panel.

### Telling the app you got what you needed

`lookout:done` retires the card on *that device, in that moment*. It is not
enough on its own, because the user can just as easily finish on your website —
they hit the browser fallback, or come back to it tomorrow, or use a second
machine. The app can see inside neither your sheet nor your site, so it would
keep showing "needs a few details" for something already done.

So whenever you consider a session's panel satisfied — from the panel, from
your own web UI, from a background job, doesn't matter — tell the server:

```bash
curl -X POST https://lookout.hackclub.com/api/internal/sessions/SESSION_ID/panel-resolved \
  -H "X-API-Key: your-api-key"
```

Takes no body. If your HTTP client always sets `Content-Type: application/json`
— most do — send `{}` rather than nothing: an empty body under a JSON content
type is rejected by the framework before the route is reached. The internal API
now treats that case as `{}` anyway, but a client sending `{}` works against
every version.

Idempotent, and a no-op success on a session that has no panel — so you can
call it unconditionally wherever you mark a timelapse published, without
branching. Once set, `GET /api/sessions/:token` reports
`"panelResolved": true` and every client stops offering the panel.

Two more things worth doing:

- **Make your panel idempotent.** The card reopens the same `panelUrl`. If the
  session is already published, don't show the form again — post
  `lookout:done` immediately and let the sheet close.
- **Send `lookout:done` as well as calling the endpoint.** The message is
  instant and local; the endpoint is authoritative and covers every other
  device. They're complements, not alternatives.

### Light and dark

The app appends **`lookout_theme=light`** or **`lookout_theme=dark`** to your
panel URL's query when it loads the frame. Read it and colour accordingly — the
app follows the OS theme, so a dark-only panel is a dark slab on a light sheet.

It arrives in the URL rather than by message on purpose: a `postMessage` can
only reach you after the frame has loaded, by which point you have already
painted, and correcting it afterwards *is* the flash. Query params are in force
before your first byte of CSS.

Only the query is touched, so your origin — and everything keyed off it — is
unchanged. Existing params on your `panelUrl` are preserved.

### Making it look like it belongs

**Paint no background, from the very first paint.** Set
`html, body { background: transparent }` in a stylesheet, not from script — a
`useEffect` (or anything else that runs after mount) is one paint too late, and
that paint is the flash. Watch for a framework default here: a global
`html, body { background: … }` rule, a `min-height: 100vh` (which floors your
height report so the sheet can only ever grow) or a `display: flex` on `body`
will all need overriding for this route. Let the sheet be the surface — your content then sits directly on the app's own
panel material instead of reading as a rectangle pasted into it. The frame is
transparent by default, so this is just about not filling it yourself.

The sheet supplies the outer padding-free surface, the rounded top corners, the
grabber and the title row, so skip your own page chrome: no full-page
background, no card wrapper around everything, no duplicate heading with your
program's name. Style your controls to sit on a dark or light surface — the app
follows the OS theme, so avoid hard-coding a background colour you then depend
on for contrast.

### Rules

- **Never ask for credentials in a panel.** No password fields, no "sign in to
  continue". The panel renders inside Lookout's window, so a login form there
  is indistinguishable from Lookout asking — that's a phishing shape, and
  panels that do it will be pulled from the registry. Authenticate via the
  session-scoped URL.
- **Don't rely on cookies or persistent storage** in the frame; assume both are
  partitioned or absent.
- **Degrade gracefully.** The same URL will sometimes be opened in a real
  browser tab (the fallback paths above), so it must work standalone too.

### One thing to drop first

Lookout already asks the user to name their timelapse when they stop
recording, and stores it — `GET /api/sessions/:token` returns `name`. If your
panel opens with a "give your timelapse a title" field, it's asking for
something they typed a minute earlier. Read the session's `name` and prefill
or skip it.

## Desktop instant start (device pairing)

> Optional. Every program works in the desktop app's + menu with nothing but
> a `newSessionUrl` — the app opens your site in the browser, you create a
> session and redirect to `lookout://session/?token=…`. Implement this
> section only if you want to remove that browser hop for repeat sessions.

The browser hop exists because only your backend can create sessions and only
your website knows which user is asking. Device pairing keeps both facts true
while paying the hop **once per device instead of once per timelapse**: the
first start opens a consent page on your site (where the user is already
logged in); every start after that is a single authenticated POST from the
desktop app to your backend.

Lookout itself gains no user model from this. The device credential is minted
by you, stored by the desktop app, presented only to you, and revocable by
you. Lookout's only involvement is carrying two extra URLs in its public
program registry and answering the app's verification GET at the end.

### What you implement

Two endpoints on **your** backend, registered in the Lookout program registry
as `pairUrl` and `startUrl` (both must be `https`; both must be set together
— ask the Lookout admin to set them on your program entry):

```
GET    {pairUrl}?challenge=…&state=…&device=…    consent page (browser)
POST   {pairUrl}   {code, verifier}              code → device token exchange
DELETE {pairUrl}   Authorization: Bearer <tok>   revoke this device
POST   {startUrl}  Authorization: Bearer <tok>   mint a session, return its token
```

### The pairing flow

1. The user picks your program in the desktop + menu. The app generates a
   random `verifier`, and opens the OS browser at:

   ```
   {pairUrl}?challenge=<b64url(sha256(verifier))>&state=<nonce>&device=<label>
   ```

2. Your consent page authenticates the user with whatever you already have
   (your session cookies — that's the whole point), shows one line of consent
   ("Link *{device}* to your account? It will be able to start Lookout
   sessions as you."), and on accept:
   - stores `{user, challenge, code, expiresAt}` where `code` is a fresh
     single-use random string with a short TTL (≤ 5 minutes),
   - redirects to `lookout://pair?code=<code>&state=<state>` — echo `state`
     back **exactly**; the app drops callbacks whose state matches nothing.
     Hardcode this redirect target. Do not accept a redirect URL as a request
     parameter, or your consent page becomes an open redirector.

3. The desktop app exchanges the code:

   ```
   POST {pairUrl}
   Content-Type: application/json

   {"code": "<code>", "verifier": "<verifier>"}
   ```

   Verify the code is unexpired and unused, check
   `b64url(sha256(verifier)) == challenge`, burn the code, and respond:

   ```json
   {"deviceToken": "<opaque credential, ≤ 4096 chars>"}
   ```

   The PKCE check means a leaked/intercepted `code` (the deep link travels
   through OS plumbing any app could register) is unredeemable without the
   verifier, which never left the desktop app.

4. Show the device in the user's account settings on your site, with a
   revoke button. Treat the token like a password: store a hash, not the
   value.

### Starting a session

```
POST {startUrl}
Authorization: Bearer <deviceToken>
```

Resolve the token to its user, create a Lookout session exactly the way your
web flow does (your internal API key, your metadata, your redirectUrl), store
the session token against the user as usual, and respond:

```json
{"sessionToken": "<the 64-hex Lookout session token>"}
```

Return `401` if the device token is revoked/expired/unknown — the app then
drops the credential and re-runs the consent flow. Any other failure makes
the app fall back to opening your `newSessionUrl` in the browser, so a broken
`startUrl` degrades to the old flow rather than a dead end.

The desktop app then verifies the token against Lookout before recording
(`GET /api/sessions/:token` must report `program` = your registry name and a
recordable status), so handing it a token from some other program or a
finished session doesn't work.

### Rules

- **Scope the credential to exactly one capability**: "create a Lookout
  session for this user". It must not authorize anything else on your site.
- **Make it revocable** from your own device list. `DELETE {pairUrl}` with the
  bearer token must also revoke (the app calls it on Settings → Linked
  Programs → Unlink, best-effort).
- **Rate limit `startUrl`** per device token as you see fit; the app calls it
  once per user gesture.
- Expire pairing codes fast and make them single-use. Expiring device tokens
  is fine too — the app re-pairs on `401` at the cost of one browser hop.

### Reference implementation (Express-ish pseudocode)

```typescript
import { createHash, randomBytes } from "node:crypto";

const b64url = (b: Buffer) => b.toString("base64url");
const sha256 = (s: string) => createHash("sha256").update(s).digest();

// GET /lookout/pair — consent page (behind your normal login)
app.get("/lookout/pair", requireLogin, (req, res) => {
  const { challenge, state, device } = req.query;
  res.render("lookout-consent", { challenge, state, device });
});

// The consent form's accept handler
app.post("/lookout/pair/accept", requireLogin, async (req, res) => {
  const { challenge, state } = req.body;
  const code = b64url(randomBytes(24));
  await db.pairingCodes.insert({
    code, challenge, userId: req.user.id,
    device: req.body.device, expiresAt: minutesFromNow(5),
  });
  res.redirect(`lookout://pair?code=${code}&state=${encodeURIComponent(state)}`);
});

// POST /lookout/pair — code → device-token exchange (no cookies; the app calls this)
app.post("/lookout/pair", async (req, res) => {
  const { code, verifier } = req.body;
  const row = await db.pairingCodes.takeUnexpired(code); // atomically burn it
  if (!row || b64url(sha256(verifier)) !== row.challenge)
    return res.status(400).json({ error: "invalid code" });
  const deviceToken = b64url(randomBytes(32));
  await db.devices.insert({
    tokenHash: sha256(deviceToken), userId: row.userId,
    label: row.device, createdAt: new Date(),
  });
  res.json({ deviceToken });
});

// DELETE /lookout/pair — revoke (also expose this in your account settings UI)
app.delete("/lookout/pair", async (req, res) => {
  await db.devices.deleteByTokenHash(sha256(bearerToken(req)));
  res.status(204).end();
});

// POST /lookout/start — mint a session for a paired device
app.post("/lookout/start", async (req, res) => {
  const device = await db.devices.findByTokenHash(sha256(bearerToken(req)));
  if (!device) return res.status(401).json({ error: "unknown device" });
  // Exactly your existing web flow, minus the browser:
  const session = await lookout.createSession({
    metadata: { userId: device.userId },
  });
  await db.sessions.insert({ userId: device.userId, token: session.token });
  res.json({ sessionToken: session.token });
});
```

## Trust Model

| What | Trusted? | Why |
|------|----------|-----|
| Session creation | Yes — server-to-server with API key | Only your backend can create sessions |
| Capture timestamps | No — server records its own timestamp when `GET /upload-url` is called | Client can't fake when a screenshot was taken |
| Upload verification | No — server calls `HeadObject` on R2 to verify the file exists | Client can't claim uploads it didn't make |
| Time tracking | No — credit-mode sessions credit 60s per capture landing within ±30s of the server's streak anchor; bucket-mode is distinct minute buckets × 60. Mode is sticky per session and decided by the first upload. | Server-side anchor + window math; clients can't fake credits |
| Pause/resume | Partially trusted | Server auto-pauses after 10 min without uploads, auto-stops after 24 h |
| Rate limiting | Server-enforced | Max 10 upload-url + 20 confirm requests per minute per session, max 720 confirmed screenshots, max 4320 total upload-url requests per session |

## API Reference

This guide covers the **integration flow and client responsibilities**. For the complete HTTP API — every endpoint, request/response shape, query param, error code, and rate limit — see the server package's reference, which is the source of truth:

**→ [`packages/server/API.md`](../packages/server/API.md)**

## Requirements for all clients

Now that you've seen the full flow: whatever you build or embed — the desktop app, the web recorder, the React SDK, or your own client — it **must** follow these. They are not optional polish; skipping them is the difference between "it works" and silent, unexplained failures that are miserable to debug.

1. **Never fail silently — surface every error and warning.** If getting an upload URL, the R2 PUT, or the confirm fails, either show it to the user **or** log it with enough detail to diagnose: the HTTP status, the endpoint, and the response body. The single worst outcome is a user watching a recording that isn't actually capturing while **nothing reports an error**. A stalled capture loop must be a loud, visible problem — not a quiet one people only discover when the timelapse comes out empty. **The session token is a secret credential** — it grants full control of the session, so never log it in full or expose it in user-facing errors; log a session ID or a truncated/redacted token instead.

2. **Use credit mode — do not use bucket mode.** Send `capturedAt` (ISO-8601, stamped at the instant the frame is grabbed) on **every** `upload-url` request. Its presence on a session's first upload sticks it into **credit mode** for life; its absence drops the session into legacy **bucket mode**, which miscounts time whenever two captures land in the same minute (jitter, retries, late uploads). Bucket mode exists only for compatibility with old shipped binaries — new clients must not rely on it. See [Tracking Modes](../packages/server/API.md#tracking-modes).

3. **Retry every network leg, with reasonable backoff.** Each of the three legs (get upload URL, R2 PUT, confirm) should retry on transient failure with exponential backoff (e.g. 2s → 4s → 8s, ~3 attempts). Treat `409` (session paused/stopped) as terminal, not retriable. Confirmation is idempotent, so retrying it is safe. Don't hammer on failure, and don't give up after one try. See [Upload resilience](#upload-resilience).

4. **Use the batch API when reading multiple sessions.** For gallery/dashboard views, fetch with a single `POST /api/sessions/batch` (up to 100 tokens) instead of N separate `GET /api/sessions/:token` calls — fewer round trips and one shared rate-limit bucket.

5. **Keep the client clock accurate.** `capturedAt` must be within **±5 minutes of server time and strictly monotonic** across *confirmed* captures (a failed upload's stamp may be reused by its retry), or the server rejects it with a `400` (`captured_at_future`, `captured_at_too_old`, `captured_at_not_monotonic`, …). A skewed device clock silently breaks credit-mode tracking. Every `upload-url`/confirm response carries `serverTime` — use it to detect skew, and schedule the next capture from `nextExpectedAt` (never a fixed `setInterval`).

6. **Honor `429` and the `Retry-After` header.** Endpoints are rate-limited (upload-url 10/min, confirm 20/min, etc.) and a throttled response sets `Retry-After: <seconds>`. Back off for exactly that long rather than retrying blindly — blind retries dig you deeper into the limit. See [Rate Limiting](../packages/server/API.md#rate-limiting).

7. **Expect the server to pause/stop sessions on its own.** A session with no uploads is **auto-paused after 10 minutes** and **auto-stopped after 24 hours**. So "captures stopped" can be the server's doing, not a bug in your client — poll `GET /api/sessions/:token` and reconcile when the session changed state underneath you (see [Session recovery](#session-recovery-after-page-refresh)).

### Good to know

- **Sessions have a hard ceiling.** Max **720 confirmed screenshots** (~12 h at 60 s) and **4320** upload-url requests per session; screenshots must be **`image/jpeg` ≤ 2 MB**. Past these, requests return `429`/`400` — long recorders should expect the session to end.
- **Embed the permanent media URLs, never the presigned R2 URLs.** Use `GET /api/media/:sessionId/video.mp4` and `…/thumbnail.jpg` (stable, safe in `<img>`/`<video>`). Presigned R2 URLs expire (2 min upload, 1 h media). See [Permanent Media Redirects](../packages/server/API.md#permanent-media-redirects).
- **CORS is allowlisted** to `*.hackclub.com`, `localhost:*`, and `tauri://`. A web client served from any other origin will be blocked — host apps embedding the recorder need to be on an allowed origin.
- **Report `clientInfo` telemetry** so a broken integration isn't blind to debug. See [Client telemetry](#client-telemetry).