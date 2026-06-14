import type { FastifyInstance } from "fastify";
import { eq, desc } from "drizzle-orm";
import { db, schema } from "../db/index.js";

// Public, unauthenticated endpoint the desktop app polls (on open and every
// ~15 min) for an admin announcement banner. Returns the latest active
// announcement, or null when none is set. Clients must handle null gracefully.
export async function announcementRoutes(app: FastifyInstance) {
  app.get("/api/announcement", async () => {
    const [a] = await db
      .select({
        level: schema.announcements.level,
        message: schema.announcements.message,
        url: schema.announcements.url,
      })
      .from(schema.announcements)
      .where(eq(schema.announcements.active, true))
      .orderBy(desc(schema.announcements.updatedAt))
      .limit(1);

    return { announcement: a ?? null };
  });
}
