# Lookout Server API Documentation

**Framework:** Fastify v5
**Base URL:** `http://localhost:3000` (configurable via `PORT` and `BASE_URL` env vars)

---

## Authentication

### Public Routes (Session Token)
Public endpoints use a 64-character hex **session token** as a path parameter. No header-based auth required.

### Internal Routes (API Key)
Internal endpoints require the `X-API-Key` header matching the `INTERNAL_API_KEY` environment variable. Uses constant-time comparison.

---

## Rate Limiting

In-memory sliding window (60-second windows). Rate-limited responses return:

- **Status:** `429 Too Many Requests`
- **Header:** `Retry-After: <seconds>`
- **Body:** `{ "error": "Rate limit exceeded" }`

| Endpoint | Limit | Key |
|----------|-------|-----|
| `GET /api/sessions/:token` | 60 req/min | per token |
| `GET /api/sessions/:token/upload-url` | 10 req/min | per session ID |
| `POST /api/sessions/:token/screenshots` | 20 req/min | per token |
| `POST /api/sessions/:token/pause` | 10 req/min | per token |
| `POST /api/sessions/:token/resume` | 10 req/min | per token |
| `POST /api/sessions/:token/stop` | 10 req/min | per token |
| `GET /api/sessions/:token/video` | 30 req/min | per token |
| `GET /api/sessions/:token/thumbnail` | 30 req/min | per token |
| `POST /api/sessions/batch` | 30 req/min | per IP |

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
  "startedAt": "2024-01-01T12:00:00.000Z",
  "totalActiveSeconds": 300,
  "createdAt": "2024-01-01T11:50:00.000Z",
  "thumbnailUrl": "https://...",
  "videoUrl": "https://...",
  "metadata": {}
}
```

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

**Response `200 OK`:**
```json
{
  "uploadUrl": "https://r2.cloudflarestorage.com/...",
  "r2Key": "screenshots/{sessionId}/{screenshotId}.jpg",
  "screenshotId": "uuid",
  "minuteBucket": 1,
  "nextExpectedAt": "2024-01-01T12:01:00.000Z",
  "serverTime": "2024-01-01T12:00:00.000Z",
  "trackingMode": "credit"
}
```

`nextExpectedAt` is the server's authoritative target for the **next** capture's `capturedAt` — clients should schedule from it (see Tracking Modes below).

**Errors:**
- `400` — `captured_at_future`, `captured_at_too_old`, `captured_at_before_session_start`, `captured_at_not_monotonic`, `captured_at_invalid`, or `credit_mode_requires_captured_at`
- `404` — Session not found
- `409` — Session not in `pending` or `active` state
- `429` — Rate limit exceeded, or max upload requests per session reached (4320)

**Notes:**
- Presigned URL expires after 2 minutes
- Client should PUT the JPEG image directly to `uploadUrl`
- Max 4320 upload requests per session
- Pre-0.2.1 binaries that don't send `capturedAt` continue to receive a usable response — additive fields (`serverTime`, `trackingMode`) are gracefully ignored

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
  "fileSize": 125000
}
```

| Field | Type | Required | Validation |
|-------|------|----------|------------|
| `screenshotId` | string (UUID) | yes | Must match a pending screenshot |
| `width` | integer | yes | ≥ 1 |
| `height` | integer | yes | ≥ 1 |
| `fileSize` | integer | yes | ≥ 1 |

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
- `400` — Invalid content type (must be `image/jpeg`), file too large (max 2 MB), or object not found in R2
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

**Response `200 OK`:**
```json
{
  "status": "stopped",
  "trackedSeconds": 123,
  "totalActiveSeconds": 300
}
```

**Errors:**
- `404` — Session not found
- `409` — Session already in terminal state

**Notes:**
- Marks session `complete` immediately if no screenshots exist (skips compilation)
- Accumulates any remaining active time

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

**Response `201 Created`:**
```json
{
  "token": "64-char hex string",
  "sessionId": "uuid",
  "sessionUrl": "https://lookout.hackclub.com/session?token=..."
}
```

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
    "videoUrl": null,
    "thumbnailUrl": null,
    "createdAt": "...",
    "updatedAt": "..."
  },
  "trackedSeconds": 123,
  "screenshotCount": 45
}
```

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
| `check-timeouts` | Every 1 min | Auto-pauses sessions idle >5 min, auto-stops sessions idle >30 min, resets stuck compilations >60 min. |
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
| `INTERNAL_API_KEY` | — | API key for internal endpoints |
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
