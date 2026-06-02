# @lookout/react — React SDK Documentation

**Package:** `@lookout/react` v0.1.0
**Peer Dependencies:** React 18+ or 19+
**Exports:** ESM + CJS with TypeScript declarations

---

## Quick Start

```tsx
import { LookoutProvider, LookoutRecorder } from "@lookout/react";

function App() {
  return (
    <LookoutProvider token="your-64-char-hex-token" apiBaseUrl="https://lookout.hackclub.com">
      <LookoutRecorder />
    </LookoutProvider>
  );
}
```

For headless usage:

```tsx
import { LookoutProvider, useLookout } from "@lookout/react";

function MyRecorder() {
  const { state, actions } = useLookout();

  return (
    <div>
      <p>Status: {state.status}</p>
      <p>Time: {state.displaySeconds}s</p>
      <button onClick={actions.startSharing}>Start</button>
      <button onClick={actions.pause}>Pause</button>
      <button onClick={() => actions.stop({ name: "My timelapse" })}>Stop</button>
    </div>
  );
}

function App() {
  return (
    <LookoutProvider token="..." apiBaseUrl="https://lookout.hackclub.com">
      <MyRecorder />
    </LookoutProvider>
  );
}
```

For camera (webcam) capture:

```tsx
import { LookoutProvider, LookoutRecorder } from "@lookout/react";

function App() {
  return (
    <LookoutProvider
      token="your-64-char-hex-token"
      apiBaseUrl="https://lookout.hackclub.com"
      capture={{ mode: "camera" }}
    >
      <LookoutRecorder />
    </LookoutProvider>
  );
}
```

---

## Provider

### `<LookoutProvider>`

Context provider that configures the API client and settings for all child hooks/components.

```tsx
<LookoutProvider
  token="..."
  apiBaseUrl="https://lookout.hackclub.com"
  capture={{ intervalMs: 30000, jpegQuality: 0.9 }}
  autoStart
>
  {children}
</LookoutProvider>
```

**Props** (`LookoutProviderProps` extends `LookoutConfig`):

| Prop | Type | Default | Description |
|------|------|---------|-------------|
| `token` | `TokenProvider` | *required* | Session token — string, sync getter, or async getter |
| `apiBaseUrl` | `string` | `""` (same origin) | Server API base URL |
| `capture` | `CaptureSettings` | See below | Screenshot capture settings |
| `retry` | `RetrySettings` | See below | Upload retry/buffer settings |
| `callbacks` | `LookoutCallbacks` | `{}` | Lifecycle event callbacks |
| `statusPollIntervalMs` | `number` | `3000` | Compilation status poll interval (ms) |
| `autoStart` | `boolean` | `false` | Auto-start screen sharing on mount |
| `appName` | `string` | — | Host program embedding Lookout (e.g. `"Fallout"`). Reported in client telemetry as `Lookout Sdk (Fallout)/<version> (…)` and surfaced server-side as the session's `clientInfo`. |
| `children` | `ReactNode` | *required* | Child components |

#### `TokenProvider`

```ts
type TokenProvider =
  | string                     // static token
  | (() => string)             // sync getter
  | (() => Promise<string>);   // async getter (e.g. fetch from your backend)
```

#### `CaptureSettings`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `intervalMs` | `number` | `60000` | Screenshot interval in ms |
| `jpegQuality` | `number` | `0.85` | JPEG quality (0–1) |
| `maxWidth` | `number` | `1920` | Max capture width in px |
| `maxHeight` | `number` | `1080` | Max capture height in px |
| `displayMediaConstraints` | `DisplayMediaStreamOptions` | — | Override `getDisplayMedia` constraints |
| `mode` | `CaptureMode` | `"screen"` | Capture source: `"screen"` or `"camera"` |
| `camera` | `CameraSettings` | `{}` | Camera-specific settings (only used when `mode` is `"camera"`) |

#### `CaptureMode`

```ts
type CaptureMode = "screen" | "camera";
```

#### `CameraSettings`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `deviceId` | `string` | — | Preferred camera device ID (from `enumerateDevices`). Omit for default camera |
| `userMediaConstraints` | `MediaTrackConstraints` | — | Additional `getUserMedia` video constraints (merged with defaults) |

#### `RetrySettings`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `maxRetries` | `number` | `3` | Max retries per upload step |
| `retryDelays` | `number[]` | `[2000, 4000, 8000]` | Backoff delays per attempt (ms) |
| `maxPendingBuffer` | `number` | `5` | Max screenshots buffered in memory |

