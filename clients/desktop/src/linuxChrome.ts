/**
 * Dressing Lookout like a citizen of the Linux desktop.
 *
 * The shared design system now carries Adwaita's surfaces itself, so the
 * palette isn't this file's job any more. What is: the session's own accent
 * colour and UI font, read from GSettings, plus the little the webview still
 * has to do inside a GTK-framed window. macOS and Windows never load any
 * of it.
 *
 * The window frame itself — shadow, outer border, rounded corners, resize
 * edges — is GTK's, not ours. The native side gives each window a
 * zero-height GTK titlebar (window_frame.rs), which flips GTK into its
 * client-side decoration mode: the frame is drawn by the session's own GTK
 * theme, so it matches the desktop on GNOME and KDE and everything else,
 * and GTK squares it off on its own when the window is maximized or tiled.
 * Earlier revisions drew all of that by hand in CSS, hardcoded to Adwaita's
 * look, which is exactly why it read wrong anywhere that wasn't GNOME.
 */
import { useEffect } from "react";
import { getCurrentWindow, currentMonitor } from "@tauri-apps/api/window";
import { setAccentColor } from "@lookout/react";
import { isLinux } from "./platform.js";
import { invoke } from "./logger.js";

export interface DesktopAppearance {
  accent: string | null;
  fontFamily: string | null;
  controlsOnRight: boolean;
}

export const DEFAULT_APPEARANCE: DesktopAppearance = {
  accent: null,
  fontFamily: null,
  controlsOnRight: true,
};

/** The header bar's height, in px. Adwaita's is 46 plus a 1px hairline. */
export const HEADER_BAR_HEIGHT = 47;

/**
 * The radius the webview rounds its own top corners at, in px.
 *
 * GTK rounds the window frame, but GTK3 does not clip child widgets to that
 * radius — a webview with an opaque background pokes square corners out
 * over the frame's rounding. So the webview stays transparent and #root
 * rounds its top corners itself, matching GTK3 Adwaita's decoration radius
 * (8px, top corners only; the bottom of a GTK3 window is square).
 *
 * A theme with a smaller radius leaves a sliver of GTK's window background
 * showing in the corners — the theme's own surface colour, which is as
 * close to invisible as a hardcoded number gets. The alternative, reading
 * the radius out of the theme at runtime, means resolving GTK's internal
 * `decoration` style node; not worth it for two corners.
 */
const WINDOW_TOP_RADIUS = 8;

/**
 * Linux-only chrome that the shared theme can't carry.
 *
 * The Adwaita palette itself now lives in the shared theme — it's the app's
 * baseline on every platform — so what's left here is the part that only
 * makes sense on a GTK desktop: the header bar's control colours, GTK's
 * cursor behaviour, and keeping the webview's top corners inside the
 * GTK-drawn frame.
 *
 * Specificity note: these still key off `html.os-linux[data-theme=…]`
 * (0,2,1) because the shared sheet's light block is `:root[data-theme=
 * "light"]` (0,2,0), and loading order can't be relied on — the shared sheet
 * injects itself on first import.
 */
