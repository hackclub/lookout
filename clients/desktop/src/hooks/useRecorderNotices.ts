import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import type { RecorderNotice } from "../recorder.js";

// Cache the permission result across the app — we only need to ask once.
let permissionState: "granted" | "denied" | "unknown" = "unknown";
let permissionInFlight: Promise<boolean> | null = null;

/** Ensure we have notification permission, requesting it once if needed. */
export async function ensureNotificationPermission(): Promise<boolean> {
  if (permissionState === "granted") return true;
  if (permissionState === "denied") return false;
  if (permissionInFlight) return permissionInFlight;

  permissionInFlight = (async () => {
    try {
      let granted = await isPermissionGranted();
      if (!granted) {
        granted = (await requestPermission()) === "granted";
      }
      permissionState = granted ? "granted" : "denied";
      return granted;
    } catch (e) {
      console.warn("[notify] permission check failed:", e);
      permissionState = "denied";
      return false;
    } finally {
      permissionInFlight = null;
    }
  })();
  return permissionInFlight;
}

/**
 * Fire a native OS notification. Fires regardless of window focus — getting a
 * notification when you pressed pause yourself is harmless, and always firing
 * keeps the logic simple and guarantees the background case is covered.
 */
async function notify(title: string, body: string) {
  if (!(await ensureNotificationPermission())) return;
  try {
    sendNotification({ title, body });
  } catch (e) {
    console.warn("[notify] sendNotification failed:", e);
  }
}

/**
 * Turn the core's notices — a pause that interrupts recording, a capture
 * failure, a lost source, a session the server ended — into native OS
 * notifications. The core decides what is worth one; this only delivers.
 */
export function useRecorderNotices(capturing: boolean) {
  // Request permission proactively once capture starts, while the window is
  // (usually) in the foreground — so the OS prompt doesn't surprise the user
  // later when they've switched away.
  useEffect(() => {
    if (capturing) void ensureNotificationPermission();
  }, [capturing]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<RecorderNotice>("recorder-notice", (event) => {
      void notify(event.payload.title, event.payload.body);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, []);
}
