# Lookout Server API Documentation

**Framework:** Fastify v5
**Base URL:** `http://localhost:3000` (configurable via `PORT` and `BASE_URL` env vars)

---

## Authentication

### Public Routes (Session Token)
Public endpoints use a 64-character hex **session token** as a path parameter. No header-based auth required.

### Internal Routes (API Key)
Internal endpoints require the `X-API-Key` header carrying a **per-program key** managed in the admin dashboard. Sessions created with a program key are tagged with that program's name (`session.program`), for attribution/tracking only — all keys grant identical access. There is no global/shared key (the legacy `INTERNAL_API_KEY` / `GLOBAL_API_KEY` has been retired); an unrecognized key is rejected with `401`.

### Admin Dashboard (`/admin`)
A minimal HTTP Basic Auth dashboard for CRUD over program keys, gated by `ADMIN_USERNAME` / `ADMIN_PASSWORD`. If either is unset, all admin routes return `503 { "error": "admin disabled" }`.

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin` | HTML dashboard |
| `GET` | `/api/admin/keys` | List keys `{ id, name, key, lastUsedAt, createdAt }` |
| `POST` | `/api/admin/keys` | Create a program key `{ name }` → `{ id, name, key }` (409 on duplicate name) |
| `DELETE` | `/api/admin/keys/:id` | Delete a key (404 if missing) |

---

## Rate Limiting

In-memory sliding window (60-second windows). Rate-limited responses return:

- **Status:** `429 Too Many Requests`
- **Header:** `Retry-After: <seconds>`
- **Body:** `{ "error": "Rate limit exceeded" }`

| Endpoint | Limit | Key |
|----------|-------|-----|
| `GET /api/sessions/:token` | 60 req/min | per token |
| `PATCH /api/sessions/:token/name` | 20 req/min | per token |
| `GET /api/sessions/:token/upload-url` | 10 req/min | per session ID |
| `POST /api/sessions/:token/screenshots` | 20 req/min | per token |
| `POST /api/sessions/:token/pause` | 10 req/min | per token |
| `POST /api/sessions/:token/resume` | 10 req/min | per token |
| `POST /api/sessions/:token/stop` | 10 req/min | per token |
| `GET /api/sessions/:token/status` | 60 req/min | per token |
| `GET /api/sessions/:token/timings` | 30 req/min | per token |
| `GET /api/sessions/:token/video` | 30 req/min | per token |
| `GET /api/sessions/:token/thumbnail` | 30 req/min | per token |
| `POST /api/sessions/batch` | 30 req/min | per IP |
| `GET /api/media/:sessionId/thumbnail.jpg` | 60 req/min | per session ID |
| `GET /api/media/:sessionId/video.mp4` | 30 req/min | per session ID |
| `GET /api/media/:sessionId/video.webm` | 30 req/min | per session ID |

---

## Error Format

All errors follow this shape:

```json
{
  "error": "Error message"
}
```

| Status | Meaning |
|--------|---------|
| 400 | Bad request / validation error |
| 401 | Unauthorized (invalid API key) |
| 404 | Resource not found |
| 409 | Conflict (invalid state transition) |
| 429 | Rate limited |
| 500 | Server error |

---

## Session States

```
pending → active → paused → active → stopped → compiling → complete
                                   ↘              ↗
                                    stopped ──────
                                                  ↘ failed
