/**
 * The desktop's view of the recording session, which lives in the Rust
 * core (`lookout-core`'s `recorder` module). The webview opens a recording,
 * issues actions, and draws the `RecorderState` it is handed on every
 * change — it holds no recording logic of its own. Not the order of
 * operations behind a pause, not which tracked-seconds number to believe,
 * not when the clock stops: all of that is in the core, where a GTK or
 * headless shell gets it too.
 *
 * What is still here is purely presentational: deriving the ticking clock
 * from the core's anchor (`deriveRecorderDisplaySeconds`), subscribing to
 * state, and typed wrappers over the commands.
 */
import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { deriveDisplaySeconds } from "@lookout/react";
import { invoke } from "./logger.js";

/** A source as the picker hands it over. Cameras are keyed by the
 *  browser's `deviceId`; everything else by the core's numeric id. */
export interface CaptureSource {
  type: "monitor" | "window" | "camera" | "pipewire";
  id: number | string;
}

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
 * The core's clock, as every surface that ticks re-derives it. Hand this
 * to `deriveRecorderDisplaySeconds` with `Date.now()`; never extrapolate
 * from an already-derived display value, which double-counts the
 * interpolated remainder (how the tray popup once drifted ahead of the
 * main window).
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

/**
 * Why a screen source stopped producing frames. `revoked`: the portal told
 * us the cast ended. `failed`: the stream went quiet and the capture loop's
 * watchdog gave up on it. Neither is an error with a retry: the only way
 * back is a trip through the picker.
 */
export interface CaptureSourceLoss {
  reason: "revoked" | "failed";
  /** What to put in front of the user. */
  message: string;
  /** The raw capture error behind a `failed` loss, for the details dialog. */
  detail?: string | null;
}

/** Everything the recorder UI renders. See `RecorderState` in the core. */
export interface RecorderState {
  token: string;
  source: RecordingSource;
  phase: RecorderPhase;
  sessionStatus: string | null;
  sessionName: string;
  sessionError: string | null;
  actionError: string | null;
  controlMode: ControlMode;
  /** A loop is running, flushing, or about to start. */
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
}

export interface RecorderNotice {
  title: string;
  body: string;
}

export interface StopOutcome {
  status: string;
  /** The stop asked for an edit hold, so open the editor. */
  edit: boolean;
}

/** The core's `RecorderError`, as it crosses the IPC. */
export type RecorderErrorPayload =
  | { kind: "noRecording" }
  | { kind: "invalidPhase"; phase: RecorderPhase }
  | { kind: "sourceLost" }
  | { kind: "api"; status: number | null; message: string }
  | { kind: "other"; message: string };

export function isRecorderError(e: unknown): e is RecorderErrorPayload {
  return typeof e === "object" && e !== null && "kind" in e && typeof (e as { kind: unknown }).kind === "string";
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
 * core and `deriveDisplaySeconds` in @lookout/react.
 */
export function deriveRecorderDisplaySeconds(clock: RecorderClock, now: number): number {
  const clockNow = clock.holdAtUnixMs != null ? Math.min(now, clock.holdAtUnixMs) : now;
  return deriveDisplaySeconds(clock.baseSeconds, clock.anchorAtUnixMs, clock.running, clockNow);
}

/** Phases during which the naming prompt is up. */
export function isPrompting(phase: RecorderPhase): boolean {
  return phase === "stopPrompting" || phase === "stopFinishing";
}

/**
 * Subscribe to the recorder's state. `ready` turns true once the listener
 * is attached and the current state (if any) has been read — open a
 * recording only after that, or its first states could be missed.
 */
export function useRecorderState(): { state: RecorderState | null; ready: boolean } {
  const [state, setState] = useState<RecorderState | null>(null);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    (async () => {
      unlisten = await listen<RecorderState | null>("recorder-state", (event) => {
        if (!cancelled) setState(event.payload);
      });
      const current = await invoke<RecorderState | null>("recorder_state");
      if (cancelled) return;
      // An event that arrived meanwhile is newer than the fetched copy.
      setState((prev) => prev ?? current);
      setReady(true);
    })().catch((e) => console.error("[recorder] state subscription failed:", e));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  return { state, ready };
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
    if (!clock) return;
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

/** Typed wrappers over the recorder commands. Errors are the core's
 *  `RecorderError` payloads — see `recorderErrorMessage`. */
export const recorder = {
  open: (token: string, apiBaseUrl: string, sources: CaptureSource[]) =>
    invoke<RecorderState>("recorder_open", { token, apiBaseUrl, sources }),
  close: () => invoke<void>("recorder_close"),
  pause: () => invoke<RecorderState>("recorder_pause"),
  resume: () => invoke<RecorderState>("recorder_resume"),
  beginStop: () => invoke<RecorderState>("recorder_begin_stop"),
  cancelStop: () => invoke<RecorderState>("recorder_cancel_stop"),
  finishStop: (name: string | null, edit: boolean) =>
    invoke<StopOutcome>("recorder_finish_stop", { name, edit }),
  state: () => invoke<RecorderState | null>("recorder_state"),
};

/** Rename (optional) and stop a session with no recording open — the
 *  source picker's Stop button. Same routine the recorder uses. */
export function finishSession(
  token: string,
  apiBaseUrl: string,
  name: string | null,
  edit: boolean,
): Promise<{ status: string }> {
  return invoke<{ status: string }>("session_finish", { token, apiBaseUrl, name, edit });
}
