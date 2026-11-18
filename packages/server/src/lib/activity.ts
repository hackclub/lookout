/**
 * Interaction metadata a native client sends with each confirm (`activity`):
 * pointer position as a fraction of its display, the frontmost app and where
 * its window sits on its display, whether the captured windows had the
 * focus, and the attached displays — sampled at every clip frame.
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
const MAX_DISPLAYS = 16;
/** Desktop coordinates past this are not a real layout. */
const MAX_LAYOUT_COORD = 1_000_000;
const MAX_DISPLAY_PIXELS = 100_000;
/** A window's frame as fractions of its display: it may hang off the edges
 *  or span displays, but not by this much. */
const MAX_WINDOW_FRACTION = 16;

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
  frontmost: ActivityFrontmost | null;
  windows?: { id: number; active: boolean }[];
  displays?: ActivityDisplay[];
};

export type ActivityFrontmost = {
  app: string;
  captured: boolean | null;
  window?: ActivityWindowRect;
};

/**
 * The focused window's frame as fractions of the display its centre is on
 * (0 = that display's left / top edge, 1 = its right / bottom edge). Not
 * clamped: off-edge windows go below 0 or past 1.
 */
export type ActivityWindowRect = { display: number; x: number; y: number; width: number; height: number };

/** One attached display, in the platform's desktop layout space. */
export type ActivityDisplay = {
  id: number;
  x: number;
  y: number;
  width: number;
  height: number;
  pixelWidth?: number;
  pixelHeight?: number;
  captured: boolean;
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

const isPositiveUpTo = (v: unknown, max: number): v is number =>
  Number.isInteger(v) && (v as number) >= 1 && (v as number) <= max;

const isFiniteWithin = (v: unknown, min: number, max: number): v is number =>
  typeof v === "number" && Number.isFinite(v) && v >= min && v <= max;

function sanitizeWindowRect(raw: unknown): ActivityWindowRect | undefined {
  if (!isObject(raw)) return undefined;
  const { display, x, y, width, height } = raw;
  if (!isU32(display)) return undefined;
  if (!isFiniteWithin(x, -MAX_WINDOW_FRACTION, MAX_WINDOW_FRACTION)) return undefined;
  if (!isFiniteWithin(y, -MAX_WINDOW_FRACTION, MAX_WINDOW_FRACTION)) return undefined;
  if (!isFiniteWithin(width, 0, MAX_WINDOW_FRACTION) || width === 0) return undefined;
  if (!isFiniteWithin(height, 0, MAX_WINDOW_FRACTION) || height === 0) return undefined;
  return { display, x, y, width, height };
}

function sanitizeFrontmost(raw: unknown): ActivitySample["frontmost"] {
  if (!isObject(raw) || typeof raw.app !== "string") return null;
  const app = raw.app.trim().slice(0, MAX_APP_LENGTH);
  if (!app) return null;
  const captured = typeof raw.captured === "boolean" ? raw.captured : null;
  const frontmost: ActivityFrontmost = { app, captured };
  const window = sanitizeWindowRect(raw.window);
  if (window) frontmost.window = window;
  return frontmost;
}

function sanitizeDisplay(raw: unknown): ActivityDisplay | null {
  if (!isObject(raw)) return null;
  const { id, x, y, width, height, pixelWidth, pixelHeight, captured } = raw;
  if (!isU32(id) || typeof captured !== "boolean") return null;
  if (!isFiniteWithin(x, -MAX_LAYOUT_COORD, MAX_LAYOUT_COORD)) return null;
  if (!isFiniteWithin(y, -MAX_LAYOUT_COORD, MAX_LAYOUT_COORD)) return null;
  if (!isFiniteWithin(width, 0, MAX_LAYOUT_COORD) || width === 0) return null;
  if (!isFiniteWithin(height, 0, MAX_LAYOUT_COORD) || height === 0) return null;
  const display: ActivityDisplay = { id, x, y, width, height, captured };
  // The resolution only as a pair.
  if (isPositiveUpTo(pixelWidth, MAX_DISPLAY_PIXELS) && isPositiveUpTo(pixelHeight, MAX_DISPLAY_PIXELS)) {
    display.pixelWidth = pixelWidth;
    display.pixelHeight = pixelHeight;
  }
  return display;
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
  if (Array.isArray(raw.displays)) {
    const displays = raw.displays
      .map(sanitizeDisplay)
      .filter((d): d is ActivityDisplay => d !== null)
      .slice(0, MAX_DISPLAYS);
    if (displays.length > 0) sample.displays = displays;
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