---

## Hooks

### `useLookout()`

Primary hook — composes all lower-level hooks and orchestrates the capture-upload loop. Must be used within `<LookoutProvider>`.

```ts
const { state, actions } = useLookout();
```

**Returns:**

#### `state: LookoutState`

| Field | Type | Description |
|-------|------|-------------|
| `status` | `RecorderStatus` | Current status (see below) |
| `isSharing` | `boolean` | Whether media capture is active (screen sharing or camera recording) |
| `isRecording` | `boolean` | `true` when actively capturing (`isSharing && (status === "active" \|\| status === "pending")`). Use this instead of compound checks in UI logic. |
| `trackedSeconds` | `number` | Server-authoritative tracked time — max of session state and the value returned by the last upload confirmation. Updates per-upload, not just on poll. Bounded by what the server has credited; never inflated by client-side estimation. |
| `displaySeconds` | `number` | Client-interpolated display time (ticks every second via RAF). Caps interpolation at one capture interval (60s) ahead of the last server credit, so stop/compile reveals at most a 60s drop. The `baseRef` ratchet defends against backward jumps on stale-read sync. |
| `screenshotCount` | `number` | Number of confirmed screenshots — max of server count and local upload count, so it updates immediately on upload. |
| `uploads` | `UploadState` | Upload queue: `{ pending, completed, failed }` |
| `lastScreenshotUrl` | `string \| null` | Object URL of last captured screenshot |
| `videoUrl` | `string \| null` | Video URL when complete. Auto-fetched from server when status reaches `"complete"`. |
| `error` | `string \| null` | Error message when status is `"error"` |
| `captureMode` | `CaptureMode` | Active capture mode (`"screen"` or `"camera"`) |
| `availableCameras` | `MediaDeviceInfo[]` | Available camera devices (populated when mode is `"camera"`) |
| `selectedCameraId` | `string \| null` | Currently selected camera device ID |
| `isPreviewing` | `boolean` | Whether camera is in preview mode (stream live, capture loop not started). Camera mode only. |
| `previewStream` | `MediaStream \| null` | Live camera MediaStream for rendering in a `<video>` element. Available during preview and recording in camera mode. |

#### `actions: LookoutActions`

| Method | Signature | Description |
|--------|-----------|-------------|
| `startSharing` | `() => Promise<void>` | Start capture source and begin the capture-upload loop. In camera mode, reuses the preview stream if one is active. |
| `stopSharing` | `() => void` | Stop capture source without stopping session (auto-pauses) |
| `pause` | `() => Promise<void>` | Pause the session |
| `resume` | `() => Promise<void>` | Resume a paused session |
| `stop` | `(options?: { name?: string }) => Promise<void>` | Stop the session and trigger compilation. Optionally name the timelapse before stopping. |
| `selectCamera` | `(deviceId: string) => void` | Select a camera device by ID. Works during preview and recording. |
| `startPreview` | `() => Promise<void>` | Acquire camera stream for live preview without starting the capture loop. Camera mode only. |
| `stopPreview` | `() => void` | Stop the preview stream. Camera mode only. |

#### `RecorderStatus`

Server states plus client-only states:

```ts
type RecorderStatus =
  | "pending"    // session created, not yet started
  | "active"     // recording in progress
  | "paused"     // paused by user or auto-pause
  | "stopped"    // stopped, waiting for compilation
  | "compiling"  // video being compiled
  | "complete"   // video ready
  | "failed"     // compilation failed
  | "loading"    // (client-only) initial session fetch
  | "no-token"   // (client-only) no token provided/resolved
  | "error";     // (client-only) error state
```

---

### `useScreenCapture(overrides?)`

Handles `getDisplayMedia`, canvas snapshots, and stream lifecycle. Can be used standalone (without provider) by passing explicit settings.

```ts
const { isSharing, startSharing, takeScreenshot, stopSharing } = useScreenCapture();
```

**Parameters:**

| Param | Type | Description |
|-------|------|-------------|
| `overrides` | `CaptureSettings` | Optional overrides (merged with provider config) |

**Returns:**

| Field | Type | Description |
|-------|------|-------------|
| `isSharing` | `boolean` | Whether screen sharing is active |
| `startSharing` | `() => Promise<void>` | Prompt user for screen sharing |
| `takeScreenshot` | `() => Promise<CaptureResult \| null>` | Capture current frame as JPEG blob |
| `stopSharing` | `() => void` | Stop all media tracks |

