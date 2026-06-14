import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { useState, useEffect } from "react";

type UpdateStatus =
  | { state: "checking" }
  | { state: "no-update"; message: string }
  | { state: "downloading"; progress: number }
  | { state: "installing" }
  | { state: "done" }
  | { state: "idle" };

export const LAST_UPDATE_KEY = "lookout_last_update_ts";
const UPDATE_COOLDOWN_MS = 60_000;
const FAIL_DISPLAY_MS = 1500; // Show failure message briefly before continuing

export function useUpdateCheck() {
  const [status, setStatus] = useState<UpdateStatus>({ state: "checking" });

  useEffect(() => {
    let cancelled = false;

    // Guard against infinite relaunch loop
    const lastUpdate = localStorage.getItem(LAST_UPDATE_KEY);
    if (lastUpdate && Date.now() - Number(lastUpdate) < UPDATE_COOLDOWN_MS) {
      console.log("[updater] skipping check — just updated");
      localStorage.removeItem(LAST_UPDATE_KEY);
      setStatus({ state: "idle" });
      return;
    }

    const continueAfterFail = (message: string) => {
      if (cancelled) return;
      setStatus({ state: "no-update", message });
      setTimeout(() => {
        if (!cancelled) setStatus({ state: "idle" });
      }, FAIL_DISPLAY_MS);
    };

    (async () => {
      try {
        const update = await check();
        if (cancelled) return;

        if (!update) {
          if (!cancelled) setStatus({ state: "idle" });
          return;
        }

        console.log(`[updater] found v${update.version}, downloading...`);
        setStatus({ state: "downloading", progress: 0 });

        let totalBytes = 0;
        let downloadedBytes = 0;
        await update.downloadAndInstall((event) => {
          if (cancelled) return;
          if (event.event === "Started" && event.data.contentLength) {
            totalBytes = event.data.contentLength;
          } else if (event.event === "Progress") {
            downloadedBytes += event.data.chunkLength;
            const progress =
              totalBytes > 0
                ? Math.round((downloadedBytes / totalBytes) * 100)
                : 0;
            setStatus({ state: "downloading", progress });
          } else if (event.event === "Finished") {
            setStatus({ state: "installing" });
          }
        });

        if (!cancelled) {
          localStorage.setItem(LAST_UPDATE_KEY, String(Date.now()));
          setStatus({ state: "done" });
          await relaunch();
        }
      } catch (e) {
        console.warn("[updater] failed:", e);
        continueAfterFail("Checking for update failed. Continuing…");
      }
    })();

    return () => {
      cancelled = true;
    };
  }, []);

  return status;
}
