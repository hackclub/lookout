import type { FastifyInstance, FastifyRequest } from "fastify";
import { eq, sql, and, desc, inArray, isNotNull, isNull, lt, gt } from "drizzle-orm";
import { SESSION_DELETION_GRACE_MINUTES } from "@lookout/shared";
import { db, schema } from "../db/index.js";
import { requireApiKey } from "../middleware/apiKey.js";
import { boss, COMPILE_JOB } from "../lib/queue.js";

// ── Shared schema fragments ─────────────────────────────────

type WritableSession = {
  id: string;
  programId: string | null;
  panelResolvedAt: Date | null;
  deleteAfter: Date | null;
  status: (typeof schema.sessionStatusEnum.enumValues)[number];
  originalVideoR2Key: string | null;
};

const sessionIdParamSchema = {
  type: "object" as const,
  properties: {
    sessionId: { type: "string" as const, format: "uuid" },
  },
  required: ["sessionId"] as const,
};

export async function internalRoutes(app: FastifyInstance) {
  /**
   * Treat an empty JSON body as `{}` on the internal API.
   *
   * Fastify rejects `Content-Type: application/json` with no body outright
   * (FST_ERR_CTP_EMPTY_JSON_BODY, a 400 raised in the parser before any route
   * or schema is consulted). That is a trap for every endpoint here that
   * legitimately takes no body: a program whose HTTP client always sets a JSON
   * content type — which is the normal way to write one — gets a 400 it cannot
   * see the cause of. `panel-resolved` was doing exactly that, so panels were
   * never marked resolved and the desktop app went on asking for details the
   * user had already given.
   *
   * Scoped to this plugin by Fastify's encapsulation, so the client-facing and
   * admin routes keep the stricter default.
   */
  app.addContentTypeParser<string>(
    "application/json",
    { parseAs: "string" },
    (_req, body, done) => {
      if (!body || (typeof body === "string" && body.trim() === "")) {
        done(null, {});
        return;
      }
      try {
        done(null, JSON.parse(body as string));
      } catch (err) {
        done(err as Error, undefined);
      }
    },
  );

  app.addHook("onRequest", requireApiKey);

  // Create a new session
  app.post<{
    Body: {
      name?: string;
      metadata?: Record<string, unknown>;
      clips?: boolean;
      redirectUrl?: string;
      panelUrl?: string;
      viewUrl?: string;
    };
  }>(
    "/api/internal/sessions",
    {
      schema: {
        body: {
          type: "object" as const,
          properties: {
            name: { type: "string" as const, minLength: 1, maxLength: 255 },
            metadata: { type: "object" as const, maxProperties: 50 },
            // Opt OUT of clip uploads (per-minute videos of ~6 frames).
            // Defaults TRUE; pass false to pin this session to the legacy
            // 1 JPEG/min payload. Immutable after creation — a session's
            // capture character never changes.
            clips: { type: "boolean" as const },
            // Redirect hook: once the timelapse finishes compiling, the
            // recording client sends the user here (desktop opens it in the
            // default browser). Immutable after creation.
            redirectUrl: {
              type: "string" as const,
              pattern: "^https?://",
              maxLength: 2048,
            },
            // Program panel: rendered in-app in an iframe when the timelapse
            // finishes, in place of the redirect hop. HTTPS, except on
            // loopback for local panel development — this URL is a
            // per-session capability and gets framed inside the app, so a
            // plain-http remote host (interceptable) is not accepted.
            // Immutable after creation.
            panelUrl: {
              type: "string" as const,
              pattern:
                "^(https://|http://(localhost|127\\.0\\.0\\.1)([:/?#]|$))",
              maxLength: 2048,
            },
            // The program's own page for this session, offered to the user as
            // "Open in <Program>". Opened in a real browser, so http(s) is
            // fine. Unlike the two above, this one can be changed later.
            viewUrl: {
              type: "string" as const,
              pattern: "^https?://",
              maxLength: 2048,
            },
          },
          additionalProperties: false,
        },
      },
    },
    async (request, reply) => {
      const { name, metadata, clips, redirectUrl, panelUrl, viewUrl } =
        request.body || {};

      const [session] = await db
        .insert(schema.sessions)
        .values({
          ...(name ? { name } : {}),
          metadata: metadata ?? {},
          // Opt-OUT: clips are the default capture mode. `clips: false`
          // pins a session to the legacy one-JPEG-per-minute payload.
          clipsEnabled: clips ?? true,
          redirectUrl: redirectUrl ?? null,
          panelUrl: panelUrl ?? null,
          viewUrl: viewUrl ?? null,
          // Attribution: tag with the creating program (null for global key).
          // `program` (name) is dual-written for backward compatibility;
          // `programId` is the canonical attribution.
          program: request.program ?? null,
          programId: request.programId ?? null,
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
  // List the calling program's sessions, newest first.
  //
  // Every other read here needs a token or a session id you already hold,
  // which means a program's own dashboard can list what it created but a
  // tool acting for that program cannot see anything it did not create
  // itself. This closes that: the key already identifies the program, and
  // sessions carry programId, so a program can enumerate its own work.
  //
  // Deliberately does NOT return tokens. A token is the capability to
  // record into a session; listing is a read, and the two should not be the
  // same permission. Everything needed to display and review a session
  // (media URLs, timings, status) is reachable from the id.
  app.get<{
    Querystring: { limit?: number; cursor?: string };
  }>(
    "/api/internal/sessions",
    {
      schema: {
        querystring: {
          type: "object" as const,
          properties: {
            limit: {
              type: "integer" as const,
              minimum: 1,
              maximum: 100,
              default: 50,
            },
            // ISO-8601. Returns sessions created strictly before it, which
            // is stable under inserts in a way OFFSET is not.
            cursor: { type: "string" as const, format: "date-time" },
          },
          additionalProperties: false,
        },
      },
    },
    async (request, reply) => {
      // A key with no program can't scope this to anything. The legacy
      // global key lands here; "every session with no program" is close
      // enough to "everything" that it isn't on offer.
      if (!request.programId) {
        return reply.code(400).send({
          error:
            "This key is not attached to a program, so it has no sessions to list",
        });
      }

      const limit = request.query.limit ?? 50;
      const cursor = request.query.cursor
        ? new Date(request.query.cursor)
        : null;
      if (cursor && Number.isNaN(cursor.getTime())) {
        return reply.code(400).send({ error: "Invalid cursor" });
      }

      // One extra row tells us whether there is another page without a
      // second count query.
      const rows = await db
        .select()
        .from(schema.sessions)
        .where(
          and(
            eq(schema.sessions.programId, request.programId),
            cursor ? lt(schema.sessions.createdAt, cursor) : undefined,
          ),
        )
        .orderBy(desc(schema.sessions.createdAt))
        .limit(limit + 1);

      const page = rows.slice(0, limit);
      const nextCursor =
        rows.length > limit
          ? page[page.length - 1].createdAt.toISOString()
          : null;

      // Screenshot counts for the page in one query, as the batch endpoint
      // does. Credit-mode sessions don't need it but mixing the two costs
      // less than branching per row.
      const ids = page.map((s) => s.id);
      const counts =
        ids.length > 0
          ? await db
              .select({
                sessionId: schema.screenshots.sessionId,
                bucketCount: sql<number>`count(distinct ${schema.screenshots.minuteBucket})`,
                screenshotCount: sql<number>`count(*)`,
              })
              .from(schema.screenshots)
              .where(
                and(
                  inArray(schema.screenshots.sessionId, ids),
                  eq(schema.screenshots.confirmed, true),
                ),
              )
              .groupBy(schema.screenshots.sessionId)
          : [];

      const countMap = new Map(
        counts.map((c) => [
          c.sessionId,
          {
            bucketTrackedSeconds: Math.max(0, (Number(c.bucketCount) - 1) * 60),
            screenshotCount: Number(c.screenshotCount),
          },
        ]),
      );

      const baseUrl = process.env.BASE_URL || "http://localhost:3000";

      return {
        sessions: page.map((s) => {
          const c = countMap.get(s.id) ?? {
            bucketTrackedSeconds: 0,
            screenshotCount: 0,
          };
          // Same rule as /api/sessions/batch: credit-mode trusts the
          // column, bucket-mode prefers it and falls back to a live count.
          const rawTrackedSeconds =
            s.trackingMode === "credit"
              ? s.trackedSeconds ?? 0
              : s.trackedSeconds ?? c.bucketTrackedSeconds;
          return {
            sessionId: s.id,
            name: s.name,
            status: s.status,
            // Cuts can only shrink this, and /timings excludes the same
            // captures, so every consumer tells one story.
            trackedSeconds: Math.max(
              0,
              rawTrackedSeconds - (s.cutSeconds ?? 0),
            ),
            screenshotCount: c.screenshotCount,
            startedAt: s.startedAt?.toISOString() ?? null,
            createdAt: s.createdAt.toISOString(),
            totalActiveSeconds: s.totalActiveSeconds,
            thumbnailUrl: s.thumbnailR2Key
              ? `${baseUrl}/api/media/${s.id}/thumbnail.jpg`
              : null,
            videoUrl: s.videoR2Key
              ? `${baseUrl}/api/media/${s.id}/video.mp4`
              : null,
            metadata: s.metadata ?? {},
            deleteAfter: s.deleteAfter?.toISOString() ?? null,
          };
        }),
        nextCursor,
      };
    },
  );

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

      // Exclude internal R2 storage keys (and the editor's unit map, which
      // is bulky plumbing) and build proper media URLs
      const baseUrl = process.env.BASE_URL || "http://localhost:3000";
      const {
        videoR2Key,
        thumbnailR2Key,
        originalVideoR2Key: _originalVideoR2Key,
        videoUnits: _videoUnits,
        ...sessionData
      } = session;
      // Bucket-mode: tracked = (distinct buckets - 1) * 60.
      // Credit-mode: read session.trackedSeconds directly (maintained per-credit).
      const liveBucketTracked = Math.max(0, (Number(count) - 1) * 60);
      const uncutTrackedSeconds =
        session.trackingMode === "credit"
          ? session.trackedSeconds ?? 0
          : session.trackedSeconds ?? liveBucketTracked;
      // Reported tracked time honors the session's cut list (user edits can
      // only shrink it); the raw value is surfaced alongside.
      const trackedSeconds = Math.max(
        0,
        uncutTrackedSeconds - (session.cutSeconds ?? 0),
      );
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
      // First recorded JA4 TLS fingerprint (edge-observed, resolved
      // independently of clientInfo). NULL when the edge never set it.
      const [firstJa4] = await db
        .select({ ja4: schema.screenshots.ja4 })
        .from(schema.screenshots)
        .where(
          and(
            eq(schema.screenshots.sessionId, sessionId),
            isNotNull(schema.screenshots.ja4),
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
        uncutTrackedSeconds,
        screenshotCount: Number(confirmedCount),
        clientInfo: firstClient?.clientInfo ?? null,
        ja4: firstJa4?.ja4 ?? null,
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

      if (session.deleteAfter) {
        return reply.code(409).send({ error: "Session is scheduled for deletion" });
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
          isNull(schema.sessions.deleteAfter),
        ))
        .returning({ id: schema.sessions.id });

      if (!updated) {
        return reply.code(409).send({ error: "Session state changed concurrently" });
      }

      await boss.send(COMPILE_JOB, { sessionId });

      return { status: "stopped" };
    },
  );

  /**
   * A session the calling key is actually entitled to write, or null.
   *
   * The list endpoint above already scopes by `programId`; these per-session
   * mutations have to as well. Without it any key-holding program could re-aim
   * another program's "Open in …" button at a page of its choosing — rendered
   * inside the desktop app under the *other* program's name, which is a
   * ready-made phishing primitive — or silently mark their panel resolved and
   * strand the recording.
   *
   * Sessions created with the legacy global key have no `programId`, and a
   * program key must not be able to adopt them, so they are out of reach too.
   */
  async function writableSession(
    request: FastifyRequest,
    sessionId: string,
  ): Promise<WritableSession | null> {
    const session = await db.query.sessions.findFirst({
      where: eq(schema.sessions.id, sessionId),
      columns: {
        id: true,
        programId: true,
        panelResolvedAt: true,
        deleteAfter: true,
        status: true,
        originalVideoR2Key: true,
      },
    });

    if (!session) return null;

    // Ownership, not "is scoped": the caller must be the session's owner, and
    // for a key with no program that owner is the unowned pool. `programId` is
    // NULL for legacy/global keys by design, so demanding a program here locked
    // every pre-programs key out of its own sessions.
    //
    // What this still refuses is the case worth refusing: one program writing
    // another's session, or either direction across the legacy boundary.
    const owner = session.programId ?? null;
    const caller = request.programId ?? null;
    if (owner !== caller) return null;

    return session;
  }

  // Mark a session's program panel as satisfied.
  //
  // Panels are shown in-app, but the user can just as well finish on the
  // program's own website — and the client can't see inside either one. So the
  // program declares it: until it does, the client keeps offering the panel on
  // the session page, and after it does, the ask disappears everywhere.
  // Idempotent; calling it on a session with no panel is a no-op success, so
  // programs can call it unconditionally when they publish.
  app.post<{
    Params: { sessionId: string };
  }>(
    "/api/internal/sessions/:sessionId/panel-resolved",
    {
      schema: { params: sessionIdParamSchema },
    },
    async (request, reply) => {
      const { sessionId } = request.params;

      const session = await writableSession(request, sessionId);
      // Deliberately 404, not 403: whether a session id exists is not
      // something one program should learn about another's.
      if (!session) {
        return reply.code(404).send({ error: "Session not found" });
      }

      // First call wins, so the timestamp reflects when it was actually
      // satisfied rather than the last time the program mentioned it.
      const resolvedAt = session.panelResolvedAt ?? new Date();
      if (!session.panelResolvedAt) {
        await db
          .update(schema.sessions)
          .set({ panelResolvedAt: resolvedAt, updatedAt: new Date() })
          .where(eq(schema.sessions.id, sessionId));
      }

      return { panelResolved: true, panelResolvedAt: resolvedAt.toISOString() };
    },
  );

  // Set or clear a session's "Open in <Program>" link.
  //
  // Separate from creation because the target usually does not exist yet when
  // the session is made — the published timelapse, the submission record. Pass
  // null (or omit) to clear.
  app.post<{
    Params: { sessionId: string };
    Body: { viewUrl?: string | null };
  }>(
    "/api/internal/sessions/:sessionId/view-url",
    {
      schema: {
        params: sessionIdParamSchema,
        body: {
          type: "object" as const,
          properties: {
            viewUrl: {
              type: ["string", "null"] as const,
              pattern: "^https?://",
              maxLength: 2048,
            },
          },
          additionalProperties: false,
        },
      },
    },
    async (request, reply) => {
      const { sessionId } = request.params;
      const next = request.body?.viewUrl ?? null;

      if (!(await writableSession(request, sessionId))) {
        return reply.code(404).send({ error: "Session not found" });
      }

      const [updated] = await db
        .update(schema.sessions)
        .set({ viewUrl: next, updatedAt: new Date() })
        .where(eq(schema.sessions.id, sessionId))
        .returning({ viewUrl: schema.sessions.viewUrl });

      if (!updated) {
        return reply.code(404).send({ error: "Session not found" });
      }
      return { viewUrl: updated.viewUrl };
    },
  );

  // Delete a session, after a grace period.
  //
  // For programs honouring a user's erasure request, or discarding a
  // recording outright. Nothing is destroyed on this call: the session is
  // hidden at once — every token and media endpoint answers 404, so the
  // recorder drops it and the video stops being served — and the purge job
  // destroys captures, video and rows once SESSION_DELETION_GRACE_MINUTES
  // have passed. Until then POST …/restore puts it back exactly as it was,
  // which is the point of the window: a program bug, a leaked key or a wrong
  // user id costs an hour of downtime instead of someone's work.
  //
  // Idempotent, and retries don't extend the window: a second call answers
  // with the deadline the first one set. 202 because the deletion hasn't
  // happened yet.
  app.delete<{
    Params: { sessionId: string };
  }>(
    "/api/internal/sessions/:sessionId",
    {
      schema: { params: sessionIdParamSchema },
    },
    async (request, reply) => {
      const { sessionId } = request.params;

      const session = await writableSession(request, sessionId);
      if (!session) {
        return reply.code(404).send({ error: "Session not found" });
      }

      // Guarded on still being unscheduled, so two racing calls agree on one
      // deadline rather than the later one quietly pushing it out.
      await db
        .update(schema.sessions)
        .set({
          deleteAfter: new Date(
            Date.now() + SESSION_DELETION_GRACE_MINUTES * 60_000,
          ),
          updatedAt: new Date(),
        })
        .where(
          and(
            eq(schema.sessions.id, sessionId),
            isNull(schema.sessions.deleteAfter),
          ),
        );

      const [scheduled] = await db
        .select({ deleteAfter: schema.sessions.deleteAfter })
        .from(schema.sessions)
        .where(eq(schema.sessions.id, sessionId));

      // Gone between the two queries: the purge got there first.
      if (!scheduled?.deleteAfter) {
        return reply.code(404).send({ error: "Session not found" });
      }

      return reply.code(202).send({
        sessionId,
        deleteAfter: scheduled.deleteAfter.toISOString(),
      });
    },
  );

  // Cancel a pending deletion while the grace period is still running.
  //
  // The session comes back as it was, with one thing to pick back up: the
  // compiler refuses scheduled sessions, so one that was stopped but not yet
  // built when it was scheduled would otherwise sit in `stopped` forever.
  // Re-enqueueing is safe — the claim is atomic.
  app.post<{
    Params: { sessionId: string };
  }>(
    "/api/internal/sessions/:sessionId/restore",
    {
      schema: { params: sessionIdParamSchema },
    },
    async (request, reply) => {
      const { sessionId } = request.params;

      const session = await writableSession(request, sessionId);
      if (!session) {
        return reply.code(404).send({ error: "Session not found" });
      }

      if (!session.deleteAfter) {
        return reply
          .code(409)
          .send({ error: "Session is not scheduled for deletion" });
      }

      // Past the deadline the purge may already be destroying it, so the
      // answer has to be no even while the row is still here.
      const [restored] = await db
        .update(schema.sessions)
        .set({ deleteAfter: null, updatedAt: new Date() })
        .where(
          and(
            eq(schema.sessions.id, sessionId),
            gt(schema.sessions.deleteAfter, new Date()),
          ),
        )
        .returning({ id: schema.sessions.id });

      if (!restored) {
        return reply
          .code(410)
          .send({ error: "Grace period has ended; the session is being deleted" });
      }

      if (session.status === "stopped" && !session.originalVideoR2Key) {
        await boss.send(COMPILE_JOB, { sessionId });
      }

      return { restored: true };
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

      if (session.deleteAfter) {
        return reply.code(409).send({ error: "Session is scheduled for deletion" });
      }

      if (session.status !== "failed") {
        return reply
          .code(409)
          .send({ error: "Only failed sessions can be recompiled" });
      }

      const [updated] = await db
        .update(schema.sessions)
        .set({ status: "compiling", updatedAt: new Date() })
        .where(and(
          eq(schema.sessions.id, sessionId),
          eq(schema.sessions.status, "failed"),
          isNull(schema.sessions.deleteAfter),
        ))
        .returning({ id: schema.sessions.id });

      if (!updated) {
        return reply.code(409).send({ error: "Session state changed concurrently" });
      }

      await boss.send(COMPILE_JOB, { sessionId });

      return { status: "compiling" };
    },
  );
}
