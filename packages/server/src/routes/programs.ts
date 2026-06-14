import type { FastifyInstance } from "fastify";
import { isNotNull, sql } from "drizzle-orm";
import { db, schema } from "../db/index.js";

// Public, unauthenticated registry of programs the desktop app can start a
// session for. Only programs that have configured a new-session URL are listed;
// the desktop app opens that URL in the OS browser to begin the (browser-authed)
// session flow. Returns an empty list when nothing is configured — clients must
// handle that gracefully.
export async function programRoutes(app: FastifyInstance) {
  app.get("/api/programs", async () => {
    const rows = await db
      .select({
        name: schema.programs.name,
        // Prefer the human-friendly display name; fall back to the raw name so
        // older programs without one still render sensibly.
        displayName: sql<string>`coalesce(${schema.programs.displayName}, ${schema.programs.name})`,
        newSessionUrl: schema.programs.newSessionUrl,
      })
      .from(schema.programs)
      .where(isNotNull(schema.programs.newSessionUrl))
      .orderBy(schema.programs.name);

    return { programs: rows };
  });
}