#### `CaptureResult`

```ts
interface CaptureResult {
  blob: Blob;            // JPEG image blob
  width: number;         // Pixel width
  height: number;        // Pixel height
  capturedAtMs?: number; // Client-clock ms when the frame was grabbed
                         // (vs. when the upload eventually arrives).
                         // Forwarded as `capturedAt` to opt into credit
                         // mode; uploader falls back to Date.now() at
                         // enqueue if missing.
}
```

---

### `useCameraCapture(overrides?)`

Handles `getUserMedia` (webcam), device enumeration, canvas snapshots, and stream lifecycle. Supports a two-phase flow: **preview** (stream live, no capture loop) → **recording** (`isSharing = true`, triggers capture loop in `useLookout`). Can be used standalone (without provider) by passing explicit settings.

```ts
const {
  isSharing, startSharing, takeScreenshot, stopSharing,
  devices, selectedDeviceId, selectDevice,
  isPreviewing, previewStream, startPreview, stopPreview,
} = useCameraCapture();
```

**Parameters:**

| Param | Type | Description |
|-------|------|-------------|
| `overrides` | `CaptureSettings` | Optional overrides (merged with provider config) |

**Returns:**

| Field | Type | Description |
|-------|------|-------------|
| `isSharing` | `boolean` | Whether camera is recording (capture loop active) |
| `startSharing` | `() => Promise<void>` | Start recording — reuses preview stream if active, otherwise acquires one |
| `takeScreenshot` | `() => Promise<CaptureResult \| null>` | Capture current frame as JPEG blob |
| `stopSharing` | `() => void` | Stop recording and release camera stream |
| `devices` | `MediaDeviceInfo[]` | Available camera devices (auto-updated on connect/disconnect) |
| `selectedDeviceId` | `string \| null` | Currently selected camera device ID |
| `selectDevice` | `(deviceId: string) => void` | Switch to a different camera (restarts stream, preserves preview/recording mode) |
| `isPreviewing` | `boolean` | Whether camera is in preview mode (stream live, not recording) |
| `previewStream` | `MediaStream \| null` | Live camera MediaStream — render in a `<video>` element for live preview |
| `startPreview` | `() => Promise<void>` | Acquire camera stream for preview without starting the capture loop |
| `stopPreview` | `() => void` | Stop preview and release camera stream |

**Notes:**
- Enumerates devices on mount and on the `devicechange` event.
- Safari may return devices with empty labels before first `getUserMedia` call — labels are populated after the first stream is acquired.
- `selectDevice` while streaming will restart the stream with the new device, preserving the current mode (preview or recording).
- Stream is cleaned up on unmount if the user navigates away without recording.

---

### `useUploader()`

Runs the serial upload pipeline (`getUploadUrl` → R2 PUT → `confirmScreenshot`) end-to-end with per-leg retries. Must be used within `<LookoutProvider>`. The pre-0.2.4 fire-and-forget queue (`enqueue`/`nextExpectedAt`) was replaced with this synchronous form — matches the desktop Rust capture loop.

```ts
const { captureUploadConfirm, uploads, trackedSeconds, lastScreenshotUrl, lastError, sessionConflict, resetConflict } = useUploader();
```

**Returns:**

| Field | Type | Description |
|-------|------|-------------|
| `captureUploadConfirm` | `(capture: CaptureResult) => Promise<{ trackedSeconds, nextExpectedAt }>` | Run the full pipeline. Resolves with the fresh `tracked_seconds` and `next_expected_at` from THIS capture's confirm response. Throws after retries — caller (the tick chain) catches and falls back to the local interval. |
| `uploads` | `UploadState` | `{ pending, completed, failed }` counts. Note: `completed` reflects successful confirms, not credited captures — a confirm can succeed with `credited_seconds = 0` in credit mode. |
| `trackedSeconds` | `number` | Server-reported tracked time from the last successful confirm |
| `lastScreenshotUrl` | `string \| null` | Object URL of last uploaded screenshot |
| `lastError` | `string \| null` | Last upload error message |
| `sessionConflict` | `boolean` | `true` when a 409 was received (session paused/stopped server-side) |
| `resetConflict` | `() => void` | Clear the `sessionConflict` flag after handling |

---

### `useSession()`

