import type { FastifyInstance } from "fastify";
import { sql } from "drizzle-orm";
import { db } from "../db/index.js";

// Readiness probe for the orchestrator (Orchard/Kubernetes). A rolling deploy
// only sends traffic to a new pod once this answers 200, so it has to mean
// "this pod can serve requests": the process is listening AND the database
// answers. 503 (not 500) when the database is down, so the pod is pulled out
// of rotation rather than reported as broken.
//
// Use it for readiness only, not liveness: a database outage fails this on
// every pod, and restarting them would not bring the database back.
export async function healthRoutes(app: FastifyInstance) {
  app.get("/api/health", async (_request, reply) => {
    try {
      // The leading comment opts out of the read retry in db/readRetry.ts:
      // a probe should report the database as it is right now, quickly.
      await db.execute(sql`/* health */ select 1`);
    } catch (err) {
      app.log.error({ err }, "health check: database unreachable");
      return reply.code(503).send({ status: "error", database: "unreachable" });
    }
    return reply.code(200).send({ status: "ok" });
  });
}
