/**
 * A client-side-decorated header bar, in the shape GTK apps use.
 *
 * On Linux the window is undecorated (see `set_decorations(false)` in
 * lib.rs), so this bar *is* the titlebar: title and subtitle on the leading
 * edge, a single close button trailing, page actions beside it.
 *
 * It is deliberately flat — no fill, no bottom border. A header that paints
 * itself a different colour reads as a separate thing stacked on the app,
 * which is the exact impression a GTK titlebar over a web page gave. Sharing
 * the window's surface is what makes it read as part of the app.
 *
 * The bar is only ever rendered on Linux. macOS keeps its overlay titlebar
 * and traffic lights; Windows keeps its own decorations.
 */
import React from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { colors, spacing, fontSize, fontWeight } from "@lookout/react";
import { HEADER_BAR_HEIGHT, type DesktopAppearance } from "../linuxChrome.js";

const CONTROL_SIZE = 24;

/**
 * Hover and focus states for the controls. Written as a stylesheet rather
 * than React state because a `:hover` that has to round-trip through a
 * re-render lags behind the pointer noticeably on WebKitGTK.
 */
const HEADER_BAR_CSS = `
  .lookout-headerbar-control {
    width: ${CONTROL_SIZE}px;
    height: ${CONTROL_SIZE}px;
    border: none;
    border-radius: 50%;
    padding: 0;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    cursor: default;
    color: var(--color-text-primary);
    background: var(--color-headerbar-control);
    transition: background 100ms ease-out;
  }
  .lookout-headerbar-control:hover {
    background: var(--color-headerbar-control-hover);
  }
  .lookout-headerbar-control:active {
    background: var(--color-border-hover);
  }
  .lookout-headerbar-control:focus-visible {
    outline: 2px solid var(--color-accent);
    outline-offset: 1px;
  }
  /* Page actions. Flat and lightly rounded, the way Adwaita draws the
     buttons either side of a header bar title — not the app's own Button,
     whose inner padding is fixed at 6px 12px and would crush a 20px icon
     into a 6px sliver at this size. */
  .lookout-headerbar-action {
    width: 30px;
    height: 30px;
    border: none;
    /* Circular, matching the close button beside them — libadwaita rounds
       its header-bar buttons fully, and a rounded rectangle next to a
       circle reads as two different button systems in one bar. */
    border-radius: 50%;
    padding: 0;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    cursor: default;
    color: var(--color-text-secondary);
    background: transparent;
    transition: background 100ms ease-out, color 100ms ease-out;
  }
  .lookout-headerbar-action:hover {
    background: var(--color-headerbar-control);
    color: var(--color-text-primary);
  }
  .lookout-headerbar-action:active {
    background: var(--color-headerbar-control-hover);
  }
  .lookout-headerbar-action:focus-visible {
    outline: 2px solid var(--color-accent);
    outline-offset: 1px;
  }
`;

// Injected on import, not from an effect. Effects run after the first paint,
// so an effect-injected sheet leaves one frame where these are unstyled
// <button> elements wearing the UA stylesheet — grey, rectangular, bordered.
// On a dark header that reads as the buttons flashing white on every launch.
if (typeof document !== "undefined" && !document.querySelector("style[data-lookout-headerbar]")) {
  const style = document.createElement("style");
  style.setAttribute("data-lookout-headerbar", "");
  style.textContent = HEADER_BAR_CSS;
  document.head.appendChild(style);
}

function BackIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M10 3L5 8l5 5" />
    </svg>
  );
}

function CloseIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" aria-hidden="true">
      <path d="M4 4l8 8M12 4l-8 8" />
    </svg>
  );
}

