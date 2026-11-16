/**
 * Scrub session tokens out of anything headed for logs or Sentry.
 *
 * A session token is the whole capability to a recording: whoever holds it
 * can upload into it, stop it and edit it. It travels in URLs
 * (`/api/sessions/<token>/…`, `/session?token=<token>`,
 * `/api/internal/sessions/by-token/<token>`), and request URLs are exactly
 * what Fastify's request logging and Sentry's HTTP tracing record — so
 * without this, log and Sentry access quietly amounted to access to every
 * live session.
 *
 * Tokens are 64 hex characters (sessions.token), which nothing else we put
 * in a URL is: session ids are dashed UUIDs and program keys are `lk_…` and
 * travel in a header.
 */
const TOKEN_RE = /[0-9a-fA-F]{64}/g;

export const REDACTED = "[token]";

export function redactTokens(s: string): string {
  return s.replace(TOKEN_RE, REDACTED);
}

/** Deep copy of a JSON-ish value with every string redacted. Used on Sentry
 *  events, where a URL can surface in the transaction name, request data,
 *  span descriptions/attributes and breadcrumbs alike. */
export function redactDeep<T>(value: T, depth = 0): T {
  if (typeof value === "string") return redactTokens(value) as T;
  if (depth > 20 || value === null || typeof value !== "object") return value;
  if (Array.isArray(value)) {
    return value.map((v) => redactDeep(v, depth + 1)) as T;
  }
  if (Object.getPrototypeOf(value) !== Object.prototype) return value;
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(value)) {
    out[redactTokens(k)] = redactDeep(v, depth + 1);
  }
  return out as T;
}

/** Fastify `req` log serializer: the default fields, with the URL scrubbed. */
export function redactedReqSerializer(req: {
  method: string;
  url: string;
  host?: string;
  ip?: string;
  socket?: { remotePort?: number };
}) {
  return {
    method: req.method,
    url: redactTokens(req.url),
    host: req.host,
    remoteAddress: req.ip,
    remotePort: req.socket?.remotePort,
  };
}
