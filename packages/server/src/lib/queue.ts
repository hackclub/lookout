import PgBoss from "pg-boss";
import * as Sentry from "@sentry/node";
import { isTransientDbError, RESILIENT_POOL_OPTIONS } from "@lookout/shared";

const DATABASE_URL = process.env.DATABASE_URL;
if (!DATABASE_URL) {
  throw new Error("DATABASE_URL environment variable is required but not set");
}

export const boss = new PgBoss({
  connectionString: DATABASE_URL,
  max: 5,
  ...RESILIENT_POOL_OPTIONS,
  retryLimit: 3,
  retryDelay: 60,
  retryBackoff: true,
  expireInHours: 2,
});

// pg-boss re-emits its pool's idle-client errors and its own polling/cron
// failures as 'error'. Without a listener EventEmitter throws them, which
// crashes the process on every DB restart. pg-boss keeps polling and recovers
// on its own once the DB is back, so a transient error only needs a log line.
boss.on("error", (err) => {
  if (isTransientDbError(err)) {
    console.warn("[pg-boss] database unavailable:", err.message);
    return;
  }
  console.error("[pg-boss] error:", err);
  Sentry.captureException(err);
});

export const COMPILE_JOB = "compile-timelapse";
export const CHECK_TIMEOUTS_JOB = "check-timeouts";
export const CLEANUP_UNCONFIRMED_JOB = "cleanup-unconfirmed";
export const CLEANUP_SCREENSHOTS_JOB = "cleanup-screenshots";

export interface CompileJobData {
  sessionId: string;
}
