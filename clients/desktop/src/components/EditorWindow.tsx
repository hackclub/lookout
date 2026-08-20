import { useCallback, useEffect, useRef, useState } from "react";
import { emit } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { confirm } from "@tauri-apps/plugin-dialog";
import { createLookoutClient, type CutInterval } from "@lookout/react";
import { TimelapseEditor, colors, fontSize, fontWeight, spacing } from "@lookout/react";
import { invoke } from "../logger.js";
import { getApiBase } from "../serverConfig.js";
import { useBackdropState, useDesktopAppearance, SHELL_DRAWS_FRAME, WINDOW_MARGIN } from "../linuxChrome.js";
import { HeaderBar } from "./HeaderBar.js";
import { WindowResizeHandles, useWindowFrameState } from "./WindowResizeHandles.js";
import { isLinux as IS_LINUX } from "../platform.js";

/** Event the editor window emits after applying cuts, so the main window
 *  can refresh the session detail + gallery. Payload: { token }. */
export const EDITED_EVENT = "lookout-edited";

/** Emitted when an editor window is opened, so the main window can step
 *  out of the way. Payload: { token }. */
export const EDITOR_OPENED_EVENT = "lookout-editor-opened";

/** Tauri window label for a session's editor. */
export function editorWindowLabel(token: string): string {
  return `editor-${token.slice(0, 8)}`;
}

/** Is the editor window for this session currently open? */
export async function isEditorWindowOpen(token: string): Promise<boolean> {
  try {
    return (await WebviewWindow.getByLabel(editorWindowLabel(token))) !== null;
  } catch {
    return false;
  }
}

/** Bring an already-open editor window to the front. */
export async function focusEditorWindow(token: string): Promise<void> {
  const win = await WebviewWindow.getByLabel(editorWindowLabel(token));
  await win?.setFocus().catch(() => {});
}

/**
 * Open (or focus) the dedicated editor window for a session. The main
 * window is a fixed 480×640 — far too small to scrub a multi-hour
 * timeline with any precision — so editing gets its own resizable window.
 */
export async function openEditorWindow(token: string): Promise<void> {
  const label = editorWindowLabel(token);
  const existing = await WebviewWindow.getByLabel(label);
  if (existing) {
    await existing.setFocus().catch(() => {});
    await emit(EDITOR_OPENED_EVENT, { token }).catch(() => {});
    return;
  }
  const isMacOS = navigator.userAgent.includes("Mac");
  // Linux reserves a transparent frame around the visible window for its
  // outer border and shadow, so every dimension grows by twice it — the
  // content keeps the size these numbers describe. No frame, no growth:
  // a shell that draws its own corners leaves us nothing to make room for.
  const pad = IS_LINUX && !SHELL_DRAWS_FRAME ? WINDOW_MARGIN * 2 : 0;
  const win = new WebviewWindow(label, {
    url: `${window.location.pathname}#/editor?token=${token}`,
    title: "Edit timelapse",
    width: 940 + pad,
    height: 660 + pad,
    // The floor is what the shell actually needs: chrome + a legible
    // stage + the dock. Below this the layout would be cramped rather
    // than broken — it still reflows — but there's no reason to allow it.
    minWidth: 620 + pad,
    minHeight: 480 + pad,
    resizable: true,
    center: true,
    // Transparent + overlay titlebar is what lets the vibrancy material
    // show through, matching the main window's chrome exactly.
    transparent: true,
    // Overlay titlebar with the title VISIBLE: macOS draws and centers it
    // for us, so it can't drift out of alignment with the traffic lights
    // the way a hand-placed label does.
    ...(isMacOS ? { titleBarStyle: "overlay" as const } : {}),
    // Linux: no GTK titlebar, because the window draws its own header bar.
    // The resize borders that disappear with it are redrawn in the webview
    // (WindowResizeHandles), so the window stays resizable.
    ...(IS_LINUX ? { decorations: false } : {}),
  });
  win.once("tauri://error", (e) => {
    console.error("[editor] failed to open editor window:", e);
  });
  await emit(EDITOR_OPENED_EVENT, { token }).catch(() => {});
}

/**
 * Close this editor window and hand focus back to the app.
 *
 * Never swallow the failure: if the close is refused (a missing
 * capability, say) a silent catch leaves the user staring at a window
 * that says it saved and won't go away. Log it, then fall back to
 * destroy(), which skips the close-requested round trip entirely.
 */
async function closeEditorWindow(): Promise<void> {
  // Bring the main window forward first — closing the frontmost window
  // otherwise drops the user behind whatever app is underneath.
  try {
    const main = await WebviewWindow.getByLabel("main");
    await main?.setFocus();
  } catch (e) {
    console.warn("[editor] could not focus main window:", e);
  }

  try {
    await getCurrentWindow().close();
  } catch (e) {
    console.error("[editor] close() failed, destroying instead:", e);
    try {
      await getCurrentWindow().destroy();
    } catch (e2) {
      console.error("[editor] destroy() failed too:", e2);
    }
  }
}

