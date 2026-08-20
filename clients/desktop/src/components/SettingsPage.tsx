import { useState, useEffect, useRef, type ReactNode, type CSSProperties } from "react";
import { AnimatePresence, motion } from "motion/react";
import {
  CaretLeftIcon,
  CaretRightIcon,
  CheckIcon,
  FunnelIcon,
  WrenchIcon,
} from "@phosphor-icons/react";
import { confirm } from "@tauri-apps/plugin-dialog";
import {
  Button,
  colors,
  spacing,
  fontSize,
  fontWeight,
  radii,
} from "@lookout/react";
import { invoke } from "../logger.js";
import { cardButtonStyle } from "./PageLayout.js";
import { isLinux } from "../platform.js";
import { usePublishHeaderNav } from "../headerNav.js";
import { useBlacklistedApps } from "../hooks/useBlacklistedApps.js";
import {
  DEFAULT_API_BASE,
  getApiBase,
  isDefaultApiBase,
  normalizeServerUrl,
  setApiBase,
} from "../serverConfig.js";

interface SettingsPageProps {
  onBack: () => void;
  isWayland?: boolean;
}

type SettingsSubpage = "menu" | "filtered-apps" | "advanced";

interface AppEntry {
  name: string;
  /** Bundle path used to look up the app's icon (macOS only). */
  path?: string | null;
  /** Whether the app is currently running (open apps sort first). */
  running?: boolean;
}

/**
 * 20px app icon with a plain-box placeholder. Fades in only when the icon
 * arrives *after* mount (initial load) — rows remounting with a cached icon
 * (e.g. while typing in search) render it instantly with no animation.
 */
function AppIcon({ icon }: { icon: string | undefined }) {
  const fadeInRef = useRef(icon === undefined);
  if (icon) {
    return (
      <img
        src={`data:image/png;base64,${icon}`}
        alt=""
        width={20}
        height={20}
        draggable={false}
        style={{
          display: "block",
          objectFit: "contain",
          animation: fadeInRef.current
            ? "lookout-icon-fade-in 0.2s ease-out"
            : undefined,
        }}
      />
    );
  }
  return (
    <div
      style={{
        width: 20,
        height: 20,
        borderRadius: radii.sm,
        background: colors.bg.selected,
      }}
    />
  );
}

/** Shared page scaffold: back button + title + description. */
function PageChrome({
  title,
  description,
  onBack,
  children,
}: {
  title: string;
  description: ReactNode;
  onBack: () => void;
  children: ReactNode;
}) {
  // The header bar takes the heading and the back action on Linux. This is
  // also how the settings subpages get a back button that returns to the
  // settings menu rather than all the way out to the gallery — App only
  // knows the route's default, and this is the page correcting it.
  usePublishHeaderNav({ title, onBack });

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        height: "100%",
        maxWidth: 480,
        margin: "0 auto",
        padding: spacing.lg,
        boxSizing: "border-box",
      }}
    >
      {/* Back button. Absent on Linux: it lives in the header bar there. */}
      {!isLinux && (
      <div style={{ flexShrink: 0, marginBottom: spacing.lg }}>
        <Button variant="secondary" size="sm" onClick={onBack} style={cardButtonStyle}>
          {navigator.userAgent.includes("Mac") ? (
            <span>&larr; Back</span>
          ) : (
            <span style={{ display: "inline-flex", alignItems: "center", gap: spacing.xs }}>
              <CaretLeftIcon size={14} weight="bold" aria-hidden="true" />
              <span>Back</span>
            </span>
          )}
        </Button>
      </div>
      )}

      {/* Header. The heading is the header bar's job on Linux; the
          description stays, as the page's opening line. */}
      <div style={{ flexShrink: 0, marginBottom: spacing.lg }}>
        {!isLinux && (
        <h2
          style={{
            fontSize: fontSize.heading,
            fontWeight: fontWeight.bold,
            color: colors.text.primary,
            margin: 0,
            marginBottom: spacing.xs,
          }}
        >
          {title}
        </h2>
        )}
        <p
          style={{
            fontSize: fontSize.sm,
            color: colors.text.secondary,
            margin: 0,
            lineHeight: 1.5,
          }}
        >
          {description}
        </p>
      </div>

      {children}
    </div>
  );
}

