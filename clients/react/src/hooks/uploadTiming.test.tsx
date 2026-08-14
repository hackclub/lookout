import { afterEach, describe, expect, it, vi } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { LookoutProvider } from "../LookoutProvider.js";
import { useUploader } from "./useUploader.js";
import { deriveDisplaySeconds, MAX_INTERPOLATION_S } from "./useSessionTimer.js";

/**
 * Tracked time must not depend on how long an upload takes.
 *
 * The server credits a capture by its `capturedAt` — when the frame or
 * clip was grabbed — measured against a streak anchor with a ±30s window.
 * So the one defect that would silently cost users credited minutes is
 * `capturedAt` drifting to reflect upload or encode time: on a slow uplink
 * every capture would land outside the window, reset the streak, and earn
 * nothing. These tests pin it to the capture moment.
 */

const TOKEN = "a".repeat(64);

function wrapper({ children }: { children: React.ReactNode }) {
  return (
    <LookoutProvider token={TOKEN} apiBaseUrl="https://api.test">
      {children}
    </LookoutProvider>
  );
}

/** Fake transport. The R2 PUT is deliberately slow. */
function mockTransport(uploadDelayMs: number) {
  const uploadUrlCalls: string[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : String(input);
      if (url.includes("/upload-url")) {
        uploadUrlCalls.push(url);
        return new Response(
          JSON.stringify({
            uploadUrl: "https://r2.test/put",
            r2Key: "k",
            screenshotId: "00000000-0000-0000-0000-000000000000",
            minuteBucket: 0,
            nextExpectedAt: new Date(Date.now() + 60_000).toISOString(),
            format: "webm",
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (init?.method === "PUT") {
        await new Promise((r) => setTimeout(r, uploadDelayMs));
        return new Response("", { status: 200 });
      }
      if (url.includes("/screenshots")) {
        return new Response(
          JSON.stringify({
            confirmed: true,
            trackedSeconds: 60,
            nextExpectedAt: new Date(Date.now() + 60_000).toISOString(),
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      return new Response("{}", { status: 200 });
    }),
  );
  return { uploadUrlCalls };
}

afterEach(() => vi.unstubAllGlobals());

describe("upload duration and credited time", () => {
  it("stamps capturedAt at capture time even when the upload is slow", async () => {
    const { uploadUrlCalls } = mockTransport(400);
    const { result } = renderHook(() => useUploader(), { wrapper });

    // A clip finalized 45s ago: the gap a long encode or a queued upload
    // introduces between grabbing footage and shipping it.
    const capturedAtMs = Date.now() - 45_000;

    await act(async () => {
      await result.current.captureUploadConfirm({
        blob: new Blob(["clip"], { type: "video/webm" }),
        width: 1920,
        height: 1080,
        capturedAtMs,
        format: "webm",
      });
    });

    expect(uploadUrlCalls).toHaveLength(1);
    const sent = new URL(uploadUrlCalls[0]).searchParams.get("capturedAt");
    expect(sent).toBe(new Date(capturedAtMs).toISOString());
    // Not "roughly now" — exactly the capture moment. A drift of even a
    // few seconds per capture accumulates into a lost streak.
    expect(Date.parse(sent!)).toBe(capturedAtMs);
  });

  it("reports the server's tracked seconds, never a locally derived count", async () => {
    mockTransport(10);
    const { result } = renderHook(() => useUploader(), { wrapper });

    await act(async () => {
      await result.current.captureUploadConfirm({
        blob: new Blob(["x"], { type: "image/jpeg" }),
        width: 100,
        height: 100,
        capturedAtMs: Date.now(),
      });
    });

    // The confirm said 60; a client that counted successful uploads would
    // say something else the moment a capture landed out of window.
    expect(result.current.trackedSeconds).toBe(60);
  });
});

describe("clock-skew tolerance", () => {
  /** Like mockTransport, but the server's clock runs `skewMs` ahead of the
   *  device's — the situation on any machine whose system clock is wrong. */
  function mockSkewedTransport(skewMs: number) {
    const uploadUrlCalls: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === "string" ? input : String(input);
        const serverNow = () => new Date(Date.now() + skewMs).toISOString();
        if (url.includes("/upload-url")) {
          uploadUrlCalls.push(url);
          return new Response(
            JSON.stringify({
              uploadUrl: "https://r2.test/put",
              r2Key: "k",
              screenshotId: "00000000-0000-0000-0000-000000000000",
              minuteBucket: 0,
              nextExpectedAt: new Date(Date.now() + skewMs + 60_000).toISOString(),
              serverTime: serverNow(),
              format: "webm",
            }),
            { status: 200, headers: { "Content-Type": "application/json" } },
          );
        }
        if (init?.method === "PUT") return new Response("", { status: 200 });
        if (url.includes("/screenshots")) {
          return new Response(
            JSON.stringify({
              confirmed: true,
              trackedSeconds: 60,
              nextExpectedAt: new Date(Date.now() + skewMs + 60_000).toISOString(),
              serverTime: serverNow(),
            }),
            { status: 200, headers: { "Content-Type": "application/json" } },
          );
        }
        return new Response("{}", { status: 200 });
      }),
    );
    return { uploadUrlCalls };
  }

  const upload = (result: { current: ReturnType<typeof useUploader> }) =>
    result.current.captureUploadConfirm({
      blob: new Blob(["clip"], { type: "video/webm" }),
      width: 1920,
      height: 1080,
      capturedAtMs: Date.now(),
      format: "webm",
    });

  it("serverNowMs converges on the server's clock after one upload", async () => {
    // The scheduler subtracts serverNowMs() from the server's nextExpectedAt.
    // If it read the raw local clock instead, a 6-minute skew would pin every
    // delay at the 2x-interval clamp — half the captures, half the video, and
    // zero credited seconds. That was a real production failure, not a
    // hypothetical: see the sessions with startedAt clamped to createdAt-5min.
    const skewMs = 6 * 60_000;
    mockSkewedTransport(skewMs);
    const { result } = renderHook(() => useUploader(), { wrapper });

    // Before any upload the estimate is empty: serverNowMs is the local clock.
    expect(Math.abs(result.current.serverNowMs() - Date.now())).toBeLessThan(1_000);

    await act(async () => {
      await upload(result);
    });

    // One round trip later the estimate has the skew, well inside the ±30s
    // credit window.
    expect(Math.abs(result.current.serverNowMs() - (Date.now() + skewMs))).toBeLessThan(
      2_000,
    );
  });

  it("stamps later captures in server time once the offset is known", async () => {
    const skewMs = 6 * 60_000;
    const { uploadUrlCalls } = mockSkewedTransport(skewMs);
    const { result } = renderHook(() => useUploader(), { wrapper });

    await act(async () => {
      await upload(result); // teaches the offset
      await upload(result); // stamped corrected
    });

    expect(uploadUrlCalls).toHaveLength(2);
    const second = new URL(uploadUrlCalls[1]).searchParams.get("capturedAt");
    // The second capture's stamp lands within jitter of the SERVER's clock,
    // not the device's — inside the envelope, inside the credit window.
    expect(Math.abs(Date.parse(second!) - (Date.now() + skewMs))).toBeLessThan(2_000);
  });
});

describe("the display timer while an upload is in flight", () => {
  const base = 120;

  it("ticks smoothly through a normal round trip", () => {
    // Credits arrive ~60s apart, so the cap is reached just as the next
    // one lands: no visible stall in the steady state, however long the
    // individual upload took.
    expect(deriveDisplaySeconds(base, 0, true, 10_000)).toBe(base + 10);
    expect(deriveDisplaySeconds(base, 0, true, 45_000)).toBe(base + 45);
    expect(deriveDisplaySeconds(base, 0, true, 59_000)).toBe(base + 59);
  });

  it("holds instead of inflating once a credit is overdue", () => {
    // Past one interval the credit is genuinely late — uploads stalling,
    // or captures falling outside the ±30s window and earning nothing.
    // Holding is honest: those seconds may never be credited.
    expect(deriveDisplaySeconds(base, 0, true, 90_000)).toBe(base + MAX_INTERPOLATION_S);
    expect(deriveDisplaySeconds(base, 0, true, 600_000)).toBe(base + MAX_INTERPOLATION_S);
  });

  it("resumes from the held value rather than jumping when it lands", () => {
    // Why the cap is exactly one interval: the held number and the
    // incoming credit are the same, so a late upload costs smoothness for
    // a moment but never shows the user time going backwards.
    const held = deriveDisplaySeconds(base, 0, true, 90_000);
    const afterCredit = deriveDisplaySeconds(base + MAX_INTERPOLATION_S, 1_000, true, 1_000);
    expect(afterCredit).toBe(held);
  });
});
