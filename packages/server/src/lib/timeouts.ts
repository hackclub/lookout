import { sql, eq, and, lt, isNotNull, isNull, inArray } from "drizzle-orm";
import { DeleteObjectCommand } from "@aws-sdk/client-s3";
import { db, schema } from "../db/index.js";
import { r2Client, R2_BUCKET } from "../config/r2.js";
import {
  boss,
  COMPILE_JOB,
  CHECK_TIMEOUTS_JOB,
  CLEANUP_UNCONFIRMED_JOB,
  CLEANUP_SCREENSHOTS_JOB,
} from "./queue.js";
import { cleanupRateLimits } from "./timing.js";
import { publishHeldSession } from "./publish.js";
import {
  AUTO_PAUSE_AFTER_MINUTES,
  AUTO_STOP_AFTER_MINUTES,
  UNCONFIRMED_CLEANUP_AFTER_MINUTES,
  STUCK_COMPILING_TIMEOUT_MINUTES,
  MAX_COMPILE_ATTEMPTS,
  SCREENSHOT_RETENTION_DAYS,
  EDIT_WINDOW_DAYS,
  EDIT_HOLD_MAX_MINUTES,
} from "@lookout/shared";

/**
 * Register periodic jobs with pgBoss.
 */
export async function registerTimeoutJobs() {
  // Create queues first (pgBoss requires queues to exist before scheduling)
  await boss.createQueue(COMPILE_JOB);
  await boss.createQueue(CHECK_TIMEOUTS_JOB);
  await boss.createQueue(CLEANUP_UNCONFIRMED_JOB);

  // Check timeouts every minute
  await boss.schedule(CHECK_TIMEOUTS_JOB, "* * * * *");
  await boss.work(CHECK_TIMEOUTS_JOB, async () => {
    await checkTimeouts();
    cleanupRateLimits();
  });

  // Cleanup unconfirmed screenshots every 5 minutes
  await boss.schedule(CLEANUP_UNCONFIRMED_JOB, "*/5 * * * *");
  await boss.work(CLEANUP_UNCONFIRMED_JOB, async () => {
    await cleanupUnconfirmed();
  });

  // Cleanup screenshots for completed sessions daily at 3am UTC
  await boss.createQueue(CLEANUP_SCREENSHOTS_JOB);
  await boss.schedule(CLEANUP_SCREENSHOTS_JOB, "0 3 * * *");
  await boss.work(CLEANUP_SCREENSHOTS_JOB, async () => {
    await cleanupCompletedScreenshots();
  });
}

/**
 * Publish sessions whose edit lease lapsed — nothing has said "still
 * editing" for a lease term — or that hit the absolute ceiling.
 *
 * This is the promise that makes "Edit & Save" safe to offer: a user who
 * closes the app mid-edit still gets their timelapse, uncut, about a lease
 * later. The hold can delay publication, never cancel it.
 */
async function publishExpiredHolds() {
  const now = new Date();
  const ceilingCutoff = new Date(
    now.getTime() - EDIT_HOLD_MAX_MINUTES * 60_000,
  );

  const expired = await db
    .select({
      id: schema.sessions.id,
      cuts: schema.sessions.cuts,
      masks: schema.sessions.masks,
    })
    .from(schema.sessions)
    .where(
      and(
        eq(schema.sessions.status, "stopped"),
        isNotNull(schema.sessions.editHoldUntil),
        sql`(${schema.sessions.editHoldUntil} < ${now}
             OR ${schema.sessions.stoppedAt} < ${ceilingCutoff})`,
      ),
    );

  for (const session of expired) {
    const hasEdits =
      ((session.cuts as unknown[] | null)?.length ?? 0) > 0 ||
      ((session.masks as unknown[] | null)?.length ?? 0) > 0;

    if (hasEdits) {
      const [claimed] = await db
        .update(schema.sessions)
        .set({
          status: "compiling",
          editHoldUntil: null,
          updatedAt: new Date(),
        })
        .where(
          and(
            eq(schema.sessions.id, session.id),
            eq(schema.sessions.status, "stopped"),
          ),
        )
        .returning({ id: schema.sessions.id });

      if (claimed) {
        await boss.send(COMPILE_JOB, { sessionId: session.id });
        console.log(`[edit-hold] enqueued compile for ${session.id} with edits (hold expired)`);
      }
      continue;
    }
    // The original exists once the build lands; if the compile is still
    // running (or failed), leave the row alone — the build path publishes
    // directly when it finds no live hold, and the stuck-compiling timeout
    // covers genuine failures.
    const published = await publishHeldSession(session.id);
    if (published) {
      console.log(`[edit-hold] auto-published ${session.id} (hold expired)`);
    } else {
      // No original yet: drop the hold so the next compile publishes
      // normally instead of the session sitting in limbo.
      await db
        .update(schema.sessions)
        .set({ editHoldUntil: null, updatedAt: new Date() })
        .where(
          and(
            eq(schema.sessions.id, session.id),
            eq(schema.sessions.status, "stopped"),
            isNull(schema.sessions.originalVideoR2Key),
          ),
        );
    }
  }
}

