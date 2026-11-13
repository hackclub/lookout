import * as Sentry from "@sentry/node";
import {
  DB_UNAVAILABLE_FINGERPRINT,
  isTransientDbError,
  retryWhileDbUnavailable,
} from "@lookout/shared";

Sentry.init({
  dsn: process.env.SENTRY_DSN,
  environment: "server",
  tracesSampleRate: 0.2,
  // A DB restart fails whatever queries were in flight; group them as one
  // "database unavailable" issue instead of one issue per query.
  beforeSend(event, hint) {
    if (isTransientDbError(hint.originalException)) {
      event.fingerprint = DB_UNAVAILABLE_FINGERPRINT;
      event.level = "warning";
    }
    return event;
  },
});

import Fastify from "fastify";
import fastifyStatic from "@fastify/static";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { existsSync } from "node:fs";
import { internalRoutes } from "./routes/internal.js";
import { sessionRoutes } from "./routes/sessions.js";
import { adminRoutes } from "./routes/admin.js";
import { programRoutes } from "./routes/programs.js";
import { announcementRoutes } from "./routes/announcements.js";
import { tipRoutes } from "./routes/tips.js";
import { healthRoutes } from "./routes/health.js";
import { registerCommon } from "./app.js";
import { sql } from "drizzle-orm";
import { db } from "./db/index.js";
import { boss } from "./lib/queue.js";
import { registerTimeoutJobs } from "./lib/timeouts.js";

const __dirname = dirname(fileURLToPath(import.meta.url));

const app = Fastify({ logger: true });

// CORS + the shared error handler (503 for DB outages), as in buildApp().
await registerCommon(app);

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
await app.register(announcementRoutes);
await app.register(tipRoutes);
await app.register(healthRoutes);

// Serve React SPA in production
const publicDir = join(__dirname, "..", "public");
if (existsSync(publicDir)) {
  await app.register(fastifyStatic, {
    root: publicDir,
    prefix: "/",
    wildcard: false,
  });

  // `.sh` resolves to application/x-sh, which browsers download rather than
  // show — so the one URL we ask people to pipe into a shell is the one they
  // can't read first.
  //
  // Done in onSend, which is the only one of the three that survives:
  // @fastify/static's own setHeaders is overwritten when it applies send's
  // headers afterwards, and a dedicated route collides with the per-file route
  // it registers under `wildcard: false`.
  app.addHook("onSend", async (request, reply, payload) => {
    if (request.url.split("?")[0].endsWith(".sh")) {
      reply.type("text/plain; charset=utf-8");
    }
    return payload;
  });

  // Two apps share this origin: the download page at the root, and the
  // hosted recorder under /session — the URL `POST /api/internal/sessions`
  // has always handed back. They have separate builds and separate
  // index.html files, so the fallback has to look at the path.
  const recorderIndex = join(publicDir, "session", "index.html");
  const hasRecorder = existsSync(recorderIndex);

  // SPA fallback — serve the right index.html for non-API routes
  app.setNotFoundHandler(async (request, reply) => {
    if (request.url.startsWith("/api/")) {
      return reply.code(404).send({ error: "Not found" });
    }
    // The session token rides in the query string, so compare on path
    // only. `/sessions` is deliberately not a match.
    const path = request.url.split("?")[0];
    if (hasRecorder && (path === "/session" || path.startsWith("/session/"))) {
      return reply.sendFile("session/index.html");
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
// Wait out a DB that's still restarting before starting pg-boss: a failed
// boss.start() can't be retried (it stays flagged as "starting"), so the
// probe is what gets retried, not the start itself.
await retryWhileDbUnavailable("startup", () => db.execute(sql`select 1`));
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
