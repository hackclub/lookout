import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { getVersion } from "@tauri-apps/api/app";
import { ask } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "../logger.js";
import { isCaptureActive } from "../captureGuard.js";
import { isLinux } from "../platform.js";

export const LAST_UPDATE_KEY = "lookout_last_update_ts";
/** Records an update that finished downloading but was never installed. */
export const PENDING_UPDATE_KEY = "lookout_pending_update";

const UPDATE_COOLDOWN_MS = 60_000;
const CHECK_INTERVAL_MS = 30 * 60_000; // 30 minutes
/** How often to re-test the capture gate while an auto-install is held. */
const HELD_RETRY_MS = 30_000;
/**
 * How many launches may auto-install the same version before we give up and
 * fall back to the click. Two is enough to ride out a one-off failure (a
 * declined UAC prompt, a locked file) without trapping someone in a loop
 * where every launch relaunches into a broken installer.
 */
const MAX_AUTO_ATTEMPTS = 2;
/** Where we send someone whose update refuses to install. */
const RELEASES_URL = "https://github.com/hackclub/lookout/releases/latest";

export type UpdatePhase =
  | { state: "idle" }
  | { state: "downloading"; version: string; progress: number }
  | { state: "ready"; version: string }
  | { state: "installing"; version: string };

interface PendingUpdate {
  version: string;
  /** Auto-install attempts already spent on this version. */
  attempts: number;
  /** Whether we've already told the user this version won't install. */
  notified: boolean;
}

