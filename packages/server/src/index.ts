import * as Sentry from "@sentry/node";

Sentry.init({
  dsn: process.env.SENTRY_DSN,
  environment: "server",
  sendDefaultPii: true,
  tracesSampleRate: 0.2,
});

import Fastify from "fastify";
import cors from "@fastify/cors";
import fastifyStatic from "@fastify/static";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { existsSync } from "node:fs";
import { internalRoutes } from "./routes/internal.js";
import { sessionRoutes } from "./routes/sessions.js";
import { adminRoutes } from "./routes/admin.js";
import { programRoutes } from "./routes/programs.js";
import { boss } from "./lib/queue.js";
import { registerTimeoutJobs } from "./lib/timeouts.js";

const __dirname = dirname(fileURLToPath(import.meta.url));

const app = Fastify({ logger: true });

const IS_DEV = process.env.NODE_ENV !== "production";

await app.register(cors, {
  origin: (origin, cb) => {
    // Allow: no origin (server-to-server), *.hackclub.com, tauri app
    // Tauri uses tauri:// on macOS/Linux but http://tauri.localhost on Windows
    if (
      !origin ||
      origin.startsWith("tauri://") ||
      origin === "http://tauri.localhost"
    ) {
      cb(null, true);
      return;
    }
    try {
      const hostname = new URL(origin).hostname;
      const isAllowed =
        /\.hackclub\.com$/.test(hostname) ||
        hostname === "hackclub.com" ||
        // Only allow localhost origins in development
        (IS_DEV && /^https?:\/\/localhost(:\d+)?$/.test(origin));

      if (isAllowed) {
        cb(null, true);
      } else {
        app.log.warn(`CORS rejected origin: ${origin}`);
        cb(new Error(`Not allowed by CORS (origin: ${origin})`), false);
      }
    } catch {
      app.log.warn(`CORS rejected origin (malformed): ${origin}`);
      cb(new Error(`Not allowed by CORS (origin: ${origin})`), false);
    }
  },
});

// Security headers
app.addHook("onSend", async (_request, reply) => {
  reply.header("X-Content-Type-Options", "nosniff");
  reply.header("X-Frame-Options", "DENY");
  reply.header("X-XSS-Protection", "0");
  reply.header("Referrer-Policy", "no-referrer");
  reply.header(
    "Strict-Transport-Security",
    "max-age=31536000; includeSubDomains",
  );
});

// Register API routes
await app.register(internalRoutes);
await app.register(sessionRoutes);
await app.register(adminRoutes);
await app.register(programRoutes);

// Serve React SPA in production
const publicDir = join(__dirname, "..", "public");
if (existsSync(publicDir)) {
  await app.register(fastifyStatic, {
    root: publicDir,
    prefix: "/",
    wildcard: false,
  });

  // SPA fallback — serve index.html for non-API routes
  app.setNotFoundHandler(async (request, reply) => {
    if (request.url.startsWith("/api/")) {
      return reply.code(404).send({ error: "Not found" });
    }
    return reply.sendFile("index.html");
  });
} else {
  app.setNotFoundHandler(async (request, reply) => {
    if (request.url.startsWith("/api/")) {
      return reply.code(404).send({ error: "Not found" });
    }
    return reply
      .code(200)
      .send({
        message:
          "The Lookout server is running. https://github.com/hackclub/lookout",
      });
  });
}

// Start pgBoss and periodic jobs
await boss.start();
await registerTimeoutJobs();

const port = Number(process.env.PORT) || 3000;
await app.listen({ port, host: "0.0.0.0" });

// Graceful shutdown
const shutdown = async () => {
  app.log.info("Shutting down...");
  await app.close();
  await boss.stop();
  process.exit(0);
};

process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
