/**
 * What this browser can actually offer, so the chooser only shows options
 * that will work.
 *
 * Kept as a pure function over a plain description of the environment
 * rather than reaching for `navigator` directly — the interesting cases
 * (iPad lying about being a Mac, Safari without getDisplayMedia) are ones
 * you want to be able to write down as a test.
 */

export interface Environment {
  userAgent: string;
  /** `navigator.maxTouchPoints`. iPadOS claims to be a Mac; this is what
   *  gives it away. */
  maxTouchPoints: number;
  hasGetDisplayMedia: boolean;
  hasGetUserMedia: boolean;
}

export interface Capabilities {
  /** Screen capture via getDisplayMedia. */
  screen: boolean;
  /** Camera capture via getUserMedia. */
  camera: boolean;
  /** Whether to offer the desktop app at all — there is no mobile build,
   *  so on a phone the handoff is a dead end, not a choice. */
  desktopApp: boolean;
}

/** Phones and tablets, including the iPad that reports itself as a Mac. */
export function isMobile(userAgent: string, maxTouchPoints: number): boolean {
  if (/Android|iPhone|iPod/i.test(userAgent)) return true;
  if (/iPad/i.test(userAgent)) return true;
  // iPadOS 13+ ships the desktop Safari UA. A Mac with a touchscreen does
  // not exist, so touch points on a "Mac" means iPad.
  if (/Macintosh/i.test(userAgent) && maxTouchPoints > 1) return true;
  return false;
}

export function detectCapabilities(env: Environment): Capabilities {
  const mobile = isMobile(env.userAgent, env.maxTouchPoints);
  return {
    // Mobile browsers expose getDisplayMedia in name only — iOS Safari
    // rejects the call. Don't offer a permission prompt that can't succeed.
    screen: env.hasGetDisplayMedia && !mobile,
    camera: env.hasGetUserMedia,
    desktopApp: !mobile,
  };
}

/** Read the live environment. Safe to call during render on the server or
 *  in a test that hasn't set up a DOM (everything reads as unavailable). */
export function readEnvironment(): Environment {
  if (typeof navigator === "undefined") {
    return {
      userAgent: "",
      maxTouchPoints: 0,
      hasGetDisplayMedia: false,
      hasGetUserMedia: false,
    };
  }
  const media = navigator.mediaDevices;
  return {
    userAgent: navigator.userAgent ?? "",
    maxTouchPoints: navigator.maxTouchPoints ?? 0,
    hasGetDisplayMedia: typeof media?.getDisplayMedia === "function",
    hasGetUserMedia: typeof media?.getUserMedia === "function",
  };
}