async function checkTimeouts() {
  const now = new Date();

  await publishExpiredHolds();

  // Auto-pause: active sessions with no screenshots for AUTO_PAUSE_AFTER_MINUTES
  const autoPauseThreshold = new Date(
    now.getTime() - AUTO_PAUSE_AFTER_MINUTES * 60_000,
  );

  const toPause = await db
    .select({ id: schema.sessions.id, startedAt: schema.sessions.startedAt, resumedAt: schema.sessions.resumedAt, totalActiveSeconds: schema.sessions.totalActiveSeconds })
    .from(schema.sessions)
    .where(
      and(
        eq(schema.sessions.status, "active"),
        lt(
          sql`COALESCE(${schema.sessions.lastScreenshotAt}, ${schema.sessions.startedAt}, ${schema.sessions.createdAt})`,
          autoPauseThreshold,
        ),
      ),
    );

  for (const session of toPause) {
    const activeFrom = session.resumedAt || session.startedAt!;
    const additionalSeconds = Math.floor(
      (now.getTime() - activeFrom.getTime()) / 1000,
    );

    await db
      .update(schema.sessions)
      .set({
        status: "paused",
        pausedAt: now,
        totalActiveSeconds: session.totalActiveSeconds + additionalSeconds,
        updatedAt: now,
      })
      .where(eq(schema.sessions.id, session.id));
  }

  // Auto-stop: any active/paused session with no screenshots for AUTO_STOP_AFTER_MINUTES
  const autoStopThreshold = new Date(
    now.getTime() - AUTO_STOP_AFTER_MINUTES * 60_000,
  );

  const toStop = await db
    .select({
      id: schema.sessions.id,
      status: schema.sessions.status,
      startedAt: schema.sessions.startedAt,
      resumedAt: schema.sessions.resumedAt,
      totalActiveSeconds: schema.sessions.totalActiveSeconds,
      trackingMode: schema.sessions.trackingMode,
      trackedSeconds: schema.sessions.trackedSeconds,
    })
    .from(schema.sessions)
    .where(
      and(
        inArray(schema.sessions.status, ["active", "paused", "pending"]),
        lt(
          sql`COALESCE(${schema.sessions.lastScreenshotAt}, ${schema.sessions.startedAt}, ${schema.sessions.createdAt})`,
          autoStopThreshold,
        ),
      ),
    );

  for (const session of toStop) {
    let totalActiveSeconds = session.totalActiveSeconds;
    if (session.status === "active" && session.startedAt) {
      const activeFrom = session.resumedAt || session.startedAt;
      totalActiveSeconds += Math.floor(
        (now.getTime() - activeFrom.getTime()) / 1000,
      );
    }

    // Compute tracked seconds before stopping. Credit-mode sessions
    // maintain the value incrementally; bucket-mode computes live.
    let trackedSeconds: number;
    if (session.trackingMode === "credit") {
      trackedSeconds = session.trackedSeconds ?? 0;
    } else {
      const [{ buckets }] = await db
        .select({
          buckets: sql<number>`count(distinct ${schema.screenshots.minuteBucket})`,
        })
        .from(schema.screenshots)
        .where(
          and(
            eq(schema.screenshots.sessionId, session.id),
            eq(schema.screenshots.confirmed, true),
          ),
        );
      trackedSeconds = Math.max(0, (Number(buckets) - 1) * 60);
    }

    await db
      .update(schema.sessions)
      .set({
        status: "stopped",
        stoppedAt: now,
        totalActiveSeconds,
        trackedSeconds,
        updatedAt: now,
      })
      .where(eq(schema.sessions.id, session.id));

    // Enqueue compilation if screenshots exist, otherwise mark complete
    const [{ count }] = await db
      .select({ count: sql<number>`count(*)::int` })
      .from(schema.screenshots)
      .where(
        and(
          eq(schema.screenshots.sessionId, session.id),
          eq(schema.screenshots.confirmed, true),
        ),
      );

    if (count > 0) {
      await boss.send(COMPILE_JOB, { sessionId: session.id });
    } else {
      await db
        .update(schema.sessions)
        .set({ status: "failed", updatedAt: now })
        .where(eq(schema.sessions.id, session.id));
    }
  }

  // Detect stuck compiling sessions (>1 hour)
  const stuckThreshold = new Date(now.getTime() - STUCK_COMPILING_TIMEOUT_MINUTES * 60_000);
  const stuck = await db
    .select({ id: schema.sessions.id, compileAttempts: schema.sessions.compileAttempts })
    .from(schema.sessions)
    .where(
      and(
        eq(schema.sessions.status, "compiling"),
        lt(schema.sessions.updatedAt, stuckThreshold),
      ),
    );

  for (const session of stuck) {
    if (session.compileAttempts >= MAX_COMPILE_ATTEMPTS) {
      await db
        .update(schema.sessions)
        .set({ status: "failed", updatedAt: now })
        .where(
          and(
            eq(schema.sessions.id, session.id),
            eq(schema.sessions.status, "compiling"),
          ),
        );
      continue;
    }

    const [reset] = await db
      .update(schema.sessions)
      .set({
        status: "stopped",
        compileAttempts: session.compileAttempts + 1,
        updatedAt: now,
      })
      .where(
        and(
          eq(schema.sessions.id, session.id),
          eq(schema.sessions.status, "compiling"),
        ),
      )
      .returning({ id: schema.sessions.id });

    if (reset) {
      await boss.send(COMPILE_JOB, { sessionId: session.id });
    }
  }
}