Manages session state, status polling, and server interactions. Must be used within `<LookoutProvider>`.

```ts
const session = useSession();
```

**Returns:**

| Field | Type | Description |
|-------|------|-------------|
| `status` | `RecorderStatus` | Current session status |
| `name` | `string` | Timelapse name |
| `trackedSeconds` | `number` | Server-tracked seconds |
| `screenshotCount` | `number` | Confirmed screenshot count |
| `startedAt` | `string \| null` | Session start timestamp |
| `createdAt` | `string \| null` | Session creation timestamp |
| `totalActiveSeconds` | `number` | Accumulated active time |
| `error` | `string \| null` | Error message |
| `pause` | `() => Promise<void>` | Pause the session |
| `resume` | `() => Promise<void>` | Resume the session |
| `stop` | `(name?: string) => Promise<void>` | Stop the session. Optionally name the timelapse before stopping (non-fatal if rename fails). |
| `reload` | `() => Promise<void>` | Re-fetch session from server |
| `syncStatus` | `() => Promise<void>` | Best-effort fetch of latest server status (used when a 409 surfaces in the uploader to reconcile local state) |
| `updateTrackedSeconds` | `(seconds: number) => void` | Update tracked seconds locally |
| `setError` | `(error: string \| null) => void` | Set error state |

---

### `useSessionTimer(serverTrackedSeconds, isActive)`

Client-side interpolated timer. Uses the server-provided seconds as ground truth, ticks every second via `requestAnimationFrame`, and **caps interpolation at one capture interval (60s)** ahead of the last server-credited value. Maintains a monotonic ratchet so a stale-read sync returning a lower value doesn't make the display jump backward.

When a new `serverTrackedSeconds` arrives, `baseRef` ratchets to `max(baseRef, serverTrackedSeconds)` and the elapsed-since-sync clock resets. Between credits, the displayed value is `baseRef + min(60, elapsed_seconds)` — so if captures stall, the display freezes 60s ahead of the last credit instead of running unbounded. When the next credit lands, the new server value equals the previously-frozen display (no visible jump).

When `isActive` flips to `false` (pause/stop/compile), the display snaps to `baseRef` — the maximum possible drop the user sees is 60s, never the full session length.

> **Important:** feed this hook only server-authoritative values. Do **not** derive a synthetic value from `uploads.completed` — in credit mode, not every successful upload credits a minute, so derived counts inflate the display. `useLookout` already enforces this via `computeBestTrackedSeconds`.

```ts
const displaySeconds = useSessionTimer(trackedSeconds, isActive);
```

**Parameters:**

| Param | Type | Description |
|-------|------|-------------|
| `serverTrackedSeconds` | `number` | Server-authoritative tracked time (from `state.trackedSeconds`, an upload-confirm response, or a status poll) |
| `isActive` | `boolean` | Whether to tick the timer. Set to `false` on pause/stop/compile to snap display to the server value |

**Returns:** `number` — display seconds (`baseRef + capped interpolated elapsed` while active; `baseRef` when inactive)

---

### `useTokenStore()`

Manages session tokens in `localStorage` with cross-tab sync. No provider required.

```ts
const store = useTokenStore();
```

**Returns (`UseTokenStore`):**

| Field | Type | Description |
|-------|------|-------------|
| `tokens` | `TokenEntry[]` | Active (non-archived) tokens |
| `archivedTokens` | `TokenEntry[]` | Archived tokens |
| `addToken` | `(token: string, label?: string) => void` | Add a token |
| `archiveToken` | `(token: string) => void` | Archive a token |
| `unarchiveToken` | `(token: string) => void` | Unarchive a token |
| `removeToken` | `(token: string) => void` | Permanently remove a token |
| `getAllTokenValues` | `() => string[]` | Get all active token strings |
| `hasToken` | `(token: string) => boolean` | Check if a token exists |

#### `TokenEntry`

```ts
interface TokenEntry {
  token: string;
  addedAt: string;    // ISO timestamp
  label?: string;
  archived: boolean;
}
```

**Storage key:** `lookout-tokens`

---

### `useGallery(options)`

Fetches multiple sessions for gallery display via the batch endpoint. Auto-refreshes on tab focus. No provider required.

```ts
const { sessions, loading, error, refresh } = useGallery({
  apiBaseUrl: "https://lookout.hackclub.com",
  tokens: ["token1", "token2"],
});
```

**Parameters (`UseGalleryOptions`):**

