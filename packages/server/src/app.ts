import Fastify, { FastifyInstance } from "fastify";
import cors from "@fastify/cors";
import { isTransientDbError } from "@lookout/shared";
import { sessionRoutes } from "./routes/sessions.js";
import { internalRoutes } from "./routes/internal.js";
import { adminRoutes } from "./routes/admin.js";
import { programRoutes } from "./routes/programs.js";
import { announcementRoutes } from "./routes/announcements.js";
import { tipRoutes } from "./routes/tips.js";
import { healthRoutes } from "./routes/health.js";

/**
 * Methods allowed on CORS preflight. @fastify/cors 11 narrowed its default to
 * GET,HEAD,POST, but the SDK embedded on program origins calls PATCH /name
 * and PUT /cuts cross-origin, so the previous default is spelled out. Shared
 * with index.ts so the tests check what production sends.
 *
 * A pre-joined string, not an array: v11 joins arrays with ", " where v10
 * sent "GET,HEAD,…", and the header should stay byte-for-byte what deployed
 * clients have always seen.
 */
export const CORS_METHODS = "GET,HEAD,PUT,PATCH,POST,DELETE";

/**
 * Origins allowed to call the API cross-origin: *.hackclub.com, BASE_URL's
 * own host (self-hosted deployments serve the admin panel from it), the
 * legacy Tauri app (tauri:// on macOS/Linux, http://tauri.localhost on
 * Windows), and localhost in development. A missing origin is a
 * server-to-server call or a same-origin GET.
 */
export function isAllowedOrigin(
  origin: string | undefined,
  opts: { isDev: boolean; baseUrlHostname: string | null },
): boolean {
  if (
    !origin ||
    origin.startsWith("tauri://") ||
    origin === "http://tauri.localhost"
  ) {
    return true;
  }
  let hostname: string;
  try {
    hostname = new URL(origin).hostname;
  } catch {
    return false;
  }
  return (
    /\.hackclub\.com$/.test(hostname) ||
    hostname === "hackclub.com" ||
    (opts.baseUrlHostname !== null && hostname === opts.baseUrlHostname) ||
    (opts.isDev && /^https?:\/\/localhost(:\d+)?$/.test(origin))
  );
}

function baseUrlHostname(): string | null {
  try {
    return process.env.BASE_URL ? new URL(process.env.BASE_URL).hostname : null;
  } catch {
    return null;
  }
}

/**
 * CORS plus the error handling every route shares. Registered by both
 * buildApp() and index.ts, before the routes, so the tests exercise exactly
 * what production runs.
 *
 * A disallowed origin gets no CORS headers — the browser then won't expose
 * the response to that page — rather than an Error, which Fastify turned
 * into a 500 that Sentry reported (LOOKOUT-BACKEND-F: a third-party site
 * showing a public thumbnail). Routes that are public by design opt in to
 * any origin with `config: { cors: { origin: "*" } }`.
 */
export async function registerCommon(app: FastifyInstance): Promise<void> {
  const isDev = process.env.NODE_ENV !== "production";
  const hostname = baseUrlHostname();

  await app.register(cors, {
    origin: (origin, cb) => {
      const allowed = isAllowedOrigin(origin, {
        isDev,
        baseUrlHostname: hostname,
      });
      if (!allowed) app.log.info(`CORS: no headers for origin ${origin}`);
      cb(null, allowed);
    },
    methods: CORS_METHODS,
  });

  // A DB restart or failover (LOOKOUT-BACKEND-1/3/4/6..E) is a temporary
  // condition, not a bug in the request: answer 503 + Retry-After so clients
  // back off and retry, instead of a 500. Sentry still sees the error, but
  // beforeSend in index.ts groups them all under one fingerprint.
  app.setErrorHandler((err, request, reply) => {
    if (isTransientDbError(err)) {
      request.log.warn({ err }, "database unavailable");
      reply
        .code(503)
        .header("Retry-After", "5")
        .send({ error: "Service temporarily unavailable, please retry" });
      return;
    }
    // Anything else falls through to Fastify's default handler.
    throw err;
  });
}

/**
 * Build a Fastify instance with our routes registered. Used by the
 * integration tests, which drive it via `app.inject(...)` without opening a
 * socket.
 *
 * NOT used by the production entrypoint: index.ts builds its own instance and
 * repeats this registration list. A route added only here passes every test
 * and 404s in production, so add it to both.
 *
 * No side effects beyond instantiation. Caller is responsible for closing.
 */
export async function buildApp(): Promise<FastifyInstance> {
  const app = Fastify({
    logger: process.env.TEST_LOG === "1" ? { level: "debug" } : false,
  });

  await registerCommon(app);
  await app.register(internalRoutes);
  await app.register(sessionRoutes);
  await app.register(adminRoutes);
  await app.register(programRoutes);
  await app.register(announcementRoutes);
  await app.register(tipRoutes);
  await app.register(healthRoutes);

  return app;
}