function readPending(): PendingUpdate | null {
  try {
    const raw = localStorage.getItem(PENDING_UPDATE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<PendingUpdate>;
    if (typeof parsed?.version !== "string") return null;
    return {
      version: parsed.version,
      attempts: Number(parsed.attempts) || 0,
      notified: !!parsed.notified,
    };
  } catch {
    return null;
  }
}

/**
 * Writes the marker, carrying `notified` forward for the same version unless
 * explicitly set — so re-recording an attempt can't accidentally re-arm the
 * dialog and nag someone on every launch.
 */
function savePending(version: string, attempts: number, notified?: boolean): void {
  const prev = readPending();
  const carried =
    notified ?? (prev && compareVersions(prev.version, version) === 0 ? prev.notified : false);
  localStorage.setItem(
    PENDING_UPDATE_KEY,
    JSON.stringify({ version, attempts, notified: !!carried } satisfies PendingUpdate),
  );
}

function clearPending(): void {
  localStorage.removeItem(PENDING_UPDATE_KEY);
}

/**
 * Tells the user an update won't install and offers the releases page.
 *
 * Deliberately not fired for ordinary check/download failures — those are
 * usually just a flaky network and are retried silently. This is only for a
 * broken install, which no amount of retrying will fix.
 */
async function reportStuckUpdate(version: string, detail?: unknown): Promise<void> {
  try {
    const openPage = await ask(
      `Lookout couldn't install version ${version}.\n\n` +
        `Your sessions and settings are safe — the app will keep working on this version. ` +
        `You can install the update yourself from the releases page.` +
        (detail ? `\n\nDetails: ${String(detail)}` : ""),
      {
        title: "Update couldn't be installed",
        kind: "warning",
        okLabel: "Open Releases Page",
        cancelLabel: "Not Now",
      },
    );
    if (openPage) await invoke("open_external_url", { url: RELEASES_URL });
  } catch (e) {
    console.error("[updater] couldn't show the failed-update dialog:", e);
  }
}

/** Compares `a.b.c` versions, tolerating a leading `v`. */
function compareVersions(a: string, b: string): number {
  const pa = a.replace(/^v/, "").split(".");
  const pb = b.replace(/^v/, "").split(".");
  for (let i = 0; i < 3; i++) {
    const diff = (Number(pa[i]) || 0) - (Number(pb[i]) || 0);
    if (diff !== 0) return diff;
  }
  return 0;
}

/**
 * Ghostty-style background updater. Checks at launch and every 30 minutes,
 * downloads a found update in the background (progress surfaces in the
 * titlebar pill), then offers "Restart to Complete Update".
 *
 * Plenty of people never click that pill, so a skipped update is remembered:
 * if a previous session downloaded an update and quit without installing it,
 * the next launch installs it on its own once the download completes. The
 * downloaded bytes themselves cannot be reused — the plugin holds them in an
 * in-memory resource that dies with the process — so the launch that installs
 * re-downloads first, which is why the pill still shows progress.
 *
 * Three things make the automatic path safe to fire without asking:
 * - it never runs while a capture is live, because installing kills the
 *   process on Windows (see captureGuard);
 * - it is skipped on Linux, where a deb/rpm install escalates via pkexec and
 *   would spring a password prompt on someone who just opened the app; and
 * - it gives up after MAX_AUTO_ATTEMPTS on one version, telling the user and
 *   falling back to the click rather than relaunching into a loop.
 *
 * Only Windows exits inside install(); on macOS and Linux it returns and
 * relaunch() does the restart.
 */
export function useAppUpdate(): { phase: UpdatePhase; restart: () => void } {
  const [phase, setPhase] = useState<UpdatePhase>({ state: "idle" });
  const updateRef = useRef<Update | null>(null);
  const demoRef = useRef(false);
  const installRef = useRef<((update: Update, autoAttempt: number | null) => Promise<void>) | null>(null);

  // Dev-only: run `__updatePillDemo()` in the webview console to watch the
  // full pill lifecycle (enter → download progress → ready → restart → exit)
  // without a real update. Clicking the pill in demo mode fakes the restart.
  useEffect(() => {
    if (!import.meta.env.DEV) return;
    (window as unknown as Record<string, unknown>).__updatePillDemo = () => {
      demoRef.current = true;
      let p = 0;
      setPhase({ state: "downloading", version: "9.9.9", progress: 0 });
      const id = setInterval(() => {
        p += 1 + Math.random() * 4;
        if (p >= 100) {
          clearInterval(id);
          setPhase({ state: "ready", version: "9.9.9" });
        } else {
          setPhase({ state: "downloading", version: "9.9.9", progress: Math.round(p) });
        }
      }, 80);
    };
    return () => {
      delete (window as unknown as Record<string, unknown>).__updatePillDemo;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    let heldTimer: ReturnType<typeof setInterval> | undefined;

    /**
     * `autoAttempt` is the attempt number to persist for an automatic
     * install, or null for a user-initiated one.
     */
    const install = async (update: Update, autoAttempt: number | null) => {
      // Persist BEFORE install(). On Windows install() never returns — it
      // launches the NSIS installer and calls std::process::exit(0) — so
      // every line after it is dead code there. The attempt counter and the
      // relaunch cooldown both have to be on disk before we jump, or a
      // failing installer would be retried on every launch forever.
      const prior = readPending();
      const spent =
        prior && compareVersions(prior.version, update.version) === 0 ? prior.attempts : 0;
      localStorage.setItem(LAST_UPDATE_KEY, String(Date.now()));
      // The marker is kept even for a user-initiated install. On Windows a
      // failed install can't be caught — the process is gone before the catch
      // runs — so the marker is the only way the next launch learns the
      // update never landed. It clears itself once we boot the new version.
      // A manual attempt doesn't burn an automatic one.
      savePending(update.version, autoAttempt ?? spent);

      setPhase({ state: "installing", version: update.version });
      try {
        // The bytes are already on disk, so install() is near-instant.
        // relaunch() only fires after a successful install, and only on
        // macOS/Linux — on Windows install() exits the process itself.
        await update.install();
        await relaunch();
      } catch (e) {
        console.error("[updater] install failed:", e);
        if (!cancelled) setPhase({ state: "ready", version: update.version });

        if (autoAttempt === null) {
          // The user clicked and watched it fail — always explain.
          await reportStuckUpdate(update.version, e);
        } else if (autoAttempt >= MAX_AUTO_ATTEMPTS && !readPending()?.notified) {
          // Out of automatic attempts. Mark before awaiting so a dialog the
          // user leaves open can't be raised twice.
          savePending(update.version, autoAttempt, true);
          await reportStuckUpdate(update.version, e);
        }
      }
    };
    installRef.current = install;

    /**
     * Called once the download lands: either install without being asked
     * (the user skipped this update last session) or park the pill.
     */
    const settle = async (update: Update, armed: boolean) => {
      const pending = readPending();
      const attempts =
        pending && compareVersions(pending.version, update.version) === 0 ? pending.attempts : 0;

      // On Linux a deb/rpm install escalates through pkexec, so installing
      // unprompted would throw a surprise password dialog at someone who
      // just opened the app. Automatic installs stay Windows/macOS-only;
      // Linux keeps the click, which is a moment the user chose.
      const canAutoInstall = armed && !isLinux;

      if (!canAutoInstall || attempts >= MAX_AUTO_ATTEMPTS) {
        if (armed && attempts >= MAX_AUTO_ATTEMPTS) {
          console.warn(
            `[updater] v${update.version} already failed ${attempts} auto-install attempt(s) — waiting for a click`,
          );
        }
        setPhase({ state: "ready", version: update.version });
        savePending(update.version, attempts);
        return;
      }

      if (isCaptureActive()) {
        // Installing would hard-exit the process and lose the recording.
        // Hold the pill up and take the update the moment capture stops.
        console.log("[updater] auto-install held — capture in progress");
        setPhase({ state: "ready", version: update.version });
        savePending(update.version, attempts);
        heldTimer = setInterval(() => {
          if (cancelled || isCaptureActive()) return;
          clearInterval(heldTimer);
          console.log("[updater] capture stopped — resuming auto-install");
          void install(update, attempts + 1);
        }, HELD_RETRY_MS);
        return;
      }

      console.log(`[updater] auto-installing v${update.version} — skipped last session`);
      await install(update, attempts + 1);
    };

    const runCheck = async (armed: boolean) => {
      if (updateRef.current) return; // already downloading or downloaded
      try {
        const update = await check();
        if (cancelled || !update) return;
        updateRef.current = update;
        console.log(`[updater] found v${update.version}, downloading in background`);
        setPhase({ state: "downloading", version: update.version, progress: 0 });

        let totalBytes = 0;
        let downloadedBytes = 0;
        await update.download((event) => {
          if (cancelled) return;
          if (event.event === "Started" && event.data.contentLength) {
            totalBytes = event.data.contentLength;
          } else if (event.event === "Progress") {
            downloadedBytes += event.data.chunkLength;
            const progress =
              totalBytes > 0
                ? Math.round((downloadedBytes / totalBytes) * 100)
                : 0;
            setPhase({ state: "downloading", version: update.version, progress });
          }
        });
        if (cancelled) return;

        console.log(`[updater] v${update.version} downloaded`);
        await settle(update, armed);
      } catch (e) {
        console.warn("[updater] background update failed:", e);
        // Clear the ref so the next interval tick retries from scratch.
        updateRef.current = null;
        if (!cancelled) setPhase({ state: "idle" });
      }
    };

    let interval: ReturnType<typeof setInterval> | undefined;

    void (async () => {
      // Right after an update-relaunch, skip the immediate re-check so a bad
      // update manifest can't relaunch-loop the app.
      let skipFirstCheck = false;
      const lastUpdate = localStorage.getItem(LAST_UPDATE_KEY);
      if (lastUpdate && Date.now() - Number(lastUpdate) < UPDATE_COOLDOWN_MS) {
        console.log("[updater] skipping immediate check — just updated");
        localStorage.removeItem(LAST_UPDATE_KEY);
        skipFirstCheck = true;
      }

      // A marker left over from a previous session means an update was
      // downloaded and never installed — unless we're already running it, in
      // which case it landed and the marker is stale.
      let armed = false;
      const pending = readPending();
      if (pending) {
        try {
          const current = await getVersion();
          if (compareVersions(pending.version, current) <= 0) {
            console.log(`[updater] v${pending.version} already installed — clearing marker`);
            clearPending();
          } else {
            armed = true;
            // We spent every automatic attempt on this version last time and
            // we're still on the old one, so it is genuinely stuck. Say so
            // now rather than after silently re-downloading it. Not awaited:
            // the check shouldn't block behind a dialog the user may ignore,
            // and the pill still offers a manual retry behind it.
            if (pending.attempts >= MAX_AUTO_ATTEMPTS && !pending.notified) {
              savePending(pending.version, pending.attempts, true);
              void reportStuckUpdate(pending.version);
            }
          }
        } catch {
          // Can't read the running version — assume the update is still due.
          armed = true;
        }
      }
      if (cancelled) return;

      if (!skipFirstCheck) await runCheck(armed);
      if (cancelled) return;
      // Only the launch check installs on its own; a mid-session check that
      // finds a brand new version still waits for the click.
      interval = setInterval(() => void runCheck(false), CHECK_INTERVAL_MS);
    })();

    return () => {
      cancelled = true;
      installRef.current = null;
      if (interval) clearInterval(interval);
      if (heldTimer) clearInterval(heldTimer);
    };
  }, []);

  const restart = useCallback(async () => {
    if (demoRef.current) {
      demoRef.current = false;
      setPhase({ state: "idle" });
      return;
    }
    const update = updateRef.current;
    if (!update) return; // the pill only becomes clickable once downloaded
    await installRef.current?.(update, null);
  }, []);

  return { phase, restart };
}
