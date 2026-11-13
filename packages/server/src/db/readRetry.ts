import { isTransientDbError } from "@lookout/shared";

/**
 * Backoff between attempts of a plain read that hit a transient connection
 * error. Short on purpose: it rides out the few seconds a restart or failover
 * takes to drop and re-accept connections, not a real outage — that should
 * surface as a 503 quickly rather than hold requests open.
 */
export const READ_RETRY_DELAYS_MS = [100, 500, 1500];

const LOCKING_CLAUSE = /\bfor\s+(update|share|no\s+key\s+update|key\s+share)\b/i;

/**
 * True for a standalone SELECT, the only thing safe to replay blindly. Text
 * must start with SELECT, so a leading comment (`/* health *\/ select 1`)
 * deliberately opts a query out of retrying.
 */
export function isPlainRead(config: unknown): boolean {
  const text =
    typeof config === "string"
      ? config
      : (config as { text?: unknown } | null)?.text;
  return (
    typeof text === "string" &&
    /^\s*select\b/i.test(text) &&
    !LOCKING_CLAUSE.test(text)
  );
}

type QueryFn = (...args: unknown[]) => unknown;

/**
 * Make `pool.query` retry plain SELECTs that fail with a transient connection
 * error (DB restart, failover, refused connection). Writes, locking reads,
 * callback-style calls and streaming queries pass straight through, as does
 * anything run on a checked-out client (transactions), since replaying those
 * isn't safe.
 *
 * Drizzle's node-postgres driver issues every non-transaction query through
 * `pool.query`, so this covers the request path without touching call sites.
 */
export function installReadRetry(
  pool: { query: unknown },
  delaysMs: readonly number[] = READ_RETRY_DELAYS_MS,
  onRetry: (err: unknown, attempt: number) => void = () => {},
): void {
  const base = (pool.query as QueryFn).bind(pool);
  pool.query = function query(...args: unknown[]) {
    const first = args[0] as { submit?: unknown } | null | undefined;
    if (
      typeof args[args.length - 1] === "function" ||
      typeof first?.submit === "function" ||
      !isPlainRead(first)
    ) {
      return base(...args);
    }
    return (async () => {
      for (let attempt = 0; ; attempt++) {
        try {
          return await base(...args);
        } catch (err) {
          if (attempt >= delaysMs.length || !isTransientDbError(err)) throw err;
          onRetry(err, attempt + 1);
          await new Promise((r) => setTimeout(r, delaysMs[attempt]));
        }
      }
    })();
  };
}