const ADWAITA_CSS = `
  html.os-linux[data-theme="dark"] {
    --color-headerbar-control: rgba(255, 255, 255, 0.1);
    --color-headerbar-control-hover: rgba(255, 255, 255, 0.18);
    /* Adwaita's popover_bg_color. Note it is LIGHTER than the #242424
       window, not darker: a GTK popover is an elevated surface, and
       reaching for the app's "panel" colour (a recessed one) is what makes
       a menu read as a hole in the window instead of a thing floating above
       it. */
    --color-popover-bg: #383838;
    /* GNOME's popovers aren't a flat fill — there's a slight vertical
       lift, lighter at the top. The tail sits above the panel, so it takes
       this top stop rather than the base colour. */
    --color-popover-bg-top: #3d3d3d;
    /* A light edge, not a dark one. The popover is an elevated surface
       sitting on a darker window, so its outline reads as the light catching
       the lifted edge; a dark border just sinks into the window behind it. */
    --color-popover-border: rgba(255, 255, 255, 0.14);
    --color-popover-hover: rgba(255, 255, 255, 0.1);
    --color-popover-separator: rgba(255, 255, 255, 0.15);
  }
  html.os-linux[data-theme="light"] {
    --color-headerbar-control: rgba(0, 0, 0, 0.08);
    --color-headerbar-control-hover: rgba(0, 0, 0, 0.16);
    --color-popover-bg: #f7f7f7;
    --color-popover-bg-top: #ffffff;
    --color-popover-border: rgba(0, 0, 0, 0.12);
    --color-popover-hover: rgba(0, 0, 0, 0.08);
    --color-popover-separator: rgba(0, 0, 0, 0.12);
  }
  /* GTK never swaps in a hand cursor over a button — the pointer is a web
     convention, and having it follow every control around is one of the
     small constant reminders that this is a web view.

     !important because the app sets cursor:pointer inline in a couple of
     dozen components, and forking each of them per platform would be a far
     worse trade than one scoped override. The selector deliberately does
     NOT match everything: the editor's scrub cursors carry real
     information, and they set their own values. */
  html.os-linux button,
  html.os-linux a,
  html.os-linux [role="button"],
  html.os-linux [style*="cursor: pointer"] {
    cursor: default !important;
  }
  html.os-linux input,
  html.os-linux textarea,
  html.os-linux [contenteditable="true"] {
    cursor: text !important;
  }
  /* GTK draws the window frame, but it cannot clip the webview to the
     frame's rounded top corners — GTK3 doesn't clip children. So the
     webview stays transparent and #root rounds itself, letting GTK's own
     corner show through the gap. Opt in per window: only the windows the
     native side gave a zero-height GTK titlebar carry .lookout-csd. */
  html.os-linux.lookout-csd, html.os-linux.lookout-csd body {
    background: transparent;
  }
  html.os-linux.lookout-csd {
    --lookout-window-radius: ${WINDOW_TOP_RADIUS}px;
  }
  /* Snapped or maximized: GTK squares its frame, and rounded content
     corners would leave two notches poking out of it. Mirrors GTK's own
     behaviour, driven by the same compositor state (useWindowFrameState). */
  html.os-linux.lookout-csd.lookout-snapped {
    --lookout-window-radius: 0px;
  }
  html.os-linux.lookout-csd #root {
    background: var(--color-bg-body);
    border-radius: var(--lookout-window-radius) var(--lookout-window-radius) 0 0;
    overflow: hidden;
  }
  /* Modal backdrops portal to <body>, so they'd square off the rounded top
     corners; give them the same rounding. */
  html.os-linux.lookout-csd [data-lookout-overlay] {
    border-radius: var(--lookout-window-radius) var(--lookout-window-radius) 0 0;
    overflow: hidden;
  }

  /* The header bar dims with the window, GTK's :backdrop state. GTK pulls
     the frame's shadow back on its own when focus leaves; this keeps the
     webview-drawn titlebar in step with it. */
  html.os-linux .lookout-headerbar {
    transition: opacity 160ms ease-out;
  }
  html.os-linux.lookout-backdrop .lookout-headerbar {
    opacity: 0.55;
  }
`;

/**
 * White or black, whichever stays legible on the given accent.
 *
 * GNOME's accent palette runs from a dark purple to a fairly bright yellow,
 * and white-on-yellow is the one combination that fails outright.
 */
export function accentForeground(hex: string): string {
  const value = hex.replace("#", "");
  if (value.length !== 6) return "#ffffff";
  const channel = (offset: number) => {
    const srgb = parseInt(value.slice(offset, offset + 2), 16) / 255;
    return srgb <= 0.04045 ? srgb / 12.92 : ((srgb + 0.055) / 1.055) ** 2.4;
  };
  const luminance = 0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4);
  return luminance > 0.45 ? "#000000" : "#ffffff";
}

/**
 * The font stack to run on Linux: the desktop's configured UI font first,
 * then the families GNOME has shipped as its default across versions, then
 * whatever the session calls `system-ui`.
 */
export function linuxFontStack(family: string | null): string {
  const fallbacks = ['Adwaita Sans', 'Cantarell'];
  const families = family ? [family, ...fallbacks.filter((f) => f !== family)] : fallbacks;
  return [...families.map((f) => `"${f}"`), 'system-ui', '"Geist"', 'sans-serif'].join(', ');
}

// Injected on import rather than from applyLinuxChrome's effect, for the
// same reason HeaderBar's sheet is: an effect runs after the first paint, so
// the frame before it has --color-headerbar-control undefined and the window
// controls render with no background at all.
if (isLinux && typeof document !== "undefined" && !document.querySelector("style[data-lookout-linux-chrome]")) {
  const style = document.createElement("style");
  style.setAttribute("data-lookout-linux-chrome", "");
  style.textContent = ADWAITA_CSS;
  document.head.appendChild(style);
}

/**
 * Whether the window manager is sizing this window — tiled, maximized,
 * fullscreen — read from the compositor rather than inferred.
 *
 * That distinction is the whole point under a tiling WM: every window there
 * is WM-sized, and rounded content corners in that state poke out of a
 * frame GTK has already squared.
 *
 * Returns null if the native side couldn't answer, so callers can fall back
 * rather than treat "unknown" as "floating".
 */
async function windowManagerSized(): Promise<boolean | null> {
  if (!isLinux) return null;
  try {
    return await invoke<boolean>("window_manager_sized");
  } catch (e) {
    // Worth knowing about, not worth breaking over: the corners simply
    // keep their rounding a beat longer.
    console.warn("[csd] could not read window state:", e);
    return null;
  }
}

/**
 * Fallback for when the native side can't report the window's state.
 *
 * A geometry guess: a window the WM has sized usually matches the work area
 * on at least one axis. It cannot see a quarter-tile, which is exactly why
 * it is only the fallback — `windowManagerSized` asks the compositor.
 *
 * Deliberately compares sizes and not positions: Wayland doesn't tell a
 * client where its own window is, so `outerPosition()` is either unavailable
 * or a lie there.
 */
