/**
 * Tests for useSilentAudioKeepAlive — the silent-oscillator hook that
 * keeps the page "audible" to bypass browser timer throttling.
 *
 * happy-dom does not implement AudioContext. We inject a minimal class
 * stub onto the global window and assert the hook's interactions.
 */
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { renderHook } from "@testing-library/react";
import { useSilentAudioKeepAlive } from "./useSilentAudioKeepAlive.js";

class FakeOscillator {
  start = vi.fn();
  stop = vi.fn();
  // connect chain returns the next node in the chain (gain or destination)
  connect = vi.fn().mockReturnThis();
}
class FakeGain {
  gain = { value: 1 };
  connect = vi.fn().mockReturnThis();
}
class FakeAudioContext {
  static instances: FakeAudioContext[] = [];
  destination = {};
  closed = false;
  oscillators: FakeOscillator[] = [];
  gains: FakeGain[] = [];

  constructor() {
    FakeAudioContext.instances.push(this);
  }

  createOscillator() {
    const o = new FakeOscillator();
    this.oscillators.push(o);
    return o;
  }
  createGain() {
    const g = new FakeGain();
    this.gains.push(g);
    return g;
  }
  async close() {
    this.closed = true;
  }
}

function installAudioContext() {
  FakeAudioContext.instances = [];
  (window as unknown as { AudioContext: typeof FakeAudioContext }).AudioContext =
    FakeAudioContext;
}

beforeEach(() => {
  installAudioContext();
});

afterEach(() => {
  delete (window as unknown as { AudioContext?: unknown }).AudioContext;
  delete (window as unknown as { webkitAudioContext?: unknown })
    .webkitAudioContext;
});

describe("useSilentAudioKeepAlive — enabled", () => {
  it("creates an AudioContext and starts a silent oscillator when enabled", () => {
    renderHook(() => useSilentAudioKeepAlive(true));
    expect(FakeAudioContext.instances).toHaveLength(1);
    const ctx = FakeAudioContext.instances[0];
    expect(ctx.oscillators).toHaveLength(1);
    expect(ctx.gains).toHaveLength(1);
    expect(ctx.gains[0].gain.value).toBe(0); // genuinely silent
    expect(ctx.oscillators[0].start).toHaveBeenCalledTimes(1);
  });

  it("does NOT create an AudioContext when disabled", () => {
    renderHook(() => useSilentAudioKeepAlive(false));
    expect(FakeAudioContext.instances).toHaveLength(0);
  });
});

describe("useSilentAudioKeepAlive — teardown", () => {
  it("stops the oscillator and closes the context on disable", () => {
    const { rerender } = renderHook(
      ({ on }) => useSilentAudioKeepAlive(on),
      { initialProps: { on: true } },
    );
    const ctx = FakeAudioContext.instances[0];
    rerender({ on: false });
    expect(ctx.oscillators[0].stop).toHaveBeenCalledTimes(1);
    expect(ctx.closed).toBe(true);
  });

  it("closes the context on unmount", () => {
    const { unmount } = renderHook(() => useSilentAudioKeepAlive(true));
    const ctx = FakeAudioContext.instances[0];
    unmount();
    expect(ctx.oscillators[0].stop).toHaveBeenCalledTimes(1);
    expect(ctx.closed).toBe(true);
  });

  it("does not leak contexts across enable/disable cycles", () => {
    const { rerender } = renderHook(
      ({ on }) => useSilentAudioKeepAlive(on),
      { initialProps: { on: true } },
    );
    rerender({ on: false });
    rerender({ on: true });
    rerender({ on: false });
    rerender({ on: true });

    expect(FakeAudioContext.instances).toHaveLength(3);
    expect(FakeAudioContext.instances[0].closed).toBe(true);
    expect(FakeAudioContext.instances[1].closed).toBe(true);
    expect(FakeAudioContext.instances[2].closed).toBe(false);
  });
});

describe("useSilentAudioKeepAlive — graceful degradation", () => {
  it("is a no-op when AudioContext is unavailable", () => {
    delete (window as unknown as { AudioContext?: unknown }).AudioContext;
    delete (window as unknown as { webkitAudioContext?: unknown })
      .webkitAudioContext;
    expect(() => renderHook(() => useSilentAudioKeepAlive(true))).not.toThrow();
  });

  it("falls back to webkitAudioContext when AudioContext is missing", () => {
    delete (window as unknown as { AudioContext?: unknown }).AudioContext;
    (
      window as unknown as { webkitAudioContext: typeof FakeAudioContext }
    ).webkitAudioContext = FakeAudioContext;

    renderHook(() => useSilentAudioKeepAlive(true));
    expect(FakeAudioContext.instances).toHaveLength(1);
    expect(FakeAudioContext.instances[0].oscillators[0].start).toHaveBeenCalled();
  });

  it("swallows AudioContext constructor errors without throwing", () => {
    class ThrowingAudioContext {
      constructor() {
        throw new Error("audio context creation failed");
      }
    }
    (
      window as unknown as { AudioContext: typeof ThrowingAudioContext }
    ).AudioContext = ThrowingAudioContext;
    expect(() => renderHook(() => useSilentAudioKeepAlive(true))).not.toThrow();
  });
});
