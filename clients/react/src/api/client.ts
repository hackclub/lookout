import { UPLOAD_STEP_TIMEOUT_MS } from "@lookout/shared";
import type {
  CaptureFormat,
  SessionResponse,
  UploadUrlResponse,
  ConfirmScreenshotRequest,
  ConfirmScreenshotResponse,
  PauseResponse,
  ResumeResponse,
  StopResponse,
  RenameSessionResponse,
  StatusResponse,
  VideoResponse,
  UnitsResponse,
  SetCutsResponse,
  ApplyCutsResponse,
  EditHeartbeatResponse,
  CutInterval,
  MaskRegion,
} from "@lookout/shared";
import type { TokenProvider } from "../types.js";

export interface LookoutClient {
  resolveToken(): Promise<string>;
  getSession(): Promise<SessionResponse>;
  /** `capturedAt` is optional. Sending it on the first request of a new
   *  session opts the session into credit-mode tracking; subsequent
   *  requests must keep sending it. Omit for legacy bucket-count behavior.
   *  `format` requests a clip upload ('webm'/'mp4'); omit for a single
   *  JPEG. The response's `format` is the GRANTED format — the caller
   *  must upload exactly that. */
  getUploadUrl(opts?: {
    capturedAt?: string;
    format?: CaptureFormat;
  }): Promise<UploadUrlResponse>;
  confirmScreenshot(body: ConfirmScreenshotRequest): Promise<ConfirmScreenshotResponse>;
  uploadToR2(uploadUrl: string, blob: Blob, contentType?: string): Promise<void>;
  pause(): Promise<PauseResponse>;
  resume(): Promise<ResumeResponse>;
  /** Stop the session. Pass `{ edit: true }` to hold it unpublished after
   *  compiling so the user can cut it first — programs never see
   *  `complete` until the edits are baked in. The hold auto-publishes if
   *  the user walks away, so this can never strand a timelapse. Only send
   *  it from a client that can actually render the editor. */
  stop(opts?: { edit?: boolean }): Promise<StopResponse>;
  rename(name: string): Promise<RenameSessionResponse>;
  getStatus(): Promise<StatusResponse>;
  getVideo(): Promise<VideoResponse>;
  /** Editor metadata: the compiled original's unit map (video second i ↔
   *  wall clock), current cut list, and a token-gated presigned URL for the
   *  UNCUT original video. */
  getUnits(): Promise<UnitsResponse>;
  /** Replace the session's cut and mask lists.
   *  Returns the normalized list plus a server-authoritative preview. */
  setCuts(cuts: CutInterval[], masks?: MaskRegion[]): Promise<SetCutsResponse>;
  /** Apply the current cut list to the published video (a cut-compile —
   *  usually a lossless stream copy, seconds not minutes). */
  applyCuts(): Promise<ApplyCutsResponse>;
  /** Renew the edit lease — "an editor is still open". Call every
   *  EDIT_HEARTBEAT_SECONDS while an editing surface is showing; stop when
   *  the response reports `held: false`. */
  heartbeatEditing(): Promise<EditHeartbeatResponse>;
}

export class HttpError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = "HttpError";
    this.status = status;
  }
}

export interface CreateClientOptions {
  baseUrl: string;
  token: TokenProvider;
  /** Free-form client telemetry string attached to every upload-url request
   *  (server query param `clientInfo`). Optional. */
  clientInfo?: string;
}

async function resolveTokenValue(provider: TokenProvider): Promise<string> {
  if (typeof provider === "string") return provider;
  const result = provider();
  return result instanceof Promise ? result : result;
}

/**
 * An AbortSignal that fires after UPLOAD_STEP_TIMEOUT_MS.
 *
 * `fetch` never times out on its own, so one half-open socket would
 * otherwise park a request forever — and the capture loop has nothing to
 * retry until it settles. Returns undefined on engines without
 * `AbortSignal.timeout` (pre-2022 Safari/Firefox) rather than shimming it:
 * those users get exactly today's behaviour, nobody gets a hard failure.
 */
