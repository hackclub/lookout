import * as Sentry from "@sentry/react";
import React, { useState, useEffect, useCallback, useMemo } from "react";
import { listen } from "@tauri-apps/api/event";
import { confirm, message } from "@tauri-apps/plugin-dialog";
import { invoke } from "./logger.js";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Menu, MenuItem, PredefinedMenuItem } from "@tauri-apps/api/menu";
import { AnimatePresence, motion } from "motion/react";
import {
  Gallery,
  SessionDetail,
  Spinner,
  useTokenStore,
  useGallery,
  useHashRouter,
  colors,
  fontSize,
  radii,
  type AddAnchor,
} from "@lookout/react";
import { getVersion } from "@tauri-apps/api/app";
import { ArrowSquareOutIcon, GearSixIcon, PlusIcon } from "@phosphor-icons/react";
import { isValidToken, extractToken } from "./utils.js";
import type { SessionResponse } from "@lookout/shared";
import {
  StartLinkedError,
  beginPairing,
  completePairing,
  getLink,
  isLinkable,
  parsePairCallback,
  startLinkedSession,
} from "./programLink.js";
import {
  checkCameraPermission,
  checkScreenRecordingPermission,
} from "tauri-plugin-macos-permissions-api";
import { PermissionScreen, permCacheKey } from "./components/PermissionScreen.js";
import { RecordPage } from "./components/RecordPage.js";
import { AddSessionPage } from "./components/AddSessionPage.js";
import { SettingsPage } from "./components/SettingsPage.js";
import { TrayApp } from "./components/TrayApp.js";
import {
  EditorWindow,
  EditorOpenPlaceholder,
  useEditorWindowOpen,
  openEditorWindow,
  EDITED_EVENT,
} from "./components/EditorWindow.js";
import { useBlacklistedApps } from "./hooks/useBlacklistedApps.js";
import { useAppUpdate } from "./hooks/useAppUpdate.js";
import { useAnnouncement } from "./hooks/useAnnouncement.js";
import { useTip } from "./hooks/useTip.js";
import { useLinuxUpdate } from "./hooks/useLinuxUpdate.js";
import { ensureNotificationPermission } from "./hooks/useRecorderNotices.js";
import {
  createTauriLookoutClient,
  fetchPrograms as fetchProgramRegistry,
  fetchSessionsBatch,
} from "./api/tauriClient.js";
import { UpdatePill } from "./components/UpdatePill.js";
import { AddMenuPopup, type AddMenuPopupItem } from "./components/AddMenuPopup.js";
import { ProgramPanel } from "./components/ProgramPanel.js";
import { PanelPrompt } from "./components/PanelPrompt.js";
import { OpenInProgram } from "./components/OpenInProgram.js";
import {
  getPanelState,
  isPanelUrlAcceptable,
  setPanelState,
  shouldOfferPanel,
} from "./programPanel.js";
import { AnnouncementBanner } from "./components/AnnouncementBanner.js";
import { TipDrawer } from "./components/TipDrawer.js";
import { UpdateDrawer } from "./components/UpdateDrawer.js";
import {
  installTipDebug,
  markTipMoment,
  recordDeepLinkSession,
  shouldShowTip,
  type Tip,
  type TipMoment,
} from "./tip.js";
import { getApiBase } from "./serverConfig.js";
import { HeaderBar } from "./components/HeaderBar.js";
import { useBackdropState, useBackgroundBlur, useDesktopAppearance } from "./linuxChrome.js";
import { isLinux } from "./platform.js";
import { HeaderNavProvider, type HeaderNav } from "./headerNav.js";
import { useWindowFrameState } from "./components/WindowResizeHandles.js";

// Read once per webview load; Settings → Server reloads the view on change.
const API_BASE = getApiBase();

// Module-level so its identity is stable across renders (it is a hook dep).
const galleryBatchLookup = (tokens: string[]) => fetchSessionsBatch(API_BASE, tokens);

// How long to keep watching a post-edit cut-compile for `complete` before
// giving up on firing the redirect hook. The worker's assemble step alone
// can run up to 30 min (ASSEMBLE_TIMEOUT_MS); add slack for queue wait and
// the final upload so a legitimately slow compile is never abandoned.
const REDIRECT_POLL_MAX_MS = 35 * 60_000;

/**
 * What starting a program did, so a caller can tell "we opened your browser,
 * keep waiting" apart from "nothing happened".
 */
export type ProgramStartOutcome =
  /** A session began and the app has already navigated to the record page. */
  | "started"
  /** The program's site or consent page opened; a deep link is expected next. */
  | "browser"
  /** Nothing opened. */
  | "failed";

interface Program {
  name: string;
  displayName?: string;
  newSessionUrl: string;
  iconUrl?: string | null;
  // Desktop instant-start endpoints (both present or both null) — see
  // programLink.ts. Older servers simply omit them.
  pairUrl?: string | null;
  startUrl?: string | null;
}

/** Server API for one session, through the Rust core (never raw fetch). */
function sessionClient(token: string) {
  return createTauriLookoutClient({ baseUrl: API_BASE, token });
}

/** Pause a session by token. Fire-and-forget, logs errors. */
async function pauseSession(token: string): Promise<void> {
  try {
    console.log(`[app] pausing session ${token.slice(0, 8)}...`);
    await sessionClient(token).pause();
    console.log(`[app] paused session ${token.slice(0, 8)}`);
  } catch (e) {
    console.error(`[app] failed to pause session ${token.slice(0, 8)}:`, e);
  }
}

/** Fetch a session's status. Returns null on error. */
async function fetchSessionStatus(token: string): Promise<string | null> {
  try {
    const data = await sessionClient(token).getStatus();
    return data.status ?? null;
  } catch {
    return null;
  }
}

/**
 * Which program a session belongs to, or null on error. Separate from the
 * status call rather than replacing it: different rate-limit buckets, issued
 * in parallel, and routing must not start depending on the heavier payload.
 */
async function fetchSessionProgram(token: string): Promise<string | null> {
  try {
    const data = await sessionClient(token).getSession();
    const program = (data as { program?: unknown }).program;
    return typeof program === "string" ? program : null;
  } catch {
    return null;
  }
}