```

Valid states: `pending`, `active`, `paused`, `stopped`, `compiling`, `complete`, `failed`

State transitions use optimistic locking — if a concurrent request changes the session state between read and update, the server returns `409 Conflict` with the message `"Session state changed concurrently, please retry"`.

---

## Tracking Modes

`trackedSeconds` is computed by one of two server-side algorithms. Mode is decided by the **first** upload of a session and stays sticky for the session's lifetime.

### Bucket mode (legacy, pre-0.2.1 clients)

- Activated when the first `GET /upload-url` request omits `capturedAt`.
- `trackedSeconds = (distinct confirmed minute buckets − 1) × 60`, where `minuteBucket = floor((serverNow − startedAt) / 60_000)`.
- Two captures landing in the same server-receive minute count as one bucket.
- Subsequent uploads can send `capturedAt` — the server stores it for debugging but won't flip the mode.

### Credit mode (0.2.1+ clients)

- Activated when the first `GET /upload-url` request includes `capturedAt`.
- A **streak anchor** is set to the seed capture's `capturedAt`; the server then expects each subsequent capture at `anchor + (creditedCount + 1) × 60s`.
- If `|capturedAt − expectedAt| ≤ 30s`: credit 60s, increment `creditedCount`, anchor unchanged.
- Else: credit 0s, reset anchor to this `capturedAt`, `creditedCount = 0`. Subsequent captures rebuild a streak from there.
- `trackedSeconds` is maintained incrementally on `sessions.tracked_seconds` — not recomputed from screenshots.
- Pause + resume clears the streak so the post-resume seed capture doesn't burn a 60s credit.
- Trust envelope: `capturedAt` must fall within `serverNow ± 5min` and be strictly monotonic.

### Why two modes exist

Pre-0.2.1 the bucket count caused timer jump-back when two captures arrived in the same minute (network jitter, late uploads). Credit mode anchors the math to the client's capture time so jitter that stays inside the ±30s window credits cleanly. Bucket mode is retained for compat with currently-shipped binaries — both run side by side on the same database.

### Client display guidance

- Trust `trackedSeconds` from the confirm response as ground truth. Do not derive a display value from `uploads.completed * intervalSeconds` — in credit mode, not every successful upload credits, and the derivation over-counts.
- Cap any client-side interpolation at one capture interval (60s) ahead of the last server credit. This bounds the worst-case drop at stop/compile to 60s, never the full session length.
- Schedule the next capture from each confirm's `nextExpectedAt`; the math behind it stays anchored to the original streak so individual upload jitter doesn't accumulate drift.

---

## Clips

Each capture unit is a **clip** by default: a per-minute video file (~6 frames captured 10s apart, WebM from Chromium/Firefox MediaRecorder, MP4 from Safari and desktop) that compiles into a 6×-smoother timelapse. Pass `"clips": false` on the [internal create endpoint](#create-session) to opt a session out and pin it to the legacy one-JPEG-per-minute payload.

A clip is still **one capture unit** — one `upload-url` request, one R2 PUT, one confirm per minute. Nothing about rate limits, session caps, credit/bucket tracking math, `trackedSeconds`, `screenshotCount`, or the `/timings` endpoint changes with clips: one confirmed unit per minute, one timestamp per minute.

**Contract:**

- **Session-level, immutable opt-out.** `clips_enabled` defaults to true, is set at creation, and is enforced server-side on every `upload-url`. It cannot be changed later — a session's capture character never changes mid-recording.
- **Capability discovery before the first upload.** `GET /api/sessions/:token` returns `clipsEnabled` and `frameIntervalMs`. A clip-capable client checks these on its session-recovery fetch and, when enabled, records clips from the very first upload — timelapses start with motion, not a still frame.
- **Granted format is law.** The client requests a format with `?format=webm|mp4`; the response's `format` is what the server *granted* (clip requests on non-clips sessions silently downgrade to `jpeg`). The presigned URL is signed with the granted format's content type, so uploading anything else fails the signature. Confirm re-validates the stored object's content type against the granted format.
- **Server-authoritative cadence.** `frameIntervalMs` (default 10000 = 6 frames/min) is dictated by the server; clients capture at exactly that rate and expose no override. Clips are VFR — a static screen legitimately produces fewer encoded frames, and the compiler derives real counts by demuxing (the confirm body's `frameCount` is telemetry only).
- **Size cap:** clips are validated at ≤ 4 MB via HeadObject (clients cap their encoder at ~400 kbps ≈ 3 MB/min worst case; static screen content lands far below since VBR undershoots easy content).
- **Mixed sessions are legal.** A clip client that hits an encoder hiccup falls back to a JPEG for that minute; the compiler handles formats per capture unit.

Sessions without the flag — and every pre-clips client — behave exactly as before.

---

## Client Info

Recording clients report a free-form **client telemetry string** on every `upload-url` request (query param `clientInfo`). It is **not** the HTTP `User-Agent` — it's explicit info the Lookout client builds for telemetry/debugging. The server stores it opaquely per screenshot (**never parses it**) and surfaces the session's first recorded value on session-info endpoints (`GET /api/sessions/:token`, `GET /api/sessions/:token/timings`, internal admin).

Recommended (not enforced) format — User-Agent–like, encoding Lookout type, version, embedded host app (web/SDK), OS, and browser (web/SDK):

```
Lookout Desktop/0.2.6 (macOS 14.3)
Lookout Web (Fallout)/0.2.6 (macOS 14.3; Chrome 120.0)
Lookout Sdk (Stardance)/0.2.6 (Windows 10; Firefox 121.0)
```

- **Type** — `Desktop`, `Web`, `Sdk`, … (which Lookout client).
- **Embedded app** — for web/SDK, the host program Lookout runs inside (e.g. `Fallout`), in parentheses after the type.
- **Version** — the Lookout client version.
- **Environment** — OS type+version, and for web/SDK the browser type+version.

The value is best-effort: a client that can't detect part of this omits it; the server truncates anything over 1024 chars and never rejects an upload over a malformed `clientInfo`. It is `null` on responses for sessions recorded before this existed or where no client sent one.

---

## Public Endpoints

### Get Session Status

```
GET /api/sessions/:token
```

Returns the current state of a session.

**Path Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `token` | string | 64-char hex session token |

**Response `200 OK`:**
```json
{
  "status": "active",
  "trackedSeconds": 123,
  "screenshotCount": 45,
  "clientInfo": "Lookout Web (Fallout)/0.2.6 (macOS 14.3; Chrome 120.0)",
  "startedAt": "2024-01-01T12:00:00.000Z",
  "totalActiveSeconds": 300,
  "createdAt": "2024-01-01T11:50:00.000Z",
  "thumbnailUrl": "https://...",
  "videoUrl": "https://...",
  "clipsEnabled": false,
  "frameIntervalMs": 4000,
  "redirectUrl": null,
  "metadata": {}
}
```

`clientInfo` is the [client telemetry string](#client-info) recorded on the session's **first** screenshot upload. It is `null` for sessions recorded before this was added, or where the client sent none.

`clipsEnabled` / `frameIntervalMs` are the [clips](#clips) capability signal. This endpoint is the session-recovery fetch clients make before recording, so a clip-capable client knows **before its first capture** whether to record clips (and at what cadence) — the very first upload of a clips session is already a clip.

`redirectUrl` is the session's [redirect hook](#create-session) (`null` when unset): clients watching the compile open it once the status flips to `complete`.

---

### Rename Session

```
PATCH /api/sessions/:token/name
```

Updates the session's display name. Allowed at any session status.

**Path Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `token` | string | 64-char hex session token |

**Request Body:**
```json
{ "name": "My new name" }
```

| Field | Type | Required | Validation |
|-------|------|----------|------------|
| `name` | string | yes | 1–255 chars |

**Response `200 OK`:**
```json
{ "name": "My new name" }
```

**Errors:**
- `400` — Body missing `name` or out of length range
- `404` — Session not found
- `429` — Rate limit exceeded

---

### Get Presigned Upload URL

```
GET /api/sessions/:token/upload-url?capturedAt=<iso8601>
```

Generates a presigned PUT URL for uploading a screenshot to R2. Activates pending sessions on first call.

**Path Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `token` | string | 64-char hex session token |

**Query Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `capturedAt` | ISO-8601 (optional) | Client-attested moment the frame was grabbed. Presence on the **first** upload of a session sticks it to **credit mode** for life; absence sticks it to **bucket mode**. Subsequent uploads on a credit-mode session **must** include it. Must fall within ±5 min of server time and must be strictly monotonic across uploads. |
| `clientInfo` | string (optional) | [Client telemetry string](#client-info) (User-Agent-like). Stored opaquely per screenshot; the session's first non-empty value is surfaced on session-info endpoints. Best-effort — never parsed or validated, silently truncated to 1024 chars; an invalid/oversized value never fails the upload. |
| `format` | `jpeg` \| `webm` \| `mp4` (optional) | Payload format for this capture unit. Omitted = `jpeg` (legacy single frame). `webm`/`mp4` request a [clip](#clips) upload. The response's `format` is the **granted** format — clip requests on sessions without clips enabled are silently downgraded to `jpeg`, and the client must upload exactly what was granted (the presigned URL is signed with that content type). |

**Response `200 OK`:**
```json
{
  "uploadUrl": "https://r2.cloudflarestorage.com/...",
  "r2Key": "screenshots/{sessionId}/{screenshotId}.jpg",
  "screenshotId": "uuid",
  "minuteBucket": 1,
  "nextExpectedAt": "2024-01-01T12:01:00.000Z",
  "serverTime": "2024-01-01T12:00:00.000Z",
  "trackingMode": "credit",
  "format": "jpeg",
  "clipsEnabled": false,
  "frameIntervalMs": 4000
}
```

`nextExpectedAt` is the server's authoritative target for the **next** capture's `capturedAt` — clients should schedule from it (see Tracking Modes below). `format` is the granted payload format (see [Clips](#clips)); `r2Key` carries the matching extension (`.jpg`/`.webm`/`.mp4`).

**Errors:**
- `400` — `captured_at_future`, `captured_at_too_old`, `captured_at_before_session_start`, `captured_at_not_monotonic`, `captured_at_invalid`, or `credit_mode_requires_captured_at`
- `404` — Session not found
- `409` — Session not in `pending` or `active` state
- `429` — Rate limit exceeded, or max upload requests per session reached (4320)

**Notes:**
- Presigned URL expires after 2 minutes
- Client should PUT the image/clip directly to `uploadUrl` with the granted format's content type
- Max 4320 upload requests per session
- Pre-0.2.1 binaries that don't send `capturedAt` continue to receive a usable response — additive fields (`serverTime`, `trackingMode`, `format`, `clipsEnabled`, `frameIntervalMs`) are gracefully ignored

---

### Confirm Screenshot Upload

```
POST /api/sessions/:token/screenshots
```

Confirms that a screenshot was successfully uploaded to R2. The server verifies the object exists and validates its content type and size.

**Path Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `token` | string | 64-char hex session token |

**Request Body:**
```json
{
  "screenshotId": "uuid",
  "width": 1920,
  "height": 1080,
  "fileSize": 125000,
  "frameCount": 20
}
```

| Field | Type | Required | Validation |
|-------|------|----------|------------|
| `screenshotId` | string (UUID) | yes | Must match a pending screenshot |
| `width` | integer | yes | ≥ 1 |
| `height` | integer | yes | ≥ 1 |
| `fileSize` | integer | yes | ≥ 1 |
| `frameCount` | integer | no | 1–600. Frames inside an uploaded [clip](#clips); informational (the compiler demuxes for the real count). Omit for JPEG captures. |

**Response `200 OK`:**
```json
{
  "confirmed": true,
  "trackedSeconds": 123,
  "nextExpectedAt": "2024-01-01T12:01:00.000Z",
  "serverTime": "2024-01-01T12:00:00.000Z"
}
```

`trackedSeconds` here is the **server's authoritative count after this capture has been credited (or not)**. Use this value to drive your timer display — see the [Tracking Modes](#tracking-modes) section for client display guidance. `nextExpectedAt` is the target for the next capture's `capturedAt`.

**Errors:**
- `400` — Content type doesn't match the granted format (`image/jpeg` / `video/webm` / `video/mp4`), file too large (2 MB for JPEG, 8 MB for clips), or object not found in R2
- `404` — Session or screenshot not found
- `409` — Session not in `pending` or `active` state
- `429` — Rate limit exceeded, or max confirmed screenshots reached (720)

**Notes:**
- Idempotent — confirming an already-confirmed screenshot returns success with current `trackedSeconds` and a freshly computed `nextExpectedAt`
- In credit mode the credit decision (60 vs 0) is recorded on the row as `credited_seconds`; the response only exposes the cumulative `trackedSeconds`

---

### Pause Session

```
POST /api/sessions/:token/pause
```

Pauses an active session. Accumulates active time up to this point.

**Path Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `token` | string | 64-char hex session token |

**Response `200 OK`:**
```json
{
  "status": "paused",
  "totalActiveSeconds": 123
}
```

**Errors:**
- `404` — Session not found
- `409` — Session in terminal state (`stopped`, `compiling`, `complete`, `failed`)

**Notes:**
- Idempotent for already-paused sessions
- Pending sessions return a no-op (0 active seconds)

---

### Resume Session

```
POST /api/sessions/:token/resume
```

Resumes a paused session.

**Path Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `token` | string | 64-char hex session token |

**Response `200 OK`:**
```json
{
  "status": "active",
  "nextExpectedAt": "2024-01-01T12:01:00.000Z"
}
```

**Errors:**
- `404` — Session not found
- `409` — Session not in `paused` state

---

### Stop Session

```
POST /api/sessions/:token/stop
```

Stops a session and enqueues video compilation if screenshots exist.

**Path Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `token` | string | 64-char hex session token |

**Body (optional):**
```json
{ "edit": true }
```

| Field | Type | Description |
|-------|------|-------------|
| `edit` | boolean | Hold the timelapse unpublished after it compiles so the owner can cut it first. Opens a lease the client must renew (see [Edits (Cuts)](#edits-cuts)). Omit for today's behavior. |

**Response `200 OK`:**
```json
{
  "status": "stopped",
  "trackedSeconds": 123,
  "totalActiveSeconds": 300,
  "editHoldUntil": "2026-07-26T14:35:00.000Z"
}
```

`editHoldUntil` is present only when the stop requested `edit: true` and the session actually has captures to edit. It is one lease term (~2 min) — keep it alive with `POST /:token/editing` for as long as your editor is open.

**Errors:**
- `404` — Session not found
- `409` — Session already in terminal state

**Notes:**
- Marks session `failed` immediately if no screenshots exist (skips compilation), regardless of `edit`
- Accumulates any remaining active time
- Only send `edit: true` from a client that will actually open an editing surface and renew the lease. An abandoned hold still publishes on its own, so nothing is lost either way — but a client that asks for a hold and never renews it just makes the user wait a lease for no reason.

---

### Poll Compilation Status

```
GET /api/sessions/:token/status
```

Lightweight endpoint for polling compilation progress.

**Path Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `token` | string | 64-char hex session token |

**Response `200 OK`:**
```json
{
  "status": "compiling",
  "videoUrl": null,
  "trackedSeconds": 123
}
```

When complete:
```json
{
  "status": "complete",
  "videoUrl": "https://...",
  "trackedSeconds": 123
}
```

Sessions created with a [redirect hook](#create-session) additionally carry
`redirectUrl` (absent otherwise) — clients watching the compile open it when
the status flips to `complete`.

---

### Get Capture Timings

```
GET /api/sessions/:token/timings
```

Returns the ISO-8601 capture timestamps of **every confirmed screenshot** in the session, oldest first. Token-gated public endpoint. Uses each screenshot's `capturedAt` (client-attested capture moment); rows predating the `captured_at` column fall back to `requestedAt` so the array is never sparse.

**Cuts are respected by default.** Captures whose timestamp falls inside the session's [cut list](#edits-cuts) are excluded from `timestamps` (so heartbeat forwarders honor user edits with no code changes) and surfaced separately: `cuts` carries the intervals, `cutCount` the number of removed captures, and `?includeCut=true` adds a `cutTimestamps` array.

**Path Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `token` | string | 64-char hex session token |

**Query Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `includeCut` | boolean | Optional. When `true`, adds `cutTimestamps` (the removed captures) to the response |

**Response `200 OK`:**
```json
{
  "status": "active",
  "count": 3,
  "first": "2024-01-01T12:00:00.000Z",
  "last": "2024-01-01T12:02:00.000Z",
  "clientInfo": "Lookout Web (Fallout)/0.2.6 (macOS 14.3; Chrome 120.0)",
  "timestamps": [
    "2024-01-01T12:00:00.000Z",
    "2024-01-01T12:01:00.000Z",
    "2024-01-01T12:02:00.000Z"
  ]
}
```

| Field | Type | Description |
|-------|------|-------------|
| `status` | string | Current session status |
| `count` | integer | Number of timestamps returned (= confirmed screenshot count) |
| `first` | string \| null | Earliest timestamp (= `timestamps[0]`); `null` if no screenshots |
| `last` | string \| null | Latest timestamp (= last element); `null` if no screenshots |
| `clientInfo` | string \| null | [Client telemetry string](#client-info) from the session's first screenshot upload; `null` if none recorded |
| `timestamps` | string[] | ISO-8601 timestamps of KEPT captures, ascending |
| `cuts` | array | The session's cut list (`[{start, end}]`); `[]` when never edited |
| `cutCount` | integer | Confirmed captures removed by the cut list |
| `cutTimestamps` | string[] | Only with `?includeCut=true`: the removed timestamps, ascending |

> **ℹ️ `count` is the number of screenshots, not minutes.** More than one capture can fall within the same minute (retries, resume, clock jitter), so `count` can exceed the number of distinct minutes — it is **not** a count of tracked minutes. For tracked time use `trackedSeconds`.

> **⚠️ `last − first` is not the recorded duration.** A session can be paused and resumed, leaving gaps between consecutive timestamps. The span from `first` to `last` is wall-clock elapsed time, which **overstates** actual capture time whenever the session was paused. To measure recorded time, use `trackedSeconds` from the [status endpoint](#poll-compilation-status), or sum the gaps between consecutive timestamps while excluding any larger than the capture interval.

**Availability:** Capture timestamps are available for timelapses recorded from **~2026-05-26** onward. Timelapses recorded before then did not have timestamps collected — for those, the endpoint returns `200` with `count: 0` and an empty `timestamps` array, even though the session is `complete` and still has a playable video.

**Timestamp precision:** For current recordings these are true capture times — the moment each frame was grabbed. Recordings from older legacy clients report a server-side time instead (when the upload was received), which trails the true capture by upload latency.

**Errors:**
- `404` — Session not found
- `429` — Rate limit exceeded

> **Note:** The original screenshot images are only retained for 7 days after a session stops, after which the JPEGs are deleted from storage. The capture timestamps returned by this endpoint — along with the compiled video and thumbnail — are kept.

---

### Edits (Cuts)

When a user stops a recording they can review it before it goes out. An edit is a **cut list** of absolute wall-clock intervals of the session that are removed from every output —

```json
[
  { "start": "2026-07-26T14:03:00.000Z", "end": "2026-07-26T14:11:00.000Z" },
  { "start": "2026-07-26T15:40:00.000Z", "end": "2026-07-26T15:44:00.000Z" }
]
```

Cuts are intervals (not video offsets) because Lookout is heartbeat-based — the same list drives all three derived views consistently:

| Consumer | Effect |
|----------|--------|
| Published video | Capture units inside a cut are removed (the video gets 1 second shorter per cut minute) |
| [`GET /timings`](#get-capture-timings) | Removed captures are excluded from `timestamps` by default |
| `trackedSeconds` | Reported as raw − `cutSeconds` on every endpoint (raw stays available as `uncutTrackedSeconds`) |

**Membership rule:** a capture is cut iff its timestamp ∈ `[start, end)` of any interval. Granularity is effectively whole minutes (one capture unit ≈ one minute ≈ one second of video).

#### Editing happens before publication, never after

`complete` is the status programs act on — forwarding heartbeats to Hackatime, accepting a submission, firing the redirect hook. So a session reaches `complete` **exactly once, with its cuts already applied**. There is no post-publication editing: the data a program reads is final the first time it sees it.

That is what the **edit hold** is for. `POST /stop` with `{"edit": true}` marks the session; it compiles as usual, but the worker leaves it `stopped` with `videoUrl` still null instead of publishing. During the hold the owner previews the built video, sets a cut list, and publishes. The lifecycle programs observe is unchanged — `stopped → compiling → complete`, or `stopped → complete` when there was nothing to cut.

```
stop {edit:true} ─> stopped (hold, compiling internally)
                      ├─ PUT /cuts … then POST /compile ─> compiling ─> complete
                      ├─ POST /compile with no cuts ────────────────────> complete
                      └─ lease lapses (~2 min unrenewed) ──────────────> complete