function stepDeadline(): AbortSignal | undefined {
  return typeof AbortSignal !== "undefined" &&
    typeof AbortSignal.timeout === "function"
    ? AbortSignal.timeout(UPLOAD_STEP_TIMEOUT_MS)
    : undefined;
}

/** True for the AbortError a stepDeadline fires. */
function isTimeout(err: unknown): boolean {
  return err instanceof Error && (err.name === "TimeoutError" || err.name === "AbortError");
}

async function fetchJson<T>(url: string, init?: RequestInit): Promise<T> {
  const headers: Record<string, string> = {};
  if (init?.body) {
    headers["Content-Type"] = "application/json";
  }
  let res: Response;
  try {
    res = await fetch(url, {
      signal: stepDeadline(),
      ...init,
      headers: { ...headers, ...(init?.headers as Record<string, string>) },
    });
  } catch (err) {
    if (isTimeout(err)) {
      throw new Error(
        `Timed out after ${UPLOAD_STEP_TIMEOUT_MS / 1000}s fetching ${url}`,
      );
    }
    // Network-level failure (DNS, connection refused, CORS, SSL)
    // WebKit just says "Load failed" — add the URL for context
    const msg = err instanceof Error ? err.message : String(err);
    throw new Error(`Network error fetching ${url}: ${msg}`);
  }
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    let detail = "";
    try {
      const json = JSON.parse(text);
      detail = json.error || json.message || text;
    } catch {
      detail = text;
    }
    throw new HttpError(
      res.status,
      `HTTP ${res.status} ${res.statusText} from ${url}${detail ? "\n" + detail.slice(0, 500) : ""}`,
    );
  }
  return res.json() as Promise<T>;
}

