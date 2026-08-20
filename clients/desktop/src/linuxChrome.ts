/**
 * Dressing Lookout like a citizen of the Linux desktop.
 *
 * The shared design system now carries Adwaita's surfaces itself, so the
 * palette isn't this file's job any more. What is: the session's own accent
 * colour and UI font, read from GSettings, plus the chrome that only an
 * undecorated GTK window needs. macOS and Windows never load any of it.
 */
import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { setAccentColor } from "@lookout/react";
import { isLinux } from "./platform.js";
import { invoke } from "./logger.js";

/**
 * The session's actual GTK theme colours, or nulls where the theme doesn't
 * define them. Read via GTK's own `lookup_color`, so this follows Yaru,
 * Breeze, Catppuccin and hand-rolled themes rather than assuming Adwaita.
 */
export interface ThemeColors {
  windowBg: string | null;
  windowFg: string | null;
  viewBg: string | null;
  border: string | null;
  popoverBg: string | null;
  accent: string | null;
}

export interface DesktopAppearance {
  accent: string | null;
  fontFamily: string | null;
  /** Close on the trailing edge of the header bar. False means the user
   *  moved their window controls to the leading edge. */
  controlsOnRight: boolean;
  colors: ThemeColors;
  /** The radius GTK rounds this theme's windows to, in px. Null leaves
   *  Lookout on Adwaita's {@link WINDOW_RADIUS}. */
  windowRadius: number | null;
}

export const NO_THEME_COLORS: ThemeColors = {
  windowBg: null,
  windowFg: null,
  viewBg: null,
  border: null,
  popoverBg: null,
  accent: null,
};

export const DEFAULT_APPEARANCE: DesktopAppearance = {
  accent: null,
  fontFamily: null,
  // GNOME's own default, and what every other platform reports.
  controlsOnRight: true,
  colors: NO_THEME_COLORS,
  windowRadius: null,
};

/** The header bar's height, in px. Adwaita's is 46 plus a 1px hairline. */
export const HEADER_BAR_HEIGHT = 47;

/**
 * The window's corner radius before the theme has answered, in px.
 *
 * The real value comes from the theme — `read_window_radius` in
 * desktop_appearance.rs — so a theme built for square windows gets square
 * corners instead of leaving Lookout as the one rounded window on the
 * desktop. This is only what the first frame paints, and the fallback for a
 * desktop that can't answer at all.
 *
 * 12 matches nothing in particular, and is kept because of that. Measured on
 * GTK 3.24: Adwaita and Adwaita-dark round their decorations at 8, Yaru — so
 * Ubuntu, so most Hack Clubbers — at 15, and a theme like System-4-1.0 at 0.
 * Sitting between the two common answers keeps the correction on first paint
 * as small as possible either way. (The 12 is libadwaita's, i.e. GTK4's; GTK3
 * Adwaita never used it, which is why this app drew 12 where every window
 * around it drew 8 or 15.)
 */
export const WINDOW_RADIUS = 12;

/**
 * Whether the shell is already drawing this window's rounded corners and
 * shadow, in which case Lookout draws no frame of its own — no margin, no
 * border, no radius, no shadow, no input shape. The header bar stays: the
 * window is still undecorated, and the extensions in question do nothing
 * about titlebars.
 *
 * The culprits are the Rounded Window Corners family of GNOME extensions,
 * which round and shade every window from its real edge — 40px outside
 * ours — so the two frames nest with a band of desktop showing between
 * them. See `shell_draws_window_frame` in desktop_appearance.rs, which is
 * what decides this.
 *
 * Read off a global rather than fetched over IPC, and deliberately so. The
 * frame is painted on the very first frame (index.html), and the native
 * side has already sized the window against this same answer, so a value
 * that landed a round trip later would show as the frame flashing in and
 * out of a mis-sized window at every launch. The native side plants it at
 * document start instead (`js_init_script` in lib.rs).
 */
export const SHELL_DRAWS_FRAME: boolean =
  isLinux &&
  (globalThis as unknown as { __LOOKOUT_SHELL_DRAWS_FRAME__?: boolean })
    .__LOOKOUT_SHELL_DRAWS_FRAME__ === true;

