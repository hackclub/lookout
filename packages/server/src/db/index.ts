import { drizzle } from "drizzle-orm/node-postgres";
import pg from "pg";
import { RESILIENT_POOL_OPTIONS } from "@lookout/shared";
import * as schema from "./schema.js";
import { installReadRetry } from "./readRetry.js";

const DATABASE_URL = process.env.DATABASE_URL;
if (!DATABASE_URL) {
  throw new Error("DATABASE_URL environment variable is required but not set");
}

const pool = new pg.Pool({
  connectionString: DATABASE_URL,
  max: 10,
  ...RESILIENT_POOL_OPTIONS,
  // Request-path queries are short; one stuck behind a lock or a dying
  // connection shouldn't pin a pool slot (and the request) indefinitely.
  statement_timeout: 60_000,
});

// An idle client whose connection drops (DB restart, failover) emits 'error'
// on the pool. With no listener that's an uncaught exception that kills the
// process; the pool already discards the broken client, so just log it.
pool.on("error", (err) => {
  console.warn("[db] idle client error (client discarded):", err.message);
});

installReadRetry(pool, undefined, (err, attempt) => {
  console.warn(
    `[db] transient error on read, retry ${attempt}:`,
    (err as Error)?.message,
  );
});

export const db = drizzle(pool, { schema });
export { schema };
