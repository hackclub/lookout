import type { SessionStatus } from "./constants.js";

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
  createdAt: string;
}

// -- API request/response types --

export interface CreateSessionRequest {
  name?: string;
  metadata?: Record<string, unknown>;
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
  metadata: Record<string, unknown>;
}

export type TrackingMode = "bucket" | "credit";

export interface UploadUrlResponse {
  uploadUrl: string;
  r2Key: string;
  screenshotId: string;
  minuteBucket: number;
  nextExpectedAt: string;
  /** Server wall-clock time at the moment this response was generated.
   *  Optional — not present on responses from pre-0.3 servers. Clients
   *  may use it for diagnostics; scheduling needs only `nextExpectedAt`. */
  serverTime?: string;
  /** Sticky tracking mode for the session. Optional for backwards compat. */
  trackingMode?: TrackingMode;
}

export interface ConfirmScreenshotRequest {
  screenshotId: string;
  width: number;
  height: number;
  fileSize: number;
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

export interface StopResponse {
  status: "stopped";
  trackedSeconds: number;
  totalActiveSeconds: number;
}

export interface StatusResponse {
  status: SessionStatus;
  progress?: number;
  videoUrl?: string;
  /** @deprecated WebM is no longer produced. Populated only for legacy clients —
   * points at a static "please update" message video. */
  videoWebmUrl?: string;
  trackedSeconds: number;
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