/**
 * Transparent frame reserved around the visible window, in px per side.
 *
 * A window is normally exactly the size of its content, which leaves nowhere
 * to draw an outer border or a shadow — both paint outside the content box,
 * i.e. off the window, where the compositor clips them. GTK solves this by
 * making the window bigger than it looks and keeping the extra transparent;
 * the shadow lives in there, and so does the invisible frame you grab to
 * resize. Same trick here.
 *
 * The native side grows each window by twice this (lib.rs for the main
 * window, EditorWindow.tsx for the editor), so the content keeps its
 * intended size. Neither grows it when `SHELL_DRAWS_FRAME`, since there is
 * then no frame to make room for.
 *
 * IT MUST BE AT LEAST AS LARGE AS THE SHADOW REACHES. A box-shadow extends
 * `offset + blur` past its box, and anything past the frame is off the
 * window and clipped away — a shadow that looks generous in a browser gets
 * a hard straight edge on the desktop. The shadows below are sized against
 * this number; raising one means raising the other.
 */
export const WINDOW_MARGIN = 40;

/**
 * How much of that frame still accepts pointer input, measured inward from
 * the visible window's edge.
 *
 * The rest of the frame is passed through to whatever is behind, so a click
 * on the shadow doesn't land on Lookout — but this band has to stay live,
 * because it's where the resize strips are. Mirrored in window_shape.rs.
 */
export const RESIZE_BAND = 8;

/**
 * The outer ring of the frame that passes clicks through: everything
 * between the window's real edge and the resize band.
 */
export const SHADOW_PASSTHROUGH = WINDOW_MARGIN - RESIZE_BAND;

/**
 * How far in from the window's real edge the interactive area starts: the
 * shadow's passthrough ring while Lookout draws its own frame, and nothing
 * at all when the shell draws it — there is no frame then, so the window's
 * real edge is already its visible one.
 *
 * Drives both the input shape and where the resize strips sit, which have
 * to agree: a strip outside the shape could never be clicked.
 */
export const FRAME_INSET = SHELL_DRAWS_FRAME ? 0 : SHADOW_PASSTHROUGH;