export function App() {
  const isTray = window.location.hash.includes("tray");
  if (isTray) {
    return <TrayApp />;
  }
  // Dedicated editor window (see EditorWindow.tsx). Branches before
  // MainWindowApp so it skips the permission gates, vibrancy, deep-link
  // handlers, and the rest of the main-window machinery.
  const editorMatch = window.location.hash.match(/^#\/?editor\?token=([0-9a-fA-F]{64})/);
  if (editorMatch) {
    return <EditorWindow token={editorMatch[1]} />;
  }
  return <MainWindowApp />;
}

function MainWindowApp() {
  const isMacOS = navigator.userAgent.includes("Mac");
  // A cached grant skips the permission gate (no boot flicker); a background
  // re-check below yanks it back if the permission was revoked since.
  const [screenPermGranted, setScreenPermGranted] = useState(
    () => !isMacOS || localStorage.getItem(permCacheKey("screen")) === "1",
  );
  const [cameraPermGranted, setCameraPermGranted] = useState(
    () => !isMacOS || localStorage.getItem(permCacheKey("camera")) === "1",
  );

  useEffect(() => {
    if (!isMacOS) return;
    (async () => {
      try {
        if (localStorage.getItem(permCacheKey("screen")) === "1" && !(await checkScreenRecordingPermission())) {
          console.warn("[permissions] screen recording revoked — regating");
          localStorage.removeItem(permCacheKey("screen"));
          setScreenPermGranted(false);
        }
        if (localStorage.getItem(permCacheKey("camera")) === "1" && !(await checkCameraPermission())) {
          console.warn("[permissions] camera revoked — regating");
          localStorage.removeItem(permCacheKey("camera"));
          setCameraPermGranted(false);
        }
      } catch {
        // Plugin unavailable — leave the cached grants alone
      }
    })();
  }, [isMacOS]);
  const [isWayland, setIsWayland] = useState(false);
  const { route, navigate } = useHashRouter();
  // Memoised on the token: SessionDetail keys its fetch effects and its
  // compiling poll off the client's identity, so a fresh object per render
  // would re-fire them on every parent re-render.
  const detailToken = route.page === "session" ? route.token : null;
  const detailClient = useMemo(
    () => (detailToken ? sessionClient(detailToken) : null),
    [detailToken],
  );
  const tokenStore = useTokenStore();
  const appUpdate = useAppUpdate();
  const gallery = useGallery({
    apiBaseUrl: API_BASE,
    tokens: tokenStore.getAllTokenValues(),
    fetchSessions: galleryBatchLookup,
  });

  // While the editor window is up, the main window steps aside entirely —
  // two views of the same session competing for attention is worse than
  // one clear pointer to where the work is happening.
  const editorWindowToken = useEditorWindowOpen();

  // Bumped when an editor window applies cuts — remounts the open
  // SessionDetail so it re-fetches (picks up the compiling → complete flip
  // and the recompiled video) and refreshes gallery thumbnails.
  const [editNonce, setEditNonce] = useState(0);
  const galleryRefreshRef = React.useRef(gallery.refresh);
  galleryRefreshRef.current = gallery.refresh;

  // The redirect hook must fire exactly once per session, from whichever
  // path observes the timelapse finish. Both paths funnel through here.
  const redirectFiredRef = React.useRef<Set<string>>(new Set());
  const fireRedirect = useCallback((token: string, url: string | null) => {
    if (!url || redirectFiredRef.current.has(token)) return;
    redirectFiredRef.current.add(token);
    console.log("[app] firing redirect hook");
    invoke("open_external_url", { url }).catch((e) =>
      console.error("[app] redirect hook failed:", e),
    );
  }, []);

  // The program panel currently on screen, if any. A session's `panelUrl` is
  // shown in-app in place of the redirect hop, so the two are mutually
  // exclusive per session — see openPanelOrRedirect.
  const [panel, setPanel] = useState<{
    token: string;
    url: string;
    fallbackUrl: string | null;
  } | null>(null);
  // Bumped on panel resolution so an open SessionDetail re-evaluates whether
  // it still owes a prompt (the state lives in localStorage, not React).
  const [panelNonce, setPanelNonce] = useState(0);

  /**
   * A display name for whoever owns a panel URL. Matched against the cached
   * program registry by origin, since the session list doesn't carry the
   * program name; falls back to the host, which is honest and specific
   * enough (the sheet shows the full origin next to it anyway).
   */
  const programLabelForUrl = useCallback((url: string): string => {
    let origin: string;
    let host: string;
    try {
      const parsed = new URL(url);
      origin = parsed.origin;
      host = parsed.hostname.replace(/^www\./, "");
    } catch {
      return "This program";
    }
    const match = programsRef.current.find((p) =>
      [p.newSessionUrl, p.pairUrl, p.startUrl].some((candidate) => {
        if (!candidate) return false;
        try {
          return new URL(candidate).origin === origin;
        } catch {
          return false;
        }
      }),
    );
    return match?.displayName || match?.name || host;
  }, []);

  /** The same registry match's icon, for the panel's attribution row. */
  const programIconForUrl = useCallback((url: string): string | null => {
    let origin: string;
    try {
      origin = new URL(url).origin;
    } catch {
      return null;
    }
    const match = programsRef.current.find((p) =>
      [p.newSessionUrl, p.pairUrl, p.startUrl].some((candidate) => {
        if (!candidate) return false;
        try {
          return new URL(candidate).origin === origin;
        } catch {
          return false;
        }
      }),
    );
    return match?.iconUrl ?? null;
  }, []);

  const openPanel = useCallback((token: string, url: string, fallbackUrl: string | null) => {
    setPanel({ token, url, fallbackUrl });
  }, []);

  /**
   * Open a session's panel, if it has one that hasn't been dealt with.
   *
   * Called when the recording is SAVED, not when it finishes compiling: a
   * compile can run for many minutes, and there is no reason to make someone
   * watch a progress bar before answering "what should this be called?". The
   * program gets its answers while the video builds.
   *
   * Never re-offered once the program says it has what it needed — the user
   * may have answered on the program's own site instead.
   */
  const maybeOpenPanel = useCallback(
    (
      token: string,
      redirectUrl: string | null,
      panelUrl: string | null | undefined,
      panelResolved?: boolean,
    ) => {
      // Already answered — on this device, on the program's website, anywhere.
      // Opening it would just have the panel tell us it is done and close, which
      // reads as the sheet flashing open and shutting for no reason.
      if (panelResolved) return false;
      if (isPanelUrlAcceptable(panelUrl) && getPanelState(token) === "pending") {
        openPanel(token, panelUrl, redirectUrl);
        return true;
      }
      return false;
    },
    [openPanel],
  );

  /**
   * The compile finished. The redirect hook is the browser handoff and still
   * belongs here — but only for sessions with no panel, since a panel is the
   * same handoff done in-app and firing both would send the user to a browser
   * tab they already dealt with.
   */
  const handleCompleted = useCallback(
    (token: string, redirectUrl: string | null, panelUrl: string | null | undefined) => {
      if (isPanelUrlAcceptable(panelUrl)) return;
      fireRedirect(token, redirectUrl);
    },
    [fireRedirect],
  );

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    listen<{ token: string; status?: string | null; redirectUrl?: string | null; panelUrl?: string | null; panelResolved?: boolean }>(
      EDITED_EVENT,
      (event) => {
        console.log("[app] editor window published — refreshing");
        setEditNonce((n) => n + 1);
        galleryRefreshRef.current();

        // Publishing from the editor can land instantly (no cuts) or after a
        // cut-compile. Either way SessionDetail may mount on an
        // already-complete session, and its onComplete deliberately doesn't
        // fire for that — so the redirect hook would be silently skipped in
        // the whole edit flow. Fire it from here instead.
        const token = event.payload?.token;
        if (!token) return;

        // Publishing from the editor IS the save for a held session, so the
        // panel opens now — whether the cut compile is instant or still
        // running. Only the redirect has to wait for `complete`.
        const publishedPanel = maybeOpenPanel(
          token,
          event.payload?.redirectUrl ?? null,
          event.payload?.panelUrl ?? null,
          event.payload?.panelResolved,
        );

        // Instant publish (no cuts): the /compile response already told us
        // it's `complete` and carried the redirect URL. Fire now — no poll.
        if (event.payload?.status === "complete") {
          handleCompleted(
            token,
            event.payload.redirectUrl ?? null,
            event.payload.panelUrl ?? null,
          );
          return;
        }
        // A panel is already open/handled; the poll below only exists to fire
        // the redirect hook, which a panelled session never does.
        if (publishedPanel) return;

        // A compile is running server-side. Poll until it's terminal.
        // The worker's assemble step alone can run up to ASSEMBLE_TIMEOUT_MS
        // (30 min); a fixed few-minute deadline abandoned long compiles
        // before they finished. Cap at that budget plus queue/upload slack,
        // and back off so a busy worker isn't hammered.
        const deadline = Date.now() + REDIRECT_POLL_MAX_MS;
        let delay = 2500;
        const poll = async () => {
          if (cancelled || Date.now() > deadline) return;
          try {
            const data = await sessionClient(token).getStatus();
            if (data.status === "complete") {
              galleryRefreshRef.current();
              handleCompleted(token, data.redirectUrl ?? null, data.panelUrl ?? null);
              return;
            }
            if (data.status === "failed") return;
          } catch {
            // Transient (or a non-2xx) — the retry below covers it.
          }
          delay = Math.min(delay * 1.5, 15_000);
          setTimeout(poll, delay);
        };
        void poll();
      },
    ).then((fn) => { unlisten = fn; });

    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, [maybeOpenPanel, handleCompleted]);

  // Initialize blacklisted apps sync from localStorage to Rust backend
  useBlacklistedApps();

  // Boot timing: first React commit and the frame after it (≈ first paint).
  useEffect(() => {
    console.log(`[boot] app mounted at ${Math.round(performance.now())}ms`);
    requestAnimationFrame(() =>
      console.log(`[boot] first frame at ${Math.round(performance.now())}ms`),
    );
  }, []);

  // Request notification permission at launch, not deferred to recording start.
  useEffect(() => {
    void ensureNotificationPermission();
  }, []);

  // Admin-authored announcement banner; checked on open and every 15 min.
  const announcement = useAnnouncement();
  // Linux only: the in-app updater is off there, so a new release is
  // surfaced as instructions rather than a download. Held back while the tip
  // sheet is up so the two never stack.
  const { update: linuxUpdate, dismiss: dismissLinuxUpdate } = useLinuxUpdate();
  // Whether a tip is published; when it opens is decided per moment against
  // the session a qualifying deep link bound it to (see tip.ts).
  const tip = useTip();
  const [tipOpen, setTipOpen] = useState(false);
  // Set only by window.__tip.show(), which can hand the sheet a tip the
  // server never published.
  const [tipOverride, setTipOverride] = useState<Tip | null>(null);
  const openTip = useCallback(
    (moment: TipMoment, token: string | null | undefined) => {
      if (!tip || !token || !shouldShowTip(tip, moment, token)) return;
      markTipMoment(tip, moment, token);
      setTipOverride(null);
      setTipOpen(true);
    },
    [tip],
  );

  // Never calls markTipMoment: debugging must not burn a real impression.
  const tipRef = React.useRef(tip);
  tipRef.current = tip;
  useEffect(
    () =>
      installTipDebug({
        current: () => tipRef.current,
        show: (t) => {
          setTipOverride(t);
          setTipOpen(true);
        },
        hide: () => setTipOpen(false),
      }),
    [],
  );

  // First moment: the source picker, where a deep link to a fresh session
  // lands. RecordPage reports when the picker is up — the route alone stays
  // true for the whole capture.
  const handleSourcePicker = useCallback(() => {
    if (route.page !== "record") return;
    openTip("source", route.token);
  }, [route, openTip]);

  // Detect Wayland — filter apps feature is unsupported there
  useEffect(() => {
    invoke<boolean>("is_wayland").then(setIsWayland).catch(() => {});
  }, []);

  // Program registry cache for the + button's native popup menu. Warmed at
  // launch and refreshed on every open so the menu appears instantly with
  // whatever we have; the AddSessionPage stays the fallback (paste-a-link,
  // empty registry, non-macOS).
  const programsRef = React.useRef<Program[]>([]);
  const fetchPrograms = useCallback(async () => {
    try {
      const data = await fetchProgramRegistry(API_BASE);
      if (Array.isArray(data.programs)) {
        programsRef.current = data.programs;
        // Warm the icon cache so the menu never opens with fallback symbols
        // while images load — the Swift-side cache on macOS, the browser's
        // HTTP cache for the DOM popup elsewhere.
        const urls = programsRef.current
          .map((p) => p.iconUrl)
          .filter((u): u is string => !!u);
        if (urls.length) {
          if (isMacOS) {
            invoke("prefetch_add_menu_icons", { urls }).catch(() => {});
          } else {
            for (const url of urls) new Image().src = url;
          }
        }
      }
    } catch (e) {
      console.warn("[programs] failed to load registry:", e);
    }
  }, [isMacOS]);
  useEffect(() => {
    void fetchPrograms();
  }, [fetchPrograms]);

  // Windows/Linux add menu — a DOM replica of the macOS NSPanel popup.
  const [addMenu, setAddMenu] = useState<{ items: AddMenuPopupItem[]; anchor: AddAnchor } | null>(null);

  // Instant-start progress pill ("Starting Lapse…"). Only spans the couple of
  // network round-trips of a linked start; cleared in every outcome.
  const [startingProgram, setStartingProgram] = useState<string | null>(null);

  /**
   * Start a session through an established device link, then route the token
   * through the same path a deep link takes. Every failure lands somewhere
   * useful: a rejected/moved credential re-runs the pairing consent (one
   * browser hop, exactly what the old flow cost), anything else falls back
   * to the program's browser flow.
   */
  const startViaLink = useCallback(
    async (program: Program): Promise<ProgramStartOutcome> => {
      const label = program.displayName || program.name;
      setStartingProgram(label);
      try {
        const token = await startLinkedSession(program);
        await handleDeepLinkRef.current([`lookout://session/?token=${token}`]);
        return "started";
      } catch (e) {
        const reason = e instanceof StartLinkedError ? e.reason : "unavailable";
        console.warn(`[pair] instant start failed (${reason}):`, e);
        if (reason === "unauthorized" || reason === "origin-changed") {
          // The link was dropped by startLinkedSession; re-run consent.
          try {
            await beginPairing(program);
            return "browser";
          } catch (e2) {
            console.error("[pair] re-pairing failed, opening browser flow:", e2);
          }
        }
        try {
          await invoke("open_external_url", { url: program.newSessionUrl });
          return "browser";
        } catch (e3) {
          console.error("[pair] browser fallback failed too:", e3);
          return "failed";
        }
      } finally {
        setStartingProgram(null);
      }
    },
    [],
  );

  /**
   * Start a session for a program, whichever way it supports: a paired
   * program skips the browser entirely; a linkable-but-unpaired one runs the
   * pairing consent hop (one browser visit, the same cost as the old flow)
   * and starts on the way back; programs without the capability open their
   * site, as always. Shared by the + menu and the Start-a-recording page.
   */
  const openProgram = useCallback(
    async (program: Program): Promise<ProgramStartOutcome> => {
      if (isLinkable(program)) {
        if (getLink(program.name)) {
          return await startViaLink(program);
        }
        try {
          await beginPairing(program);
          return "browser";
        } catch (e) {
          console.error("[programs] pairing start failed, opening browser flow:", e);
        }
      }
      try {
        await invoke("open_external_url", { url: program.newSessionUrl });
        return "browser";
      } catch (e) {
        console.error("[programs] failed to open program url:", e);
        return "failed";
      }
    },
    [startViaLink],
  );

  /** Acts on an add-menu choice, from either the native panel or the DOM popup. */
  const handleMenuChoice = useCallback(
    async (choice: string | null) => {
      if (!choice) return; // dismissed
      if (choice === "create-new") {
        navigate({ page: "add" });
        return;
      }
      const program = programsRef.current.find((p) => `program:${p.name}` === choice);
      if (!program) return;
      if ((await openProgram(program)) === "failed") {
        navigate({ page: "add" });
      }
    },
    [navigate, openProgram],
  );

  const handleAdd = useCallback(
    async (anchor: AddAnchor) => {
      // Clicking the + while the DOM popup is open toggles it closed (the
      // popup ignores pointerdowns on the anchor so this click reaches us).
      if (addMenu) {
        setAddMenu(null);
        return;
      }
      const programs = programsRef.current;
      void fetchPrograms(); // refresh behind the menu for next open
      if (programs.length === 0) {
        navigate({ page: "add" });
        return;
      }
      if (!isMacOS) {
        setAddMenu({
          items: [
            ...programs.map((p) => ({
              id: `program:${p.name}`,
              label: p.displayName || p.name,
              iconUrl: p.iconUrl ?? undefined,
              // Stays visible while the icon loads or when a program has none.
              fallbackIcon: <ArrowSquareOutIcon size={15} weight="bold" />,
            })),
            { separator: true },
            { id: "create-new", label: "Start from link", fallbackIcon: <PlusIcon size={15} weight="bold" /> },
          ],
          anchor,
        });
        return;
      }
      const entries = [
        ...programs.map((p) => ({
          id: `program:${p.name}`,
          label: p.displayName || p.name,
          // The symbol stays as the fallback while the icon loads or when a
          // program has none.
          symbol: "arrow.up.forward.app",
          iconUrl: p.iconUrl ?? undefined,
        })),
        { separator: true },
        { id: "create-new", label: "Start from link", symbol: "plus" },
      ];
      let choice: string | null;
      try {
        choice = await invoke<string | null>("show_add_menu", { entries, anchor });
      } catch (e) {
        console.warn("[add-menu] native menu failed, falling back to page:", e);
        navigate({ page: "add" });
        return;
      }
      await handleMenuChoice(choice);
    },
    [isMacOS, addMenu, fetchPrograms, navigate, handleMenuChoice],
  );

  // Opens a session the way clicking its card would: recordable sessions go to
  // the record page, finished ones to their detail view.
  const openSession = useCallback(
    (token: string) => {
      const session = gallery.sessions.find((s) => s.token === token);
      if (session && ["pending", "active", "paused"].includes(session.status)) {
        navigate({ page: "record", token });
      } else {
        navigate({ page: "session", token });
      }
    },
    [gallery.sessions, navigate],
  );

  const archiveSession = useCallback(
    async (token: string) => {
      const yes = await confirm("Are you sure you want to archive this session?", {
        title: "Archive Session",
        kind: "warning",
      });
      if (yes) {
        tokenStore.archiveToken(token);
        gallery.refresh();
      }
    },
    [tokenStore, gallery],
  );

  // Native right-click menu for a gallery card. Uses Tauri's menu plugin so the
  // popup is a real OS context menu rather than a DOM overlay.
  const handleSessionContextMenu = useCallback(
    async (token: string) => {
      const session = gallery.sessions.find((s) => s.token === token);
      const items: (MenuItem | PredefinedMenuItem)[] = [
        await MenuItem.new({ text: "Open", action: () => openSession(token) }),
      ];
      if (session && session.status === "complete") {
        items.push(
          await MenuItem.new({
            text: "Open in Editor",
            action: () => { void openEditorWindow(token); },
          }),
        );
      }
      items.push(await PredefinedMenuItem.new({ item: "Separator" }));
      items.push(
        await MenuItem.new({ text: "Archive", action: () => { void archiveSession(token); } }),
      );
      const menu = await Menu.new({ items });
      await menu.popup();
    },
    [gallery.sessions, openSession, archiveSession],
  );

  // Deep link handler -- saves token and navigates appropriately.
  // If currently recording another session, pauses it first.
  // Tracks the last processed URL to deduplicate retried cold-start emits.
  const lastDeepLink = React.useRef<string | null>(null);
  const handleDeepLinkUrls = useCallback(
    async (urls: string[]) => {
      console.log("[app] deep link received:", urls);
      for (const url of urls) {
        if (url === lastDeepLink.current) return; // already handled

        // Pairing callback (lookout://pair?code=…&state=…): finish the
        // device link, then start the session the user originally asked for.
        // Stale/forged callbacks match no pending pairing and are ignored.
        if (parsePairCallback(url)) {
          lastDeepLink.current = url;
          getCurrentWindow().setFocus().catch(() => {});
          try {
            const linked = await completePairing(url);
            if (linked) {
              if (!programsRef.current.some((p) => p.name === linked)) {
                await fetchPrograms();
              }
              const program = programsRef.current.find((p) => p.name === linked);
              // `openProgram` rather than `startViaLink`: the credential is in
              // hand, so it takes the instant path, and a program whose row
              // lost its `startUrl` while the user was consenting still opens
              // its site. Pairing and then getting nothing is the one outcome
              // worth ruling out - the user asked for a recording.
              if (program) {
                await openProgram(program);
              }
            }
          } catch (e) {
            console.error("[pair] pairing failed:", e);
            await message("Linking failed — you can retry from the + menu.", {
              title: "Lookout",
              kind: "error",
            }).catch(() => {});
          }
          return;
        }

        const token = extractToken(url);
        if (!token) continue;

        console.log(`[app] extracted token: ${token.slice(0, 8)}...`);
        lastDeepLink.current = url;
        tokenStore.addToken(token);

        // If we're currently recording a different session, pause it first
        if (route.page === "record" && route.token && route.token !== token) {
          console.log(`[app] deep link interrupting active session ${route.token.slice(0, 8)}...`);
          await pauseSession(route.token);
        }

        // Status decides where to go; program decides whether a tip targets
        // it. In parallel, so the tip lookup adds no latency to recording.
        const [status, program] = await Promise.all([
          fetchSessionStatus(token),
          fetchSessionProgram(token),
        ]);
        console.log(`[app] incoming session status: ${status} (program: ${program ?? "none"})`);
        // Before navigating, so the destination page sees the record.
        recordDeepLinkSession(program, token);

        if (status && ["stopped", "compiling", "complete", "failed"].includes(status)) {
          // Session is finished — go to detail view
          navigate({ page: "session", token });
        } else {
          // Session is recordable (pending/active/paused) or unknown — go to record
          navigate({ page: "record", token });
        }

        // Bring window to front
        getCurrentWindow().setFocus().catch(() => {});
        return;
      }
    },
    [tokenStore, navigate, route, fetchPrograms, openProgram],
  );
  // Ref so effects can call the latest version without depending on it
  const handleDeepLinkRef = React.useRef(handleDeepLinkUrls);
  handleDeepLinkRef.current = handleDeepLinkUrls;

  // Listen for deep links while app is running (warm start).
  // We use our custom "lookout-deep-link" event to avoid conflicts and infinite loops
  // with the tauri-plugin-deep-link internal event loops.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<string[]>("lookout-deep-link", (event) => {
      handleDeepLinkRef.current(event.payload);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => { if (unlisten) unlisten(); };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Poll for cold-start deep link URLs exactly once. The Rust side stashes
  // URLs from both get_current() (immediate) and on_open_url (delayed Apple
  // Event). We poll a few times to catch URLs that arrive after launch.
  const coldStartRan = React.useRef(false);
  useEffect(() => {
    if (coldStartRan.current) return;
    coldStartRan.current = true;

    let cancelled = false;
    const check = async () => {
      for (let i = 0; i < 10 && !cancelled; i++) {
        try {
          console.debug(`[app] cold-start poll attempt ${i + 1}/10`);
          const urls = await invoke<string[]>("get_cold_start_urls");
          if (urls.length > 0) {
            handleDeepLinkRef.current(urls);
            return;
          }
        } catch (e) {
          console.debug("[app] cold-start poll miss:", e);
        }
        await new Promise((r) => setTimeout(r, 500));
      }
      console.debug("[app] cold-start poll finished, no urls found");
    };
    check();
    return () => { cancelled = true; };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Handle ?token= query param (dev mode) — route through the same handler
  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const token = params.get("token");
    if (token && isValidToken(token)) {
      handleDeepLinkRef.current([`lookout://session/?token=${token}`]);
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Listen to Tauri theme changes (for Linux where prefers-color-scheme might fail)
  useEffect(() => {
    const updateTheme = (theme: "light" | "dark" | null) => {
      if (theme) {
        document.documentElement.setAttribute("data-theme", theme);
      } else {
        document.documentElement.removeAttribute("data-theme");
      }
    };

    if (isLinux) {
      // On Linux, Tauri's window.theme() can incorrectly report "light" and override the native GTK webview behavior.
      // WebKitGTK natively supports prefers-color-scheme (via xdg-desktop-portal / org.freedesktop.appearance).
      // So we just rely on standard browser matchMedia to get the universal native standard.
      const mediaQuery = window.matchMedia("(prefers-color-scheme: dark)");
      const getSystemTheme = () => mediaQuery.matches ? "dark" : "light";

      const applyTheme = () => {
        const theme = getSystemTheme();
        updateTheme(theme);
        // Force the Tauri window GTK decorations to match the media query since winit is confused
        getCurrentWindow().setTheme(theme).catch(() => {});
      };

      applyTheme();

      const listener = () => applyTheme();
      mediaQuery.addEventListener("change", listener);

      return () => mediaQuery.removeEventListener("change", listener);
    } else {
      getCurrentWindow().theme().then(updateTheme).catch(() => {});

      let unlisten: (() => void) | undefined;
      getCurrentWindow().onThemeChanged((event) => {
        updateTheme(event.payload);
      }).then((fn) => { unlisten = fn; }).catch(() => {});

      return () => { if (unlisten) unlisten(); };
    }
  }, []);

  // Linux only: overlay the Adwaita palette, adopt the session's accent and
  // UI font, and learn which edge the user keeps their window controls on.
  // Resolves to the defaults untouched on every other platform.
  // The main window is undecorated on Linux (lib.rs), so it owns its corners
  // and its header bar. Re-read when the window regains focus, so a trip to
  // GNOME Settings and back is reflected without a relaunch.
  const appearance = useDesktopAppearance({ undecorated: true });
  useBackdropState();
  // Fixed size hints don't stop a tiling WM sizing this window, so its frame
  // has to collapse under one just as the editor's does.
  useWindowFrameState();
  // Blur behind the window where the compositor offers it. A no-op — and an
  // opaque window — everywhere it doesn't.
  useBackgroundBlur();
  // Listen for native menu navigation events
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<string>("lookout-navigate", (event) => {
      if (event.payload === "/add") {
        navigate({ page: "add" });
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => { if (unlisten) unlisten(); };
  }, [navigate]);

  // Set window title with version. Not on Linux: the title is drawn in the
  // app's own header bar there, where the version rides along as the
  // subtitle on the screens that have nothing better to say.
  const [appVersion, setAppVersion] = useState<string | null>(null);
  useEffect(() => {
    getVersion()
      .then((v) => {
        setAppVersion(v);
        void getCurrentWindow().setTitle(isLinux ? "Lookout" : `Lookout v${v}`);
      })
      .catch(() => {});
  }, []);

  // Enable vibrancy globally for the app
  useEffect(() => {
    const html = document.documentElement;
    const body = document.body;
    const root = document.getElementById("root");
    const prevHtmlBg = html.style.background;
    const prevBodyBg = body.style.background;
    const prevRootBg = root?.style.background ?? "";

    let effectsApplied = false;

    if (!isLinux) {
      invoke("enable_vibrancy")
        .then(() => {
          effectsApplied = true;
          html.style.background = "transparent";
          body.style.background = "transparent";
          if (root) root.style.background = "transparent";
        })
        .catch((err) => {
          console.warn("Failed to enable vibrancy", err);
        });
    } else {
      console.log("[vibrancy] skipped on Linux");
      // Nothing to do: the window is undecorated here and linuxChrome.ts
      // paints #root (rounding its corners) while leaving html and body
      // transparent, which is what lets those corners read as corners
      // rather than as squares in the window colour.
    }

    return () => {
      if (effectsApplied) {
        invoke("disable_vibrancy").catch(() => {});
      }
      html.style.background = prevHtmlBg;
      body.style.background = prevBodyBg;
      if (root) root.style.background = prevRootBg;
    };
  }, []);

  // The two ways out of a subpage. Named so the header bar and the pages'
  // own controls invoke exactly the same thing rather than two hand-copied
  // versions that can drift.
  const toGallery = useCallback(() => navigate({ page: "gallery" }), [navigate]);
  const toGalleryRefreshed = useCallback(() => {
    gallery.refresh();
    navigate({ page: "gallery" });
  }, [gallery, navigate]);

  // Per-session program bits for the session view: an outstanding panel
  // request, and the program's own link for this session. One fetch, recomputed
  // when the route changes or a panel resolves.
  const [panelPrompt, setPanelPrompt] = useState<{
    token: string;
    url: string;
    fallbackUrl: string | null;
  } | null>(null);
  const [openIn, setOpenIn] = useState<{
    token: string;
    url: string;
    label: string;
  } | null>(null);
  // The owning program, straight from the session's own `program` id. Both the
  // name and the icon are authoritative this way, unlike matching a URL's origin
  // against the registry — a program's panel can sit on a different host from
  // the URLs it registered, and then origin matching finds nothing.
  const [sessionProgram, setSessionProgram] = useState<{
    token: string;
    label: string;
    iconUrl: string | null;
  } | null>(null);
  useEffect(() => {
    if (route.page !== "session" || !route.token) {
      setPanelPrompt(null);
      setOpenIn(null);
      setSessionProgram(null);
      return;
    }
    const token = route.token;
    let cancelled = false;
    (async () => {
      try {
        // The session record plus the desktop-only fields the shared type
        // doesn't declare (they ride along in the same JSON).
        const data = (await sessionClient(token).getSession()) as SessionResponse & {
          program?: string | null;
          editHoldUntil?: string | null;
          viewUrl?: unknown;
        };
        if (cancelled) return;
        const rawProgram: string | null = data.program ?? null;
        if (rawProgram) {
          const known = programsRef.current.find((p) => p.name === rawProgram);
          setSessionProgram({
            token,
            label: known?.displayName || rawProgram,
            iconUrl: known?.iconUrl ?? null,
          });
        }
        else {
          setSessionProgram(null);
        }

        // Only once the recording is actually over. A session still being
        // captured has nothing to publish yet, so asking for its title would
        // be nagging about work in progress — and a held one is on its way to
        // the editor, where the ask belongs after publishing.
        const finishedRecording = ["stopped", "compiling", "complete"].includes(data.status);
        const held = Boolean(data.editHoldUntil) && data.status !== "failed";

        const panelUrl = data.panelUrl;
        setPanelPrompt(
          finishedRecording &&
          !held &&
          typeof panelUrl === "string" &&
          shouldOfferPanel(token, panelUrl, data.panelResolved)
            ? { token, url: panelUrl, fallbackUrl: data.redirectUrl ?? null }
            : null,
        );
        // Only offered when the program actually published a link. The label
        // comes from the registry when it's known, so the button reads "Open
        // in Lapse" rather than "Open in lapse"; the raw name is the fallback
        // for a program that has left the registry.
        const viewUrl: unknown = data.viewUrl;
        if (typeof viewUrl === "string" && /^https?:\/\//i.test(viewUrl)) {
          const raw: string | null = data.program ?? null;
          const known = raw
            ? programsRef.current.find((p) => p.name === raw)
            : undefined;
          setOpenIn({
            token,
            url: viewUrl,
            label: known?.displayName || raw || "program",
          });
        } else {
          setOpenIn(null);
        }
      } catch {
        // Offline or transient — showing neither is the safe default; both
        // reappear next time the page is opened.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [route, panelNonce]);

  // Step 2: Route
  const content = (() => {
    // The editor owns the session while its window is open.
    if (editorWindowToken) {
      return <EditorOpenPlaceholder token={editorWindowToken} />;
    }
    switch (route.page) {
      case "gallery":
        return (
          <Gallery
            sessions={gallery.sessions}
            loading={gallery.loading}
            error={gallery.error}
            onSessionClick={openSession}
            onArchive={archiveSession}
            onSessionContextMenu={handleSessionContextMenu}
            onAdd={handleAdd}
            // Always available: the Server subpage works everywhere; only the
            // Filtered Apps subpage is Wayland-restricted (it shows a notice).
            onSettings={() => navigate({ page: "settings" })}
            banner={announcement ? <AnnouncementBanner announcement={announcement} /> : undefined}
            // On Linux the gallery's title and its gear/+ live in the header
            // bar instead, so stating them again here would be a second
            // title row directly under the first.
            showHeader={!isLinux}
            // The list ends against the window's own rounded edge and
            // border here, and a fade into that reads as the content
            // dissolving rather than running under chrome.
            showBottomFade={!isLinux}
          />
        );
      case "settings":
        return (
          <SettingsPage
            onBack={toGallery}
            isWayland={isWayland}
          />
        );
      case "add":
        return (
          <AddSessionPage
            onBack={toGallery}
            onOpenProgram={openProgram}
            onStart={(token) => {
              tokenStore.addToken(token);
              handleDeepLinkRef.current([`lookout://session/?token=${token}`]);
            }}
          />
        );
      case "record":
        return (
          <RecordPage
            key={route.token}
            token={route.token}
            onBack={toGalleryRefreshed}
            onViewSession={(token) => {
              tokenStore.addToken(token);
              navigate({ page: "session", token });
            }}
            onSourcePicker={handleSourcePicker}
          />
        );
      case "session":
        return (
          <SessionDetail
            key={`${route.token}:${editNonce}`}
            token={route.token}
            apiBaseUrl={API_BASE}
            client={detailClient ?? undefined}
            onEdit={() => { void openEditorWindow(route.token); }}
            onRecordingFinished={({ redirectUrl, panelUrl, panelResolved }) => {
              // The recording is over, so if the program wants something, ask
              // now rather than after the compile — the sheet sits over the
              // progress bar and the answers land while it builds.
              //
              // Fires on every visit to a finished session, not just the one
              // where it stopped; `maybeOpenPanel` is what makes it once-only,
              // by refusing anything the user has already dealt with.
              maybeOpenPanel(route.token, redirectUrl, panelUrl, panelResolved);
            }}
            onComplete={({ redirectUrl, panelUrl }) => {
              // Browser handoff for sessions with no panel. Shared de-dupe
              // with the post-edit watcher above, so a session seen finishing
              // by both paths only redirects once.
              handleCompleted(route.token, redirectUrl, panelUrl);
            }}
            titleAction={
              openIn && openIn.token === route.token ? (
                <OpenInProgram programLabel={openIn.label} url={openIn.url} />
              ) : undefined
            }
            belowVideo={
              panelPrompt && panel?.token !== panelPrompt.token && (
                <PanelPrompt
                  programLabel={
                    sessionProgram?.token === panelPrompt.token
                      ? sessionProgram.label
                      : programLabelForUrl(panelPrompt.url)
                  }
                  onOpen={() => openPanel(panelPrompt.token, panelPrompt.url, panelPrompt.fallbackUrl)}
                />
              )
            }
            onBack={toGalleryRefreshed}
            showBack={!isLinux}
            onArchive={async () => {
              const yes = await confirm("Are you sure you want to archive this session?", { title: "Archive Session", kind: "warning" });
              if (yes) {
                tokenStore.archiveToken(route.token);
                gallery.refresh();
                navigate({ page: "gallery" });
              }
            }}
          />
        );
      default:
        return null;
    }
  })();

  // Sequential, independent permission gates — not else-if.
  // key= forces React to mount a fresh instance for each type (resets `requested` state).
  const mainView = !screenPermGranted ? (
    <PermissionScreen key="screen" type="screen" onGranted={() => setScreenPermGranted(true)} />
  ) : !cameraPermGranted ? (
    <PermissionScreen key="camera" type="camera" onGranted={() => setCameraPermGranted(true)} />
  ) : (
    content
  );

  // GNOME names the window after what's in it, not after the app, so the
  // header bar carries the page rather than "Lookout". The subtitle is
  // optional and only earns its line where there's something worth saying.
  // What the mounted page has published for the header, if anything.
  //
  // Stamped with two things. The publisher's id, so an outgoing page's
  // unmount can't wipe the header the incoming one just set. And the route
  // it was published for, because the outgoing page stays mounted for the
  // length of the exit animation — without the stamp its back button would
  // sit in the header for a few hundred ms after the title had already
  // moved on, which is exactly as broken as it looks.
  const navRouteKey = `${route.page}:${(route as { token?: string }).token ?? ""}`;
  const navRouteKeyRef = React.useRef(navRouteKey);
  navRouteKeyRef.current = navRouteKey;

  const [pageNav, setPageNav] = useState<
    { owner: string; nav: HeaderNav; routeKey: string } | null
  >(null);
  const publishHeaderNav = useCallback((owner: string, nav: HeaderNav | null) => {
    setPageNav((prev) => {
      if (nav) return { owner, nav, routeKey: navRouteKeyRef.current };
      return prev && prev.owner !== owner ? prev : null;
    });
  }, []);

  // A contribution only counts while its own route is the current one. The
  // route's own defaults cover the gap until the incoming page publishes,
  // so nothing flickers on the way in either.
  const activeNav = pageNav && pageNav.routeKey === navRouteKey ? pageNav.nav : null;

  const routeHeader = ((): { title: string; subtitle?: string; onBack?: () => void } => {
    // "Lookout" on its own is a bare word in a bare bar — the version is
    // the one thing worth saying underneath it, and it lost its old home in
    // the window title when the header bar took over.
    const appTitle = { title: "Lookout", subtitle: appVersion ? `v${appVersion}` : undefined };
    if (!screenPermGranted || !cameraPermGranted) return appTitle;
    if (editorWindowToken) return { title: "Lookout", subtitle: "Editing in another window" };
    switch (route.page) {
      // The home screen is the app itself, so it takes the app's name and
      // nothing else — a running count under it is noise, not information.
      case "gallery": return { title: "Lookout" };
      case "settings": return { title: "Settings", onBack: toGallery };
      case "add": return { title: "New Timelapse", onBack: toGallery };
      case "record": return { title: "Recording", onBack: toGalleryRefreshed };
      case "session": return { title: "Timelapse", onBack: toGalleryRefreshed };
      default: return appTitle;
    }
  })();

  // A page's own contribution wins over the route default — Settings uses
  // this so its subpages send you back to the settings menu rather than all
  // the way out to the gallery.
  const header = {
    title: activeNav?.title ?? routeHeader.title,
    subtitle: activeNav?.title ? activeNav.subtitle : (activeNav?.subtitle ?? routeHeader.subtitle),
    onBack: activeNav?.onBack ?? routeHeader.onBack,
  };

  // The gallery is the app's home, and the only place chrome that talks
  // about the app as a whole — its actions, its update state — belongs. A
  // permission gate or an open editor isn't it, even at the gallery route.
  const onMainPage =
    route.page === "gallery" && screenPermGranted && cameraPermGranted && !editorWindowToken;

  const galleryActions = onMainPage ? (
      <>
        <button
          type="button"
          className="lookout-headerbar-action"
          onClick={() => navigate({ page: "settings" })}
          title="Settings"
          aria-label="Settings"
        >
          <GearSixIcon size={15} weight="fill" aria-hidden="true" />
        </button>
        <button
          type="button"
          className="lookout-headerbar-action"
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            void handleAdd({ x: r.left, y: r.top, width: r.width, height: r.height });
          }}
          title="Start"
          aria-label="Start"
        >
          <PlusIcon size={17} weight="bold" aria-hidden="true" />
        </button>
      </>
    ) : undefined;

  // The pill belongs in the titlebar — that's what it was built for, and on
  // Linux the header bar is the titlebar. It rides ahead of the page actions
  // so it doesn't push the close button around as it changes width.
  const headerActions = (
    <>
      {onMainPage && <UpdatePill phase={appUpdate.phase} onRestart={appUpdate.restart} />}
      {galleryActions}
    </>
  );

  const prevRouteRef = React.useRef(route);
  const routeDirection = React.useMemo(() => {
    const prev = prevRouteRef.current;
    const prevPage = prev.page;
    const nextPage = route.page;

    if (prevPage === "gallery" && nextPage !== "gallery") return 1;
    if (prevPage !== "gallery" && nextPage === "gallery") return -1;
    if (prevPage === "record" && nextPage === "session") return 1;
    if (prevPage === "session" && nextPage === "record") return -1;
    return 1;
  }, [route]);

  useEffect(() => {
    prevRouteRef.current = route;
    const token = (route as { token?: string }).token;
    Sentry.setTag("session_token", token ?? null);
  }, [route]);

  // The editor placeholder participates in the route transition, so the
  // main window slides to it and back instead of hard-cutting.
  const routeKey = editorWindowToken
    ? `editor-open:${editorWindowToken}`
    : `${route.page}:${(route as { token?: string }).token ?? ""}`;

  return (
    <HeaderNavProvider publish={publishHeaderNav}>
    <div style={{ display: "flex", flexDirection: "column", height: "100%", position: "relative" }}>
      {/* Draggable Titlebar Area that dodges the traffic lights (macOS only).
          The update pill lives here, Ghostty-style — the titlebar is a
          transparent webview overlay, so it renders inside the real titlebar. */}
      {isLinux && (
        <HeaderBar
          title={header.title}
          subtitle={header.subtitle}
          appearance={appearance}
          actions={headerActions}
          onBack={header.onBack}
        />
      )}
      {isMacOS ? (
        <div
          data-tauri-drag-region
          className="titlebar"
          style={{ height: 32, flexShrink: 0, width: "100%", zIndex: 9999, background: "transparent", cursor: "default", display: "flex", alignItems: "center", justifyContent: "flex-end", paddingRight: 6, boxSizing: "border-box" }}
        >
          {onMainPage && <UpdatePill phase={appUpdate.phase} onRestart={appUpdate.restart} />}
        </div>
      ) : isLinux ? null : (
        /* Windows has no overlay titlebar to put it in — float it instead.
           Linux used to land here too, but its pill now lives in the header
           bar above, which is a real titlebar. */
        <div style={{ position: "absolute", bottom: 8, left: 8, zIndex: 9999 }}>
          {onMainPage && <UpdatePill phase={appUpdate.phase} onRestart={appUpdate.restart} origin="bottom" />}
        </div>
      )}
      {/* Windows/Linux + menu. Rendered here, outside the route transition's
          transformed wrapper, so position:fixed anchors to the viewport. */}
      <AnimatePresence>
        {addMenu && (
          <AddMenuPopup
            items={addMenu.items}
            anchor={addMenu.anchor}
            onSelect={(choice) => {
              setAddMenu(null);
              void handleMenuChoice(choice);
            }}
          />
        )}
      </AnimatePresence>
      {/* Program panel: the session owner's own page, in a sheet, instead of
          bouncing the user to a browser tab. */}
      {panel && (
        <ProgramPanel
          key={panel.token}
          url={panel.url}
          programLabel={
            sessionProgram?.token === panel.token
              ? sessionProgram.label
              : programLabelForUrl(panel.url)
          }
          programIconUrl={
            (sessionProgram?.token === panel.token ? sessionProgram.iconUrl : null) ??
            programIconForUrl(panel.url)
          }
          fallbackUrl={panel.fallbackUrl}
          onDone={() => {
            setPanelState(panel.token, "done");
            setPanel(null);
            setPanelNonce((n) => n + 1);
            gallery.refresh();
            // Second moment: they just did by hand what the tip offers to
            // make one click.
            openTip("panel-done", panel.token);
          }}
          onDismiss={() => {
            // Closing is always free. The ask persists as a card on the
            // session page rather than vanishing silently.
            setPanelState(panel.token, "dismissed");
            setPanel(null);
            setPanelNonce((n) => n + 1);
          }}
        />
      )}

      {/* Mounted whenever a tip is published, so vaul can animate the exit;
          opens only when a moment says so. */}
      <TipDrawer tip={tipOverride ?? tip} open={tipOpen} onClose={() => setTipOpen(false)} />

      {/* Opens itself once per release, only on the main page and never over
          the tip sheet or during a recording. */}
      <UpdateDrawer
        update={linuxUpdate}
        open={Boolean(linuxUpdate) && onMainPage && !tipOpen}
        onClose={dismissLinuxUpdate}
      />

      {/* Instant-start progress: shows while a linked program mints the
          session. Fixed to the viewport, outside the route transition. */}
      <AnimatePresence>
        {startingProgram && (
          <motion.div
            initial={{ opacity: 0, y: 8 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0, y: 8, transition: { duration: 0.12 } }}
            style={{
              position: "fixed",
              bottom: 20,
              left: 0,
              right: 0,
              display: "flex",
              justifyContent: "center",
              zIndex: 10001,
              pointerEvents: "none",
            }}
          >
            <div
              style={{
                display: "flex",
                alignItems: "center",
                gap: 8,
                padding: "8px 14px",
                borderRadius: radii.lg,
                border: `1px solid ${colors.border.default}`,
                background: colors.bg.panel,
                color: colors.text.secondary,
                fontSize: fontSize.sm,
                boxShadow: "0 4px 18px rgba(0, 0, 0, 0.18)",
              }}
            >
              <Spinner size="sm" />
              <span>Starting {startingProgram}…</span>
            </div>
          </motion.div>
        )}
      </AnimatePresence>
      <div style={{
        flex: 1,
        overflow: "hidden",
        display: "flex",
        flexDirection: "column",
        position: "relative",
      }}>
        <AnimatePresence mode="sync" initial={false} custom={routeDirection}>
          <motion.div
            key={routeKey}
            custom={routeDirection}
            initial="enter"
            animate="center"
            exit="exit"
            variants={{
              enter: (direction: number) => ({ opacity: 0, x: direction > 0 ? 14 : -14 }),
              center: {
                opacity: 1,
                x: 0,
                transition: {
                  x: { type: "spring", stiffness: 460, damping: 36, mass: 0.7 },
                  opacity: { duration: 0.16, delay: 0.04, ease: "easeOut" },
                },
              },
              exit: (direction: number) => ({
                opacity: 0,
                x: direction > 0 ? -14 : 14,
                transition: {
                  x: { type: "spring", stiffness: 460, damping: 36, mass: 0.7 },
                  opacity: { duration: 0.14, ease: "easeOut" },
                },
              }),
            }}
            style={{
              position: "absolute",
              inset: 0,
              height: "100%",
              overflowY: (route.page === "gallery") ? "hidden" : "auto",
            }}
          >
            {mainView}
          </motion.div>
        </AnimatePresence>
      </div>
    </div>
    </HeaderNavProvider>
  );
}
