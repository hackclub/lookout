/**
 * Tests for the web client's useSessionTimer.
 *
 * This implementation does NOT have the ratchet that the React SDK has —
 * server values are accepted as-is (including lower ones, if they ever
 * arrived). The web bug reports were against this exact hook.
 *
 * Invariants:
 *   1. Display caps at server + 60s.
 *   2. Unfreeze on server credit is smooth (new server == frozen value).
 *   3. Snap to server on pause/stop.
 */
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useSessionTimer } from "./useSessionTimer.js";

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

function tickClock(ms: number) {
  act(() => {
    vi.advanceTimersByTime(ms);
  });
}

describe("web useSessionTimer — cap and freeze", () => {
  it("starts at server value", () => {
    const { result } = renderHook(() => useSessionTimer(0, true));
    expect(result.current).toBe(0);
  });

  it("ticks forward at wall-clock rate while active", () => {
    const { result } = renderHook(({ s, a }) => useSessionTimer(s, a), {
      initialProps: { s: 0, a: true },
    });
    tickClock(30_000);
    expect(result.current).toBeGreaterThanOrEqual(29);
    expect(result.current).toBeLessThanOrEqual(31);
  });

  it("caps interpolation at one capture interval (60s)", () => {
    const { result } = renderHook(({ s, a }) => useSessionTimer(s, a), {
      initialProps: { s: 0, a: true },
    });
    tickClock(180_000);
    expect(result.current).toBe(60);
  });

  it("stays frozen for any duration of stall", () => {
    const { result } = renderHook(({ s, a }) => useSessionTimer(s, a), {
      initialProps: { s: 120, a: true },
    });
    tickClock(10 * 60_000); // 10 min stall
    expect(result.current).toBe(180); // 120 + 60 cap
  });
});

describe("web useSessionTimer — unfreeze", () => {
  it("the credit that unfreezes equals the frozen value", () => {
    const { result, rerender } = renderHook(
      ({ s, a }) => useSessionTimer(s, a),
      { initialProps: { s: 60, a: true } },
    );
    tickClock(120_000);
    expect(result.current).toBe(120);
    rerender({ s: 120, a: true });
    expect(result.current).toBe(120);
  });

  it("resumes interpolation from the new server value", () => {
    const { result, rerender } = renderHook(
      ({ s, a }) => useSessionTimer(s, a),
      { initialProps: { s: 60, a: true } },
    );
    tickClock(120_000);
    rerender({ s: 120, a: true });
    tickClock(30_000);
    expect(result.current).toBeGreaterThanOrEqual(149);
    expect(result.current).toBeLessThanOrEqual(151);
  });
});

describe("web useSessionTimer — pause/stop", () => {
  it("snaps display to server when isActive flips false", () => {
    const { result, rerender } = renderHook(
      ({ s, a }) => useSessionTimer(s, a),
      { initialProps: { s: 60, a: true } },
    );
    tickClock(45_000);
    expect(result.current).toBeGreaterThan(100);
    rerender({ s: 60, a: false });
    expect(result.current).toBe(60);
  });

  it("max drop on pause is bounded by one interval", () => {
    const { result, rerender } = renderHook(
      ({ s, a }) => useSessionTimer(s, a),
      { initialProps: { s: 0, a: true } },
    );
    tickClock(10 * 60_000);
    const before = result.current;
    rerender({ s: 0, a: false });
    expect(before - result.current).toBeLessThanOrEqual(60);
  });

  it("display is frozen at server value during pause (no interpolation)", () => {
    const { result } = renderHook(() => useSessionTimer(120, false));
    tickClock(60_000);
    expect(result.current).toBe(120);
  });
});

describe("web useSessionTimer — resume", () => {
  it("does not double-count pause duration after resume", () => {
    const { result, rerender } = renderHook(
      ({ s, a }) => useSessionTimer(s, a),
      { initialProps: { s: 60, a: true } },
    );
    tickClock(30_000);
    rerender({ s: 60, a: false });
    tickClock(300_000); // 5 min pause
    rerender({ s: 60, a: true });
    tickClock(1_000);
    expect(result.current).toBeLessThanOrEqual(62);
  });

  it("ticks correctly after multiple pause/resume cycles", () => {
    const { result, rerender } = renderHook(
      ({ s, a }) => useSessionTimer(s, a),
      { initialProps: { s: 60, a: true } },
    );
    for (let i = 0; i < 3; i++) {
      tickClock(20_000);
      rerender({ s: 60, a: false });
      tickClock(60_000);
      rerender({ s: 60, a: true });
    }
    // Should still be within cap of server value
    expect(result.current).toBeLessThanOrEqual(60 + 60);
    expect(result.current).toBeGreaterThanOrEqual(60);
  });
});

describe("web useSessionTimer — network loss / latency", () => {
  it("captures stall for 5min then a single credit lands: smooth unfreeze", () => {
    const { result, rerender } = renderHook(
      ({ s, a }) => useSessionTimer(s, a),
      { initialProps: { s: 60, a: true } },
    );
    tickClock(300_000);
    expect(result.current).toBe(120);
    rerender({ s: 120, a: true });
    expect(result.current).toBe(120);
  });

  it("delayed first credit: cap holds before any credit lands", () => {
    const { result } = renderHook(() => useSessionTimer(0, true));
    tickClock(180_000);
    expect(result.current).toBe(60);
  });

  it("rapid sequential credits (catch-up after queue flush) follow server", () => {
    const { result, rerender } = renderHook(
      ({ s, a }) => useSessionTimer(s, a),
      { initialProps: { s: 0, a: true } },
    );
    tickClock(200_000);
    rerender({ s: 60, a: true });
    rerender({ s: 120, a: true });
    rerender({ s: 180, a: true });
    expect(result.current).toBe(180);
  });
});
