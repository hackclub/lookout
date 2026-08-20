/**
 * Resize borders for a window that no longer has any.
 *
 * Taking GTK's decorations away (which is what buys us a real header bar)
 * also takes away the invisible frame you grab to resize. GTK apps that go
 * client-side draw that frame themselves; so do we. Eight strips around the
 * edge, each handing off to the compositor's own resize-drag the moment the
 * pointer goes down — from there the window manager is driving, so snapping,
 * aspect constraints and multi-monitor all behave exactly as they would on a
 * decorated window.
 *
 * Only mounted on Linux, and only for windows that are both undecorated and
 * resizable. The main window is a fixed 480×640 and needs none of this.
 */
import React, { useEffect, useState } from "react";
import { getCurrentWindow, currentMonitor } from "@tauri-apps/api/window";
import { FRAME_INSET, RESIZE_BAND, syncWindowFrame } from "../linuxChrome.js";

/** Mirrors Tauri's ResizeDirection, which the package declares but doesn't export. */
type ResizeDirection =
  | "East" | "North" | "NorthEast" | "NorthWest"
  | "South" | "SouthEast" | "SouthWest" | "West";

/**
 * The strips sit in the band of the frame that still accepts input, not at
 * the window's real edge — everything outside that band is passed through
 * to the desktop (window_shape.rs), so a handle out there could never be
 * clicked.
 *
 * `FRAME_INSET` is 0 when the shell draws the frame for us: there is no
 * transparent margin then, so the strips sit at the visible edge and the
 * grab area comes out of the content instead of out of the frame. That is
 * the same trade any undecorated window without an invisible border makes,
 * and 8px is thinner than the border GTK would have given us.
 */
const GRAB = RESIZE_BAND;
const INSET = FRAME_INSET;

/**
 * Corners reach a little further in than the edges do, as they do in GTK.
 * Kept modest on purpose: they overlap the visible window, and a larger
 * value would start swallowing clicks on the header bar's own buttons.
 */
const CORNER = RESIZE_BAND + 6;

interface Zone {
  direction: ResizeDirection;
  cursor: string;
  style: React.CSSProperties;
}

const ZONES: Zone[] = [
  { direction: "North", cursor: "n-resize", style: { top: INSET, left: INSET, right: INSET, height: GRAB } },
  { direction: "South", cursor: "s-resize", style: { bottom: INSET, left: INSET, right: INSET, height: GRAB } },
  { direction: "West", cursor: "w-resize", style: { top: INSET, bottom: INSET, left: INSET, width: GRAB } },
  { direction: "East", cursor: "e-resize", style: { top: INSET, bottom: INSET, right: INSET, width: GRAB } },
  { direction: "NorthWest", cursor: "nw-resize", style: { top: INSET, left: INSET, width: CORNER, height: CORNER } },
  { direction: "NorthEast", cursor: "ne-resize", style: { top: INSET, right: INSET, width: CORNER, height: CORNER } },
  { direction: "SouthWest", cursor: "sw-resize", style: { bottom: INSET, left: INSET, width: CORNER, height: CORNER } },
  { direction: "SouthEast", cursor: "se-resize", style: { bottom: INSET, right: INSET, width: CORNER, height: CORNER } },
];

/**
 * Fallback for when the native side can't report the window's state.
 *
 * A geometry guess: a window the WM has sized usually matches the work area
 * on at least one axis. It cannot see a quarter-tile, which is exactly why
 * it is only the fallback — `syncWindowFrame` asks the compositor.
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
    // that took the editor down with it would not be.
    console.warn("[csd] could not read window geometry:", e);
    return false;
  }
}

/**
 * Keep the window's frame in step with the compositor: drawn while the
 * window floats, collapsed the moment the window manager takes over sizing
 * it.
 *
 * Both halves matter. Rounded corners on a maximized window leave four
 * notches of desktop showing through, and a transparent frame under a
 * tiling WM becomes a gap between neighbouring windows rather than a
 * shadow.
 */
export function useWindowFrameState(): void {
  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const sync = async () => {
      // One round trip: applies the input shape (recomputed from the current
      // size, so this has to run on every resize) and reports whether the
      // compositor is sizing us.
      const reported = await syncWindowFrame(FRAME_INSET);
      const collapsed = reported ?? (await isFlushWithWorkArea());
      if (cancelled) return;
      document.documentElement.classList.toggle("lookout-snapped", collapsed);
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

export function WindowResizeHandles() {
  // A snapped or maximized window has no edge to drag — the compositor owns
  // those edges — and leaving live handles there would swallow clicks on
  // content sitting underneath them.
  const [snapped, setSnapped] = useState(false);
  useEffect(() => {
    const target = document.documentElement;
    const observer = new MutationObserver(() => {
      setSnapped(target.classList.contains("lookout-snapped"));
    });
    observer.observe(target, { attributes: true, attributeFilter: ["class"] });
    setSnapped(target.classList.contains("lookout-snapped"));
    return () => observer.disconnect();
  }, []);

  if (snapped) return null;

  return (
    <>
      {ZONES.map((zone) => (
        <div
          key={zone.direction}
          aria-hidden="true"
          onMouseDown={(e) => {
            // Left button only: a right-click here should fall through to
            // the window menu, not start a resize.
            if (e.button !== 0) return;
            e.preventDefault();
            void getCurrentWindow().startResizeDragging(zone.direction).catch((err) => {
              console.warn(`[csd] resize drag (${zone.direction}) failed:`, err);
            });
          }}
          style={{
            position: "fixed",
            zIndex: 10000,
            cursor: zone.cursor,
            ...zone.style,
          }}
        />
      ))}
    </>
  );
}
