import type {
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
} from "@lookout/shared";
import type { TokenProvider } from "../types.js";

export interface LookoutClient {
  resolveToken(): Promise<string>;
  getSession(): Promise<SessionResponse>;
  /** `capturedAt` is optional. Sending it on the first request of a new
   *  session opts the session into credit-mode tracking; subsequent
   *  requests must keep sending it. Omit for legacy bucket-count behavior. */
  getUploadUrl(opts?: { capturedAt?: string }): Promise<UploadUrlResponse>;
  confirmScreenshot(body: ConfirmScreenshotRequest): Promise<ConfirmScreenshotResponse>;
  uploadToR2(uploadUrl: string, blob: Blob): Promise<void>;
  pause(): Promise<PauseResponse>;
  resume(): Promise<ResumeResponse>;
  stop(): Promise<StopResponse>;
  rename(name: string): Promise<RenameSessionResponse>;
  getStatus(): Promise<StatusResponse>;
  getVideo(): Promise<VideoResponse>;
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
}

async function resolveTokenValue(provider: TokenProvider): Promise<string> {
  if (typeof provider === "string") return provider;
  const result = provider();
  return result instanceof Promise ? result : result;
}

async function fetchJson<T>(url: string, init?: RequestInit): Promise<T> {
  const headers: Record<string, string> = {};
  if (init?.body) {
    headers["Content-Type"] = "application/json";
  }
  let res: Response;
  try {
    res = await fetch(url, { ...init, headers: { ...headers, ...(init?.headers as Record<string, string>) } });
  } catch (err) {
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
  const { baseUrl, token } = options;

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
      const url = opts?.capturedAt
        ? `${base}?capturedAt=${encodeURIComponent(opts.capturedAt)}`
        : base;
      return fetchJson<UploadUrlResponse>(url);
    },

    async confirmScreenshot(body) {
      return fetchJson<ConfirmScreenshotResponse>(await sessionUrl("/screenshots"), {
        method: "POST",
        body: JSON.stringify(body),
      });
    },

    async uploadToR2(uploadUrl, blob) {
      if (!uploadUrl.startsWith("https://") && !uploadUrl.startsWith("/")) {
        throw new Error("Invalid upload URL: must be HTTPS or a relative path.");
      }
      let res: Response;
      try {
        res = await fetch(uploadUrl, {
          method: "PUT",
          body: blob,
          headers: { "Content-Type": "image/jpeg" },
        });
      } catch (err) {
        if (err instanceof TypeError) {
          throw new Error(
            "Upload failed: network error or CORS misconfiguration on R2 bucket.",
          );
        }
        throw err;
      }
      if (!res.ok) {
        const text = await res.text().catch(() => "");
        throw new Error(
          `R2 upload failed: HTTP ${res.status}${text ? " — " + text.slice(0, 200) : ""}`,
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

    async stop() {
      return fetchJson<StopResponse>(await sessionUrl("/stop"), {
        method: "POST",
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
  };
}
