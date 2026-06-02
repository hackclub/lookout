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
- `sessionUrl` — a convenience URL you can redirect the user to.
- `metadata` — any JSON you want to associate with the session (user info, project, etc.)

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
| POST | `/api/sessions/:token/screenshots` | Confirm upload. Body: `{ screenshotId, width, height, fileSize }`. Returns `{ confirmed, trackedSeconds, nextExpectedAt }`. Server verifies R2 object exists. Rate limited: 20/min per token. |
| POST | `/api/sessions/:token/pause` | Pause session |
| POST | `/api/sessions/:token/resume` | Resume session |
| POST | `/api/sessions/:token/stop` | Stop session, trigger compilation |
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

## Trust Model

| What | Trusted? | Why |
|------|----------|-----|
| Session creation | Yes — server-to-server with API key | Only your backend can create sessions |
| Capture timestamps | No — server records its own timestamp when `GET /upload-url` is called | Client can't fake when a screenshot was taken |
| Upload verification | No — server calls `HeadObject` on R2 to verify the file exists | Client can't claim uploads it didn't make |
| Time tracking | No — credit-mode sessions credit 60s per capture landing within ±30s of the server's streak anchor; bucket-mode is distinct minute buckets × 60. Mode is sticky per session and decided by the first upload. | Server-side anchor + window math; clients can't fake credits |
| Pause/resume | Partially trusted | Server auto-pauses after 10 min without uploads, auto-stops after 24 h |
| Rate limiting | Server-enforced | Max 10 upload-url + 20 confirm requests per minute per session, max 720 confirmed screenshots, max 4320 total upload-url requests per session |