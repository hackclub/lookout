/**
 * The recorder's state as the shared Rust publishes it (`RecorderState`
 * in `lookout-session`), and the one clock derivation every surface uses.
 * Nothing here decides anything about the recording; it reads what the
 * recorder decided.
 */
import { useEffect, useState } from "react";
import { deriveDisplaySeconds } from "../hooks/useSessionTimer.js";

export type RecordingSource =
  | { type: "screen"; sources: Array<{ type: "monitor" | "window" | "pipewire"; id: number }> }
  | { type: "camera"; deviceId: string };

export type RecorderPhase =
  | "loading"
  | "loadError"
  | "recording"
  | "pausing"
  | "paused"
  | "resuming"
  | "stopPausing"
  | "stopPrompting"
  | "stopFinishing"
  | "finished";

export type ControlMode = "recording" | "paused";

/**
 * The recorder's clock, as every surface that ticks re-derives it. Hand
 * this to `deriveRecorderDisplaySeconds` with `Date.now()`; never
 * extrapolate from an already-derived display value, which double-counts
 * the interpolated remainder.
 */
export interface RecorderClock {
  /** Ratcheted server-credited seconds the display is anchored to. */
  baseSeconds: number;
  /** Wall-clock ms when `baseSeconds` last advanced (carry-adjusted). */
  anchorAtUnixMs: number;
  /** Whether the clock is interpolating. False shows the bare base. */
  running: boolean;
  /** Wall-clock ms the clock is frozen at while a pause/stop is in flight. */
  holdAtUnixMs: number | null;
  maxInterpolationSeconds: number;
}

export interface CaptureSourceLoss {
  reason: "revoked" | "failed";
  message: string;
  detail?: string | null;
}

/** Everything the recorder publishes. */
export interface RecorderState {
  token: string;
  source: RecordingSource;
  phase: RecorderPhase;
  sessionStatus: string | null;
  sessionName: string;
  sessionError: string | null;
  actionError: string | null;
  controlMode: ControlMode;
  capturing: boolean;
  clock: RecorderClock;
  displaySeconds: number;
  trackedSeconds: number;
  tooShortToCompile: boolean;
  captureCount: number;
  captureError: string | null;
  sourceLost: CaptureSourceLoss | null;
  stale: boolean;
  lastCaptureAtUnixMs: number | null;
  /** Captured intervals the uploader still owes the server. */
  pendingUploads: number;
}

export interface RecorderNotice {
  title: string;
  body: string;
}

export interface StopOutcome {
  status: string;
  edit: boolean;
}

/** The recorder's `RecorderError`, as it crosses into JavaScript. */
export type RecorderErrorPayload =
  | { kind: "noRecording" }
  | { kind: "invalidPhase"; phase: RecorderPhase }
  | { kind: "sourceLost" }
  | { kind: "api"; status: number | null; message: string }
  | { kind: "other"; message: string };

export function isRecorderError(e: unknown): e is RecorderErrorPayload {
  return (
    typeof e === "object" &&
    e !== null &&
    "kind" in e &&
    typeof (e as { kind: unknown }).kind === "string"
  );
}

export function recorderErrorMessage(e: unknown): string {
  if (isRecorderError(e)) {
    switch (e.kind) {
      case "noRecording":
        return "No recording is open.";
      case "invalidPhase":
        return `That doesn't apply right now (${e.phase}).`;
      case "sourceLost":
        return "The capture source is gone — pick one again.";
      case "api":
      case "other":
        return e.message;
    }
  }
  return e instanceof Error ? e.message : String(e);
}

/**
 * The one derivation every surface uses, with the hold applied: the
 * clock's "now" is wall clock, stopped at the hold instant while a
 * pause/stop is in flight. Mirrors `ClockState::display_seconds` in the
 * shared crate.
 */
export function deriveRecorderDisplaySeconds(clock: RecorderClock, now: number): number {
  const clockNow = clock.holdAtUnixMs != null ? Math.min(now, clock.holdAtUnixMs) : now;
  return deriveDisplaySeconds(clock.baseSeconds, clock.anchorAtUnixMs, clock.running, clockNow);
}

/**
 * The ticking clock for a surface. Re-derives from the anchor every frame
 * while running (and once, when not), re-rendering only when the second
 * changes.
 */
export function useRecorderClock(clock: RecorderClock | null): number {
  const base = clock?.baseSeconds ?? 0;
  const anchor = clock?.anchorAtUnixMs ?? 0;
  const running = clock?.running ?? false;
  const hold = clock?.holdAtUnixMs ?? null;
  const max = clock?.maxInterpolationSeconds ?? 0;

  const derive = (now: number) =>
    deriveRecorderDisplaySeconds(
      { baseSeconds: base, anchorAtUnixMs: anchor, running, holdAtUnixMs: hold, maxInterpolationSeconds: max },
      now,
    );

  const [display, setDisplay] = useState(() => (clock ? derive(Date.now()) : 0));

  useEffect(() => {
    if (!clock) {
      setDisplay(0);
      return;
    }
    let raf = 0;
    let last = -1;
    const tick = () => {
      const next = derive(Date.now());
      if (next !== last) {
        last = next;
        setDisplay(next);
      }
      // A held or stopped clock shows a fixed value; no need to spin.
      if (running && hold == null) raf = requestAnimationFrame(tick);
    };
    tick();
    return () => cancelAnimationFrame(raf);
    // The clock object is re-created on every state event; its fields are
    // what matters.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [base, anchor, running, hold, max, clock == null]);

  return display;
}
