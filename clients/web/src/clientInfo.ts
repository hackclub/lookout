import { buildBrowserClientInfo } from "@lookout/shared";

// Injected by Vite (see vite.config.ts `define`).
declare const __LOOKOUT_VERSION__: string;

/** The host program embedding Lookout Web, e.g. "Fallout" / "Stardance".
 *  Sourced from the `?app=` URL param (host passes it on the recorder link),
 *  falling back to the VITE_LOOKOUT_EMBEDDED_APP build-time env var. */
function embeddedApp(): string | undefined {
  const fromUrl = new URLSearchParams(window.location.search)
    .get("app")
    ?.trim();
  return fromUrl || import.meta.env.VITE_LOOKOUT_EMBEDDED_APP || undefined;
}

/** Telemetry string sent on every upload-url request, e.g.
 *  "Lookout Web (Fallout)/0.2.6 (macOS 14.3; Chrome 120.0)". Computed once. */
export const CLIENT_INFO = buildBrowserClientInfo({
  type: "web",
  version: __LOOKOUT_VERSION__,
  embeddedApp: embeddedApp(),
});