async function isFlushWithWorkArea(): Promise<boolean> {
  const win = getCurrentWindow();
  try {
    if (await win.isMaximized()) return true;
    const monitor = await currentMonitor();
    if (!monitor) return false;
    const size = await win.outerSize();
    const work = monitor.workArea.size;
    // A couple of physical pixels of slack, scaled: fractional scaling means
    // an exactly-snapped window can land a pixel off its own work area.
    const slack = Math.max(2, Math.ceil(monitor.scaleFactor));
    return (
      Math.abs(size.height - work.height) <= slack ||
      Math.abs(size.width - work.width) <= slack
    );
  } catch (e) {
    // Rounded corners on a snapped window are a cosmetic wart; a throw here
    // that took the window down with it would not be.
    console.warn("[csd] could not read window geometry:", e);
    return false;
  }
}

/**
 * Keep the webview's corner rounding in step with the compositor: rounded
 * while the window floats, squared the moment the window manager takes over
 * sizing it — which is when GTK squares the frame the corners sit inside.
 */
export function useWindowFrameState(): void {
  useEffect(() => {
    if (!isLinux) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const sync = async () => {
      const reported = await windowManagerSized();
      const snapped = reported ?? (await isFlushWithWorkArea());
      if (cancelled) return;
      document.documentElement.classList.toggle("lookout-snapped", snapped);
    };

    // `onResized` fires continuously through a drag, and each check costs a
    // few IPC round trips. Snapping is a discrete event, so trailing-edge
    // debouncing loses nothing and spares the bridge.
    const schedule = () => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => { void sync(); }, 100);
    };

    void sync();

    let unlisten: (() => void) | undefined;
    void getCurrentWindow().onResized(schedule).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });

    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
      if (unlisten) unlisten();
      document.documentElement.classList.remove("lookout-snapped");
    };
  }, []);
}

/**
 * Track focus and mirror it onto the document as GTK's :backdrop state.
 *
 * GTK dims the frame's shadow itself when focus leaves; the header bar is
 * the webview's, so its half of the signal has to be driven from here, off
 * the same window-focus events GTK is reacting to.
 */
export function useBackdropState(): void {
  useEffect(() => {
    if (!isLinux) return;

    const apply = (focused: boolean) => {
      document.documentElement.classList.toggle("lookout-backdrop", !focused);
    };

    // Ask the WINDOW whether it's focused, not the document. Dragging the
    // window by the header bar takes a pointer grab, which costs the webview
    // its DOM focus — so document.hasFocus() goes false and the titlebar
    // dimmed the whole time you were moving the window. The window itself
    // never stopped being focused.
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    const win = getCurrentWindow();
    void win.isFocused().then((f) => { if (!cancelled) apply(f); }).catch(() => {
      apply(document.hasFocus());
    });
    void win.onFocusChanged(({ payload }) => apply(payload)).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    }).catch(() => {
      // No window events (a plain browser, say) — fall back to the document.
      const sync = () => apply(document.hasFocus());
      window.addEventListener("focus", sync);
      window.addEventListener("blur", sync);
      unlisten = () => {
        window.removeEventListener("focus", sync);
        window.removeEventListener("blur", sync);
      };
    });

    return () => {
      cancelled = true;
      if (unlisten) unlisten();
      document.documentElement.classList.remove("lookout-backdrop");
    };
  }, []);
}

/**
 * Apply the Adwaita palette immediately, then fold in whatever GSettings
 * reports once it answers.
 *
 * `csd` says the native side gave this window a zero-height GTK titlebar
 * (window_frame.rs) — GTK draws its frame, and the webview must keep its
 * top corners inside it.
 *
 * The stylesheet itself is already in the document by the time this runs —
 * it goes in on import, above. What's left here needs a round trip to the
 * native side, so it necessarily lands a moment after the window opens;
 * that's better than holding the first paint hostage to an IPC call.
 */
export async function applyLinuxChrome(
  { csd = false }: { csd?: boolean } = {},
): Promise<DesktopAppearance> {
  if (!isLinux) return DEFAULT_APPEARANCE;

  document.documentElement.classList.add("os-linux");
  // Opts this window into transparent-background-plus-rounded-#root. Only
  // the windows wearing GTK's client-side frame may claim it.
  if (csd) {
    document.documentElement.classList.add("lookout-csd");
  }

  let appearance = DEFAULT_APPEARANCE;
  try {
    appearance = await invoke<DesktopAppearance>("desktop_appearance");
  } catch (e) {
    console.warn("[linux-chrome] could not read desktop appearance:", e);
    return DEFAULT_APPEARANCE;
  }

  if (appearance.accent) {
    setAccentColor(appearance.accent, accentForeground(appearance.accent));
  }
  document.body.style.fontFamily = linuxFontStack(appearance.fontFamily);

  return appearance;
}