```

**The hold is a lease, not a countdown.** An open editing surface calls `POST /:token/editing` every 30 s; the server holds the session for 120 s past the last renewal. So editing takes exactly as long as it takes — there's no deadline to race on a three-hour recording — and an abandoned session publishes about two minutes later instead of sitting unpublished for half an hour. An absolute ceiling of 120 minutes from the stop bounds the pathological case (an editor left open overnight).

The hold can only **delay** publication, never cancel it. A stop without `{"edit": true}` behaves exactly as it always has, so existing clients are unaffected.

#### Renew the Edit Lease

```
POST /api/sessions/:token/editing
```

"An editor is still open." Extends the hold to `now + 120s`. Idempotent and cheap; call it every 30 s while an editing surface is showing. Rate limit: 20 req/min per token.

**Response `200 OK`:** `{ "editHoldUntil": ISO-8601, "held": boolean }`

`held: false` means the session is no longer holdable — it published, failed, or passed the ceiling. Stop renewing and show the published state; the call never resurrects a published session.

**Mechanics:** the compile always produces the **uncut original** and records its unit map. Publishing with cuts is a lossless stream-copy of the kept ranges (seconds, even for 12-hour sessions, no quality loss), after which the uncut original is **deleted immediately** — cut content does not outlive the publish. Publishing without cuts just points the session at the original, with no worker round-trip.

Cutting can only *reduce* tracked time — there is no fraud surface.

Session responses (`GET /:token`, `/status`, internal) carry `cuts`, `cutSeconds`, `uncutTrackedSeconds`, `editable`, and `editHoldUntil`.

#### Get Editor Units

```
GET /api/sessions/:token/units
```

Editor metadata. Rate limit: 10 req/min per token.

**Response `200 OK`:**
```json
{
  "units": [ { "capturedAt": "2026-07-26T14:00:12.000Z", "screenshotId": "…" } ],
  "cuts": [],
  "editable": true,
  "editHoldUntil": "2026-07-26T14:35:00.000Z",
  "expectedUnits": 47,
  "originalVideoUrl": "https://…presigned, ~1h…",
  "recompilesRemaining": 5
}
```

- `units` — the capture units of the compiled **original** video, in output order. Array index = video second = real-world minute: the exact video-time ↔ wall-clock map. Empty until the preview finishes building.
- `originalVideoUrl` — presigned GET for the unpublished original (the editor's preview source). Deliberately not the public media URL, which is null until the session publishes. `null` when not editable.
- `editable` / `editableReason` — `false` with one of:

  | Reason | Meaning | What a client should do |
  |--------|---------|-------------------------|
  | `preparing` | Hold is active; the preview video is still compiling (the session reads `stopped` or `compiling`) | **Poll** — this is the normal state right after a stop, not an error. Show progress |
  | `no_original` | Hold active but no original recorded | Poll; same as above |
  | `not_ready` | No hold, or it lapsed | Editing isn't on offer |
  | `published` | Already `complete` | Editing is over — by design |
  | `failed` | The compile failed | Show the failure; there's nothing to edit |
  | `recompiles_exhausted` | Publish budget spent | Editing is over |

- `editHoldUntil` — when the session auto-publishes; `null` when no hold is active.
- `expectedUnits` — confirmed captures ≈ units the finished video will hold. Lets a client waiting on the build size a progress estimate (compile time scales with unit count).

#### Set Cut List

```
PUT /api/sessions/:token/cuts
Body: { "cuts": [{ "start": ISO-8601, "end": ISO-8601 }, …] }
```

Replaces the whole cut list (idempotent; `[]` clears all edits). **Only valid during an active edit hold** — a published session is immutable. The server normalizes (sorts, merges overlaps, clamps to the session envelope, caps at 120 intervals) and rejects a list that would remove **every** unit. The cuts are baked into the video by the publish call below. Rate limit: 20 req/min per token.

**Response `200 OK`:**
```json
{
  "cuts": [ { "start": "…", "end": "…" } ],
  "unitsTotal": 47,
  "unitsCut": 8,
  "trackedSeconds": 2340,
  "uncutTrackedSeconds": 2820
}
```

**Errors:** `400` invalid/entire-timelapse cut list · `409` compiling or not editable · `429` rate limit.

#### Publish (End the Hold)

```
POST /api/sessions/:token/compile
```

Ends the edit hold and publishes the timelapse with the current cut list baked in.

- **With cuts:** `stopped → compiling → complete` (poll [`/status`](#poll-compilation-status)); the worker stream-copies the kept ranges, usually in seconds, then deletes the uncut original. Burns one of **5** publishes per session.
- **Without cuts:** returns `{ "instant": true, "status": "complete" }` immediately — the built original is simply published, no worker involved.
- **Before the preview finishes building** (`editableReason: "preparing"`): drops the hold and returns `200` with `instant: false`. The in-flight compile publishes normally when it lands, so "publish as recorded" works without waiting for a preview the user just declined.

Rate limit: 5 req/min per token.

**Response `200 OK`:** `{ "status": "compiling" | "complete", "instant": boolean, "recompilesRemaining": number }`
**Errors:** `202` publish already running (safe to retry/poll) · `409` hold lapsed or not editable · `429` rate limit. Calling it on an already-published session is a no-op `200` with `instant: true`, so a client racing the expiry job never sees a spurious failure.

---

### Get Video URL

```
GET /api/sessions/:token/video?format=mp4
```

Returns a URL to the compiled timelapse video. MP4 / H.264 is the only encoded format. The `?format=webm` query parameter is retained for pre-0.2.0 binaries — it returns a URL to a static "please update your client" video instead of an error.

**Path Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `token` | string | 64-char hex session token |

**Response `200 OK`:**
```json
{
  "videoUrl": "https://r2.cloudflarestorage.com/..."
}
```

**Errors:**
- `404` — Session not found or video not yet available
- `429` — Rate limit exceeded

**Notes:**
- Only available when session status is `complete`
- Presigned URL expires after 1 hour
- Output is H.264 MP4. WebKitGTK-based Linux browsers may need `gst-plugins-bad`/OpenH264 installed for playback.

---

### Get Thumbnail URL

```
GET /api/sessions/:token/thumbnail
```

Returns a presigned URL for the session thumbnail.

**Path Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `token` | string | 64-char hex session token |

**Response `200 OK`:**
```json
{
  "thumbnailUrl": "https://r2.cloudflarestorage.com/..."
}
```

**Errors:**
- `404` — Session not found, or thumbnail not available
- `429` — Rate limit exceeded

**Notes:**
- Presigned URL expires after 1 hour

---

### Permanent Media Redirects

Three endpoints serve **stable, shareable URLs** that 302-redirect to a short-lived presigned R2 URL. These take a public session ID (UUID) in the path rather than the secret token — safe to embed in `<img>`, `<video>`, or external services.

```
GET /api/media/:sessionId/thumbnail.jpg   →  302 to presigned JPEG URL (1h)
GET /api/media/:sessionId/video.mp4       →  302 to presigned MP4 URL (1h)
GET /api/media/:sessionId/video.webm      →  302 to the static "please update" video
```

- `Cache-Control: public, max-age=1800` on `thumbnail.jpg` and `video.mp4` (the *redirect itself* is cacheable for 30 min — the presigned URL it points at lives 1 hour).
- `Cache-Control: public, max-age=86400` on `video.webm` — pre-0.2.0 binaries hit this; we tell them to update. WebM encoding was dropped in 0.2.0.
- `video.mp4` returns 404 unless the session is `complete` with a stored video.
- `thumbnail.jpg` returns 404 if no thumbnail has been compiled yet.
- These are the URLs surfaced in `videoUrl` / `thumbnailUrl` / `videoWebmUrl` response fields elsewhere — never link to the presigned R2 URLs directly because they expire.

---

### Batch Get Sessions

```
POST /api/sessions/batch
```

Fetch multiple sessions at once (for gallery views). Results sorted by creation date (newest first).

**Request Body:**
```json
{
  "tokens": ["token1", "token2", "..."]
}
```

| Field | Type | Required | Validation |
|-------|------|----------|------------|
| `tokens` | string[] | yes | Max 100 tokens, each must be 64-char hex |

**Response `200 OK`:**
```json
{
  "sessions": [
    {
      "token": "...",
      "status": "complete",
      "trackedSeconds": 123,
      "screenshotCount": 45,
      "startedAt": "2024-01-01T12:00:00.000Z",
      "createdAt": "2024-01-01T11:50:00.000Z",
      "totalActiveSeconds": 300,
      "thumbnailUrl": "https://...",
      "videoUrl": "https://...",
      "metadata": {}
    }
  ]
}
```

**Errors:**
- `400` — Missing or invalid tokens array, or more than 100 tokens
- `429` — Rate limit exceeded

---

## Internal Endpoints

All internal endpoints require the `X-API-Key` header.

### Create Session

```
POST /api/internal/sessions
```

Creates a new session in `pending` state.

**Request Body:**
```json
{
  "metadata": {}
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | no | Session name (1-255 chars) |
| `metadata` | object | no | Arbitrary JSON metadata to attach to the session (max 50 properties) |
| `clips` | boolean | no | Whether this session accepts [clip uploads](#clips) (~6 frames/min video). Default **`true`**; pass `false` to opt out and get the legacy 1 JPEG/min payload. **Immutable after creation.** |
| `redirectUrl` | string | no | Redirect hook: http(s) URL (max 2048 chars) the recording client opens in the user's browser once the timelapse finishes compiling. Fires at most once, only for a live completion (not on re-opening a finished session). **Immutable after creation.** |

**Response `201 Created`:**
```json
{
  "token": "64-char hex string",
  "sessionId": "uuid",
  "sessionUrl": "https://lookout.hackclub.com/session?token=..."
}
```

`sessionUrl` serves a recorder Lookout hosts, where the user chooses between
the desktop app, this browser, and a camera. Alongside the token it takes
`?app=` (your program's name, for telemetry) and `?edit=false` (drop the
"Edit & save" option from the stop dialog).

---

### Get Session Details (Admin)

```
GET /api/internal/sessions/:sessionId
```

Returns full session details including internal fields.

**Path Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `sessionId` | string (UUID) | Session ID |

**Response `200 OK`:**
```json
{
  "session": {
    "id": "uuid",
    "token": "64-char hex",
    "name": "...",
    "metadata": {},
    "status": "active",
    "startedAt": "...",
    "stoppedAt": null,
    "pausedAt": null,
    "lastScreenshotAt": "...",
    "resumedAt": "...",
    "totalActiveSeconds": 123,
    "clipsEnabled": false,
    "videoUrl": null,
    "thumbnailUrl": null,
    "createdAt": "...",
    "updatedAt": "..."
  },
  "trackedSeconds": 123,
  "screenshotCount": 45,
  "clientInfo": "Lookout Web (Fallout)/0.2.6 (macOS 14.3; Chrome 120.0)"
}
```

`clientInfo` is the [client telemetry string](#client-info) from the session's first screenshot upload; `null` if none recorded.

---

### Lookup Session by Token (Admin)

```
GET /api/internal/sessions/by-token/:token
```

Returns the session ID for a given session token.

**Path Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `token` | string | 64-char hex session token |

**Response `200 OK`:**
```json
{
  "sessionId": "uuid"
}
```

**Errors:**
- `404` — Session not found

---

### Force-Stop Session (Admin)

```
POST /api/internal/sessions/:sessionId/stop
```

Force stops a session regardless of current state and enqueues compilation.

**Path Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `sessionId` | string (UUID) | Session ID |

**Response `200 OK`:**
```json
{
  "status": "stopped"
}
```

**Errors:**
- `404` — Session not found
- `409` — Session already in terminal state

---

### Recompile Failed Session (Admin)

```
POST /api/internal/sessions/:sessionId/recompile
```

Re-enqueues compilation for a failed session.

**Path Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `sessionId` | string (UUID) | Session ID |

**Response `200 OK`:**
```json
{
  "status": "compiling"
}
```

**Errors:**
- `404` — Session not found
- `409` — Session not in `failed` state

---

## Background Jobs

The server uses **PG Boss** for background job processing.

| Job | Schedule | Description |
|-----|----------|-------------|
| `compile-timelapse` | On demand | Compiles screenshots into an H.264 MP4 timelapse. Retries 3x with backoff. |
| `check-timeouts` | Every 1 min | Auto-pauses sessions idle >10 min, auto-stops sessions idle >24 h, resets stuck compilations >60 min. |
| `cleanup-unconfirmed` | Every 5 min | Deletes unconfirmed screenshot records older than 10 minutes. |

---

## Client Upload Flow

1. **Create session** — `POST /api/internal/sessions` (server-side)
2. **Get upload URL** — `GET /api/sessions/:token/upload-url`
3. **Upload JPEG** — `PUT <uploadUrl>` with `Content-Type: image/jpeg` (direct to R2)
4. **Confirm upload** — `POST /api/sessions/:token/screenshots`
5. Repeat steps 2-4 every 60 seconds
6. **Stop session** — `POST /api/sessions/:token/stop`
7. **Poll status** — `GET /api/sessions/:token/status` until `complete`
8. **Get video** — `GET /api/sessions/:token/video`

---

## Configuration

| Environment Variable | Default | Description |
|---------------------|---------|-------------|
| `PORT` | 3000 | Server port |
| `DATABASE_URL` | — | PostgreSQL connection string |
| `ADMIN_USERNAME` | — | Basic-auth username for the `/admin` dashboard (admin disabled if unset) |
| `ADMIN_PASSWORD` | — | Basic-auth password for the `/admin` dashboard (admin disabled if unset) |
| `BASE_URL` | `http://localhost:3000` | Base URL for generated links |
| `R2_ACCOUNT_ID` | — | Cloudflare R2 account ID |
| `R2_ACCESS_KEY_ID` | — | R2 access key |
| `R2_SECRET_ACCESS_KEY` | — | R2 secret key |
| `R2_BUCKET_NAME` | — | R2 bucket name |
| `R2_PUBLIC_DOMAIN` | — | Public domain for R2 URLs |
| `RATE_LIMIT_PER_MINUTE` | 3 | Upload URL rate limit |

---

## CORS

Allowed origins:
- `*.hackclub.com`
- `localhost:*` (any port)
- `tauri://` (desktop app)
- Server-to-server (no origin header)