export interface HeaderBarProps {
  /**
   * Whether double-clicking the bar should toggle maximize, the way a GTK
   * titlebar does. Off for the fixed-size main window, which can't maximize.
   */
  maximizable?: boolean;
  /** The window title. Usually the current page, GNOME-style. */
  title: string;
  /**
   * A quieter second line under the title — a count, a session name, the
   * state of the thing on screen. Omit it and the title centres itself
   * vertically on its own.
   */
  subtitle?: string;
  /** Which edge the user keeps their window controls on, read from GSettings. */
  appearance: DesktopAppearance;
  /** Page actions, placed on the edge opposite the window controls. */
  actions?: React.ReactNode;
  /**
   * Shown as a back button ahead of the title. Pages publish this through
   * headerNav rather than drawing their own — see HeaderNavProvider.
   */
  onBack?: () => void;
  /**
   * What the close button does. Defaults to asking the window to close,
   * which fires `onCloseRequested` — the editor hangs its publish-on-close
   * confirmation off that, so overriding is rarely what you want.
   */
  onClose?: () => void;
}

export function HeaderBar({ title, subtitle, appearance, actions, onBack, onClose, maximizable = false }: HeaderBarProps) {
  // One button, no minimize and no maximize. That's what GNOME ships now:
  // the window menu and the keyboard still do everything else, and a row of
  // three controls is the look this was meant to get away from. Which *edge*
  // it sits on is the user's call, though — see `close_on_trailing_edge`.
  const closeButton = (
    <button
      type="button"
      className="lookout-headerbar-control"
      aria-label="Close"
      title="Close"
      onClick={() => {
        if (onClose) onClose();
        else void getCurrentWindow().close().catch(() => {});
      }}
    >
      <CloseIcon />
    </button>
  );

  const backButton = onBack && (
    <button
      type="button"
      className="lookout-headerbar-action"
      aria-label="Back"
      title="Back"
      onClick={onBack}
      style={{ flexShrink: 0 }}
    >
      <BackIcon />
    </button>
  );

  // The title sits on the leading edge rather than centred — two lines when
  // there's a subtitle, one when there isn't, and the block stays optically
  // centred either way because it's the flex item that defines the height.
  const titleBlock = (
    <div
      data-tauri-drag-region
      style={{
        display: "flex",
        flexDirection: "column",
        justifyContent: "center",
        minWidth: 0,
        // No gap, tight leading: the two lines are one label, not a stack.
        gap: 0,
        pointerEvents: "none",
      }}
    >
      <span
        style={{
          fontSize: subtitle ? fontSize.md : fontSize.lg,
          fontWeight: fontWeight.bold,
          lineHeight: 1.15,
          color: colors.text.primary,
          whiteSpace: "nowrap",
          overflow: "hidden",
          textOverflow: "ellipsis",
        }}
      >
        {title}
      </span>
      {subtitle && (
        <span
          style={{
            fontSize: fontSize.xs,
            fontWeight: fontWeight.normal,
            lineHeight: 1.15,
            color: colors.text.secondary,
            whiteSpace: "nowrap",
            overflow: "hidden",
            textOverflow: "ellipsis",
          }}
        >
          {subtitle}
        </span>
      )}
    </div>
  );

  const pageActions = (
    <div style={{ display: "flex", alignItems: "center", gap: spacing.xs, flexShrink: 0 }}>
      {actions}
      {appearance.controlsOnRight && closeButton}
    </div>
  );

  return (
    <div
      data-tauri-drag-region
      className="lookout-headerbar"
      onDoubleClick={maximizable ? () => {
        void getCurrentWindow().toggleMaximize().catch(() => {});
      } : undefined}
      style={{
        position: "relative",
        flexShrink: 0,
        height: HEADER_BAR_HEIGHT,
        display: "flex",
        alignItems: "center",
        gap: spacing.sm,
        padding: onBack ? `0 ${spacing.sm}px` : `0 ${spacing.sm}px 0 ${spacing.md}px`,
        boxSizing: "border-box",
        // No fill and no divider: the bar is the top of the window, not a
        // strip bolted to it. This is how libadwaita's flat header bars read
        // — the title and the close button appear to float on the content's
        // own surface, and the window looks like one object.
        background: "transparent",
        cursor: "default",
        zIndex: 10,
      }}
    >
      {/* The close button leads instead when the user has moved their window
          controls to the left, which GNOME allows and some distros default to. */}
      {!appearance.controlsOnRight && closeButton}
      {backButton}
      {titleBlock}
      <div data-tauri-drag-region style={{ flex: 1, alignSelf: "stretch" }} />
      {pageActions}
    </div>
  );
}
