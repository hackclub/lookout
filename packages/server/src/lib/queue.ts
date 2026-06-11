import PgBoss from "pg-boss";

const DATABASE_URL = process.env.DATABASE_URL;
if (!DATABASE_URL) {
  throw new Error("DATABASE_URL environment variable is required but not set");
}

export const boss = new PgBoss({
  connectionString: DATABASE_URL,
  max: 5,
  retryLimit: 3,
  retryDelay: 60,
  retryBackoff: true,
  expireInHours: 2,
});

export const COMPILE_JOB = "compile-timelapse";
export const CHECK_TIMEOUTS_JOB = "check-timeouts";
export const CLEANUP_UNCONFIRMED_JOB = "cleanup-unconfirmed";
export const CLEANUP_SCREENSHOTS_JOB = "cleanup-screenshots";

export interface CompileJobData {
  sessionId: string;
}
