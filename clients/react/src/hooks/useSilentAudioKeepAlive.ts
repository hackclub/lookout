import { useEffect, useRef } from "react";

/**
 * Plays a silent oscillator while `enabled` is true. The page counts
 * as "audible" to the browser, which exempts it from Chrome's intensive
 * timer throttling and Energy Saver freeze. Long-standing workaround used
 * by Google Meet, Zoom, and similar.
 *
 * We already get partial throttling exemption from the live
 * `getDisplayMedia` MediaStreamTrack, but Low Power Mode + backgrounded
 * tab can still stall the capture loop. The silent oscillator is the
 * next lever before moving the timer into a SharedWorker.
 *
 * Notes:
 *   - AudioContext construction requires a prior user gesture in
 *     Chrome/Safari. We're always invoked after the user clicked
 *     "Share screen", so the gesture is satisfied.
 *   - gain.value = 0 is genuinely silent (no audible output) — but the
 *     audio *graph* runs, which is what the browser checks.
 *   - We clean up on disable so we don't leak an AudioContext between
 *     pause/resume cycles.
 *   - No-op on platforms without AudioContext (older Safari iOS,
 *     non-browser environments like Tauri's Rust capture loop where
 *     this hook is mounted but irrelevant — Rust does its own thing).
 */
export function useSilentAudioKeepAlive(enabled: boolean) {
  const contextRef = useRef<AudioContext | null>(null);
  const oscillatorRef = useRef<OscillatorNode | null>(null);

  useEffect(() => {
    if (!enabled) return;

    const Ctor: typeof AudioContext | undefined =
      typeof window === "undefined"
        ? undefined
        : window.AudioContext ??
          (window as unknown as { webkitAudioContext?: typeof AudioContext })
            .webkitAudioContext;
    if (!Ctor) return;

    let ctx: AudioContext;
    try {
      ctx = new Ctor();
    } catch {
      return;
    }
    contextRef.current = ctx;

    try {
      const oscillator = ctx.createOscillator();
      const gain = ctx.createGain();
      gain.gain.value = 0;
      oscillator.connect(gain).connect(ctx.destination);
      oscillator.start();
      oscillatorRef.current = oscillator;
    } catch {
      // ignore
    }

    return () => {
      try {
        oscillatorRef.current?.stop();
      } catch {
        // already stopped
      }
      oscillatorRef.current = null;
      contextRef.current?.close().catch(() => {});
      contextRef.current = null;
    };
  }, [enabled]);
}
