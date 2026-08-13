import { describe, it, expect } from "vitest";
import { detectCapabilities, isMobile, type Environment } from "./capabilities.js";

const MAC =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";
const IPAD_MODERN = MAC; // iPadOS 13+ ships the desktop Safari UA verbatim
const IPHONE =
  "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";
const ANDROID =
  "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36";
const WINDOWS =
  "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

function env(overrides: Partial<Environment> = {}): Environment {
  return {
    userAgent: WINDOWS,
    maxTouchPoints: 0,
    hasGetDisplayMedia: true,
    hasGetUserMedia: true,
    ...overrides,
  };
}

describe("isMobile", () => {
  it("recognises phones", () => {
    expect(isMobile(IPHONE, 5)).toBe(true);
    expect(isMobile(ANDROID, 5)).toBe(true);
  });

  it("recognises an iPad wearing the desktop Safari UA", () => {
    // The whole reason maxTouchPoints is threaded through: on UA alone
    // this is indistinguishable from a MacBook.
    expect(isMobile(IPAD_MODERN, 5)).toBe(true);
  });

  it("does not mistake a real desktop for a tablet", () => {
    expect(isMobile(MAC, 0)).toBe(false);
    expect(isMobile(WINDOWS, 0)).toBe(false);
  });
});

describe("detectCapabilities", () => {
  it("offers everything on a capable desktop browser", () => {
    expect(detectCapabilities(env())).toEqual({
      screen: true,
      camera: true,
      desktopApp: true,
    });
  });

  it("withholds screen capture when the browser lacks getDisplayMedia", () => {
    expect(detectCapabilities(env({ hasGetDisplayMedia: false })).screen).toBe(
      false,
    );
  });

  it("withholds screen capture on mobile even when the API is present", () => {
    // iOS Safari exposes getDisplayMedia and then rejects the call. Offering
    // it would spend the user's tap on a guaranteed failure.
    const caps = detectCapabilities(
      env({ userAgent: IPHONE, maxTouchPoints: 5, hasGetDisplayMedia: true }),
    );
    expect(caps.screen).toBe(false);
    expect(caps.camera).toBe(true);
  });

  it("hides the desktop app on mobile, where there is no build to install", () => {
    expect(
      detectCapabilities(env({ userAgent: ANDROID, maxTouchPoints: 5 }))
        .desktopApp,
    ).toBe(false);
  });

  it("withholds camera capture when getUserMedia is missing", () => {
    // Insecure origins drop mediaDevices entirely.
    expect(
      detectCapabilities(
        env({ hasGetUserMedia: false, hasGetDisplayMedia: false }),
      ),
    ).toEqual({ screen: false, camera: false, desktopApp: true });
  });
});
