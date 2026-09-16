// Mask/pixelation regions: normalized [0, 1] coordinates relative to frame size.

export interface MaskRegion {
    id: string;
    start: string;     // ISO timestamp
    end: string;       // ISO timestamp
    startSec?: number; // Video second offset
    endSec?: number;
    x: number;         // 0 to 1
    y: number;         // 0 to 1
    width: number;     // 0 to 1
    height: number;    // 0 to 1
}

export type BlurRegion = MaskRegion;

// Max masks allowed per session.
export const MAX_MASK_REGIONS = 100;

export const MASK_BOUNDS_SLACK_MS = 5 * 60_000;

export type NormalizeMasksResult =
    | { ok: true; masks: MaskRegion[] }
    | { ok: false; error: string };

function clamp(n: number, min: number, max: number): number {
    return Math.min(Math.max(n, min), max);
}

// Validate, clamp coords to [0, 1], and sort by start time.
export function normalizeMasks(
  raw: unknown,
  bounds?: { minMs: number; maxMs: number },
): NormalizeMasksResult {
  if (!Array.isArray(raw)) {
    return { ok: false, error: "masks must be an array" };
  }
  if (raw.length > MAX_MASK_REGIONS) {
    return { ok: false, error: `masks cannot exceed ${MAX_MASK_REGIONS} regions` };
  }

  const minMs = bounds ? bounds.minMs - MASK_BOUNDS_SLACK_MS : -Infinity;
  const maxMs = bounds ? bounds.maxMs + MASK_BOUNDS_SLACK_MS : Infinity;

  const validated: MaskRegion[] = [];

  for (let i = 0; i < raw.length; i++) {
    const entry = raw[i];
    if (typeof entry !== "object" || entry === null) {
      return { ok: false, error: `mask[${i}] must be an object` };
    }

    const rec = entry as Record<string, unknown>;
    const { id, start, end, startSec, endSec, x, y, width, height } = rec;

    if (typeof start !== "string" || typeof end !== "string") {
      return { ok: false, error: `mask[${i}] start and end must be ISO-8601 strings` };
    }

    const startMs = Date.parse(start);
    const endMs = Date.parse(end);
    if (Number.isNaN(startMs) || Number.isNaN(endMs)) {
      return { ok: false, error: `mask[${i}] start and end must be valid ISO-8601 dates` };
    }
    if (endMs <= startMs) {
      return { ok: false, error: `mask[${i}] end must be after start` };
    }

    if (
      typeof x !== "number" || !Number.isFinite(x) ||
      typeof y !== "number" || !Number.isFinite(y) ||
      typeof width !== "number" || !Number.isFinite(width) ||
      typeof height !== "number" || !Number.isFinite(height)
    ) {
      return { ok: false, error: `mask[${i}] coordinates (x, y, width, height) must be finite numbers` };
    }

    const clampedStart = Math.max(startMs, minMs);
    const clampedEnd = Math.min(endMs, maxMs);
    if (clampedEnd <= clampedStart) continue;

    const right = Math.min(1, x + width);
    const bottom = Math.min(1, y + height);
    const clampedX = Math.max(0, x);
    const clampedY = Math.max(0, y);
    const clampedW = Math.max(0, right - clampedX);
    const clampedH = Math.max(0, bottom - clampedY);

    if (clampedW <= 0.001 || clampedH <= 0.001) continue;

    const hasSec =
      typeof startSec === "number" &&
      Number.isFinite(startSec) &&
      typeof endSec === "number" &&
      Number.isFinite(endSec) &&
      endSec > startSec;

    validated.push({
      id: typeof id === "string" && id.trim().length > 0 ? id : `mask-${i}-${startMs}`,
      start: new Date(clampedStart).toISOString(),
      end: new Date(clampedEnd).toISOString(),
      ...(hasSec
        ? {
            startSec: Math.max(0, Math.round((startSec as number) * 10_000) / 10_000),
            endSec: Math.max(0, Math.round((endSec as number) * 10_000) / 10_000),
          }
        : {}),
      x: Math.round(clampedX * 10_000) / 10_000,
      y: Math.round(clampedY * 10_000) / 10_000,
      width: Math.round(clampedW * 10_000) / 10_000,
      height: Math.round(clampedH * 10_000) / 10_000,
    });
  }

  validated.sort((a, b) => {
    const aStart = Date.parse(a.start);
    const bStart = Date.parse(b.start);
    return aStart !== bStart ? aStart - bStart : Date.parse(a.end) - Date.parse(b.end);
  });

  return { ok: true, masks: validated };
}

// Returns true if a timestamp falls within the mask's time interval.
export function isMaskActiveAt(timeMs: number, mask: MaskRegion): boolean {
    const startMs = Date.parse(mask.start);
    const endMs = Date.parse(mask.end);
    return timeMs >= startMs && timeMs < endMs;
}

// Returns all masks active at the given timestamp.
export function activeMasksAt(timeMs: number, masks: MaskRegion[]): MaskRegion[] {
    return masks.filter((m) => isMaskActiveAt(timeMs, m));
}
