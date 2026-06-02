import type {
  SessionResponse,
  UploadUrlResponse,
  ConfirmScreenshotRequest,
  ConfirmScreenshotResponse,
  PauseResponse,
  ResumeResponse,
  StopResponse,
  StatusResponse,
  VideoResponse,
} from "@lookout/shared";
import { CLIENT_INFO } from "../clientInfo";

export class HttpError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = "HttpError";
    this.status = status;
  }
}

const API_BASE = "/api";

function getToken(): string {
  const params = new URLSearchParams(window.location.search);
  const token = params.get("token");
  if (!token) throw new Error("No session token in URL");
  return token;
}

async function fetchJson<T>(url: string, init?: RequestInit): Promise<T> {
  const headers: Record<string, string> = { ...init?.headers as Record<string, string> };
  // Only set Content-Type for requests with a body
  if (init?.body) {
    headers["Content-Type"] = "application/json";
  }
  const res = await fetch(url, { ...init, headers });
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

export const api = {
  getToken,

  getSession(): Promise<SessionResponse> {
    return fetchJson(`${API_BASE}/sessions/${getToken()}`);
  },

  /** `capturedAt` is optional — sending it on a session's first request
   *  opts that session into credit-mode tracking. Omit for legacy behavior.
   *  `clientInfo` telemetry is attached automatically on every request. */
  getUploadUrl(opts?: { capturedAt?: string }): Promise<UploadUrlResponse> {
    const base = `${API_BASE}/sessions/${getToken()}/upload-url`;
    const params = new URLSearchParams();
    if (opts?.capturedAt) params.set("capturedAt", opts.capturedAt);
    params.set("clientInfo", CLIENT_INFO);
    return fetchJson(`${base}?${params.toString()}`);
  },

  confirmScreenshot(
    body: ConfirmScreenshotRequest,
  ): Promise<ConfirmScreenshotResponse> {
    return fetchJson(`${API_BASE}/sessions/${getToken()}/screenshots`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  async uploadToR2(uploadUrl: string, blob: Blob): Promise<void> {
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

  pause(): Promise<PauseResponse> {
    return fetchJson(`${API_BASE}/sessions/${getToken()}/pause`, {
      method: "POST",
    });
  },

  resume(): Promise<ResumeResponse> {
    return fetchJson(`${API_BASE}/sessions/${getToken()}/resume`, {
      method: "POST",
    });
  },

  stop(): Promise<StopResponse> {
    return fetchJson(`${API_BASE}/sessions/${getToken()}/stop`, {
      method: "POST",
    });
  },

  getStatus(): Promise<StatusResponse> {
    return fetchJson(`${API_BASE}/sessions/${getToken()}/status`);
  },

  getVideo(): Promise<VideoResponse> {
    return fetchJson(`${API_BASE}/sessions/${getToken()}/video`);
  },
};
