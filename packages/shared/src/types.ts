import type { CaptureFormat, SessionStatus } from "./constants.js";
import type { CutInterval, VideoUnit } from "./cuts.js";
import type { MaskRegion } from "./masks.js";

export interface Session {
  id: string;
  token: string;
  name: string;
  metadata: Record<string, unknown>;
  status: SessionStatus;
  startedAt: string | null;
  stoppedAt: string | null;
  pausedAt: string | null;
  lastScreenshotAt: string | null;
  resumedAt: string | null;
  totalActiveSeconds: number;
  videoUrl: string | null;
  videoR2Key: string | null;
  thumbnailUrl: string | null;
  thumbnailR2Key: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface Screenshot {
  id: string;
  sessionId: string;
  r2Key: string;
  requestedAt: string;
  minuteBucket: number;
  confirmed: boolean;
  width: number | null;
  height: number | null;
  fileSizeBytes: number | null;
  sampled: boolean;
  /** Payload format of this capture unit. "jpeg" = legacy single frame;
   *  "webm"/"mp4" = per-minute clip. */
  format: CaptureFormat;
  /** Client-reported frame count inside a clip. Informational only —
   *  the worker derives the real count by demuxing. NULL for jpeg rows. */
  frameCount: number | null;
  createdAt: string;
}

/**
 * Free-form client telemetry string, reported by the recording client on the
 * `upload-url` request (query param `clientInfo`) and stored opaquely per
 * screenshot. This is NOT the HTTP User-Agent — it's explicit, for
 * telemetry/debugging. The server never parses it.
 *
 * Recommended (not enforced) format, User-Agent–like, encoding: Lookout type,
 * version, embedded host app (web/sdk), OS type+version, browser type+version
 * (web/sdk):
 *   "Lookout Desktop/0.2.6 (macOS 14.3)"
 *   "Lookout Web (Fallout)/0.2.6 (macOS 14.3; Chrome 120.0)"
 *   "Lookout SDK (Stardance)/0.2.6 (Windows 10; Firefox 121.0)"
 */
export type ClientInfo = string;

// -- API request/response types --

export interface CreateSessionRequest {
  name?: string;
  metadata?: Record<string, unknown>;
  /** Whether this session receives clip uploads (per-minute videos of ~6
   *  frames) instead of one JPEG per minute. Defaults to TRUE — pass false
   *  to opt this session out and pin it to the legacy payload. Immutable
   *  after creation. */
  clips?: boolean;
  /** Redirect hook: http(s) URL the recording client sends the user to once
   *  the timelapse finishes compiling (the desktop app opens it in the
   *  default browser). Immutable after creation. */
  redirectUrl?: string;
  /** The program's own page for this session (its permalink), offered to the
   *  user as an "Open in <Program>" action. Unlike the other two URLs this one
   *  is mutable — see POST /api/internal/sessions/:id/view-url. */
  viewUrl?: string;
  /** Program panel: an https URL the recording client renders IN-APP (a sheet
   *  with an iframe) when the timelapse finishes, in place of the redirect
   *  hop, so the program can collect what it needs without an app switch.
   *  Must be an unguessable per-session URL — it is the panel's only
   *  credential. Immutable after creation. */
  panelUrl?: string;
}

export interface CreateSessionResponse {
  token: string;
  sessionId: string;
  sessionUrl: string;
}

export interface SessionResponse {
  name: string;
  status: SessionStatus;
  trackedSeconds: number;
  screenshotCount: number;
  startedAt: string | null;
  totalActiveSeconds: number;
  createdAt: string;
  thumbnailUrl: string | null;
  videoUrl: string | null;
  /** @deprecated WebM is no longer produced. Populated only for legacy clients —
   * points at a static "please update" message video. */
  videoWebmUrl?: string | null;
  /** First recorded client telemetry for the session; `null` if none captured. */
  clientInfo?: ClientInfo | null;
  /** Whether this session accepts clip uploads. Clients read this BEFORE
   *  the first capture (this endpoint is the session-recovery fetch) so
   *  the very first upload can already be a clip. Absent on pre-clips
   *  servers — treat as false. */
  clipsEnabled?: boolean;
  /** Server-authoritative clip cadence (ms between frames). Absent on
   *  pre-clips servers. */
  frameIntervalMs?: number;
  /** Redirect hook URL to open once the timelapse completes; `null`/absent
   *  when the session has none. */
  redirectUrl?: string | null;
  /** Program panel URL, rendered in-app in place of the redirect. `null`/
   *  absent when the session has none, or on pre-panel servers. */
  panelUrl?: string | null;
  /** The program's page for this session, if it set one. Clients offer it as
   *  an "Open in <Program>" action. `null`/absent when there is none. */
  viewUrl?: string | null;
  /** Whether the program has confirmed the panel's ask is satisfied (it may
   *  have been answered on the program's own site rather than in the sheet).
   *  Clients stop offering the panel once this is true. */
  panelResolved?: boolean;
  /** The session's cut list; `[]` when never edited. Absent on pre-edits
   *  servers. Note `trackedSeconds` already reflects these cuts. */
  cuts?: CutInterval[];
  /** Credited seconds removed by `cuts` (trackedSeconds = uncut − this). */
  cutSeconds?: number;
  /** Tracked seconds before subtracting cuts. */
  uncutTrackedSeconds?: number;
  /** Whether the compiled timelapse can (still) be edited. */
  editable?: boolean;
  metadata: Record<string, unknown>;
}

export type TrackingMode = "bucket" | "credit";

export interface TimingsResponse {
  status: SessionStatus;
  count: number;
  first: string | null;
  last: string | null;
  /** First recorded client telemetry for the session; `null` if none captured. */
  clientInfo: ClientInfo | null;
  /** Kept capture timestamps — captures inside a cut interval are EXCLUDED
   *  so heartbeat forwarders respect edits with no code changes. */
  timestamps: string[];
  /** The session's cut list; `[]` when never edited. Absent on pre-edits
   *  servers. */
  cuts?: CutInterval[];
  /** Number of confirmed captures removed by `cuts`. */
  cutCount?: number;
  /** Capture timestamps removed by `cuts`. Only present when the request
   *  passed `?includeCut=true`. */
  cutTimestamps?: string[];
}

// -- Edits (cuts & masks) --

export interface VideoShot {
  id: string;
  unitIndex: number; // Minute/unit index
  frameIndex: number;// Frame index within unit
  startSec: number;  // Offset in seconds
  endSec: number;    // Offset in seconds
  duration: number;  // Shot length in seconds
}

export interface UnitsResponse {
  /** Units of the compiled ORIGINAL video, in output order: array index =
   *  video second = real-world minute. Empty for sessions compiled before
   *  edit support (not editable). */
  units: VideoUnit[];
  /** Video shots with sub-second timestamps. */
  shots?: VideoShot[];
  cuts: CutInterval[];
  /** Active mask regions. */
  masks?: MaskRegion[];
  /** Whether the session is currently editable: an edit hold is active,
   *  the original video is built, and recompile budget remains. Editing is
   *  only possible during the hold — never after `complete`. */
  editable: boolean;
  /** Why `editable` is false (for UX copy); absent when editable.
   *  `"preparing"` means the hold is active and the preview video is still
   *  compiling — keep polling, it will become editable. */
  editableReason?:
    | "preparing"
    | "no_original"
    | "recompiles_exhausted"
    | "not_ready"
    | "failed"
    | "published";
  /** When the edit hold auto-publishes; null when no hold is active. */
  editHoldUntil?: string | null;
  /** Confirmed captures in the session ≈ units the finished video will
   *  hold. Lets a client waiting on the build size a progress estimate. */
  expectedUnits?: number;
  /** Presigned GET URL (~1h) of the UNCUT original video — the editor's
   *  preview source. Token-gated by this endpoint; deliberately NOT the
   *  public media URL, which after an edit serves the cut version only.
   *  Null when not editable. */
  originalVideoUrl: string | null;
  /** Remaining user-initiated cut-compiles. */
  recompilesRemaining: number;
}

export interface SetCutsRequest {
  cuts: CutInterval[];
  masks?: MaskRegion[];
}

export interface SetCutsResponse {
  cuts: CutInterval[];
  /** Persisted mask regions. */
  masks?: MaskRegion[];
  /** Units in the original video. */
  unitsTotal: number;
  /** Units removed by the normalized list. */
  unitsCut: number;
  /** Post-cut tracked seconds (what GET /sessions/:token will report once
   *  the cuts are applied). */
  trackedSeconds: number;
  /** Tracked seconds before subtracting cuts. */
  uncutTrackedSeconds: number;
}

export interface ApplyCutsResponse {
  status: SessionStatus;
  /** True when the change was applied instantly without a compile job
   *  (clearing all cuts just repoints the published video at the original). */
  instant: boolean;
  recompilesRemaining: number;
  /** The session's redirect hook URL (immutable, set at creation). Echoed
   *  here so the recording client can fire the redirect the instant publish
   *  completes — no second request, no race. Null when none was configured. */
  redirectUrl: string | null;
  /** The session's program panel URL (immutable, set at creation). Echoed
   *  alongside `redirectUrl` for the same reason: the client that publishes
   *  from the editor decides between panel and redirect the instant it lands,
   *  and needs both to make that choice. Null when none was configured. */
  panelUrl?: string | null;
  /** Whether the program has confirmed the panel is satisfied. When true, the
   *  client skips opening the panel — the answers already exist. */
  panelResolved?: boolean;
}

export interface UploadUrlResponse {
  uploadUrl: string;
  r2Key: string;
  screenshotId: string;
  minuteBucket: number;
  nextExpectedAt: string;
  /** Server wall-clock time at the moment this response was generated.
   *  Optional — not present on responses from pre-0.3 servers. Clients use
   *  it to learn their own clock offset (see `capturedAtAdopted`);
   *  scheduling needs only `nextExpectedAt`. */
  serverTime?: string;
  /** Set when the server replaced this capture's `capturedAt` with its own
   *  clock because the client's was outside the trust envelope.
   *
   *  The upload still succeeded — a wrong system clock never costs a
   *  recording. But the capture was stamped on ARRIVAL, so it carries upload
   *  latency and its credit is measured slightly late. A client seeing this
   *  should re-derive its offset from `serverTime` and apply it to later
   *  timestamps. Absent on servers that predate skew adoption. */
  capturedAtAdopted?: boolean;
  /** Sticky tracking mode for the session. Optional for backwards compat. */
  trackingMode?: TrackingMode;
  /** Echo of the GRANTED capture format — may differ from the requested
   *  one (the server downgrades clip formats to "jpeg" on sessions where
   *  clips are disabled). Absent on pre-clips servers — clients MUST
   *  treat absence as "server only supports jpeg". The client must
   *  upload exactly this format. */
  format?: CaptureFormat;
  /** Whether this session accepts clip uploads. Absent on pre-clips
   *  servers (treat as false). */
  clipsEnabled?: boolean;
  /** Server-authoritative clip cadence (ms between frames inside a
   *  clip). Absent on pre-clips servers. Clients must capture at exactly
   *  this rate — there is deliberately no client-side override. */
  frameIntervalMs?: number;
}

export interface ConfirmScreenshotRequest {
  screenshotId: string;
  width: number;
  height: number;
  fileSize: number;
  /** Frames inside the uploaded clip. Omit for jpeg captures. */
  frameCount?: number;
  /** The capture flushed at pause/stop. Credit-mode servers credit the
   *  partial minute since the last credited mark (creditFinalCapture)
   *  instead of the all-or-nothing streak rule. Omit on ordinary ticks;
   *  pre-final servers reject unknown fields, so only send when true. */
  final?: boolean;
}

export interface ConfirmScreenshotResponse {
  confirmed: true;
  trackedSeconds: number;
  nextExpectedAt: string;
  /** Server wall-clock time. Optional for backwards compat. */
  serverTime?: string;
}

export interface PauseResponse {
  status: "paused";
  totalActiveSeconds: number;
}

export interface ResumeResponse {
  status: "active";
  nextExpectedAt: string;
  /** Optional server wall-clock time. */
  serverTime?: string;
}

export interface StopRequest {
  /** Hold the session unpublished after compiling so the user can edit
   *  (cut) it before programs see `complete`. The hold is a lease the open
   *  editor renews (see `POST /:token/editing`); it publishes on its own
   *  once nothing is renewing it. Only send this from a client that will
   *  actually open an editing surface. */
  edit?: boolean;
}

export interface EditHeartbeatResponse {
  /** Extended lease deadline. The session publishes at this time unless
   *  renewed again. */
  editHoldUntil: string;
  /** False once the session published anyway (lease lapsed earlier, or the
   *  absolute ceiling was hit) — the caller should stop renewing and show
   *  the published state. */
  held: boolean;
}

export interface StopResponse {
  status: "stopped";
  trackedSeconds: number;
  totalActiveSeconds: number;
  /** When the edit hold auto-publishes; present only when the stop
   *  requested `edit: true`. */
  editHoldUntil?: string;
}

export interface StatusResponse {
  status: SessionStatus;
  /** Real compile progress as a fraction in [0, ~0.95], reported by the
   *  worker while it builds an original timelapse (the per-unit
   *  download+encode stage — the part whose cost scales with session
   *  length). Capped below 1: assembly/upload still run after the last unit,
   *  and only the status flip ends the wait. Absent for cut-apply compiles
   *  and workers predating the column — fall back to the time estimate. */
  progress?: number;
  videoUrl?: string;
  /** @deprecated WebM is no longer produced. Populated only for legacy clients —
   * points at a static "please update" message video. */
  videoWebmUrl?: string;
  trackedSeconds: number;
  /** Redirect hook URL — clients watching the compile open this when the
   *  status flips to "complete". Absent when the session has none. */
  redirectUrl?: string;
  /** Program panel URL — clients that can render one show it in-app when the
   *  status flips to "complete", instead of following `redirectUrl` out to a
   *  browser. Absent when the session has none, or on pre-panel servers. */
  panelUrl?: string;
  /** Whether the program has confirmed the panel's ask is satisfied. Clients
   *  must not open a panel when this is true: it would close itself at once. */
  panelResolved?: boolean;
  /** Whether the session is editable RIGHT NOW: an edit hold is active and
   *  its preview video has finished building. Only ever true while
   *  `stopped` — never after `complete`, which is the point at which
   *  programs consume the session's data. */
  editable?: boolean;
  /** When the edit hold auto-publishes the session (uncut). Absent when no
   *  hold is active. While this is set and `editable` is false, the
   *  preview is still compiling — show "preparing", not "done". */
  editHoldUntil?: string;
}

export interface VideoResponse {
  videoUrl: string;
}

export interface ThumbnailResponse {
  thumbnailUrl: string;
}

export interface SessionSummary {
  token: string;
  name: string;
  status: SessionStatus;
  trackedSeconds: number;
  screenshotCount: number;
  startedAt: string | null;
  createdAt: string;
  totalActiveSeconds: number;
  thumbnailUrl: string | null;
  videoUrl: string | null;
  /** @deprecated WebM is no longer produced. Populated only for legacy clients —
   * points at a static "please update" message video. */
  videoWebmUrl?: string | null;
  metadata: Record<string, unknown>;
}

export interface RenameSessionRequest {
  name: string;
}

export interface RenameSessionResponse {
  name: string;
}

export interface BatchSessionsRequest {
  tokens: string[];
}

export interface BatchSessionsResponse {
  sessions: SessionSummary[];
}