| Field | Type | Description |
|-------|------|-------------|
| `apiBaseUrl` | `string` | Server API base URL |
| `tokens` | `string[]` | Token strings to fetch |

**Returns (`UseGallery`):**

| Field | Type | Description |
|-------|------|-------------|
| `sessions` | `SessionSummary[]` | Fetched sessions (newest first) |
| `loading` | `boolean` | Whether fetch is in progress |
| `error` | `string \| null` | Fetch error message |
| `refresh` | `() => void` | Manually re-fetch |

---

### `useHashRouter()`

Simple hash-based router for single-page app navigation. No provider required.

```ts
const { route, navigate } = useHashRouter();
```

**Returns:**

| Field | Type | Description |
|-------|------|-------------|
| `route` | `Route` | Current route |
| `navigate` | `(route: Route) => void` | Navigate to a route |

#### `Route`

```ts
type Route =
  | { page: "gallery" }                   // #/
  | { page: "record"; token: string }     // #/record?token=...
  | { page: "session"; token: string };   // #/session?token=...
```

---

## Components

### `<LookoutRecorder>`

Drop-in recorder widget. Handles the full lifecycle: capture, upload, pause/resume/stop, compilation polling, and video display. Adapts its UI based on the configured `capture.mode`. Must be used within `<LookoutProvider>`.

```tsx
<LookoutRecorder />
```

No props — reads everything from context.

**Renders based on status:**
- `loading` — spinner
- `no-token` — "no session token" message
- `error` — error display
- `stopped` / `compiling` / `complete` / `failed` — `<ProcessingState>`
- `pending` / `active` / `paused` — capture UI (varies by mode, see below)

**Screen mode** (`capture.mode: "screen"`, default):
- `<StatusBar>` + `<ScreenPreview>` + `<RecordingControls>`
- Copy: "Share Screen & Start Recording", "Share Screen & Resume"

**Camera mode** (`capture.mode: "camera"`):
- Three-phase flow: idle → preview → recording
- **Idle**: "Start Camera" button (acquires camera stream for preview)
- **Preview**: `<CameraPreview>` (live video) + `<CameraSelector>` (if multiple cameras) + "Start Recording" / "Cancel"
- **Recording**: `<CameraPreview>` + standard Pause/Stop controls
- Copy adapts: "Start Camera", "Start Recording", "Start Camera & Resume"

---

### `<StatusBar>`

Displays timer, screenshot count, and upload queue status.

```tsx
<StatusBar displaySeconds={120} screenshotCount={5} uploads={{ pending: 1, completed: 4, failed: 0 }} />
```

**Props (`StatusBarProps`):**

| Prop | Type | Description |
|------|------|-------------|
| `displaySeconds` | `number` | Seconds to display (formatted as H:MM:SS) |
| `screenshotCount` | `number` | Confirmed screenshot count |
| `uploads` | `UploadState` | `{ pending, completed, failed }` |

---

### `<RecordingControls>`

Action buttons for start/pause/resume/stop, adapts to current state.

```tsx
<RecordingControls
  status="active"
  isSharing={true}
  onStartSharing={() => {}}
  onPause={() => {}}
  onResume={() => {}}
  onStop={() => {}}
/>
```

**Props (`RecordingControlsProps`):**

| Prop | Type | Description |
|------|------|-------------|
| `status` | `RecorderStatus` | Current session status |
| `isSharing` | `boolean` | Whether screen sharing is active |
| `onStartSharing` | `() => void` | Start screen sharing callback |
| `onPause` | `() => void` | Pause callback |
| `onResume` | `() => void` | Resume callback |
| `onStop` | `() => void` | Stop callback |
| `loading` | `boolean?` | Show loading state on buttons |

---

### `<ScreenPreview>`

Displays the last captured screenshot. Renders nothing if no image.

```tsx
<ScreenPreview imageUrl={lastScreenshotUrl} />
```

**Props (`ScreenPreviewProps`):**

| Prop | Type | Description |
|------|------|-------------|
| `imageUrl` | `string \| null` | Object URL of the screenshot |

---

### `<CameraPreview>`

Live camera preview using a `<video>` element. Falls back to a static image when no stream is provided. Mirrors the video horizontally for a natural selfie-view.

```tsx
<CameraPreview stream={state.previewStream} fallbackImageUrl={state.lastScreenshotUrl} />
```

**Props (`CameraPreviewProps`):**

