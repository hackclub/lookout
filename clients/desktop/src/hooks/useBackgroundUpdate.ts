import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { useCallback, useEffect, useRef, useState } from "react";
import { LAST_UPDATE_KEY } from "./useUpdateCheck.js";

// How often to ask the update server whether a newer build exists.
const CHECK_INTERVAL_MS = 30 * 60_000; // 30 minutes

/**
 * Periodically checks the update server while the app is running and reports
 * when a newer version becomes available. Unlike useUpdateCheck — which
 * auto-updates at launch behind a full-screen gate — this never interrupts the
 * user. It only surfaces availability so the UI can show a "restart to update"
 * banner; the actual download + install is deferred until the user opts in via
 * restart(), reusing the same flow the launch-time updater uses.
 */
export function useBackgroundUpdate(enabled: boolean) {
  const [availableVersion, setAvailableVersion] = useState<string | null>(null);
  const [restarting, setRestarting] = useState(false);
  const updateRef = useRef<Update | null>(null);

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;

    const runCheck = async () => {
      if (updateRef.current) return; // already found one — nothing more to do
      try {
        const update = await check();
        if (cancelled || !update) return;
        console.log(`[updater] background check found v${update.version}`);
        updateRef.current = update;
        setAvailableVersion(update.version);
      } catch (e) {
        console.warn("[updater] background check failed:", e);
      }
    };

    // The launch-time updater already checked once, so wait a full interval
    // before the first background check.
    const id = setInterval(runCheck, CHECK_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [enabled]);

  const restart = useCallback(async () => {
    const update = updateRef.current;
    if (!update) return; // the banner only renders when we have one in hand
    setRestarting(true);
    try {
      console.log(`[updater] installing v${update.version} on user request`);
      // downloadAndInstall() and relaunch() are both Tauri's own APIs (updater
      // + process plugins) — this is the exact same sequence the launch-time
      // updater runs. relaunch() only fires after a successful install, so a
      // failed download never restarts the app.
      await update.downloadAndInstall();
      // Skip the launch-time re-check on the very next start.
      localStorage.setItem(LAST_UPDATE_KEY, String(Date.now()));
      await relaunch();
    } catch (e) {
      console.error("[updater] restart-to-update failed:", e);
      setRestarting(false);
    }
  }, []);

  return { availableVersion, restarting, restart };
}
