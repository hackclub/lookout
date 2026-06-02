import { createContext, useContext, useEffect, useMemo, type ReactNode } from "react";
import { buildBrowserClientInfo } from "@lookout/shared";
import { createLookoutClient, type LookoutClient } from "./api/client.js";
import { resolveConfig } from "./defaults.js";
import type { LookoutConfig, ResolvedConfig } from "./types.js";

// Injected at build time by tsup (see tsup.config.ts `define`). Falls back to
// "0.0.0" if a consumer bundles the source without the define.
declare const __LOOKOUT_VERSION__: string;
const SDK_VERSION =
  typeof __LOOKOUT_VERSION__ === "string" ? __LOOKOUT_VERSION__ : "0.0.0";

interface LookoutContextValue {
  config: ResolvedConfig;
  client: LookoutClient;
}

const LookoutContext = createContext<LookoutContextValue | null>(null);

export function useLookoutContext(): LookoutContextValue {
  const ctx = useContext(LookoutContext);
  if (!ctx) {
    throw new Error(
      "Lookout hooks must be used within a <LookoutProvider>. " +
        "Wrap your component tree with <LookoutProvider token=\"...\">.",
    );
  }
  return ctx;
}

export interface LookoutProviderProps extends LookoutConfig {
  children: ReactNode;
}

export function LookoutProvider({
  children,
  ...config
}: LookoutProviderProps) {
  const resolved = useMemo(() => resolveConfig(config), [config]);

  // Telemetry string, e.g. "Lookout Sdk (Fallout)/0.2.6 (macOS 14.3; Chrome 120.0)".
  const clientInfo = useMemo(
    () =>
      buildBrowserClientInfo({
        type: "sdk",
        version: SDK_VERSION,
        embeddedApp: config.appName,
      }),
    [config.appName],
  );

  const client = useMemo(
    () =>
      createLookoutClient({
        baseUrl: resolved.apiBaseUrl,
        token: resolved.token,
        clientInfo,
      }),
    [resolved.apiBaseUrl, resolved.token, clientInfo],
  );

  const value = useMemo(() => ({ config: resolved, client }), [resolved, client]);

  // Inject CSS @keyframes used by SDK components (pulse, spin).
  // Deduped: only injects once even if multiple providers mount.
  useEffect(() => {
    if (typeof document === "undefined") return;
    if (document.querySelector("style[data-lookout-keyframes]")) return;
    const style = document.createElement("style");
    style.setAttribute("data-lookout-keyframes", "");
    style.textContent = [
      "@keyframes pulse{0%,100%{opacity:1}50%{opacity:.4}}",
      "@keyframes spin{from{transform:rotate(0deg)}to{transform:rotate(360deg)}}",
      "@keyframes shimmer{0%{background-position:200% 0}100%{background-position:-200% 0}}",
    ].join("");
    document.head.appendChild(style);
  }, []);

  return (
    <LookoutContext.Provider value={value}>
      {children}
    </LookoutContext.Provider>
  );
}
