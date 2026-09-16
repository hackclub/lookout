/**
 * The desktop's Lookout API client: every server call goes through the Rust
 * core (`lookout-core`'s `api` module) via Tauri commands, so the webview
 * never speaks HTTP to the Lookout server itself. The same code path a GTK
 * or Qt shell would use — one source of truth for URLs, bodies, timeouts
 * and error shaping.
 *
 * Implements the shared `LookoutClient` interface so `@lookout/react`'s
 * hooks and components run unchanged on top of it.
 */
import { invoke } from "@tauri-apps/api/core";
import {
  HttpError,
  type LookoutClient,
  type TokenProvider,
} from "@lookout/react";
import type {
  ApplyCutsResponse,
  BatchSessionsResponse,
  ConfirmScreenshotRequest,
  ConfirmScreenshotResponse,
  CutInterval,
  EditHeartbeatResponse,
  MaskRegion,
  PauseResponse,
  RenameSessionResponse,
  ResumeResponse,
  SessionResponse,
  SetCutsResponse,
  StatusResponse,
  StopResponse,
  UnitsResponse,
  UploadUrlResponse,
  VideoResponse,
} from "@lookout/shared";

/** What the Rust side's `ApiError` looks like once it crosses the IPC. */
interface ApiErrorPayload {
  status: number | null;
  message: string;
}

function isApiErrorPayload(e: unknown): e is ApiErrorPayload {
  return (
    typeof e === "object" &&
    e !== null &&
    "message" in e &&
    typeof (e as ApiErrorPayload).message === "string" &&
    "status" in e
  );
}

/** Rebuild the shared client's error types from the Rust `ApiError`, so the
 *  hooks' `instanceof HttpError && status === 409` checks keep working. */
function toError(e: unknown): Error {
  if (isApiErrorPayload(e)) {
    return e.status != null
      ? new HttpError(e.status, e.message)
      : new Error(e.message);
  }
  return e instanceof Error ? e : new Error(String(e));
}

/** What the old `[net]` fetch log recorded, minus the token: the endpoint
 *  and the outcome. Failures carry the Rust side's full message, which
 *  names the URL, the HTTP status and the server's error body. */
function describeArgs(args: Record<string, unknown>): string {
  const parts: string[] = [];
  if (typeof args.apiBaseUrl === "string") parts.push(args.apiBaseUrl);
  if (typeof args.token === "string") parts.push(`token ${args.token.slice(0, 8)}…`);
  return parts.join(" ");
}

async function call<T>(command: string, args: Record<string, unknown>): Promise<T> {
  const what = `[api] ${command} ${describeArgs(args)}`.trimEnd();
  console.log(what);
  try {
    const result = await invoke<T>(command, args);
    console.debug(`${what} → ok`);
    return result;
  } catch (e) {
    const err = toError(e);
    console.error(`${what} → FAILED: ${err.message}`);
    throw err;
  }
}

async function resolveTokenValue(provider: TokenProvider): Promise<string> {
  if (typeof provider === "string") return provider;
  return provider();
}

function blobToBase64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error("Failed to read blob"));
    reader.onload = () => {
      // data:<mime>;base64,<payload>
      const result = String(reader.result);
      resolve(result.slice(result.indexOf(",") + 1));
    };
    reader.readAsDataURL(blob);
  });
}

export interface CreateTauriClientOptions {
  baseUrl: string;
  token: TokenProvider;
}

