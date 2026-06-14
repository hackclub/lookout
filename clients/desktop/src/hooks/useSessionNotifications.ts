import { useEffect, useRef } from "react";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";

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

interface SessionNotificationArgs {
  /** True while the capture loop is running. Used to request permission up front. */
  isCapturing: boolean;
  /** The latest capture error message, or null. */
  captureError: string | null;
  /** The current session status. */
  status: string;
}

/**
 * Watches session state and fires a native notification when a session is
 * paused, errors out, or is terminated unexpectedly by the server.
 */
export function useSessionNotifications({
  isCapturing,
  captureError,
  status,
}: SessionNotificationArgs) {
  // Request permission proactively once capture starts, while the window is
  // (usually) in the foreground — so the OS prompt doesn't surprise the user
  // later when they've switched away.
  useEffect(() => {
    if (isCapturing) void ensureNotificationPermission();
  }, [isCapturing]);

  // Notify when a capture error first appears (null -> non-null transition).
  const prevError = useRef(captureError);
  useEffect(() => {
    if (captureError && !prevError.current) {
      const firstLine = captureError.split("\n")[0];
      void notify(
        "Lookout — recording error",
        `Your screen isn't being captured: ${firstLine}`,
      );
    }
    prevError.current = captureError;
  }, [captureError]);

  // Notify on meaningful status transitions.
  const prevStatus = useRef(status);
  useEffect(() => {
    const prev = prevStatus.current;
    prevStatus.current = status;
    if (status === prev) return;
    // Only notify on a pause that interrupts active recording — not the
    // loading -> paused transition you get when simply opening an already
    // paused session.
    if (status === "paused" && (prev === "active" || prev === "pending")) {
      void notify(
        "Lookout — recording paused",
        "Your session is paused and isn't capturing screenshots.",
      );
    } else if (status === "failed") {
      void notify(
        "Lookout — session failed",
        "Your recording session ended unexpectedly.",
      );
    }
  }, [status]);
}