/** A tappable settings-menu row: icon, title, description, chevron. */
function MenuRow({
  icon,
  title,
  description,
  onClick,
}: {
  icon: ReactNode;
  title: string;
  description: ReactNode;
  onClick: () => void;
}) {
  return (
    <button
      className="lookout-app-row"
      onClick={onClick}
      style={{
        position: "relative",
        display: "flex",
        alignItems: "center",
        gap: spacing.md,
        width: "100%",
        padding: `${spacing.md}px ${spacing.md}px`,
        background: "transparent",
        border: "none",
        borderRadius: radii.md,
        cursor: "pointer",
        textAlign: "left",
        color: colors.text.primary,
      }}
    >
      <div className="lookout-app-row-bg" style={{ borderRadius: radii.md }} />
      <div
        style={{
          position: "relative",
          zIndex: 1,
          width: 32,
          height: 32,
          borderRadius: radii.md,
          background: colors.bg.selected,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          flexShrink: 0,
          color: colors.text.secondary,
        }}
      >
        {icon}
      </div>
      <div style={{ flex: 1, minWidth: 0, position: "relative", zIndex: 1 }}>
        <div style={{ fontSize: fontSize.md, fontWeight: fontWeight.medium }}>{title}</div>
        <div
          style={{
            fontSize: fontSize.sm,
            color: colors.text.tertiary,
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {description}
        </div>
      </div>
      <CaretRightIcon
        size={16}
        weight="bold"
        aria-hidden="true"
        style={{ position: "relative", zIndex: 1, color: colors.text.tertiary, flexShrink: 0 }}
      />
    </button>
  );
}

export function SettingsPage({ onBack, isWayland }: SettingsPageProps) {
  const [subpage, setSubpage] = useState<SettingsSubpage>("menu");
  // Single hook instance shared with the Filtered Apps subpage so the menu
  // row's count stays live as apps are toggled.
  const { blacklistedApps, toggleApp } = useBlacklistedApps();

  const backToMenu = () => setSubpage("menu");

  const content = (() => {
    switch (subpage) {
      case "filtered-apps":
        return (
          <FilteredAppsSettings
            onBack={backToMenu}
            isWayland={isWayland}
            blacklistedApps={blacklistedApps}
            toggleApp={toggleApp}
          />
        );
      case "advanced":
        return <AdvancedSettings onBack={backToMenu} />;
      default:
        return (
          <PageChrome
            title="Settings"
            description="Configure Lookout."
            onBack={onBack}
          >
            {/* Row hover/press styles shared with the app list */}
            <style>{`
              .lookout-app-row-bg {
                position: absolute;
                inset: 0;
                background: transparent;
                transition: background 0.12s ease-out, transform 0.12s ease-out;
              }
              .lookout-app-row:hover .lookout-app-row-bg { background: ${colors.bg.selected}; }
              .lookout-app-row:active .lookout-app-row-bg { transform: scale(0.96); background: ${colors.bg.selected}; }
            `}</style>
            <div
              style={{
                borderRadius: radii.lg,
                border: `1px solid ${colors.border.default}`,
                background: colors.bg.surface,
                padding: spacing.xs,
              }}
            >
              <MenuRow
                icon={<FunnelIcon size={18} weight="fill" aria-hidden="true" />}
                title="Filtered Apps"
                description={
                  isWayland
                    ? "Not supported on Wayland"
                    : blacklistedApps.length > 0
                      ? `${blacklistedApps.length} app${blacklistedApps.length !== 1 ? "s" : ""} filtered`
                      : "Black out selected apps in captures"
                }
                onClick={() => setSubpage("filtered-apps")}
              />
              <MenuRow
                icon={<WrenchIcon size={18} weight="fill" aria-hidden="true" />}
                title="Advanced"
                description={
                  isDefaultApiBase()
                    ? "Developer options"
                    : // Make an active server override impossible to miss from
                      // the menu — it changes where every recording goes.
                      `Custom server: ${getApiBase().replace(/^https?:\/\//, "")}`
                }
                onClick={() => setSubpage("advanced")}
              />
            </div>
          </PageChrome>
        );
    }
  })();

  // Subpage transition — same directional slide as the app's route
  // transitions in App.tsx: drilling into a subpage slides forward,
  // returning to the menu slides back, and AnimatePresence keeps the
  // outgoing page mounted so it animates out instead of vanishing.
  // With only two levels, direction derives from the destination.
  const direction = subpage === "menu" ? -1 : 1;

  return (
    <div style={{ position: "relative", height: "100%", overflow: "hidden" }}>
      {/* initial={false}: on first mount the route-level transition in
          App.tsx already animates the whole page in — don't double up. */}
      <AnimatePresence mode="sync" initial={false} custom={direction}>
        <motion.div
          key={subpage}
          custom={direction}
          initial="enter"
          animate="center"
          exit="exit"
          variants={{
            enter: (d: number) => ({ opacity: 0, x: d > 0 ? 14 : -14 }),
            center: {
              opacity: 1,
              x: 0,
              transition: {
                x: { type: "spring", stiffness: 460, damping: 36, mass: 0.7 },
                opacity: { duration: 0.16, delay: 0.04, ease: "easeOut" },
              },
            },
            exit: (d: number) => ({
              opacity: 0,
              x: d > 0 ? -14 : 14,
              transition: {
                x: { type: "spring", stiffness: 460, damping: 36, mass: 0.7 },
                opacity: { duration: 0.14, ease: "easeOut" },
              },
            }),
          }}
          style={{ position: "absolute", inset: 0, overflowY: "auto" }}
        >
          {content}
        </motion.div>
      </AnimatePresence>
    </div>
  );
}

// ── Advanced subpage ────────────────────────────────────────

function AdvancedSettings({ onBack }: { onBack: () => void }) {
  const current = getApiBase();
  const [value, setValue] = useState(current);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const normalized = normalizeServerUrl(value);
  const isDirty = normalized !== current;

  /** Confirm → probe the server → persist → reload the webview so every
   *  module-scope API_BASE read picks up the new value. */
  const save = async (target: string | null) => {
    setError(null);
    if (target !== null && target !== DEFAULT_API_BASE) {
      // Native dialog as a final speed bump — a wrong server means every
      // recording from here on lands somewhere else.
      const yes = await confirm(
        `Point this app at ${target}?\n\nAll new recordings will upload there instead of the official Lookout server. Only continue if someone from Hack Club asked you to.`,
        { title: "Switch Lookout server", kind: "warning" },
      );
      if (!yes) return;
    }
    setSaving(true);
    try {
      if (target !== null) {
        // Probe a cheap public endpoint so a typo'd host fails here, not
        // silently during the next recording.
        const controller = new AbortController();
        const timer = setTimeout(() => controller.abort(), 6_000);
        try {
          const res = await fetch(`${target}/api/programs`, {
            signal: controller.signal,
          });
          if (!res.ok) {
            throw new Error(`server responded ${res.status}`);
          }
        } finally {
          clearTimeout(timer);
        }
      }
      setApiBase(target);
      window.location.reload();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(
        msg.includes("abort")
          ? "Could not reach the server (timed out)."
          : `Could not reach the server: ${msg}`,
      );
      setSaving(false);
    }
  };

  const inputStyle: CSSProperties = {
    width: "100%",
    padding: `${spacing.sm}px ${spacing.md}px`,
    fontSize: fontSize.md,
    color: colors.text.primary,
    background: colors.bg.surface,
    border: `1px solid ${error ? colors.status.danger : colors.border.default}`,
    borderRadius: radii.md,
    outline: "none",
    boxSizing: "border-box",
    fontFamily: "monospace",
  };

  return (
    <PageChrome
      title="Advanced"
      description="Options for testing and development. Please don't touch anything if you don't know what you're doing."
      onBack={onBack}
    >
      <div
        style={{
          borderRadius: radii.lg,
          border: `1px solid ${colors.border.default}`,
          background: colors.bg.surface,
          padding: spacing.lg,
          display: "flex",
          flexDirection: "column",
          gap: spacing.md,
        }}
      >
        <div>
          <div
            style={{
              fontSize: fontSize.sm,
              fontWeight: fontWeight.medium,
              color: colors.text.secondary,
              marginBottom: spacing.xs,
            }}
          >
            Lookout server
          </div>
          <input
            type="text"
            value={value}
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
            placeholder={DEFAULT_API_BASE}
            onChange={(e) => {
              setValue(e.target.value);
              setError(null);
            }}
            style={inputStyle}
            onFocus={(e) => {
              e.currentTarget.style.borderColor = colors.border.hover;
            }}
            onBlur={(e) => {
              e.currentTarget.style.borderColor = error
                ? colors.status.danger
                : colors.border.default;
            }}
          />
          <div
            style={{
              fontSize: fontSize.sm,
              color: colors.text.tertiary,
              marginTop: spacing.xs,
              lineHeight: 1.5,
            }}
          >
            Please enter a VALID lookout service URL.
          </div>
        </div>

        {value.trim() !== "" && normalized === null && (
          <div style={{ fontSize: fontSize.sm, color: colors.status.danger }}>
            Enter a valid HTTPS URL (e.g. https://lookout-stage.example.com).
          </div>
        )}
        {error && (
          <div style={{ fontSize: fontSize.sm, color: colors.status.danger }}>
            {error}
          </div>
        )}

        <div style={{ display: "flex", gap: spacing.sm }}>
          <Button
            variant="primary"
            size="sm"
            disabled={!isDirty || normalized === null || saving}
            onClick={() => normalized && save(normalized)}
          >
            {saving ? "Checking…" : "Save"}
          </Button>
          {current !== DEFAULT_API_BASE && (
            <Button
              variant="secondary"
              size="sm"
              disabled={saving}
              onClick={() => save(null)}
            >
              Reset to default
            </Button>
          )}
        </div>
      </div>

      {current !== DEFAULT_API_BASE && (
        <div
          style={{
            marginTop: spacing.md,
            padding: `${spacing.sm}px ${spacing.md}px`,
            borderRadius: radii.md,
            border: `1px solid ${colors.status.warning}`,
            background: "transparent",
            color: colors.status.warning,
            fontSize: fontSize.sm,
            lineHeight: 1.5,
          }}
        >
          Using a custom server. Timelapses recorded here won't appear on the
          default Lookout server.
        </div>
      )}
    </PageChrome>
  );
}

// ── Filtered Apps subpage ───────────────────────────────────

function FilteredAppsSettings({
  onBack,
  isWayland,
  blacklistedApps,
  toggleApp,
}: {
  onBack: () => void;
  isWayland?: boolean;
  blacklistedApps: string[];
  toggleApp: (appName: string) => void;
}) {
  const [apps, setApps] = useState<AppEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [searchQuery, setSearchQuery] = useState("");
  // App icons as base64 PNG keyed by app name, fetched once per app.
  // Requested names live in a ref so re-renders never refetch.
  const [appIcons, setAppIcons] = useState<Record<string, string>>({});
  const requestedIconsRef = useRef(new Set<string>());
  // Apps that were already filtered when the page OPENED get pinned to the
  // top. Snapshot in a ref, not live state: unchecking (or re-checking) an
  // app must not reshuffle rows under the user's cursor mid-visit — the new
  // order applies on the next visit.
  const pinnedAppsRef = useRef<Set<string>>(new Set(blacklistedApps));
  const pinnedSet = pinnedAppsRef.current;

  // The installed-app list is static while the page is open — fetch once.
  useEffect(() => {
    let cancelled = false;
    invoke<AppEntry[]>("list_installed_apps")
      .then((list) => {
        if (!cancelled) setApps(list);
      })
      .catch((e) => console.warn("[settings] failed to list apps:", e))
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Merge installed apps with already-blacklisted ones (which may have been
  // uninstalled since); those keep the letter-tile fallback icon.
  const byName = new Set(apps.map((a) => a.name));
  const allApps: AppEntry[] = [
    ...apps,
    ...blacklistedApps.filter((n) => !byName.has(n)).map((n) => ({ name: n })),
  ].sort((a, b) => a.name.localeCompare(b.name));

  // Load icons lazily: a shared IntersectionObserver requests an icon only
  // when its row scrolls near the viewport, instead of hitting the backend
  // for every installed app at once. Each fetch is cached on both sides.
  const observerRef = useRef<IntersectionObserver | null>(null);
  const observedAppsRef = useRef(new Map<Element, AppEntry>());

  useEffect(() => {
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (!entry.isIntersecting) continue;
          const app = observedAppsRef.current.get(entry.target);
          observer.unobserve(entry.target);
          observedAppsRef.current.delete(entry.target);
          if (!app?.path || requestedIconsRef.current.has(app.name)) continue;
          requestedIconsRef.current.add(app.name);
          invoke<string | null>("get_app_icon", { path: app.path })
            .then((icon) => {
              if (icon) setAppIcons((prev) => ({ ...prev, [app.name]: icon }));
            })
            .catch(() => {});
        }
      },
      // Start fetching slightly before a row becomes visible so icons are
      // usually there by the time it scrolls in.
      { rootMargin: "200px" }
    );
    observerRef.current = observer;
    return () => observer.disconnect();
  }, []);

  const observeIcon = (el: Element | null, app: AppEntry) => {
    const observer = observerRef.current;
    if (!el || !observer) return undefined;
    if (!app.path || requestedIconsRef.current.has(app.name)) return undefined;
    observedAppsRef.current.set(el, app);
    observer.observe(el);
    return () => {
      observer.unobserve(el);
      observedAppsRef.current.delete(el);
    };
  };

  const filtered = searchQuery
    ? allApps.filter((app) =>
        app.name.toLowerCase().includes(searchQuery.toLowerCase())
      )
    : allApps;

  // Already-filtered apps first (what the user came to review), then open
  // apps (the likeliest new filter targets), then everything else.
  const pinnedApps = filtered.filter((app) => pinnedSet.has(app.name));
  const openApps = filtered.filter((app) => app.running && !pinnedSet.has(app.name));
  const otherApps = filtered.filter((app) => !app.running && !pinnedSet.has(app.name));

  const blacklistedCount = blacklistedApps.length;

  const renderRow = (app: AppEntry) => {
    const isBlacklisted = blacklistedApps.includes(app.name);
    return (
      <button
        key={app.name}
        className="lookout-app-row"
        onClick={() => toggleApp(app.name)}
        style={{
          position: "relative",
          display: "flex",
          alignItems: "center",
          // 6px icon-to-name gap; the checkbox adds 2px margin for an 8px
          // checkbox-to-icon gap.
          gap: 6,
          width: "100%",
          padding: "6px 8px",
          background: "transparent",
          border: "none",
          borderRadius: radii.md,
          cursor: "pointer",
          textAlign: "left",
          color: colors.text.primary,
          fontSize: fontSize.md,
        }}
      >
        {/* Hover/press highlight (pure CSS so search stays instant) */}
        <div className="lookout-app-row-bg" style={{ borderRadius: radii.md }} />

        {/* Checkbox */}
        <div
          style={{
            position: "relative",
            zIndex: 1,
            width: 18,
            height: 18,
            marginRight: 2,
            borderRadius: radii.sm,
            border: `1.5px solid ${isBlacklisted ? colors.status.danger : colors.border.hover}`,
            background: isBlacklisted ? colors.status.danger : "transparent",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            flexShrink: 0,
            transition: "all 0.15s",
          }}
        >
          {isBlacklisted && (
            <CheckIcon size={12} weight="bold" color="#fff" aria-hidden="true" />
          )}
        </div>

        {/* App icon (observed so its data loads only when scrolled into view) */}
        <div
          ref={(el) => observeIcon(el, app)}
          style={{ position: "relative", zIndex: 1, width: 20, height: 20, flexShrink: 0 }}
        >
          <AppIcon icon={appIcons[app.name]} />
        </div>

        {/* App name */}
        <div style={{ flex: 1, minWidth: 0, position: "relative", zIndex: 1 }}>
          <span
            style={{
              display: "block",
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
              fontWeight: isBlacklisted ? fontWeight.medium : fontWeight.normal,
            }}
          >
            {app.name}
          </span>
        </div>
      </button>
    );
  };

  return (
    <PageChrome
      title="Filtered Apps"
      description={
        <>
          Selected apps will be blacked out in monitor screen captures.
          {blacklistedCount > 0 && (
            <span style={{ color: colors.status.warning }}>
              {" "}{blacklistedCount} app{blacklistedCount !== 1 ? "s" : ""} filtered.
            </span>
          )}
        </>
      }
      onBack={onBack}
    >
      {/* Search + App list (hidden on Wayland) */}
      {isWayland ? (
        <div
          style={{
            flex: 1,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            borderRadius: radii.lg,
            border: `1px solid ${colors.border.default}`,
            background: colors.bg.surface,
            padding: spacing.xxl,
            color: colors.text.tertiary,
            fontSize: fontSize.sm,
            textAlign: "center",
            lineHeight: 1.5,
          }}
        >
          App filtering is not supported on Wayland.
        </div>
      ) : (
        <>
      {/* Row hover/press animation + icon fade-in, as real CSS so rows stay
          plain DOM nodes and search filtering has zero animation overhead */}
      <style>{`
        @keyframes lookout-icon-fade-in { from { opacity: 0; } to { opacity: 1; } }
        .lookout-app-row-bg {
          position: absolute;
          inset: 0;
          background: transparent;
          transition: background 0.12s ease-out, transform 0.12s ease-out;
        }
        .lookout-app-row:hover .lookout-app-row-bg { background: ${colors.bg.selected}; }
        .lookout-app-row:active .lookout-app-row-bg { transform: scale(0.96); background: ${colors.bg.selected}; }
      `}</style>
      {/* Search */}
      <div style={{ flexShrink: 0, marginBottom: spacing.md }}>
        <input
          type="text"
          placeholder="Search apps..."
          value={searchQuery}
          onChange={(e) => setSearchQuery(e.target.value)}
          style={{
            width: "100%",
            padding: `${spacing.sm}px ${spacing.md}px`,
            fontSize: fontSize.md,
            color: colors.text.primary,
            background: colors.bg.surface,
            border: `1px solid ${colors.border.default}`,
            borderRadius: radii.md,
            outline: "none",
            boxSizing: "border-box",
          }}
          onFocus={(e) => {
            e.currentTarget.style.borderColor = colors.border.hover;
          }}
          onBlur={(e) => {
            e.currentTarget.style.borderColor = colors.border.default;
          }}
        />
      </div>

      {/* App list */}
      <div
        style={{
          flex: 1,
          overflowY: "auto",
          borderRadius: radii.lg,
          border: `1px solid ${colors.border.default}`,
          background: colors.bg.surface,
        }}
      >
        {loading ? (
          // Skeleton rows while the app list loads
          <div style={{ padding: spacing.xs }}>
            {Array.from({ length: 12 }).map((_, i) => (
              <motion.div
                key={i}
                animate={{ opacity: [0.45, 0.9, 0.45] }}
                transition={{
                  duration: 1.2,
                  repeat: Infinity,
                  ease: "easeInOut",
                  delay: i * 0.05,
                }}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 6,
                  padding: "6px 8px",
                }}
              >
                <div
                  style={{
                    width: 18,
                    height: 18,
                    marginRight: 2,
                    borderRadius: radii.sm,
                    background: colors.bg.selected,
                    flexShrink: 0,
                  }}
                />
                <div
                  style={{
                    width: 20,
                    height: 20,
                    borderRadius: radii.sm,
                    background: colors.bg.selected,
                    flexShrink: 0,
                  }}
                />
                <div
                  style={{
                    height: 12,
                    width: `${40 + ((i * 23) % 40)}%`,
                    borderRadius: radii.sm,
                    background: colors.bg.selected,
                  }}
                />
              </motion.div>
            ))}
          </div>
        ) : filtered.length === 0 ? (
          <div
            style={{
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              padding: spacing.xxl,
              color: colors.text.tertiary,
              fontSize: fontSize.sm,
            }}
          >
            {searchQuery ? "No matching apps" : "No apps detected"}
          </div>
        ) : (
          <div style={{ padding: spacing.xs }}>
            {pinnedApps.map(renderRow)}
            {openApps.map(renderRow)}
            {otherApps.map(renderRow)}
          </div>
        )}
      </div>
        </>
      )}
    </PageChrome>
  );
}