/**
 * Linux-only chrome that the shared theme can't carry.
 *
 * The Adwaita palette itself now lives in the shared theme — it's the app's
 * baseline on every platform — so what's left here is the part that only
 * makes sense on a GTK desktop: the header bar's control colours, GTK's
 * cursor behaviour, and the rounded corners an undecorated window owns.
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
    --color-window-border: rgba(255, 255, 255, 0.18);
  }
  html.os-linux[data-theme="light"] {
    --color-headerbar-control: rgba(0, 0, 0, 0.08);
    --color-headerbar-control-hover: rgba(0, 0, 0, 0.16);
    --color-popover-bg: #f7f7f7;
    --color-popover-bg-top: #ffffff;
    --color-popover-border: rgba(0, 0, 0, 0.12);
    --color-popover-hover: rgba(0, 0, 0, 0.08);
    --color-popover-separator: rgba(0, 0, 0, 0.12);
    --color-window-border: rgba(0, 0, 0, 0.18);
  }
  /* GTK never swaps in a hand cursor over a button — the pointer is a web
     convention, and having it follow every control around is one of the
     small constant reminders that this is a web view.

     !important because the app sets cursor:pointer inline in a couple of
     dozen components, and forking each of them per platform would be a far
     worse trade than one scoped override. The selector deliberately does
     NOT match everything: the resize handles and the editor's scrub cursors
     carry real information, and they set their own values. */
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
  /* Undecorated windows draw their own corners. They're clipped on #root
     rather than the body because the body has to stay transparent for the
     rounding to show anything but a square. Opt in per window, since a
     window that kept its GTK titlebar must not round its own content. */
  html.os-linux.lookout-csd, html.os-linux.lookout-csd body {
    background: transparent;
  }
  html.os-linux.lookout-csd {
    /* Via a second variable, not by setting --lookout-window-radius inline on
       :root: an inline custom property would beat the .lookout-snapped rule
       below, and a maximized window would keep its rounded corners. The theme
       feeds --lookout-theme-radius; both rules here stay stylesheet rules, so
       the more specific one still wins. */
    --lookout-window-radius: var(--lookout-theme-radius, ${WINDOW_RADIUS}px);
    --lookout-window-margin: ${WINDOW_MARGIN}px;
  }
  /* Snapped or maximized: the window is flush with the screen edge, and
     rounding it there leaves four notches of desktop showing through — the
     clearest tell that an app is drawing its own decorations badly. */
  html.os-linux.lookout-csd.lookout-snapped {
    --lookout-window-radius: 0px;
  }
  /* The visible window: inset from the real one by the transparent frame,
     so the outer border and the shadow have somewhere to land. Both are
     spread/blur on one box-shadow — the 1px spread ring IS the outer
     border, which keeps it off the content box entirely and lets it follow
     the corner radius. */
  html.os-linux.lookout-csd #root {
    position: fixed;
    inset: var(--lookout-window-margin);
    height: auto;
    background: var(--color-bg-body);
    border-radius: var(--lookout-window-radius);
    overflow: hidden;
    /* Wide and faint rather than tight and dark. A compositor shadow is
       mostly a large, very soft falloff — reading the alpha off a dark
       screenshot tempts you into something far heavier than GNOME's, which
       then looks like a drop shadow on a web card. */
    /* Reach is 8 + 28 = 36, inside the 40px frame. */
    box-shadow:
      0 0 0 1px var(--color-window-border),
      0 2px 8px rgba(0, 0, 0, 0.1),
      0 8px 28px rgba(0, 0, 0, 0.14);
    transition: box-shadow 160ms ease-out;
  }
  /* Unfocused windows cast less. GTK pulls its shadow back in :backdrop so
     the focused window is the one that looks lifted. */
  html.os-linux.lookout-csd.lookout-backdrop #root {
    box-shadow:
      0 0 0 1px var(--color-window-border),
      0 1px 4px rgba(0, 0, 0, 0.06),
      0 4px 16px rgba(0, 0, 0, 0.09);
  }
  /* Snapped or maximized: the frame collapses so the content fills the
     screen edge to edge, and the outline and shadow have nothing to
     separate the window from. */
  html.os-linux.lookout-csd.lookout-snapped #root {
    inset: 0;
    border-radius: 0;
    box-shadow: none;
  }
  /* Modal backdrops portal to <body>, so they cover the whole window —
     including the transparent frame, which paints the dim over the shadow
     and squares off the rounded corners. Pull them in to the visible window.

     !important because these set their own inset inline, and an inline
     declaration otherwise beats anything a stylesheet says. */
  html.os-linux.lookout-csd [data-lookout-overlay] {
    inset: var(--lookout-window-margin) !important;
    border-radius: var(--lookout-window-radius);
    overflow: hidden;
  }
  html.os-linux.lookout-csd.lookout-snapped [data-lookout-overlay] {
    inset: 0 !important;
    border-radius: 0;
  }

  /* The header bar dims with the window, GTK's :backdrop state. */
  html.os-linux .lookout-headerbar {
    transition: opacity 160ms ease-out;
  }
  html.os-linux.lookout-backdrop .lookout-headerbar {
    opacity: 0.55;
  }
`;

/**
 * Split `#rrggbb` into its channels, or null if it isn't one.
 *
 * The native side only ever emits this form (rgba_to_hex in
 * desktop_appearance.rs), so anything else means the value didn't come from
 * there and shouldn't be trusted into a colour calculation.
 */
function parseHex(hex: string): [number, number, number] | null {
  const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex.trim());
  if (!m) return null;
  return [parseInt(m[1], 16), parseInt(m[2], 16), parseInt(m[3], 16)];
}

/**
 * The same colour at a given opacity.
 *
 * This is the whole reason reading the theme works as cleanly as it does.
 * The shared design system builds its surfaces and borders as translucent
 * overlays (`rgba(255,255,255,0.08)` and friends) rather than as opaque
 * greys, so deriving those overlays from the theme's *foreground* makes them
 * correct for any theme automatically: a light theme yields dark overlays, a
 * dark one yields light, and a theme that is neither still gets overlays in
 * its own hue instead of a borrowed grey.
 */
export function withAlpha(hex: string, alpha: number): string | null {
  const rgb = parseHex(hex);
  return rgb ? `rgba(${rgb[0]}, ${rgb[1]}, ${rgb[2]}, ${alpha})` : null;
}

/**
 * Nudge a colour towards white by `amount` (0–1).
 *
 * Used for the popover's top stop: GNOME's menus aren't a flat fill, there's
 * a slight lift lighter at the top, and it goes the same direction in both
 * light and dark themes.
 */
export function lighten(hex: string, amount: number): string | null {
  const rgb = parseHex(hex);
  if (!rgb) return null;
  const lift = (v: number) => Math.round(v + (255 - v) * amount);
  return `#${rgb.map((v) => lift(v).toString(16).padStart(2, "0")).join("")}`;
}