async function cleanupUnconfirmed() {
  const threshold = new Date(
    Date.now() - UNCONFIRMED_CLEANUP_AFTER_MINUTES * 60_000,
  );

  // Find unconfirmed screenshot records older than threshold
  const stale = await db
    .select({ id: schema.screenshots.id, r2Key: schema.screenshots.r2Key })
    .from(schema.screenshots)
    .where(
      and(
        eq(schema.screenshots.confirmed, false),
        lt(schema.screenshots.createdAt, threshold),
      ),
    );

  // Delete orphaned R2 objects to prevent storage abuse
  for (const ss of stale) {
    try {
      await r2Client.send(
        new DeleteObjectCommand({ Bucket: R2_BUCKET, Key: ss.r2Key }),
      );
    } catch {
      // Non-fatal: object may not exist if upload never completed
    }
  }

  // Delete the database records
  if (stale.length > 0) {
    await db
      .delete(schema.screenshots)
      .where(
        and(
          eq(schema.screenshots.confirmed, false),
          lt(schema.screenshots.createdAt, threshold),
        ),
      );
  }
}

async function cleanupCompletedScreenshots() {
  const threshold = new Date(
    Date.now() - SCREENSHOT_RETENTION_DAYS * 24 * 60 * 60_000,
  );

  // Find completed sessions with a video that stopped more than
  // SCREENSHOT_RETENTION_DAYS ago and whose R2 objects haven't been purged yet.
  // We delete the R2 image objects but KEEP the screenshot rows so capture
  // timings (GET /timings) stay queryable indefinitely; screenshotsPurgedAt
  // gates this so already-purged sessions aren't reprocessed every run.
  const sessions = await db
    .select({ id: schema.sessions.id })
    .from(schema.sessions)
    .where(
      and(
        eq(schema.sessions.status, "complete"),
        isNotNull(schema.sessions.videoR2Key),
        lt(schema.sessions.stoppedAt, threshold),
        isNull(schema.sessions.screenshotsPurgedAt),
      ),
    );

  for (const session of sessions) {
    const screenshots = await db
      .select({ id: schema.screenshots.id, r2Key: schema.screenshots.r2Key })
      .from(schema.screenshots)
      .where(eq(schema.screenshots.sessionId, session.id));

    // Delete R2 objects — DeleteObject is idempotent (204 whether object exists or not)
    for (const ss of screenshots) {
      try {
        await r2Client.send(
          new DeleteObjectCommand({ Bucket: R2_BUCKET, Key: ss.r2Key }),
        );
      } catch {
        // Non-fatal: log and continue, will be retried next run
        console.warn(`Failed to delete R2 object ${ss.r2Key}, skipping`);
      }
    }

    // Keep the screenshot rows (timings); just mark the session purged so we
    // don't re-issue these R2 deletes on every subsequent run.
    await db
      .update(schema.sessions)
      .set({ screenshotsPurgedAt: new Date() })
      .where(eq(schema.sessions.id, session.id));
  }

  await purgeEditedOriginals();
}

/**
 * Privacy backstop for the edit feature: cut minutes vanish from the
 * published video but survive inside the (token-gated) uncut original. Once
 * an EDITED session's re-edit window closes, delete the original so the cut
 * content is truly gone; nulling original_video_r2_key freezes further
 * editing. Sessions whose published video IS the original (never edited, or
 * edits cleared) keep their single video file forever, as always.
 */
async function purgeEditedOriginals() {
  const threshold = new Date(
    Date.now() - EDIT_WINDOW_DAYS * 24 * 60 * 60_000,
  );

  const editedSessions = await db
    .select({
      id: schema.sessions.id,
      originalVideoR2Key: schema.sessions.originalVideoR2Key,
    })
    .from(schema.sessions)
    .where(
      and(
        eq(schema.sessions.status, "complete"),
        isNotNull(schema.sessions.originalVideoR2Key),
        isNotNull(schema.sessions.videoR2Key),
        sql`${schema.sessions.videoR2Key} <> ${schema.sessions.originalVideoR2Key}`,
        lt(schema.sessions.lastEditCompileAt, threshold),
      ),
    );

  for (const session of editedSessions) {
    try {
      await r2Client.send(
        new DeleteObjectCommand({
          Bucket: R2_BUCKET,
          Key: session.originalVideoR2Key!,
        }),
      );
    } catch {
      console.warn(
        `Failed to delete original video for session ${session.id}, will retry next run`,
      );
      continue;
    }

    await db
      .update(schema.sessions)
      .set({ originalVideoR2Key: null, updatedAt: new Date() })
      .where(eq(schema.sessions.id, session.id));
  }
}
