import type { ClientInfo } from "./types.js";

/**
 * Pieces of client telemetry. `type` and `version` are required; the rest are
 * optional and surface-specific (browser fields are web/SDK only).
 */
export interface ClientInfoParts {
  /** "desktop" | "web" | "sdk" (extensible). */
  type: string;
  /** Lookout client version, e.g. "0.2.6". */
  version: string;
  /** Web/SDK only: host program embedding Lookout, e.g. "Fallout". */
  embeddedApp?: string;
  osType?: string;
  osVersion?: string;
  browserType?: string;
  browserVersion?: string;
}

const titleCase = (s: string) => (s ? s[0].toUpperCase() + s.slice(1) : s);

/**
 * Format the parts into a User-Agent-like `clientInfo` string, e.g.
 *   "Lookout Desktop/0.2.6 (macOS 14.3)"
 *   "Lookout Web (Fallout)/0.2.6 (macOS 14.3; Chrome 120.0)"
 * The server stores this opaquely; the format is a convention, not enforced.
 */
export function formatClientInfo(parts: ClientInfoParts): ClientInfo {
  const app = parts.embeddedApp ? ` (${parts.embeddedApp})` : "";
  const env: string[] = [];
  if (parts.osType) {
    env.push(parts.osVersion ? `${parts.osType} ${parts.osVersion}` : parts.osType);
  }
  if (parts.browserType) {
    env.push(
      parts.browserVersion
        ? `${parts.browserType} ${parts.browserVersion}`
        : parts.browserType,
    );
  }
  const envStr = env.length ? ` (${env.join("; ")})` : "";
  return `Lookout ${titleCase(parts.type)}${app}/${parts.version}${envStr}`;
}

/** Best-effort browser family + version from a User-Agent string. Heuristic;
 *  order matters (Edge/Opera masquerade as Chrome). Returns {} if unknown. */
export function detectBrowser(
  ua: string,
): { browserType?: string; browserVersion?: string } {
  const match = (re: RegExp) => ua.match(re)?.[1];
  // Order: most-specific first.
  if (/Edg\//.test(ua)) return { browserType: "Edge", browserVersion: match(/Edg\/([\d.]+)/) };
  if (/OPR\//.test(ua)) return { browserType: "Opera", browserVersion: match(/OPR\/([\d.]+)/) };
  if (/Firefox\//.test(ua)) return { browserType: "Firefox", browserVersion: match(/Firefox\/([\d.]+)/) };
  if (/Chrome\//.test(ua)) return { browserType: "Chrome", browserVersion: match(/Chrome\/([\d.]+)/) };
  if (/Version\/[\d.]+.*Safari/.test(ua)) return { browserType: "Safari", browserVersion: match(/Version\/([\d.]+)/) };
  return {};
}

/** Best-effort OS family + version from a User-Agent string. Returns {} if
 *  unknown. Version is coarse (and unavailable for recent macOS, which freezes
 *  the UA at 10_15_7). */
export function detectOS(ua: string): { osType?: string; osVersion?: string } {
  let m = ua.match(/Windows NT ([\d.]+)/);
  if (m) return { osType: "Windows", osVersion: m[1] };
  m = ua.match(/Mac OS X ([\d_]+)/);
  if (m) return { osType: "macOS", osVersion: m[1].replace(/_/g, ".") };
  m = ua.match(/Android ([\d.]+)/);
  if (m) return { osType: "Android", osVersion: m[1] };
  m = ua.match(/(?:iPhone|iPad).*OS ([\d_]+)/);
  if (m) return { osType: "iOS", osVersion: m[1].replace(/_/g, ".") };
  if (/Linux/.test(ua)) return { osType: "Linux" };
  return {};
}

/**
 * Build a `clientInfo` string for a browser-based client (web app / SDK),
 * auto-detecting browser and OS from `navigator.userAgent`. Pass the Lookout
 * `type`, `version`, and optional `embeddedApp` (the host program).
 */
export function buildBrowserClientInfo(opts: {
  type: string;
  version: string;
  embeddedApp?: string;
}): ClientInfo {
  const ua =
    typeof navigator !== "undefined" && navigator.userAgent
      ? navigator.userAgent
      : "";
  return formatClientInfo({
    type: opts.type,
    version: opts.version,
    embeddedApp: opts.embeddedApp,
    ...detectOS(ua),
    ...detectBrowser(ua),
  });
}
