/**
 * Classifying database errors that mean "Postgres is briefly unreachable"
 * rather than "this query is wrong". Shared by the server and the worker so
 * both agree on what a DB restart, failover or move looks like.
 *
 * Pure (no pg import): the checks read `code` / `message` off whatever was
 * thrown, and walk `cause` because drizzle wraps the driver error in a
 * DrizzleQueryError ("Failed query: …") with the pg error as its cause.
 */

/** SQLSTATEs seen while Postgres restarts, fails over or refuses connections. */
const TRANSIENT_PG_CODES = new Set([
  "57P01", // admin_shutdown — "terminating connection due to administrator command"
  "57P02", // crash_shutdown
  "57P03", // cannot_connect_now — "the database system is starting up / shutting down"
  "08000", // connection_exception
  "08001", // sqlclient_unable_to_establish_sqlconnection
  "08003", // connection_does_not_exist
  "08004", // sqlserver_rejected_establishment_of_sqlconnection
  "08006", // connection_failure
  "25006", // read_only_sql_transaction — connected to a replica mid-failover
  "53300", // too_many_connections — everyone reconnecting at once
]);

/** Socket-level errors from node:net while the DB host is gone or moving. */
const TRANSIENT_NODE_CODES = new Set([
  "ECONNREFUSED",
  "ECONNRESET",
  "ETIMEDOUT",
  "EPIPE",
  "EHOSTUNREACH",
  "ENETUNREACH",
  "ENOTFOUND",
  "EAI_AGAIN",
]);

/** node-postgres reports these as plain Errors with no code. */
const TRANSIENT_MESSAGES = [
  "Connection terminated",
  "timeout exceeded when trying to connect",
  "Client has encountered a connection error",
];

export function isTransientDbError(err: unknown): boolean {
  let cur: unknown = err;
  for (let depth = 0; cur && depth < 5; depth++) {
    if (typeof cur !== "object") return false;
    const { code, message, cause } = cur as {
      code?: unknown;
      message?: unknown;
      cause?: unknown;
    };
    if (typeof code === "string") {
      if (TRANSIENT_PG_CODES.has(code) || TRANSIENT_NODE_CODES.has(code)) {
        return true;
      }
    }
    if (
      typeof message === "string" &&
      TRANSIENT_MESSAGES.some((m) => message.includes(m))
    ) {
      return true;
    }
    cur = cause;
  }
  return false;
}

/**
 * Run `fn`, retrying while it fails with a transient DB error — for a startup
 * probe (`select 1`) so a pod that comes up while Postgres is still
 * restarting waits instead of crashing. ~1 minute by default. Only retry
 * idempotent work here: pg-boss's start() in particular can't be re-run
 * after it throws.
 */
export async function retryWhileDbUnavailable<T>(
  label: string,
  fn: () => Promise<T>,
  attempts = 12,
  delayMs = 5_000,
): Promise<T> {
  for (let attempt = 1; ; attempt++) {
    try {
      return await fn();
    } catch (err) {
      if (attempt >= attempts || !isTransientDbError(err)) throw err;
      console.warn(
        `[db] ${label}: database unavailable (${(err as Error)?.message}), retry ${attempt}/${attempts - 1} in ${delayMs}ms`,
      );
      await new Promise((r) => setTimeout(r, delayMs));
    }
  }
}

/**
 * Sentry fingerprint for transient DB errors: one "database unavailable"
 * issue per outage instead of one per query that happened to be in flight.
 */
export const DB_UNAVAILABLE_FINGERPRINT = ["database-unavailable"];

/**
 * pg.Pool options that keep a pool usable across a DB restart: fail fast
 * when the host is unreachable instead of queueing forever, keep sockets
 * alive so dead peers are noticed, and recycle idle clients.
 */
export const RESILIENT_POOL_OPTIONS = {
  connectionTimeoutMillis: 10_000,
  idleTimeoutMillis: 30_000,
  keepAlive: true,
  keepAliveInitialDelayMillis: 10_000,
} as const;
