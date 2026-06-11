import * as Sentry from "@sentry/node";

Sentry.init({
  dsn: process.env.SENTRY_DSN,
  environment: "worker",
  sendDefaultPii: true,
  tracesSampleRate: 0.2,
});

import PgBoss from "pg-boss";
import { drizzle } from "drizzle-orm/node-postgres";
import pg from "pg";
import { eq } from "drizzle-orm";
import { compileTimelapse } from "./compile.js";
import * as schema from "./schema.js";

const DATABASE_URL = process.env.DATABASE_URL;
if (!DATABASE_URL) {
  throw new Error("DATABASE_URL environment variable must be set");
}

const COMPILE_JOB = "compile-timelapse";
const RETRY_LIMIT = 3;

// Pools are sized for horizontal scaling: each replica opens this Drizzle pool
// AND pg-boss's own pool. Keep (replicas × (5 + 5)) + server well under
// Postgres max_connections (100). See queue.ts / server db for the budget.
const pool = new pg.Pool({ connectionString: DATABASE_URL, max: 5 });
const db = drizzle(pool, { schema });

const boss = new PgBoss({
  connectionString: DATABASE_URL,
  max: 5,
});

await boss.start();

console.log("Worker started, listening for compilation jobs...");

await boss.work<{ sessionId: string }>(
  COMPILE_JOB,
  async (jobs) => {
    for (const job of jobs) {
      const { sessionId } = job.data;
      console.log(`Compiling timelapse for session ${sessionId}...`);

      try {
        const result = await compileTimelapse(sessionId);
        console.log(
          `Compilation complete for session ${sessionId}: ${result.videoR2Key}`,
        );
      } catch (error) {
        console.error(
          `Compilation failed for session ${sessionId}:`,
          error,
        );
        // On final retry, mark session as failed immediately
        // instead of waiting for the 60-min stuck-compiling timeout
        const retryCount = ((job as unknown as Record<string, unknown>).retrycount as number) ?? 0;
        if (retryCount >= RETRY_LIMIT - 1) {
          console.error(`Final retry exhausted for session ${sessionId}, marking as failed`);
          await db
            .update(schema.sessions)
            .set({ status: "failed", updatedAt: new Date() })
            .where(eq(schema.sessions.id, sessionId))
            .catch((e) => console.error("Failed to mark session as failed:", e));
        }
        throw error; // pgBoss will retry (or complete if final)
      }
    }
  },
);

// Heartbeat — each replica enqueues its OWN heartbeat through pg-boss on a local
// timer, and whichever worker dequeues it pings the URL carried in the payload.
// Routing through the queue makes one mechanism verify two things, using only the
// per-replica URLs (UPTIME_PUSH_URL is distinct per replica):
//   - This replica is alive   → it stops enqueuing when down, so ITS URL goes stale.
//   - pg-boss is processing    → if jobs enqueue but never run, ALL URLs go stale.
// Monitor read: one URL stale = that replica; all URLs stale = pg-boss/DB down.
// (The tick is a local interval, not a pg-boss cron, because cron is global — one
// job per tick claimed by one worker — and so can't drive 3 per-replica URLs.)
const HEARTBEAT_QUEUE = "heartbeat";
const HEARTBEAT_INTERVAL_MS = 60_000;
const heartbeatUrl = process.env.UPTIME_PUSH_URL;

await boss.createQueue(HEARTBEAT_QUEUE);
await boss.work<{ url: string }>(HEARTBEAT_QUEUE, async (jobs) => {
  for (const job of jobs) {
    try {
      const res = await fetch(job.data.url, {
        signal: AbortSignal.timeout(10_000),
      });
      if (!res.ok) {
        console.warn(`Heartbeat ping to ${job.data.url} returned ${res.status}`);
      }
    } catch (err) {
      // Swallow rather than throw: a failed ping must NOT be retried, or pg-boss
      // would later fire a stale, backdated heartbeat. The next 60s tick is the
      // correct signal — let this one die.
      console.warn(`Heartbeat ping failed for ${job.data.url}:`, err);
    }
  }
});

let heartbeatTimer: NodeJS.Timeout | undefined;
if (heartbeatUrl) {
  heartbeatTimer = setInterval(() => {
    boss
      // expireInSeconds < interval: an unprocessed heartbeat is dropped before the
      // next one, so a recovered queue never replays a burst of backdated pings.
      // retryLimit 0: same reasoning — never retry a now-stale tick.
      // retentionMinutes: heartbeats are high-frequency and disposable; don't bloat.
      .send(
        HEARTBEAT_QUEUE,
        { url: heartbeatUrl },
        { expireInSeconds: 50, retryLimit: 0, retentionMinutes: 10 },
      )
      .catch((err) => console.error("Failed to enqueue heartbeat:", err));
  }, HEARTBEAT_INTERVAL_MS);
} else {
  console.warn("UPTIME_PUSH_URL not set — this replica will not emit a heartbeat");
}

// Graceful shutdown
const shutdown = async () => {
  console.log("Worker shutting down...");
  if (heartbeatTimer) clearInterval(heartbeatTimer);
  await boss.stop();
  process.exit(0);
};

process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
