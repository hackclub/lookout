import Fastify, { FastifyInstance } from "fastify";
import cors from "@fastify/cors";
import { sessionRoutes } from "./routes/sessions.js";
import { internalRoutes } from "./routes/internal.js";
import { adminRoutes } from "./routes/admin.js";

/**
 * Build a Fastify instance with our routes registered. Used by:
 *   - production entrypoint (`index.ts`) — adds Sentry, static files,
 *     pg-boss start, listen()
 *   - integration tests — drives the in-process app via `app.inject(...)`
 *     without ever opening a socket
 *
 * No side effects beyond instantiation. Caller is responsible for closing.
 */
export async function buildApp(): Promise<FastifyInstance> {
  const app = Fastify({
    logger: process.env.TEST_LOG === "1" ? { level: "debug" } : false,
  });

  await app.register(cors, { origin: true });
  await app.register(internalRoutes);
  await app.register(sessionRoutes);
  await app.register(adminRoutes);

  return app;
}