| Prop | Type | Description |
|------|------|-------------|
| `stream` | `MediaStream \| null` | Live camera MediaStream to display |
| `fallbackImageUrl` | `string \| null?` | Fallback static image URL (e.g. last captured screenshot) |

---

### `<CameraSelector>`

Camera device picker dropdown. Renders nothing if no devices are available.

```tsx
<CameraSelector
  devices={state.availableCameras}
  selectedDeviceId={state.selectedCameraId}
  onSelect={actions.selectCamera}
  disabled={state.isSharing}
/>
```

**Props (`CameraSelectorProps`):**

| Prop | Type | Description |
|------|------|-------------|
| `devices` | `MediaDeviceInfo[]` | Available camera devices |
| `selectedDeviceId` | `string \| null` | Currently selected device ID |
| `onSelect` | `(deviceId: string) => void` | Device selection callback |
| `disabled` | `boolean?` | Disable selection (e.g., while recording) |

---

### `<ProcessingState>`

Displays compilation progress, video player, or failure state.

```tsx
<ProcessingState status="compiling" trackedSeconds={300} />
<ProcessingState status="complete" trackedSeconds={300} videoUrl="https://..." />
```

**Props (`ProcessingStateProps`):**

| Prop | Type | Description |
|------|------|-------------|
| `status` | `string` | Session status |
| `trackedSeconds` | `number` | Tracked time to display |
| `videoUrl` | `string?` | Video URL (shown when complete) |
| `error` | `string?` | Error message |
| `onVideoLoaded` | `() => void?` | Callback when video element loads |

---

### `<ResultView>`

Wraps `<ProcessingState>` with automatic video URL fetching from the API. Must be used within `<LookoutProvider>`.

```tsx
<ResultView status="complete" trackedSeconds={300} />
```

**Props (`ResultViewProps`):**

| Prop | Type | Description |
|------|------|-------------|
| `status` | `RecorderStatus` | Session status |
| `trackedSeconds` | `number` | Tracked time |

---

### `<Gallery>`

Grid of session cards with loading, empty, and error states.

```tsx
<Gallery
  sessions={sessions}
  loading={false}
  error={null}
  onSessionClick={(token) => navigate({ page: "session", token })}
  onArchive={(token) => store.archiveToken(token)}
  onRefresh={refresh}
/>
```

**Props (`GalleryProps`):**

| Prop | Type | Description |
|------|------|-------------|
| `sessions` | `SessionSummary[]` | Sessions to display |
| `loading` | `boolean` | Show skeleton loader |
| `error` | `string \| null` | Error message |
| `onSessionClick` | `(token: string) => void?` | Card click handler |
| `onArchive` | `(token: string) => void?` | Archive button handler |
| `onRefresh` | `() => void?` | Refresh button handler |

---

### `<SessionCard>`

Individual session card with thumbnail, status badge, timelapse name, tracked time, and recording date.

```tsx
<SessionCard session={session} onClick={() => {}} onArchive={() => {}} />
```

**Props (`SessionCardProps`):**

| Prop | Type | Description |
|------|------|-------------|
| `session` | `SessionSummary` | Session data |
| `onClick` | `() => void?` | Click handler |
| `onArchive` | `() => void?` | Archive button handler |

---

### `<SessionDetail>`

Full session detail view with video player, stats, and compilation polling. Standalone (no provider needed).

```tsx
<SessionDetail
  token="..."
  apiBaseUrl="https://lookout.hackclub.com"
  onBack={() => navigate({ page: "gallery" })}
  onArchive={() => store.archiveToken(token)}
/>
```

**Props (`SessionDetailProps`):**

| Prop | Type | Description |
|------|------|-------------|
| `token` | `string` | Session token |
| `apiBaseUrl` | `string` | Server API base URL |
| `onBack` | `() => void?` | Back button handler |
| `onArchive` | `() => void?` | Archive button handler |

---

## Callbacks

Pass via `LookoutProvider`'s `callbacks` prop:

```tsx
<LookoutProvider
  token="..."
  callbacks={{
    onShareStart: () => console.log("sharing started"),
    onCapture: (capture) => console.log("captured", capture.width, "x", capture.height),
    onUploadSuccess: ({ screenshotId, trackedSeconds }) => {},
    onUploadFailure: (error) => {},
    onPause: ({ totalActiveSeconds }) => {},
    onResume: () => {},
    onStop: ({ trackedSeconds, totalActiveSeconds }) => {},
    onComplete: ({ videoUrl }) => {},
    onCompilationFailed: () => {},
    onError: (error, context) => {},
    onStatusChange: (prev, next) => {},
  }}
>
```

