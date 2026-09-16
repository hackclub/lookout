// Cut lists: the canonical representation of session edits.
//
// An edit is a list of ABSOLUTE wall-clock intervals of the session that
// should not exist in any output — never "video offset + duration". Lookout
// is heartbeat-based: the session's identity is its per-minute capture
// timestamps, and the compiled video, /timings (→ Hackatime heartbeats), and
// trackedSeconds are all derived views of them. Expressing the edit in the
// same domain lets one list drive all three consistently.
//
// THE membership rule (shared by server, worker, and clients — never
// reimplement it): a capture unit is cut iff its capture timestamp
// (coalesce(captured_at, requested_at)) falls in [start, end) of any
// interval. Granularity is therefore whole capture units (minutes), which is
// also heartbeat granularity.

/** One cut interval. ISO-8601 UTC wall-clock times; `end` exclusive. */
export interface CutInterval {
  start: string;
  end: string;
}

/** Max intervals per session. Bounds hostile payloads; a 12h session has at
 *  most 720 units, and real edits are a handful of regions. */
export const MAX_CUT_INTERVALS = 120;

/** Max user-initiated cut-compiles per session. Each is cheap (stream copy)
 *  but enqueues worker jobs — bound the loop. */
export const MAX_USER_RECOMPILES = 5;

/**
 * The edit hold is a LEASE, not a countdown.
 *
 * A session stopped with `{edit: true}` stays unpublished while an editing
 * surface is actually open: that surface renews the lease every
 * EDIT_HEARTBEAT_SECONDS, and the server holds the session for
 * EDIT_LEASE_SECONDS past the last renewal. Stop renewing — close the
 * window, quit the app, lose the machine — and it publishes within about a
 * lease.
 *
 * A fixed deadline was wrong in both directions: it cut off someone
 * carefully trimming a long recording, and it made an abandoned session sit
 * unpublished for half an hour. A lease has neither failure: edit for as
 * long as you like, and walking away is detected in a minute or two.
 *
 * Editing happens ONLY inside this hold — never after `complete`, because
 * `complete` is the signal programs act on (forwarding heartbeats,
 * accepting submissions, firing the redirect hook); the data must be final
 * the first time they see it.
 */
export const EDIT_LEASE_SECONDS = 120;

/** How often an open editing surface renews the lease. Comfortably inside
 *  EDIT_LEASE_SECONDS so one dropped request never ends an edit. */
export const EDIT_HEARTBEAT_SECONDS = 30;

/**
 * Absolute ceiling on a hold, measured from the stop. A safety valve, not
 * the mechanism: an editor left open overnight must not keep a program
 * waiting on a session forever.
 */
export const EDIT_HOLD_MAX_MINUTES = 120;

/** Backstop retention for uncut originals of EDITED sessions (the worker
 *  deletes them immediately after an edited publish; this catches crashed
 *  flows). Uncut sessions keep their single video forever. */
export const EDIT_WINDOW_DAYS = 7;

/** How far outside [startedAt, stoppedAt] a cut interval may reach before
 *  being clamped. Mirrors the capture-time trust envelope. */
export const CUT_BOUNDS_SLACK_MS = 5 * 60_000;

export type NormalizeCutsResult =
  | { ok: true; cuts: CutInterval[] }
  | { ok: false; error: string };

/**
 * Validate and canonicalize a raw cut list: parseable ISO dates, end > start,
 * clamped to the session bounds, sorted by start, overlapping/adjacent
 * intervals merged. The canonical form is what gets persisted, so equality
 * checks and previews are stable regardless of how the client drew regions.
 */
export function normalizeCuts(
  raw: unknown,
  bounds?: { minMs: number; maxMs: number },
): NormalizeCutsResult {
  if (!Array.isArray(raw)) {
    return { ok: false, error: "cuts must be an array" };
  }
  if (raw.length > MAX_CUT_INTERVALS) {
    return { ok: false, error: `cuts cannot exceed ${MAX_CUT_INTERVALS} intervals` };
  }

  const minMs = bounds ? bounds.minMs - CUT_BOUNDS_SLACK_MS : -Infinity;
  const maxMs = bounds ? bounds.maxMs + CUT_BOUNDS_SLACK_MS : Infinity;

  const parsed: Array<{ startMs: number; endMs: number }> = [];
  for (const entry of raw) {
    if (typeof entry !== "object" || entry === null) {
      return { ok: false, error: "each cut must be an object with start and end" };
    }
    const { start, end } = entry as Record<string, unknown>;
    if (typeof start !== "string" || typeof end !== "string") {
      return { ok: false, error: "cut start and end must be ISO-8601 strings" };
    }
    const startMs = Date.parse(start);
    const endMs = Date.parse(end);
    if (Number.isNaN(startMs) || Number.isNaN(endMs)) {
      return { ok: false, error: "cut start and end must be valid ISO-8601 dates" };
    }
    if (endMs <= startMs) {
      return { ok: false, error: "cut end must be after start" };
    }
    // Clamp to the session envelope; drop intervals entirely outside it.
    const clampedStart = Math.max(startMs, minMs);
    const clampedEnd = Math.min(endMs, maxMs);
    if (clampedEnd <= clampedStart) continue;
    parsed.push({ startMs: clampedStart, endMs: clampedEnd });
  }

  parsed.sort((a, b) => (a.startMs !== b.startMs ? a.startMs - b.startMs : a.endMs - b.endMs));

  const merged: Array<{ startMs: number; endMs: number }> = [];
  for (const cur of parsed) {
    const last = merged[merged.length - 1];
    if (last && cur.startMs <= last.endMs) {
      last.endMs = Math.max(last.endMs, cur.endMs);
    } else {
      merged.push({ ...cur });
    }
  }

  return {
    ok: true,
    cuts: merged.map((m) => ({
      start: new Date(m.startMs).toISOString(),
      end: new Date(m.endMs).toISOString(),
    })),
  };
}