/**
 * What the main window shows while the editor window is up. The editing
 * happens over there, so anything rendered here would just be a second,
 * stale copy of the same session competing for attention.
 */
export function EditorOpenPlaceholder({ token }: { token: string }) {
  const [focusing, setFocusing] = useState(false);

  return (
    <div
      role="button"
      tabIndex={0}
      onClick={() => {
        setFocusing(true);
        void focusEditorWindow(token).finally(() => setFocusing(false));
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") void focusEditorWindow(token);
      }}
      style={{
        height: "100%",
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        gap: spacing.md,
        padding: spacing.xxl,
        textAlign: "center",
        cursor: "pointer",
        userSelect: "none",
        opacity: focusing ? 0.6 : 1,
        transition: "opacity 0.15s ease",
      }}
    >
      <svg
        width="40"
        height="40"
        viewBox="0 0 24 24"
        fill="none"
        stroke={colors.text.tertiary}
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden="true"
      >
        <rect x="2" y="4" width="20" height="16" rx="2" />
        <path d="M2 9h20" />
        <path d="M9 14h6" />
      </svg>
      <div
        style={{
          fontSize: fontSize.lg,
          fontWeight: fontWeight.bold,
          color: colors.text.primary,
        }}
      >
        Edit your timelapse in the edit window.
      </div>
      <div style={{ fontSize: fontSize.sm, color: colors.text.tertiary }}>
        Click here to bring it to the front.
      </div>
    </div>
  );
}

/**
 * Tracks whether an editor window is open, for the main window.
 *
 * Listens for the open event, then polls for the window's existence — the
 * poll is what guarantees the main window can never get stuck behind the
 * placeholder if the editor window is force-quit or crashes.
 */
export function useEditorWindowOpen(): string | null {
  const [token, setToken] = useState<string | null>(null);

  useEffect(() => {
    const unlisteners: Array<() => void> = [];
    void import("@tauri-apps/api/event").then(async ({ listen }) => {
      unlisteners.push(
        await listen<{ token: string }>(EDITOR_OPENED_EVENT, (e) => {
          if (e.payload?.token) setToken(e.payload.token);
        }),
        // Publishing closes the editor window; clear immediately rather
        // than waiting for the poll, so the session view is back the
        // instant the user saves.
        await listen(EDITED_EVENT, () => setToken(null)),
      );
    });
    return () => {
      for (const un of unlisteners) un();
    };
  }, []);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    const id = setInterval(async () => {
      if (cancelled) return;
      if (!(await isEditorWindowOpen(token))) setToken(null);
    }, 1000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [token]);

  return token;
}

/**
 * The editor window's root view (route `#/editor?token=…`).
 *
 * An app shell, not a page: a draggable strip clearing the window
 * controls, a body that owns all remaining height, and nothing that can
 * push content past the window edge. The chrome is the system vibrancy
 * material, so this window reads as the same app as the main one in both
 * light and dark.
 *
 * The title is the WINDOW's title, drawn by the OS — centered and aligned
 * to the traffic lights for free. A hand-placed label next to them has to
 * be pixel-matched against a position that varies by OS version, and it
 * was visibly off.
 */