export function createLookoutClient(options: CreateClientOptions): LookoutClient {
  const { baseUrl, token, clientInfo } = options;

  const resolveToken = () => resolveTokenValue(token);

  async function sessionUrl(path = ""): Promise<string> {
    const t = await resolveToken();
    return `${baseUrl}/api/sessions/${t}${path}`;
  }

  return {
    resolveToken,

    async getSession() {
      return fetchJson<SessionResponse>(await sessionUrl());
    },

    async getUploadUrl(opts) {
      const base = await sessionUrl("/upload-url");
      const params = new URLSearchParams();
      if (opts?.capturedAt) params.set("capturedAt", opts.capturedAt);
      if (opts?.format) params.set("format", opts.format);
      if (clientInfo) params.set("clientInfo", clientInfo);
      const qs = params.toString();
      return fetchJson<UploadUrlResponse>(qs ? `${base}?${qs}` : base);
    },

    async confirmScreenshot(body) {
      return fetchJson<ConfirmScreenshotResponse>(await sessionUrl("/screenshots"), {
        method: "POST",
        body: JSON.stringify(body),
      });
    },

    async uploadToR2(uploadUrl, blob, contentType = "image/jpeg") {
        let isAllowed = false;
        if (uploadUrl.startsWith("/") && !uploadUrl.startsWith("//")) {
            isAllowed = true;
        }
        else {
            try {
                const parsed = new URL(uploadUrl);
                const isLocal = (parsed.hostname === "localhost" || parsed.hostname === "127.0.0.1") && (parsed.protocol === "http:" || parsed.protocol === "https:");
                isAllowed = parsed.protocol === "https:" || isLocal;
            }
            catch {
                isAllowed = false;
            }
        }
        if (!isAllowed)
            throw new Error("Invalid upload URL: must be HTTPS or a relative path.");
      let res: Response;
      try {
        res = await fetch(uploadUrl, {
          method: "PUT",
          body: blob,
          // Must match the content type the presigned URL was signed with.
          headers: { "Content-Type": contentType },
          // The step most exposed to a weak uplink: this is the multi-MB
          // payload, and a stalled PUT used to hang the capture loop with
          // no error for it to fall back on.
          signal: stepDeadline(),
        });
      } catch (err) {
        if (isTimeout(err)) {
          throw new Error(
            `R2 upload timed out after ${UPLOAD_STEP_TIMEOUT_MS / 1000}s ` +
              `(${blob.size} bytes) — the connection stalled mid-transfer.`,
          );
        }
        if (err instanceof TypeError) {
          throw new Error(
            "Upload failed: network error or CORS misconfiguration on R2 bucket.",
          );
        }
        throw err;
      }
      if (!res.ok) {
        const text = await res.text().catch(() => "");
        // R2 answers with S3-style XML. A bare "403" is unactionable and
        // every cause has a different fix, so name the cause rather than
        // making the next person bisect it.
        const code = /<Code>([^<]+)<\/Code>/.exec(text)?.[1] ?? "";
        const detail = /<Message>([^<]+)<\/Message>/.exec(text)?.[1] ?? "";
        const hint =
          code === "SignatureDoesNotMatch"
            ? " The request didn't match the presigned URL — something between the client and R2 is altering the request (a proxy, or a rewritten method/headers)."
            : code === "RequestTimeTooSkewed"
              ? " The signing server's clock is off relative to R2; fix NTP on the API server."
              : code === "AccessDenied" || /expire/i.test(detail)
                ? " The presigned URL had expired (they last ~2 minutes) or the credentials can't write this key. A slow upload of a large clip can outrun the expiry."
                : res.status === 403 && !text
                  ? " Empty 403 body usually means CORS stripped the response: check the R2 bucket's CORS rules allow PUT from this origin."
                  : "";
        // The UI truncates; make the whole thing reachable in the console.
        console.error("[lookout] R2 upload failed", {
          status: res.status,
          code,
          detail,
          body: text.slice(0, 1000),
        });
        throw new Error(
          `R2 upload failed: HTTP ${res.status}${code ? ` (${code})` : ""}${
            detail ? ` — ${detail}` : text ? " — " + text.slice(0, 200) : ""
          }${hint}`,
        );
      }
    },

    async pause() {
      return fetchJson<PauseResponse>(await sessionUrl("/pause"), {
        method: "POST",
      });
    },

    async resume() {
      return fetchJson<ResumeResponse>(await sessionUrl("/resume"), {
        method: "POST",
      });
    },

    async stop(opts) {
      return fetchJson<StopResponse>(await sessionUrl("/stop"), {
        method: "POST",
        // Old servers ignore an unknown body; omit it entirely for the
        // plain stop so the request stays byte-identical to before.
        ...(opts?.edit ? { body: JSON.stringify({ edit: true }) } : {}),
      });
    },

    async rename(name: string) {
      return fetchJson<RenameSessionResponse>(await sessionUrl("/name"), {
        method: "PATCH",
        body: JSON.stringify({ name }),
      });
    },

    async getStatus() {
      return fetchJson<StatusResponse>(await sessionUrl("/status"));
    },

    async getVideo() {
      return fetchJson<VideoResponse>(await sessionUrl("/video"));
    },

    async getUnits() {
      return fetchJson<UnitsResponse>(await sessionUrl("/units"));
    },

    async setCuts(cuts, masks) {
      return fetchJson<SetCutsResponse>(await sessionUrl("/cuts"), {
        method: "PUT",
        body: JSON.stringify({ cuts, ...(masks !== undefined ? { masks } : {}) }),
      });
    },

    async applyCuts() {
      return fetchJson<ApplyCutsResponse>(await sessionUrl("/compile"), {
        method: "POST",
      });
    },

    async heartbeatEditing() {
      return fetchJson<EditHeartbeatResponse>(await sessionUrl("/editing"), {
        method: "POST",
      });
    },
  };
}
