import * as Sentry from "@sentry/react";
import { fetch as tauriFetch } from "@tauri-apps/plugin-http";
import { emit } from "@tauri-apps/api/event";
import { createRoot } from "react-dom/client";
import React from "react";
import { getReport } from "./logger.js"; // side-effect: captures console, renders debug panel
import { App } from "./App.js";

declare const __APP_VERSION__: string;

// Boot timing — measured from the webview's navigation start. Shows up in
// the console and the backtick debug log as [boot] lines.
console.log(`[boot] main.tsx eval at ${Math.round(performance.now())}ms`);

Sentry.init({
  dsn: import.meta.env.VITE_SENTRY_DSN,
  release: `lookout-desktop@${__APP_VERSION__}`,
  environment: "desktop",
  tracesSampleRate: 0.2,
  integrations: [
    Sentry.consoleLoggingIntegration({ levels: ["warn", "error"] }),
  ],
});

// Add global debug helper for deep links
(window as any).__simulateDeepLink = (url: string) => {
  emit('lookout-deep-link', [url]).catch(err => console.error("Simulate deep link failed:", err));
};

// Wrap fetch so only cross-origin requests go through Tauri's HTTP plugin.
// Keeping native fetch for same-origin/local requests avoids breaking React internals.
// Lookout server API calls do NOT come through here any more: they go through
// the Rust core via Tauri commands (see api/tauriClient.ts). What's left is
// media — R2 video/thumbnail URLs and program icons.
const originalFetch = window.fetch;
window.fetch = function (input, init) {
  const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : (input as Request).url;
  const method = init?.method || "GET";
  // Preview frames fetch once per second per source — logging them floods
  // the 200-entry debug buffer and buries real diagnostics. Failures are
  // still logged below.
  const isPreviewFrame = url.includes("lookout-preview");
  if (!isPreviewFrame) console.log(`[net] ${method} ${url}`);

  // On Windows, Tauri v2 uses fetch('http://ipc.localhost/...') for IPC and
  // 'http://asset.localhost/...' for assets. We must NOT intercept these or
  // tauriFetch recurses through invoke → fetch → tauriFetch → OOM.
  const isExternal = (url.startsWith("http://") || url.startsWith("https://"))
    && !url.includes(".localhost");
  const doFetch = isExternal
    ? tauriFetch(input as any, init) as any
    : originalFetch.call(window, input, init);

  return (doFetch as Promise<Response>).then(
    (res) => {
      if (!isPreviewFrame) console.log(`[net] ${method} ${url} → ${res.status}`);
      return res;
    },
    (err: Error) => {
      console.error(`[net] ${method} ${url} → FAILED: ${err.message}`);
      throw err;
    },
  );
};

// Clear any bad stored tokens from previous testing
try {
  const raw = localStorage.getItem("lookout-tokens");
  if (raw) JSON.parse(raw);
} catch {
  localStorage.removeItem("lookout-tokens");
}

class ErrorBoundary extends React.Component<
  { children: React.ReactNode },
  { error: string | null; copied: boolean }
> {
  state = { error: null as string | null, copied: false };
  static getDerivedStateFromError(err: Error) {
    return { error: err.stack || err.message, copied: false };
  }
  componentDidCatch(error: Error, errorInfo: React.ErrorInfo) {
    console.error("[react] render crash:", error.message, error.stack);
    Sentry.captureException(error, { contexts: { react: { componentStack: errorInfo.componentStack ?? undefined } } });
  }
  handleCopy = () => {
    const report = `REACT CRASH:\n${this.state.error}\n\n--- LOG ---\n${getReport()}`;
    navigator.clipboard.writeText(report).then(() => {
      this.setState({ copied: true });
      setTimeout(() => this.setState({ copied: false }), 2000);
    });
  };
  render() {
    if (this.state.error) {
      return (
        <div style={{ padding: 24, fontFamily: "monospace", background: "transparent", color: "var(--color-text-primary, #e5e5e5)", minHeight: "100%" }}>
          <div style={{ maxWidth: 500, margin: "0 auto", border: "1px solid #f44", borderRadius: 8, padding: 20 }}>
            <h2 style={{ color: "#f44", margin: "0 0 12px 0", fontSize: 18 }}>Something went wrong</h2>
            <pre style={{ color: "#faa", fontSize: 12, whiteSpace: "pre-wrap", wordBreak: "break-all", maxHeight: 300, overflowY: "auto", marginBottom: 16 }}>
              {this.state.error}
            </pre>
            <div style={{ display: "flex", gap: 8 }}>
              <button
                onClick={this.handleCopy}
                style={{ padding: "8px 16px", background: "#333", color: "#0f0", border: "1px solid #555", borderRadius: 4, cursor: "pointer", fontSize: 13 }}
              >
                {this.state.copied ? "Copied!" : "Copy Error Report"}
              </button>
              <button
                onClick={() => this.setState({ error: null, copied: false })}
                style={{ padding: "8px 16px", background: "#333", color: "#ff0", border: "1px solid #555", borderRadius: 4, cursor: "pointer", fontSize: 13 }}
              >
                Try Again
              </button>
            </div>
            <p style={{ color: "#888", fontSize: 11, marginTop: 12 }}>
              Press ` to open the debug log. Copy and send to the developer.
            </p>
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}

// Disable right click context menu
document.addEventListener("contextmenu", (e) => e.preventDefault());

createRoot(document.getElementById("root")!).render(
  <ErrorBoundary>
    <App />
  </ErrorBoundary>
);