| Callback | Arguments | When |
|----------|-----------|------|
| `onShareStart` | — | Screen sharing started |
| `onShareStop` | — | Screen sharing ended |
| `onCapture` | `CaptureResult` | Screenshot captured (before upload) |
| `onUploadSuccess` | `{ screenshotId, trackedSeconds }` | Screenshot uploaded and confirmed |
| `onUploadFailure` | `Error` | Upload failed after all retries |
| `onPause` | `{ totalActiveSeconds }` | Session paused |
| `onResume` | — | Session resumed |
| `onStop` | `{ trackedSeconds, totalActiveSeconds }` | Session stopped |
| `onComplete` | `{ videoUrl }` | Compilation complete, video ready |
| `onCompilationFailed` | — | Compilation failed |
| `onError` | `(Error, context: string)` | Any non-fatal error |
| `onStatusChange` | `(prev, next)` | Status transition |

---

## API Client

### `createLookoutClient(options)`

Standalone API client with no React dependency. Useful for server-side or non-React contexts.

```ts
import { createLookoutClient } from "@lookout/react";

const client = createLookoutClient({
  baseUrl: "https://lookout.hackclub.com",
  token: "your-token",
});

const session = await client.getSession();
```

**Options (`CreateClientOptions`):**

| Field | Type | Description |
|-------|------|-------------|
| `baseUrl` | `string` | Server API base URL |
| `token` | `TokenProvider` | Session token (string, sync, or async getter) |

**Returns (`LookoutClient`):**

| Method | Signature | Description |
|--------|-----------|-------------|
| `resolveToken` | `() => Promise<string>` | Resolve the token value |
| `getSession` | `() => Promise<SessionResponse>` | Fetch session status |
| `getUploadUrl` | `() => Promise<UploadUrlResponse>` | Get presigned upload URL |
| `confirmScreenshot` | `(body) => Promise<ConfirmScreenshotResponse>` | Confirm upload |
| `uploadToR2` | `(uploadUrl, blob) => Promise<void>` | PUT blob to presigned URL |
| `pause` | `() => Promise<PauseResponse>` | Pause session |
| `resume` | `() => Promise<ResumeResponse>` | Resume session |
| `stop` | `() => Promise<StopResponse>` | Stop session |
| `rename` | `(name: string) => Promise<RenameSessionResponse>` | Rename the timelapse |
| `getStatus` | `() => Promise<StatusResponse>` | Poll compilation status |
| `getVideo` | `() => Promise<VideoResponse>` | Get video URL |

---

## UI Primitives

The SDK exports styled UI primitives used by its components. All use inline styles (no CSS imports needed).

| Export | Description |
|--------|-------------|
| `Button` | Styled button with variants: `primary`, `secondary`, `success`, `warning`, `danger`, `ghost` and sizes: `sm`, `md`, `lg` |
| `Spinner` | Loading spinner with sizes: `sm`, `md`, `lg` |
| `Badge` | Status badge with variants: `default`, `overlay` |
| `Card` | Styled card container |
| `ErrorDisplay` | Error message display with variants: `inline`, `banner`, `page` |
| `PageContainer` | Page layout wrapper |
| `Skeleton` / `GallerySkeleton` / `SessionDetailSkeleton` / `RecordPageSkeleton` | Loading skeletons |
| `colors` / `spacing` / `radii` / `fontSize` / `fontWeight` / `statusConfig` | Theme tokens |

---

## Utilities

### `formatTime(totalSeconds)`

Formats seconds as `H:MM:SS` or `M:SS`. Used for the live timer display.

```ts
import { formatTime } from "@lookout/react";

formatTime(0);     // "0:00"
formatTime(65);    // "1:05"
formatTime(3661);  // "1:01:01"
```

### `formatTrackedTime(totalSeconds)`

Formats seconds as human-readable tracked time. Used for static time displays (gallery cards, stats) where second-level precision is unnecessary.

```ts
import { formatTrackedTime } from "@lookout/react";

formatTrackedTime(0);      // "< 1min"
formatTrackedTime(300);    // "5min"
formatTrackedTime(5640);   // "1h 34min"
formatTrackedTime(7200);   // "2h"
```
