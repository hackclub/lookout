import Fastify, { FastifyInstance } from "fastify";
import cors from "@fastify/cors";
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

  await app.register(cors, { origin: true, methods: CORS_METHODS });
  await app.register(internalRoutes);
  await app.register(sessionRoutes);
  await app.register(adminRoutes);
  await app.register(programRoutes);
  await app.register(announcementRoutes);
  await app.register(tipRoutes);
  await app.register(healthRoutes);

  return app;
}
