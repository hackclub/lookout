import * as Sentry from "@sentry/react";
import React, { useState, useEffect, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import { confirm } from "@tauri-apps/plugin-dialog";
import { invoke } from "./logger.js";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { AnimatePresence, motion } from "motion/react";
import {
  Gallery,
  SessionDetail,
  useTokenStore,
  useGallery,
  useHashRouter,
} from "@lookout/react";
import { getVersion } from "@tauri-apps/api/app";
import { isValidToken, extractToken } from "./utils.js";
import { PermissionScreen } from "./components/PermissionScreen.js";
import { RecordPage } from "./components/RecordPage.js";
import { AddSessionPage } from "./components/AddSessionPage.js";
import { SettingsPage } from "./components/SettingsPage.js";
import { TrayApp } from "./components/TrayApp.js";
import { useBlacklistedApps } from "./hooks/useBlacklistedApps.js";
import { useUpdateCheck } from "./hooks/useUpdateCheck.js";
import { ensureNotificationPermission } from "./hooks/useSessionNotifications.js";

const API_BASE = "https://lookout.hackclub.com";

/** Pause a session by token. Fire-and-forget, logs errors. */
async function pauseSession(token: string): Promise<void> {
  try {
    console.log(`[app] pausing session ${token.slice(0, 8)}...`);
    await fetch(`${API_BASE}/api/sessions/${token}/pause`, { method: "POST" });
    console.log(`[app] paused session ${token.slice(0, 8)}`);
  } catch (e) {
    console.error(`[app] failed to pause session ${token.slice(0, 8)}:`, e);
  }
}

/** Fetch a session's status. Returns null on error. */
async function fetchSessionStatus(token: string): Promise<string | null> {
  try {
    const res = await fetch(`${API_BASE}/api/sessions/${token}/status`);
    if (!res.ok) return null;
    const data = await res.json();
    return data.status ?? null;
  } catch {
    return null;
  }
}

export function App() {
  const isTray = window.location.hash.includes("tray");
  if (isTray) {
    return <TrayApp />;
  }
  return <MainWindowApp />;
}

function MainWindowApp() {
  const isMacOS = navigator.userAgent.includes("Mac");
  const [screenPermGranted, setScreenPermGranted] = useState(!isMacOS);
  const [cameraPermGranted, setCameraPermGranted] = useState(!isMacOS);
  const [isWayland, setIsWayland] = useState(false);
  const { route, navigate } = useHashRouter();
  const tokenStore = useTokenStore();
  const updateStatus = useUpdateCheck();
  const gallery = useGallery({
    apiBaseUrl: API_BASE,
    tokens: tokenStore.getAllTokenValues(),
  });

  // Initialize blacklisted apps sync from localStorage to Rust backend
  useBlacklistedApps();

  // Request notification permission as soon as the app is past the update
  // gate — so the OS prompt appears at launch, not deferred to recording start.
  const updateSettled = updateStatus.state === "idle";
  useEffect(() => {
    if (updateSettled) void ensureNotificationPermission();
  }, [updateSettled]);

  // Detect Wayland — filter apps feature is unsupported there
  useEffect(() => {
    invoke<boolean>("is_wayland").then(setIsWayland).catch(() => {});
  }, []);

  // Deep link handler -- saves token and navigates appropriately.
  // If currently recording another session, pauses it first.
  // Tracks the last processed URL to deduplicate retried cold-start emits.
  const lastDeepLink = React.useRef<string | null>(null);
  const handleDeepLinkUrls = useCallback(
    async (urls: string[]) => {
      console.log("[app] deep link received:", urls);
      for (const url of urls) {
        if (url === lastDeepLink.current) return; // already handled
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

        // Check the incoming session's status to decide where to go
        const status = await fetchSessionStatus(token);
        console.log(`[app] incoming session status: ${status}`);

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
    [tokenStore, navigate, route],
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

    const isLinux = navigator.userAgent.toLowerCase().includes("linux");

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

  // Set window title with version
  useEffect(() => {
    getVersion().then((v) => {
      getCurrentWindow().setTitle(`Lookout v${v}`);
    }).catch(() => {});
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
    
    const isLinux = navigator.userAgent.toLowerCase().includes("linux");
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
      // Explicitly set opaque background on Linux to override any default transparent styling
      html.style.background = "var(--color-bg-body)";
      body.style.background = "var(--color-bg-body)";
      if (root) root.style.background = "var(--color-bg-body)";
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

  // Step 2: Route
  const content = (() => {
    switch (route.page) {
      case "gallery":
        return (
          <Gallery
            sessions={gallery.sessions}
            loading={gallery.loading}
            error={gallery.error}
            onSessionClick={(token) => {
              const session = gallery.sessions.find((s) => s.token === token);
              if (session && ["pending", "active", "paused"].includes(session.status)) {
                navigate({ page: "record", token });
              } else {
                navigate({ page: "session", token });
              }
            }}
            onArchive={async (token) => {
              const yes = await confirm("Are you sure you want to archive this session?", { title: "Archive Session", kind: "warning" });
              if (yes) {
                tokenStore.archiveToken(token);
                gallery.refresh();
              }
            }}
            onAdd={() => navigate({ page: "add" })}
            onSettings={isWayland ? undefined : () => navigate({ page: "settings" })}
          />
        );
      case "settings":
        return (
          <SettingsPage
            onBack={() => navigate({ page: "gallery" })}
            isWayland={isWayland}
          />
        );
      case "add":
        return (
          <AddSessionPage
            onBack={() => navigate({ page: "gallery" })}
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
            onBack={() => {
              gallery.refresh();
              navigate({ page: "gallery" });
            }}
            onViewSession={(token) => {
              tokenStore.addToken(token);
              navigate({ page: "session", token });
            }}
          />
        );
      case "session":
        return (
          <SessionDetail
            key={route.token}
            token={route.token}
            apiBaseUrl={API_BASE}
            onBack={() => {
              gallery.refresh();
              navigate({ page: "gallery" });
            }}
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

  const routeKey = `${route.page}:${(route as { token?: string }).token ?? ""}`;

  if (updateStatus.state !== "idle") {
    return (
      <div style={{
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        height: "100vh",
        gap: 12,
        fontFamily: "inherit",
      }}>
        {isMacOS && (
          <div
            data-tauri-drag-region
            style={{ position: "absolute", top: 0, left: 0, right: 0, height: 32, background: "transparent", cursor: "default" }}
          />
        )}
        <div style={{ fontSize: 14, opacity: 0.7 }}>
          {updateStatus.state === "checking" && "Checking for updates…"}
          {updateStatus.state === "no-update" && updateStatus.message}
          {updateStatus.state === "downloading" && `Updating… ${updateStatus.progress}%`}
          {updateStatus.state === "installing" && "Installing update…"}
          {updateStatus.state === "done" && "Restarting…"}
        </div>
      </div>
    );
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100vh" }}>
      {/* Draggable Titlebar Area that dodges the traffic lights (macOS only) */}
      {isMacOS && (
        <div
          data-tauri-drag-region
          className="titlebar"
          style={{ height: 32, flexShrink: 0, width: "100%", zIndex: 9999, background: "transparent", cursor: "default" }}
        />
      )}
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
  );
}