/**
 * Whether the document is currently in its dark scheme.
 *
 * Reads the attribute the app actually renders from, falling back to the
 * media query for the frame before anything has set it.
 */
function documentIsDark(): boolean {
  const attr = document.documentElement.getAttribute("data-theme");
  if (attr === "dark") return true;
  if (attr === "light") return false;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/**
 * Paint the session's GTK palette over the app's own.
 *
 * Written as inline custom properties on `:root` deliberately: that beats
 * every `:root[data-theme=…]` rule in the shared sheet without having to
 * out-specify anything or care about injection order, which is the problem
 * the rest of this file's CSS has to work around.
 *
 * What it does and does not claim:
 *
 * * The window's own surfaces and text follow the theme — background, view
 *   background, foreground, borders, popovers, accent. That is what makes a
 *   Breeze or Catppuccin desktop stop showing one Adwaita-grey window.
 * * The app's *content* components keep the design system. A theme is not
 *   asked to supply Lookout's semantics (error red, cut-region fill), and
 *   inventing mappings for them from six colours would break more than it
 *   fixed.
 *
 * Each role is independent, and a role the theme doesn't define is *removed*
 * rather than left behind — so re-applying after a theme change can take
 * colours away as well as add them, and never leaves a half-applied mixture
 * from the previous theme.
 */
export function applyThemePalette(colors: ThemeColors, documentIsDark: boolean): void {
  const root = document.documentElement;
  const set = (name: string, value: string | null) => {
    if (value) root.style.setProperty(name, value);
    else root.style.removeProperty(name);
  };

  // Refuse a palette whose polarity disagrees with the document's.
  //
  // The six roles below are a fraction of the design system. The rest —
  // tertiary text, sunken surfaces, skeletons, the editor's well — stay on
  // whichever `data-theme` block is active, and those two blocks are *not*
  // mirror images of each other: they carry different alphas, and several
  // tokens are translucent in one and opaque in the other. So they cannot be
  // derived from a foreground, and a dark GTK theme applied under a light
  // `data-theme` would leave `rgba(0,0,0,0.45)` tertiary text on a dark
  // window — invisible.
  //
  // This happens for real: installing a third-party theme (WhiteSur, Breeze,
  // any of the macOS lookalikes) sets `gtk-theme` and doesn't touch GNOME's
  // `color-scheme`, so a dark theme can sit in a light-scheme session.
  //
  // Falling back to the app's own coherent palette is a much smaller loss
  // than a half-flipped one, so a disagreement clears everything rather than
  // applying what it can.
  const paletteIsDark = colors.windowBg ? isDarkSurface(colors.windowBg) : null;
  if (paletteIsDark !== null && paletteIsDark !== documentIsDark) {
    console.warn(
      `[linux-chrome] ignoring the GTK palette: it is ${paletteIsDark ? "dark" : "light"} ` +
        `but the session's colour scheme is ${documentIsDark ? "dark" : "light"}`,
    );
    colors = NO_THEME_COLORS;
  }

  set("--color-bg-body", colors.windowBg);
  set("--color-bg-panel", colors.viewBg);
  set("--color-text-primary", colors.windowFg);

  // Everything derived from the foreground, so the polarity is always the
  // theme's own rather than whatever `data-theme` happens to say.
  const fg = colors.windowFg;
  set("--color-text-secondary", fg && withAlpha(fg, 0.7));
  set("--color-bg-surface", fg && withAlpha(fg, 0.08));
  set("--color-border-default", fg && withAlpha(fg, 0.12));
  set("--color-border-hover", fg && withAlpha(fg, 0.22));
  set("--color-headerbar-control", fg && withAlpha(fg, 0.1));
  set("--color-headerbar-control-hover", fg && withAlpha(fg, 0.18));
  set("--color-popover-hover", fg && withAlpha(fg, 0.1));

  // A popover floats above the window, so it must never be darker than it.
  //
  // No GTK3 theme measured here defines `popover_bg_color` at all (Adwaita,
  // Adwaita-dark, Yaru, Yaru-dark, HighContrast: none), so this derivation is
  // the path that actually runs. `theme_base_color` looks like the answer and
  // is only half of one — it's a *recessed* content surface, and on every
  // dark theme measured it sits below the window (Adwaita-dark #2d2d2d under
  // #353535, Yaru-dark #272727 under #2c2c2c). Using it there would make
  // every menu read as a hole punched in the window.
  //
  // On light themes it's exactly right, though: #ffffff over an off-white
  // window is the elevated surface Adwaita itself draws. So take it when it
  // really is lighter, and lift off the window when it isn't. The 9% is not
  // arbitrary — it takes Adwaita's #242424 to exactly the #383838 popover
  // Adwaita ships.
  const lighterThanWindow = (candidate: string | null): string | null => {
    if (!candidate || !colors.windowBg) return null;
    const c = relativeLuminance(candidate);
    const w = relativeLuminance(colors.windowBg);
    return c !== null && w !== null && c > w ? candidate : null;
  };
  const popoverBg =
    colors.popoverBg ??
    lighterThanWindow(colors.viewBg) ??
    (colors.windowBg ? lighten(colors.windowBg, documentIsDark ? 0.09 : 0) : null);

  set("--color-popover-bg", popoverBg);
  // 2.5% reproduces Adwaita's own hand-picked pair (#383838 -> #3d3d3d)
  // exactly, which is the best evidence available that the derived lift
  // matches what GNOME draws.
  set("--color-popover-bg-top", popoverBg && lighten(popoverBg, 0.025));

  // GTK gives one border colour; the window outline wants it as-is, and the
  // popover's separators want it quieter than its outline.
  set("--color-window-border", colors.border);
  set("--color-popover-border", colors.border);
  set("--color-popover-separator", colors.border && withAlpha(colors.border, 0.6));
}

/**
 * Round the window to whatever the GTK theme rounds its windows to.
 *
 * Only ever sets `--lookout-theme-radius`, never `--lookout-window-radius`
 * itself — see the note in the stylesheet: an inline custom property would
 * out-rank the `.lookout-snapped` rule and a maximized window would keep its
 * corners.
 *
 * A null radius removes the override, so the CSS falls back to
 * {@link WINDOW_RADIUS} — which is also what the first frame painted, making
 * "the theme didn't say" indistinguishable from "we haven't asked yet".
 */
export function applyWindowRadius(radius: number | null): void {
  const root = document.documentElement;
  if (radius === null) root.style.removeProperty("--lookout-theme-radius");
  else root.style.setProperty("--lookout-theme-radius", `${radius}px`);
}

/**
 * White or black, whichever stays legible on the given accent.
 *
 * GNOME's accent palette runs from a dark purple to a fairly bright yellow,
 * and white-on-yellow is the one combination that fails outright.
 */
/**
 * WCAG relative luminance, or null if the input isn't a six-digit hex.
 */
export function relativeLuminance(hex: string): number | null {
  const value = hex.replace("#", "");
  if (value.length !== 6) return null;
  const channel = (offset: number) => {
    const srgb = parseInt(value.slice(offset, offset + 2), 16) / 255;
    return srgb <= 0.04045 ? srgb / 12.92 : ((srgb + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4);
}

/**
 * Whether a surface colour is a dark one, or null if it can't be read.
 *
 * The threshold is generous on purpose. Real themes are not subtle about
 * this — dark window backgrounds sit well under 0.1 and light ones well over
 * 0.7 — so anything near the line is a theme doing something unusual, and
 * the answer only decides whether to trust the palette at all.
 */
export function isDarkSurface(hex: string): boolean | null {
  const luminance = relativeLuminance(hex);
  return luminance === null ? null : luminance < 0.25;
}

export function accentForeground(hex: string): string {
  const luminance = relativeLuminance(hex);
  if (luminance === null) return "#ffffff";
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
 * Reconcile the window's frame with how the compositor is treating it.
 *
 * Applies the input shape and answers whether the frame is collapsed —
 * true when the window manager is sizing this window (tiled, maximized,
 * fullscreen), which is read from the compositor rather than inferred.
 *
 * That distinction is the whole point under a tiling WM: every window there
 * is WM-sized, and a frame kept in that state is not a shadow but a band of
 * desktop wedged between neighbours.
 *
 * Returns null if the native side couldn't answer, so callers can fall back
 * rather than treat "unknown" as "floating".
 */
export async function syncWindowFrame(inset: number): Promise<boolean | null> {
  if (!isLinux) return null;
  try {
    return await invoke<boolean>("sync_window_frame", { inset });
  } catch (e) {
    // Worth knowing about, not worth breaking over: the window keeps
    // catching clicks on its own shadow.
    console.warn("[csd] could not sync the window frame:", e);
    return null;
  }
}

/**
 * Track focus and mirror it onto the document as GTK's :backdrop state.
 *
 * One class for the whole window rather than per-component opacity, because
 * both the header bar and the window's own shadow have to dim together —
 * they're the same signal, and driving them from two places is how they end
 * up disagreeing.
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
 * `undecorated` says this window has had its GTK titlebar removed and is
 * therefore responsible for its own corners and header bar.
 *
 * The stylesheet itself is already in the document by the time this runs —
 * it goes in on import, above. What's left here needs a round trip to the
 * native side, so it necessarily lands a moment after the window opens;
 * that's better than holding the first paint hostage to an IPC call.
 */
export async function applyLinuxChrome(
  { undecorated = false }: { undecorated?: boolean } = {},
): Promise<DesktopAppearance> {
  if (!isLinux) return DEFAULT_APPEARANCE;

  document.documentElement.classList.add("os-linux");
  // Opts this window into drawing its own rounded corners. Only the windows
  // that actually had their decorations taken away may claim it, and only
  // when the shell isn't drawing corners for them already.
  //
  // index.html has normally added this class before the first paint; this is
  // the same decision from the same global, kept here so a window that keeps
  // its GTK titlebar never inherits it.
  if (undecorated && !SHELL_DRAWS_FRAME) {
    document.documentElement.classList.add("lookout-csd");
    // Stop the shadow catching clicks, and collapse the frame straight away
    // if we opened into a tile. Re-checked on every resize by
    // useWindowFrameState.
    void syncWindowFrame(FRAME_INSET);
  } else {
    document.documentElement.classList.remove("lookout-csd");
  }

  let appearance = DEFAULT_APPEARANCE;
  try {
    appearance = await invoke<DesktopAppearance>("desktop_appearance");
  } catch (e) {
    console.warn("[linux-chrome] could not read desktop appearance:", e);
    return DEFAULT_APPEARANCE;
  }

  applyThemePalette(appearance.colors, documentIsDark());
  applyWindowRadius(appearance.windowRadius);

  // The theme's accent beats the GSettings one. GNOME's accent-color is a
  // name from a fixed palette; a theme that defines `accent_bg_color`
  // outright has said something more specific than "blue", and on themes
  // that ship their own accent the GSettings name is often just left at the
  // default nobody changed.
  const accent = appearance.colors.accent ?? appearance.accent;
  if (accent) {
    setAccentColor(accent, accentForeground(accent));
  }
  document.body.style.fontFamily = linuxFontStack(appearance.fontFamily);

  return appearance;
}

/**
 * The desktop's appearance, kept current.
 *
 * Re-read on two signals, which between them cover how this actually changes
 * in practice:
 *
 * * The window regaining focus. Changing your accent, GTK theme or window
 *   controls means going to Settings or Tweaks and coming back, so the
 *   return trip is the moment the answer is stale — and it costs one IPC
 *   call at a point where nothing is animating.
 * * `prefers-color-scheme`. A light/dark switch changes every colour we
 *   read, and it can happen with Lookout focused (a night-light schedule,
 *   or the portal following sunset).
 *
 * This is deliberately not a GTK settings subscription. That would catch the
 * change a fraction of a second earlier, at the cost of a GTK signal handler
 * per window whose lifetime has to be managed against the webview's — and
 * being a fraction of a second late to a theme change nobody is looking at
 * yet is not worth that.
 */
export function useDesktopAppearance(
  { undecorated = false }: { undecorated?: boolean } = {},
): DesktopAppearance {
  const [appearance, setAppearance] = useState<DesktopAppearance>(DEFAULT_APPEARANCE);

  useEffect(() => {
    if (!isLinux) return;

    let cancelled = false;
    const reapply = () => {
      void applyLinuxChrome({ undecorated }).then((next) => {
        if (!cancelled) setAppearance(next);
      });
    };

    reapply();

    const scheme = window.matchMedia("(prefers-color-scheme: dark)");
    scheme.addEventListener("change", reapply);

    let unlisten: (() => void) | undefined;
    void getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        if (focused) reapply();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => {
        // No window events: the media query alone still covers the common
        // case, and the initial read already happened.
      });

    return () => {
      cancelled = true;
      scheme.removeEventListener("change", reapply);
      if (unlisten) unlisten();
    };
  }, [undecorated]);

  return appearance;
}