/** Membership: is a capture taken at `timeMs` removed by `cuts`?
 *  Interval semantics are [start, end) — end-exclusive. */
export function isCutAt(timeMs: number, cuts: CutInterval[]): boolean {
  for (const c of cuts) {
    const s = Date.parse(c.start);
    const e = Date.parse(c.end);
    if (timeMs >= s && timeMs < e) return true;
  }
  return false;
}

/** A contiguous run of KEPT units, as half-open video-second indices.
 *  Because one unit = exactly one second of compiled output, these double
 *  as ffmpeg inpoint/outpoint pairs on the original video. */
export interface KeptRange {
  /** First kept unit index (inclusive) = video inpoint in seconds. */
  start: number;
  /** One past the last kept unit index = video outpoint in seconds. */
  end: number;
}

/**
 * Map unit capture times (epoch ms, in video order) through the cut list to
 * the contiguous kept ranges of the compiled video. Returns [] when
 * everything is cut.
 */
export function computeKeptRanges(
  unitTimesMs: number[],
  cuts: CutInterval[],
): KeptRange[] {
  const ranges: KeptRange[] = [];
  let open: KeptRange | null = null;
  for (let i = 0; i < unitTimesMs.length; i++) {
    if (isCutAt(unitTimesMs[i], cuts)) {
      if (open) {
        ranges.push(open);
        open = null;
      }
    } else {
      if (open) open.end = i + 1;
      else open = { start: i, end: i + 1 };
    }
  }
  if (open) ranges.push(open);
  return ranges;
}

/** Count units removed by the cut list. */
export function countCutUnits(unitTimesMs: number[], cuts: CutInterval[]): number {
  let n = 0;
  for (const t of unitTimesMs) if (isCutAt(t, cuts)) n++;
  return n;
}

/** One entry of `sessions.video_units`: a capture unit that made it into the
 *  compiled original video, in output order. Index in the array = the second
 *  of the video the unit occupies = its minute of real time. */
export interface VideoUnit {
  /** Capture moment (coalesce(captured_at, requested_at)), ISO-8601. */
  capturedAt: string;
  /** Screenshot row id, for debugging/traceability. */
  screenshotId: string;
  /** Number of individual screenshot frames inside this 1-second segment. */
  frameCount?: number;
}

/** The slice of a confirmed screenshot row that cut/tracked-time math needs. */
export interface CaptureRowForCuts {
  /** coalesce(captured_at, requested_at), epoch ms. */
  timeMs: number;
  /** Credit-mode per-capture credit (0 or 60); null on bucket-mode rows. */
  creditedSeconds: number | null;
  minuteBucket: number;
}

/**
 * Credited seconds removed by `cuts`, per tracking mode — the delta between
 * raw tracked time and what the kept captures are worth. Used identically by
 * the server's PUT /cuts preview and the worker's authoritative cut-compile
 * write, so the preview a user sees is exactly what lands.
 *
 * - credit mode: sum of credited_seconds over CUT rows.
 * - bucket mode: raw − max(0, (distinct kept minute buckets − 1) × 60),
 *   mirroring the legacy bucket formula so an empty cut list yields 0.
 */
export function computeCutSeconds(
  rows: CaptureRowForCuts[],
  trackingMode: "credit" | "bucket",
  rawTrackedSeconds: number,
  cuts: CutInterval[],
): number {
  if (cuts.length === 0) return 0;
  if (trackingMode === "credit") {
    let cut = 0;
    for (const r of rows) {
      if (isCutAt(r.timeMs, cuts)) cut += r.creditedSeconds ?? 0;
    }
    return Math.min(cut, rawTrackedSeconds);
  }
  const keptBuckets = new Set<number>();
  for (const r of rows) {
    if (!isCutAt(r.timeMs, cuts)) keptBuckets.add(r.minuteBucket);
  }
  const keptTracked = Math.max(0, (keptBuckets.size - 1) * 60);
  return Math.max(0, rawTrackedSeconds - keptTracked);
}
