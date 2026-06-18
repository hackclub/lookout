/**
 * Centralized logger for the Lookout desktop app.
 *
 * - Captures all console.log/warn/error/debug output
 * - Renders to the #debug panel (toggled with backtick)
 * - Provides getReport() for copy-paste error reports
 * - Re-exports a logged `invoke` wrapper for Tauri IPC
 * - Listens for "capture-progress" Tauri events from the Rust pipeline
 */

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface LogEntry {
  time: number;
  level: "debug" | "info" | "warn" | "error";
  message: string;
}

// ---------------------------------------------------------------------------
// Circular buffer
// ---------------------------------------------------------------------------

const MAX_ENTRIES = 200;
const entries: LogEntry[] = [];
let appVersion = "unknown";
getVersion().then((v) => { appVersion = v; }).catch(() => {});

function push(level: LogEntry["level"], message: string) {
  entries.push({ time: Date.now(), level, message });
  if (entries.length > MAX_ENTRIES) entries.shift();
  scheduleRender();
}

// ---------------------------------------------------------------------------
// Monkey-patch console so EVERY console call flows into the debug panel
// ---------------------------------------------------------------------------

const _log = console.log.bind(console);
const _debug = console.debug.bind(console);
const _warn = console.warn.bind(console);
const _error = console.error.bind(console);

function argsToString(args: unknown[]): string {
  return args
    .map((a) => {
      if (typeof a === "string") return a;
      try {
        return JSON.stringify(a);
      } catch {
        return String(a);
      }
    })
    .join(" ");
}

console.log = (...args: unknown[]) => {
  _log(...args);
  push("info", argsToString(args));
};
console.debug = (...args: unknown[]) => {
  _debug(...args);
  push("debug", argsToString(args));
};
console.warn = (...args: unknown[]) => {
  _warn(...args);
  push("warn", argsToString(args));
};
console.error = (...args: unknown[]) => {
  _error(...args);
  push("error", argsToString(args));
};

// ---------------------------------------------------------------------------
// Global error handlers (supersede the inline script in index.html)
// ---------------------------------------------------------------------------

window.onerror = (msg, src, line) => {
  push("error", `UNCAUGHT: ${msg} (${src}:${line})`);
};
window.onunhandledrejection = (e) => {
  const reason = e.reason instanceof Error ? e.reason.message + (e.reason.stack ? "\n" + e.reason.stack : "") : String(e.reason);
  push("error", `UNHANDLED REJECTION: ${reason}`);
};

// ---------------------------------------------------------------------------
// Listen for Tauri events from Rust pipeline
// ---------------------------------------------------------------------------

listen<string>("capture-progress", (event) => {
  console.log(`[capture] ${event.payload}`);
}).catch(() => {
  // If listen fails (e.g. Tauri not ready yet), that's fine
});

// ---------------------------------------------------------------------------
// Debug panel rendering
// ---------------------------------------------------------------------------

const LEVEL_COLORS: Record<LogEntry["level"], string> = {
  debug: "#888",
  info: "#0f0",
  warn: "#ff0",
  error: "#f44",
};

let renderTimer: ReturnType<typeof setTimeout> | null = null;

function scheduleRender() {
  if (renderTimer !== null) return;
  // Skip entirely when panel is hidden — no timer, no rAF, zero overhead
  const el = document.getElementById("debug");
  if (!el || el.style.display === "none") return;
  renderTimer = setTimeout(() => {
    renderTimer = null;
    render();
  }, 250);
}

// Event delegation: one listener on #debug, never re-created.
// This avoids leaking closures from addEventListener on nodes that
// innerHTML will destroy on the next render cycle.
let delegationReady = false;
function ensureDelegation() {
  if (delegationReady) return;
  const el = document.getElementById("debug");
  if (!el) return;
  delegationReady = true;
  el.addEventListener("click", (e) => {
    const target = e.target as HTMLElement;
    if (target.id === "dbg-copy") {
      navigator.clipboard.writeText(getReport()).then(
        () => {
          const btn = document.getElementById("dbg-copy");
          if (btn) btn.textContent = "Copied!";
          setTimeout(() => {
            const btn = document.getElementById("dbg-copy");
            if (btn) btn.textContent = "Copy Log";
          }, 1500);
        },
        () => {},
      );
    }
    if (target.id === "dbg-clear") {
      entries.length = 0;
      render();
    }
  });
}

function render() {
  const el = document.getElementById("debug");
  if (!el || el.style.display === "none") return;
  ensureDelegation();

  // Build HTML with array join (avoids O(n^2) string concatenation)
  const parts: string[] = [
    '<div style="position:sticky;top:0;background:#111;padding:4px 8px;border-bottom:1px solid #333;display:flex;gap:8px;z-index:1;">',
    '<button id="dbg-copy" style="background:#333;color:#0f0;border:1px solid #555;border-radius:3px;padding:2px 8px;cursor:pointer;font-size:11px;">Copy Log</button>',
    '<button id="dbg-clear" style="background:#333;color:#ff0;border:1px solid #555;border-radius:3px;padding:2px 8px;cursor:pointer;font-size:11px;">Clear</button>',
    "</div>",
  ];

  for (const entry of entries) {
    const t = new Date(entry.time).toLocaleTimeString("en-US", { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" });
    const color = LEVEL_COLORS[entry.level];
    const levelTag = entry.level.toUpperCase().padEnd(5);
    const safe = entry.message.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
    parts.push(`<div style="color:${color};white-space:pre-wrap;word-break:break-all;">${t} ${levelTag} ${safe}</div>`);
  }

  el.innerHTML = parts.join("");
  el.scrollTop = el.scrollHeight;
}

// ---------------------------------------------------------------------------
// Report generation (for copy-paste)
// ---------------------------------------------------------------------------

export function getReport(headline?: string): string {
  const now = new Date().toISOString();
  const platform = navigator.platform || "unknown";
  const ua = navigator.userAgent || "";

  const lines: string[] = [];
  // Surface the specific error first so a copied report leads with what
  // actually went wrong, rather than burying it in the log buffer below.
  if (headline) lines.push(`Error: ${headline}`, "");
  lines.push(`Lookout Desktop v${appVersion} | ${platform} | ${now}`, `UA: ${ua}`, "---");

  // Last 100 entries
  const recent = entries.slice(-100);
  for (const entry of recent) {
    const t = new Date(entry.time).toLocaleTimeString("en-US", { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" });
    const levelTag = entry.level.toUpperCase().padEnd(5);
    lines.push(`${t} ${levelTag} ${entry.message}`);
  }

  return lines.join("\n");
}

// ---------------------------------------------------------------------------
// Logged invoke wrapper — drop-in replacement for @tauri-apps/api/core invoke
// ---------------------------------------------------------------------------

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  console.log(`[ipc] → ${cmd}`, args ?? "");
  try {
    const result = await tauriInvoke<T>(cmd, args);
    console.debug(`[ipc] ← ${cmd} ok`);
    return result;
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    console.error(`[ipc] ← ${cmd} FAILED: ${msg}`);
    throw err;
  }
}

// ---------------------------------------------------------------------------
// Expose render so the backtick toggle (in index.html) can trigger a
// catch-up render when the panel is made visible.
// ---------------------------------------------------------------------------

(window as any).__dbgRender = render;

// ---------------------------------------------------------------------------
// Init log
// ---------------------------------------------------------------------------

console.log("[logger] initialized");
