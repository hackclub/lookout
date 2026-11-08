/**
 * Interaction metadata a native client sends with each confirm (`activity`):
 * pointer position as a fraction of its display, the frontmost app, and
 * whether the captured windows had the focus — sampled at every clip frame.
 *
 * Client-reported, so it is spoofable the way clientInfo is (unlike the
 * edge-observed JA4) — but it is what makes lazy cheating visible: a pointer
 * that never moves, identical coordinates minute after minute, a captured
 * window that is never in front.
 *
 * Like the JA4 this is INTERNAL ONLY: stored on the screenshot row and
 * returned on the api-key-authenticated internal API, never on a token
 * endpoint (the token belongs to the person being recorded, and echoing it
 * back would let a forger tune what they send).
 *
 * Sanitized rather than validated by the route schema: a confirm is what
 * credits a minute, and it must never fail because this side-channel came in
 * malformed. Anything off-shape is dropped (whole sample or whole field) and
 * the confirm goes through.
 */

/** Samples kept per capture. Matches the client's MAX_ACTIVITY_SAMPLES. */
export const ACTIVITY_MAX_SAMPLES = 120;
const MAX_APP_LENGTH = 256;
const MAX_WINDOWS = 32;
const METHOD_SHAPE = /^[a-z0-9-]{1,32}$/;

export type ActivityCursor = {
  display: number;
  x: number;
  y: number;
  inCapture: boolean;
};

export type ActivitySample = {
  at: number;
  method: string;
  cursor: ActivityCursor | null;
  frontmost: { app: string; captured: boolean | null } | null;
  windows?: { id: number; active: boolean }[];
};

export type Activity = { v: number; samples: ActivitySample[] };

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);

const isU32 = (v: unknown): v is number =>
  Number.isInteger(v) && (v as number) >= 0 && (v as number) <= 0xffffffff;

const isFraction = (v: unknown): v is number =>
  typeof v === "number" && Number.isFinite(v) && v >= 0 && v <= 1;

function sanitizeCursor(raw: unknown): ActivityCursor | null {
  if (!isObject(raw)) return null;
  const { display, x, y, inCapture } = raw;
  if (!isU32(display) || !isFraction(x) || !isFraction(y)) return null;
  if (typeof inCapture !== "boolean") return null;
  return { display, x, y, inCapture };
}

function sanitizeFrontmost(raw: unknown): ActivitySample["frontmost"] {
  if (!isObject(raw) || typeof raw.app !== "string") return null;
  const app = raw.app.trim().slice(0, MAX_APP_LENGTH);
  if (!app) return null;
  const captured = typeof raw.captured === "boolean" ? raw.captured : null;
  return { app, captured };
}

function sanitizeSample(raw: unknown): ActivitySample | null {
  if (!isObject(raw)) return null;
  const { at, method } = raw;
  if (!Number.isSafeInteger(at) || (at as number) <= 0) return null;
  if (typeof method !== "string" || !METHOD_SHAPE.test(method)) return null;
  const sample: ActivitySample = {
    at: at as number,
    method,
    cursor: sanitizeCursor(raw.cursor),
    frontmost: sanitizeFrontmost(raw.frontmost),
  };
  if (Array.isArray(raw.windows)) {
    const windows = raw.windows
      .filter(
        (w): w is { id: number; active: boolean } =>
          isObject(w) && isU32(w.id) && typeof w.active === "boolean",
      )
      .slice(0, MAX_WINDOWS)
      .map(({ id, active }) => ({ id, active }));
    if (windows.length > 0) sample.windows = windows;
  }
  return sample;
}

/**
 * The storable form of a confirm's `activity`, or null when absent or
 * unusable. Never throws.
 */
export function sanitizeActivity(raw: unknown): Activity | null {
  if (!isObject(raw) || !Array.isArray(raw.samples)) return null;
  const v = Number.isInteger(raw.v) && (raw.v as number) >= 1 ? (raw.v as number) : 1;
  const samples = raw.samples
    // The newest samples are the ones that describe this capture.
    .slice(-ACTIVITY_MAX_SAMPLES)
    .map(sanitizeSample)
    .filter((s): s is ActivitySample => s !== null);
  if (samples.length === 0) return null;
  return { v, samples };
}
