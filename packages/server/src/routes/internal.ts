import type { FastifyInstance } from "fastify";
import { eq, sql, and, isNotNull } from "drizzle-orm";
import { db, schema } from "../db/index.js";
import { requireApiKey } from "../middleware/apiKey.js";
import { boss, COMPILE_JOB } from "../lib/queue.js";

// ── Shared schema fragments ─────────────────────────────────

const sessionIdParamSchema = {
  type: "object" as const,
  properties: {
    sessionId: { type: "string" as const, format: "uuid" },
  },
  required: ["sessionId"] as const,
};

export async function internalRoutes(app: FastifyInstance) {
  app.addHook("onRequest", requireApiKey);

  // Create a new session
  app.post<{
    Body: { name?: string; metadata?: Record<string, unknown> };
  }>(
    "/api/internal/sessions",
    {
      schema: {
        body: {
          type: "object" as const,
          properties: {
            name: { type: "string" as const, minLength: 1, maxLength: 255 },
            metadata: { type: "object" as const, maxProperties: 50 },
          },
          additionalProperties: false,
        },
      },
    },
    async (request, reply) => {
      const { name, metadata } = request.body || {};

      const [session] = await db
        .insert(schema.sessions)
        .values({
          ...(name ? { name } : {}),
          metadata: metadata ?? {},
          // Attribution: tag with the creating program (null for global key).
          program: request.program ?? null,
        })
        .returning();

      const baseUrl = process.env.BASE_URL || "http://localhost:3000";

      return reply.code(201).send({
        token: session.token,
        sessionId: session.id,
        sessionUrl: `${baseUrl}/session?token=${session.token}`,
      });
    },
  );

  // Get session details (includes token)
  app.get<{
    Params: { sessionId: string };
  }>(
    "/api/internal/sessions/:sessionId",
    {
      schema: { params: sessionIdParamSchema },
    },
    async (request, reply) => {
      const { sessionId } = request.params;

      const session = await db.query.sessions.findFirst({
        where: eq(schema.sessions.id, sessionId),
      });

      if (!session) {
        return reply.code(404).send({ error: "Session not found" });
      }

      const [{ count }] = await db
        .select({
          count: sql<number>`count(distinct ${schema.screenshots.minuteBucket})`,
        })
        .from(schema.screenshots)
        .where(
          and(
            eq(schema.screenshots.sessionId, sessionId),
            eq(schema.screenshots.confirmed, true),
          ),
        );

      // Exclude internal R2 storage keys and build proper media URLs
      const baseUrl = process.env.BASE_URL || "http://localhost:3000";
      const { videoR2Key, thumbnailR2Key, ...sessionData } = session;
      // Bucket-mode: tracked = (distinct buckets - 1) * 60.
      // Credit-mode: read session.trackedSeconds directly (maintained per-credit).
      const liveBucketTracked = Math.max(0, (Number(count) - 1) * 60);
      const trackedSeconds =
        session.trackingMode === "credit"
          ? session.trackedSeconds ?? 0
          : session.trackedSeconds ?? liveBucketTracked;
      const [{ confirmedCount }] = await db
        .select({ confirmedCount: sql<number>`count(*)::int` })
        .from(schema.screenshots)
        .where(
          and(
            eq(schema.screenshots.sessionId, sessionId),
            eq(schema.screenshots.confirmed, true),
          ),
        );
      // First recorded client telemetry: the clientInfo on the earliest
      // screenshot row that carries one. NULL for sessions with none captured.
      const [firstClient] = await db
        .select({ clientInfo: schema.screenshots.clientInfo })
        .from(schema.screenshots)
        .where(
          and(
            eq(schema.screenshots.sessionId, sessionId),
            isNotNull(schema.screenshots.clientInfo),
          ),
        )
        .orderBy(sql`${schema.screenshots.requestedAt} ASC`)
        .limit(1);
      return {
        session: {
          ...sessionData,
          thumbnailUrl: thumbnailR2Key
            ? `${baseUrl}/api/media/${session.id}/thumbnail.jpg`
            : null,
          videoUrl: videoR2Key
            ? `${baseUrl}/api/media/${session.id}/video.mp4`
            : null,
        },
        trackedSeconds,
        screenshotCount: Number(confirmedCount),
        clientInfo: firstClient?.clientInfo ?? null,
      };
    },
  );

  // Lookup session by token
  app.get<{
    Params: { token: string };
  }>(
    "/api/internal/sessions/by-token/:token",
    {
      schema: {
        params: {
          type: "object" as const,
          properties: {
            token: { type: "string" as const, minLength: 64, maxLength: 64 },
          },
          required: ["token"] as const,
        },
      },
    },
    async (request, reply) => {
      const { token } = request.params;

      const session = await db.query.sessions.findFirst({
        where: eq(schema.sessions.token, token),
      });

      if (!session) {
        return reply.code(404).send({ error: "Session not found" });
      }

      return { sessionId: session.id };
    },
  );

  // Force-stop a session
  app.post<{
    Params: { sessionId: string };
  }>(
    "/api/internal/sessions/:sessionId/stop",
    {
      schema: { params: sessionIdParamSchema },
    },
    async (request, reply) => {
      const { sessionId } = request.params;

      const session = await db.query.sessions.findFirst({
        where: eq(schema.sessions.id, sessionId),
      });

      if (!session) {
        return reply.code(404).send({ error: "Session not found" });
      }

      if (
        session.status === "stopped" ||
        session.status === "compiling" ||
        session.status === "complete"
      ) {
        return reply
          .code(409)
          .send({ error: `Session already ${session.status}` });
      }

      // Accumulate active time if session was active
      let totalActiveSeconds = session.totalActiveSeconds;
      if (session.status === "active" && session.startedAt) {
        const activeFrom =
          session.resumedAt || session.startedAt;
        totalActiveSeconds += Math.floor(
          (Date.now() - activeFrom.getTime()) / 1000,
        );
      }

      // Compute tracked seconds before stopping. Credit-mode sessions
      // already have it maintained on the row; bucket-mode computes live.
      let trackedSeconds: number;
      if (session.trackingMode === "credit") {
        trackedSeconds = session.trackedSeconds ?? 0;
      } else {
        const [{ buckets }] = await db
          .select({
            buckets: sql<number>`count(distinct ${schema.screenshots.minuteBucket})`,
          })
          .from(schema.screenshots)
          .where(
            and(
              eq(schema.screenshots.sessionId, sessionId),
              eq(schema.screenshots.confirmed, true),
            ),
          );
        trackedSeconds = Math.max(0, (Number(buckets) - 1) * 60);
      }

      const [updated] = await db
        .update(schema.sessions)
        .set({
          status: "stopped",
          stoppedAt: new Date(),
          totalActiveSeconds,
          trackedSeconds,
          updatedAt: new Date(),
        })
        .where(and(
          eq(schema.sessions.id, sessionId),
          sql`${schema.sessions.status} IN ('active', 'paused', 'pending')`,
        ))
        .returning({ id: schema.sessions.id });

      if (!updated) {
        return reply.code(409).send({ error: "Session state changed concurrently" });
      }

      await boss.send(COMPILE_JOB, { sessionId });

      return { status: "stopped" };
    },
  );

  // Re-trigger compilation for failed sessions
  app.post<{
    Params: { sessionId: string };
  }>(
    "/api/internal/sessions/:sessionId/recompile",
    {
      schema: { params: sessionIdParamSchema },
    },
    async (request, reply) => {
      const { sessionId } = request.params;

      const session = await db.query.sessions.findFirst({
        where: eq(schema.sessions.id, sessionId),
      });

      if (!session) {
        return reply.code(404).send({ error: "Session not found" });
      }

      if (session.status !== "failed") {
        return reply
          .code(409)
          .send({ error: "Only failed sessions can be recompiled" });
      }

      const [updated] = await db
        .update(schema.sessions)
        .set({ status: "compiling", updatedAt: new Date() })
        .where(and(eq(schema.sessions.id, sessionId), eq(schema.sessions.status, "failed")))
        .returning({ id: schema.sessions.id });

      if (!updated) {
        return reply.code(409).send({ error: "Session state changed concurrently" });
      }

      await boss.send(COMPILE_JOB, { sessionId });

      return { status: "compiling" };
    },
  );
}