export function EditorWindow({ token }: { token: string }) {
  const isMacOS = navigator.userAgent.includes("Mac");
  // Undecorated on Linux, same as the main window, so it owns its corners and
  // header bar — and follows the GTK theme live.
  const appearance = useDesktopAppearance({ undecorated: true });
  const [sessionName, setSessionName] = useState<string | null>(null);
  useWindowFrameState();
  useBackdropState();

  // Vibrancy: the main window does this too. The webview must be
  // transparent for the material to show, so only go transparent once the
  // native side confirms it applied — otherwise (Linux, or a failure) the
  // window would render see-through with nothing behind it.
  useEffect(() => {
    const html = document.documentElement;
    const body = document.body;
    const root = document.getElementById("root");
    // Linux's own chrome is handled by useDesktopAppearance above; the
    // vibrancy below is a macOS/Windows affair.
    if (IS_LINUX) return;

    let applied = false;
    invoke("enable_vibrancy")
      .then(() => {
        applied = true;
        html.style.background = "transparent";
        body.style.background = "transparent";
        if (root) root.style.background = "transparent";
      })
      .catch((err) => {
        console.warn("[editor] vibrancy unavailable, falling back:", err);
        html.style.background = "var(--color-bg-body)";
        body.style.background = "var(--color-bg-body)";
        if (root) root.style.background = "var(--color-bg-body)";
      });

    return () => {
      if (applied) invoke("disable_vibrancy").catch(() => {});
    };
  }, []);

  // The window title carries the session name on the native title bar;
  // the in-window strip shows it too, since the title is hidden on macOS.
  useEffect(() => {
    fetch(`${getApiBase()}/api/sessions/${token}`)
      .then((r) => (r.ok ? r.json() : null))
      .then((d) => {
        if (d?.name) {
          setSessionName(d.name);
          void getCurrentWindow().setTitle(`Edit — ${d.name}`);
        }
      })
      .catch(() => {
        // Name is decoration — the editor works without it.
      });
  }, [token]);

  // Closing the window IS the decision to finish: the timelapse publishes
  // with whatever cuts are on screen. There's no "leave it hanging" exit —
  // the session is unpublished until someone decides, so an editor that
  // could be dismissed without deciding would just strand it until the
  // lease lapsed. Hence: confirm, publish, then close.
  const cutsRef = useRef<CutInterval[]>([]);
  const dirtyRef = useRef(false);
  const finishedRef = useRef(false);
  const client = useRef(
    createLookoutClient({ baseUrl: getApiBase(), token }),
  ).current;

  const finishAndClose = useCallback(async () => {
    finishedRef.current = true;
    let published: Awaited<ReturnType<typeof client.applyCuts>> | null = null;
    try {
      await client.setCuts(cutsRef.current);
      published = await client.applyCuts();
    } catch (e) {
      console.error("[editor] publish on close failed:", e);
      // Don't trap the user in a window they asked to close: the hold
      // lapses on its own and publishes as recorded shortly after.
    }
    // Fire-and-forget: the close must not wait on the notification. Carry
    // the publish result so the main window can fire the redirect the
    // instant it's done (`complete`) or watch the compile to completion.
    emit(EDITED_EVENT, {
      token,
      status: published?.status ?? null,
      redirectUrl: published?.redirectUrl ?? null,
    }).catch((e) => console.error("[editor] emit failed:", e));
    await closeEditorWindow();
  }, [client, token]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void getCurrentWindow()
      .onCloseRequested(async (event) => {
        if (finishedRef.current) return;
        event.preventDefault();
        const removed = cutsRef.current.length;
        const ok = await confirm(
          dirtyRef.current && removed > 0
            ? `Closing publishes this timelapse with ${removed} cut${
                removed === 1 ? "" : "s"
              } applied. This can't be undone.`
            : "Closing publishes this timelapse as recorded. This can't be undone.",
          { title: "Finish timelapse?", kind: "warning" },
        );
        if (ok) void finishAndClose();
      })
      .then((fn) => {
        unlisten = fn;
      });
    return () => {
      if (unlisten) unlisten();
    };
  }, [finishAndClose]);

  return (
    <div
      style={{
        height: "100%",
        minHeight: 0,
        boxSizing: "border-box",
        color: colors.text.primary,
        display: "flex",
        flexDirection: "column",
      }}
    >
      {/* Drag strip clearing the window controls and the OS-drawn title.
          macOS only: elsewhere the window has real decorations above the
          webview, so the content can start at the very top. */}
      {isMacOS && (
        <div
          data-tauri-drag-region
          style={{ flex: "0 0 auto", height: 30, cursor: "default" }}
        />
      )}

      {/* Linux: the header bar IS the titlebar. Its close button goes
          through the window's normal close path, so the publish-on-close
          confirmation below still runs. */}
      {IS_LINUX && (
        <>
          <HeaderBar
            title={sessionName ?? "Edit timelapse"}
            subtitle={sessionName ? "Editing timelapse" : undefined}
            appearance={appearance}
            maximizable
          />
          <WindowResizeHandles />
        </>
      )}

      {/* Body owns the rest. min-height:0 is what lets the stage inside
          letterboxe down instead of clipping the dock off the bottom. */}
      <div
        style={{
          flex: "1 1 auto",
          minHeight: 0,
          display: "flex",
          flexDirection: "column",
          padding: isMacOS || IS_LINUX
            ? `0 ${spacing.lg}px ${spacing.lg}px`
            : spacing.lg,
        }}
      >
        <TimelapseEditor
          token={token}
          apiBaseUrl={getApiBase()}
          onCutsChange={(cuts, dirty) => {
            cutsRef.current = cuts;
            dirtyRef.current = dirty;
          }}
          onApplied={(result) => {
            // Saved from inside the editor. Flag it first so the close
            // handler doesn't prompt to publish what's already published.
            finishedRef.current = true;
            emit(EDITED_EVENT, {
              token,
              status: result.status,
              redirectUrl: result.redirectUrl,
            }).catch((e) => console.error("[editor] emit failed:", e));
            void closeEditorWindow();
          }}
        />
      </div>
    </div>
  );
}