export function createTauriLookoutClient({
  baseUrl,
  token,
}: CreateTauriClientOptions): LookoutClient {
  const resolveToken = () => resolveTokenValue(token);
  const session = async () => ({ token: await resolveToken(), apiBaseUrl: baseUrl });

  return {
    resolveToken,

    async getSession() {
      return call<SessionResponse>("api_session_get", await session());
    },

    async getUploadUrl(opts) {
      return call<UploadUrlResponse>("api_session_upload_url", {
        ...(await session()),
        capturedAt: opts?.capturedAt ?? null,
        format: opts?.format ?? null,
      });
    },

    async confirmScreenshot(body: ConfirmScreenshotRequest) {
      return call<ConfirmScreenshotResponse>("api_session_confirm_screenshot", {
        ...(await session()),
        body,
      });
    },

    async uploadToR2(uploadUrl, blob, contentType = "image/jpeg") {
      await call<void>("api_upload_to_r2", {
        uploadUrl,
        bytesBase64: await blobToBase64(blob),
        contentType,
      });
    },

    async pause() {
      return call<PauseResponse>("api_session_pause", await session());
    },

    async resume() {
      return call<ResumeResponse>("api_session_resume", await session());
    },

    async stop(opts) {
      return call<StopResponse>("api_session_stop", {
        ...(await session()),
        edit: opts?.edit === true,
      });
    },

    async rename(name: string) {
      return call<RenameSessionResponse>("api_session_rename", {
        ...(await session()),
        name,
      });
    },

    async getStatus() {
      return call<StatusResponse>("api_session_status", await session());
    },

    async getVideo() {
      return call<VideoResponse>("api_session_video", await session());
    },

    async getUnits() {
      return call<UnitsResponse>("api_session_units", await session());
    },

    async setCuts(cuts: CutInterval[], masks?: MaskRegion[]) {
      return call<SetCutsResponse>("api_session_set_cuts", {
        ...(await session()),
        cuts,
        masks: masks ?? [],
      });
    },

    async applyCuts() {
      return call<ApplyCutsResponse>("api_session_apply_cuts", await session());
    },

    async heartbeatEditing() {
      return call<EditHeartbeatResponse>("api_session_heartbeat_editing", await session());
    },
  };
}

// ── Not session-scoped ──────────────────────────────────────────────

export interface Program {
  name: string;
  displayName?: string;
  newSessionUrl: string;
  iconUrl?: string | null;
  // Desktop instant-start endpoints (both present or both null) — see
  // programLink.ts. Older servers simply omit them.
  pairUrl?: string | null;
  startUrl?: string | null;
}

export interface ProgramsResponse {
  programs: Program[];
}

/** `GET /api/programs`. `timeoutMs` overrides the core's 30s default — the
 *  Settings page probes a user-typed server with a short one. */
export function fetchPrograms(apiBaseUrl: string, timeoutMs?: number): Promise<ProgramsResponse> {
  return call<ProgramsResponse>("api_programs", {
    apiBaseUrl,
    timeoutMs: timeoutMs ?? null,
  });
}

export interface AnnouncementResponse {
  announcement: {
    level: "info" | "success" | "warning" | "danger";
    message: string;
    url: string | null;
  } | null;
}

/** `GET /api/announcement`. `client`/`version` report who is asking so the
 *  server can target announcements; both optional. */
export function fetchAnnouncement(
  apiBaseUrl: string,
  opts?: { client?: string; version?: string },
): Promise<AnnouncementResponse> {
  return call<AnnouncementResponse>("api_announcement", {
    apiBaseUrl,
    client: opts?.client ?? null,
    version: opts?.version ?? null,
  });
}

/** `GET /api/tip` — the active tip sheet, if any. Same targeting as
 *  `fetchAnnouncement`. The `tip` shape is the desktop's `Tip` type. */
export function fetchTip<T = unknown>(
  apiBaseUrl: string,
  opts?: { client?: string; version?: string },
): Promise<{ tip: T | null }> {
  return call<{ tip: T | null }>("api_tip", {
    apiBaseUrl,
    client: opts?.client ?? null,
    version: opts?.version ?? null,
  });
}

/** `POST /api/sessions/batch` — gallery summaries for up to 100 tokens. */
export function fetchSessionsBatch(
  apiBaseUrl: string,
  tokens: string[],
): Promise<BatchSessionsResponse> {
  return call<BatchSessionsResponse>("api_sessions_batch", { apiBaseUrl, tokens });
}
