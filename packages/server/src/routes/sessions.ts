import type { FastifyInstance } from "fastify";
import { eq, sql, and, inArray } from "drizzle-orm";
import { PutObjectCommand, HeadObjectCommand } from "@aws-sdk/client-s3";
import { getSignedUrl } from "@aws-sdk/s3-request-presigner";
import { randomUUID } from "node:crypto";
import { db, schema } from "../db/index.js";
import { r2Client, R2_BUCKET } from "../config/r2.js";
import { boss, COMPILE_JOB } from "../lib/queue.js";
import {
  computeMinuteBucket,
  checkRateLimit,
  checkGenericRateLimit,
  creditCapture,
  validateCapturedAt,
} from "../lib/timing.js";
import { now } from "../lib/clock.js";
import {
  SCREENSHOT_INTERVAL_MS,
  PRESIGNED_URL_EXPIRY_SECONDS,
  MAX_SCREENSHOT_BYTES,
  MAX_SCREENSHOTS_PER_SESSION,
  MAX_UPLOAD_REQUESTS_PER_SESSION,
} from "@lookout/shared";

/** Tracked-seconds dispatcher. Routes to bucket-count math for legacy
 *  sessions or reads the incrementally-maintained value for credit-mode
 *  sessions. Always go through this — never inline the SQL. */
async function getTrackedSecondsForSession(session: {
  id: string;
  trackingMode: string;
  trackedSeconds: number | null;
}): Promise<number> {
  if (session.trackingMode === "credit") {
    return session.trackedSeconds ?? 0;
  }
  return getTrackedSecondsBucket(session.id);
}

async function getTrackedSecondsBucket(sessionId: string): Promise<number> {
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
  return Math.max(0, (Number(count) - 1) * 60);
}

// ── Shared schema fragments ─────────────────────────────────

const tokenParamSchema = {
  type: "object" as const,
  properties: {
    token: { type: "string" as const, pattern: "^[0-9a-fA-F]{64}$" },
  },
  required: ["token"] as const,
};

const sessionIdParamSchema = {
  type: "object" as const,
  properties: {
    sessionId: { type: "string" as const, format: "uuid" },
  },
  required: ["sessionId"] as const,
};

/** Helper to look up session by token */
async function findSession(token: string) {
  return db.query.sessions.findFirst({
    where: eq(schema.sessions.token, token),
  });
}

/** Count total confirmed screenshots */
async function getScreenshotCount(sessionId: string): Promise<number> {
  const [{ count }] = await db
    .select({ count: sql<number>`count(*)` })
    .from(schema.screenshots)
    .where(
      and(
        eq(schema.screenshots.sessionId, sessionId),
        eq(schema.screenshots.confirmed, true),
      ),
    );
  return Number(count);
}

/** Count total upload-url requests (confirmed + unconfirmed) */
async function getTotalUploadRequests(sessionId: string): Promise<number> {
  const [{ count }] = await db
    .select({ count: sql<number>`count(*)` })
    .from(schema.screenshots)
    .where(eq(schema.screenshots.sessionId, sessionId));
  return Number(count);
}

export async function sessionRoutes(app: FastifyInstance) {
  // Get session status (used for recovery after refresh)
  app.get<{ Params: { token: string } }>(
    "/api/sessions/:token",
    {
      schema: { params: tokenParamSchema },
    },
    async (request, reply) => {
      // Rate limit: 60 req/min per token (status polling)
      const rl = checkGenericRateLimit("session-get", request.params.token, 60);
      if (!rl.allowed) {
        reply.header(
          "Retry-After",
          String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)),
        );
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      const session = await findSession(request.params.token);
      if (!session) return reply.code(404).send({ error: "Session not found" });

      const liveTrackedSeconds = await getTrackedSecondsForSession(session);
      const screenshotCount = await getScreenshotCount(session.id);
      // Prefer stored value (survives screenshot cleanup), fall back to live count.
      // For credit mode, both paths read sessions.tracked_seconds so they match.
      const trackedSeconds =
        session.trackingMode === "credit"
          ? liveTrackedSeconds
          : session.trackedSeconds ?? liveTrackedSeconds;

      const baseUrl = process.env.BASE_URL || "http://localhost:3000";
      return {
        name: session.name,
        status: session.status,
        trackedSeconds,
        screenshotCount,
        startedAt: session.startedAt?.toISOString() ?? null,
        totalActiveSeconds: session.totalActiveSeconds,
        createdAt: session.createdAt.toISOString(),
        thumbnailUrl: session.thumbnailR2Key
          ? `${baseUrl}/api/media/${session.id}/thumbnail.jpg`
          : null,
        videoUrl: session.videoR2Key
          ? `${baseUrl}/api/media/${session.id}/video.mp4`
          : null,
        // Backwards compat: legacy clients keyed off this. Points at a static
        // "please update" video when the session is otherwise playable.
        videoWebmUrl: session.videoR2Key ? `${baseUrl}/please-update.webm` : null,
        metadata: session.metadata ?? {},
      };
    },
  );

  // Rename session
  app.patch<{
    Params: { token: string };
    Body: { name: string };
  }>(
    "/api/sessions/:token/name",
    {
      schema: {
        params: tokenParamSchema,
        body: {
          type: "object" as const,
          required: ["name"] as const,
          properties: {
            name: { type: "string" as const, minLength: 1, maxLength: 255 },
          },
          additionalProperties: false,
        },
      },
    },
    async (request, reply) => {
      const rl = checkGenericRateLimit("session-rename", request.params.token, 20);
      if (!rl.allowed) {
        reply.header(
          "Retry-After",
          String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)),
        );
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      const session = await findSession(request.params.token);
      if (!session) return reply.code(404).send({ error: "Session not found" });

      await db
        .update(schema.sessions)
        .set({ name: request.body.name, updatedAt: new Date() })
        .where(eq(schema.sessions.id, session.id));

      return { name: request.body.name };
    },
  );

  // Get presigned upload URL.
  // Accepts optional `capturedAt` (ISO string) in the querystring. Presence
  // on the first request flips the session into credit mode for life; mode
  // is sticky thereafter. See plan doc for details.
  app.get<{
    Params: { token: string };
    Querystring: { capturedAt?: string };
  }>(
    "/api/sessions/:token/upload-url",
    {
      schema: {
        params: tokenParamSchema,
        querystring: {
          type: "object" as const,
          properties: {
            capturedAt: { type: "string" as const, format: "date-time" },
          },
          additionalProperties: false,
        },
      },
    },
    async (request, reply) => {
      const session = await findSession(request.params.token);
      if (!session) return reply.code(404).send({ error: "Session not found" });

      // Activate pending sessions on first upload-url request
      const isActivating = session.status === "pending";
      if (!isActivating && session.status !== "active") {
        return reply
          .code(409)
          .send({ error: `Session is ${session.status}, cannot upload` });
      }

      // Rate limiting
      const rl = checkRateLimit(session.id);
      if (!rl.allowed) {
        reply.header(
          "Retry-After",
          String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)),
        );
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      // Session-level hard cap
      const totalRequests = await getTotalUploadRequests(session.id);
      if (totalRequests >= MAX_UPLOAD_REQUESTS_PER_SESSION) {
        return reply
          .code(429)
          .send({ error: "Max upload requests per session exceeded" });
      }

      const serverNow = now();
      const clientCapturedAtRaw = request.query.capturedAt;
      const clientCapturedAt = clientCapturedAtRaw
        ? new Date(clientCapturedAtRaw)
        : null;
      if (clientCapturedAt && Number.isNaN(clientCapturedAt.getTime())) {
        return reply
          .code(400)
          .send({ error: "captured_at_invalid" });
      }

      // Activate session if pending, and resolve the effective tracking mode.
      // Mode-flip is atomic: only the very first upload (no existing
      // screenshot rows) that carries capturedAt can switch to credit.
      let trackingMode = session.trackingMode;
      let startedAt: Date;

      if (isActivating) {
        // We may flip the mode atomically with activation. If capturedAt is
        // present AND no screenshots exist yet, flip to credit.
        const wantsCredit = clientCapturedAt !== null;
        const noScreenshots =
          totalRequests === 0; // we measured this above; race-free for activation

        // Use the client's capturedAt as started_at when it's there: the
        // session "starts" at the moment the first screenshot was taken,
        // not when the server's HTTP handler ran. Without this, upload
        // latency + client clock skew make the very first capturedAt fall
        // microseconds before serverNow and trip captured_at_before_session_start.
        //
        // Envelope check below would catch a wildly-skewed clientCapturedAt;
        // do a quick anti-future-cheat check here so an attacker can't set
        // startedAt far in the future. clamp to ≤ serverNow.
        let activationStartedAt = serverNow;
        if (clientCapturedAt) {
          // bound to past envelope (5min) so a malicious client can't push
          // started_at arbitrarily into the past.
          const minAllowed = new Date(serverNow.getTime() - 5 * 60_000);
          const clamped = clientCapturedAt < minAllowed ? minAllowed : clientCapturedAt;
          // never set started_at in the future.
          activationStartedAt = clamped > serverNow ? serverNow : clamped;
        }

        const setFields: Record<string, unknown> = {
          status: "active",
          startedAt: activationStartedAt,
          lastScreenshotAt: serverNow,
          updatedAt: serverNow,
        };
        if (wantsCredit && noScreenshots) {
          setFields.trackingMode = "credit";
        }
        const [updated] = await db
          .update(schema.sessions)
          .set(setFields)
          .where(and(eq(schema.sessions.id, session.id), eq(schema.sessions.status, "pending")))
          .returning({
            id: schema.sessions.id,
            trackingMode: schema.sessions.trackingMode,
            startedAt: schema.sessions.startedAt,
          });
        if (!updated) {
          // Another request already activated; re-fetch and continue
          const refreshed = await findSession(request.params.token);
          if (!refreshed || (refreshed.status !== "active" && refreshed.status !== "pending")) {
            return reply.code(409).send({ error: `Session is ${refreshed?.status ?? "unknown"}, cannot upload` });
          }
          trackingMode = refreshed.trackingMode;
          startedAt = refreshed.startedAt!;
        } else {
          trackingMode = updated.trackingMode;
          startedAt = updated.startedAt!;
        }
      } else {
        // Existing active session. Try a one-shot mode flip if we're the
        // very first upload of an already-active session (rare but possible:
        // session was activated by some other code path with no screenshots).
        // Guarded on tracking_mode='bucket' AND no screenshots rows.
        if (clientCapturedAt && trackingMode === "bucket") {
          const [{ count }] = await db
            .select({ count: sql<number>`count(*)::int` })
            .from(schema.screenshots)
            .where(eq(schema.screenshots.sessionId, session.id));
          if (Number(count) === 0) {
            const [flipped] = await db
              .update(schema.sessions)
              .set({ trackingMode: "credit", updatedAt: serverNow })
              .where(
                and(
                  eq(schema.sessions.id, session.id),
                  eq(schema.sessions.trackingMode, "bucket"),
                ),
              )
              .returning({ trackingMode: schema.sessions.trackingMode });
            if (flipped) trackingMode = flipped.trackingMode;
          }
        }

        await db
          .update(schema.sessions)
          .set({ lastScreenshotAt: serverNow, updatedAt: serverNow })
          .where(eq(schema.sessions.id, session.id));
        startedAt = session.startedAt!;
      }

      // Resolve the row's `captured_at` value — populated in both modes for
      // debugging. In bucket mode it's never read for math.
      const rowCapturedAt = clientCapturedAt ?? serverNow;

      // Credit-mode: capturedAt is required and must pass the envelope.
      let nextExpectedAt: Date;
      if (trackingMode === "credit") {
        if (!clientCapturedAt) {
          return reply
            .code(400)
            .send({ error: "credit_mode_requires_captured_at" });
        }

        // Look up the latest existing capturedAt for monotonicity.
        const [latest] = await db
          .select({ capturedAt: schema.screenshots.capturedAt })
          .from(schema.screenshots)
          .where(eq(schema.screenshots.sessionId, session.id))
          .orderBy(sql`${schema.screenshots.capturedAt} DESC NULLS LAST`)
          .limit(1);

        const validation = validateCapturedAt(
          clientCapturedAt,
          serverNow,
          startedAt,
          latest?.capturedAt ?? null,
        );
        if (!validation.ok) {
          return reply.code(400).send({ error: validation.code });
        }

        // Predict nextExpectedAt assuming this capture will credit. The
        // confirm response returns the authoritative post-credit value.
        // Note: streak_credited_count is the count BEFORE this capture.
        // If anchor is null, this will seed → next is captured + 60s.
        // Else this is the (count+1)th capture, so the *next next* mark is
        // anchor + (count + 2) * 60s.
        if (session.streakAnchorAt === null) {
          nextExpectedAt = new Date(
            clientCapturedAt.getTime() + SCREENSHOT_INTERVAL_MS,
          );
        } else {
          nextExpectedAt = new Date(
            session.streakAnchorAt.getTime() +
              (session.streakCreditedCount + 2) * SCREENSHOT_INTERVAL_MS,
          );
        }
      } else {
        // Bucket mode: existing semantics.
        nextExpectedAt = new Date(serverNow.getTime() + SCREENSHOT_INTERVAL_MS);
      }

      const minuteBucket = computeMinuteBucket(serverNow, startedAt);
      const screenshotId = randomUUID();
      const r2Key = `screenshots/${session.id}/${screenshotId}.jpg`;

      // Create screenshot record (unconfirmed)
      await db.insert(schema.screenshots).values({
        id: screenshotId,
        sessionId: session.id,
        r2Key,
        requestedAt: serverNow,
        minuteBucket,
        confirmed: false,
        capturedAt: rowCapturedAt,
      });

      // Generate presigned PUT URL
      // Note: Don't set ContentLength — it signs an exact size and rejects
      // anything different. Size is validated at confirmation via HeadObject.
      // Orphaned uploads are cleaned up by the unconfirmed cleanup job.
      const command = new PutObjectCommand({
        Bucket: R2_BUCKET,
        Key: r2Key,
        ContentType: "image/jpeg",
      });

      const uploadUrl = await getSignedUrl(r2Client, command, {
        expiresIn: PRESIGNED_URL_EXPIRY_SECONDS,
      });

      return {
        uploadUrl,
        r2Key,
        screenshotId,
        minuteBucket,
        nextExpectedAt: nextExpectedAt.toISOString(),
        serverTime: serverNow.toISOString(),
        trackingMode,
      };
    },
  );

  // Confirm screenshot upload
  app.post<{
    Params: { token: string };
    Body: {
      screenshotId: string;
      width: number;
      height: number;
      fileSize: number;
    };
  }>(
    "/api/sessions/:token/screenshots",
    {
      schema: {
        params: tokenParamSchema,
        body: {
          type: "object" as const,
          required: ["screenshotId", "width", "height", "fileSize"] as const,
          properties: {
            screenshotId: { type: "string" as const, format: "uuid" },
            width: { type: "integer" as const, minimum: 1 },
            height: { type: "integer" as const, minimum: 1 },
            fileSize: { type: "integer" as const, minimum: 1 },
          },
          additionalProperties: false,
        },
      },
    },
    async (request, reply) => {
      // Rate limit: 20 req/min per token (screenshot confirmation).
      // Paired with RATE_LIMIT_PER_MINUTE=10 on upload-url + 3 client
      // retries — 20 leaves headroom for retried confirms after a
      // transient network failure between PUT and POST.
      const rl = checkGenericRateLimit(
        "screenshot-confirm",
        request.params.token,
        20,
      );
      if (!rl.allowed) {
        reply.header(
          "Retry-After",
          String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)),
        );
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      const session = await findSession(request.params.token);
      if (!session) return reply.code(404).send({ error: "Session not found" });

      if (session.status !== "active" && session.status !== "pending") {
        return reply
          .code(409)
          .send({ error: `Session is ${session.status}, cannot confirm` });
      }

      const { screenshotId, width, height, fileSize } = request.body;

      // Validate screenshot belongs to this session and isn't already confirmed
      const screenshot = await db.query.screenshots.findFirst({
        where: and(
          eq(schema.screenshots.id, screenshotId),
          eq(schema.screenshots.sessionId, session.id),
        ),
      });

      if (!screenshot) {
        return reply.code(404).send({ error: "Screenshot not found" });
      }

      const serverNow = now();

      // Idempotent: already confirmed. Return cached trackedSeconds and a
      // freshly-computed nextExpectedAt (the streak may have advanced since
      // the original confirm — never return a stale target).
      if (screenshot.confirmed) {
        const trackedSeconds = await getTrackedSecondsForSession(session);
        let nextExpectedAt: string;
        if (session.trackingMode === "credit" && session.streakAnchorAt) {
          nextExpectedAt = new Date(
            session.streakAnchorAt.getTime() +
              (session.streakCreditedCount + 1) * SCREENSHOT_INTERVAL_MS,
          ).toISOString();
        } else {
          nextExpectedAt = new Date(
            serverNow.getTime() + SCREENSHOT_INTERVAL_MS,
          ).toISOString();
        }
        return {
          confirmed: true,
          trackedSeconds,
          nextExpectedAt,
          serverTime: serverNow.toISOString(),
        };
      }

      // Verify the object actually exists in R2 and is within size limits
      try {
        const head = await r2Client.send(
          new HeadObjectCommand({ Bucket: R2_BUCKET, Key: screenshot.r2Key }),
        );

        // Validate ContentType is image/jpeg
        if (head.ContentType !== "image/jpeg") {
          return reply
            .code(400)
            .send({ error: "Invalid content type — expected image/jpeg" });
        }

        // Validate file size is within limits
        if (head.ContentLength && head.ContentLength > MAX_SCREENSHOT_BYTES) {
          return reply.code(400).send({ error: "Uploaded object is too large" });
        }
      } catch {
        return reply
          .code(400)
          .send({ error: "Screenshot not found in storage — upload may have failed" });
      }

      // Check confirmed screenshot cap
      const confirmedCount = await getScreenshotCount(session.id);
      if (confirmedCount >= MAX_SCREENSHOTS_PER_SESSION) {
        return reply
          .code(429)
          .send({ error: "Max screenshots per session exceeded" });
      }

      let nextExpectedAtIso: string;
      let trackedSeconds: number;

      if (session.trackingMode === "credit") {
        // Credit-mode: run streak math + writes in one transaction with a
        // row lock on the session so concurrent confirms serialize.
        const result = await db.transaction(async (tx) => {
          // SELECT FOR UPDATE serializes concurrent confirms for this session.
          // node-postgres returns timestamps as strings by default — coerce
          // to Date before any time math.
          const locked = await tx.execute(sql`
            SELECT id, streak_anchor_at, streak_credited_count, tracked_seconds
            FROM sessions WHERE id = ${session.id} FOR UPDATE
          `);
          const rawRow = (locked as unknown as { rows: Array<{
            streak_anchor_at: Date | string | null;
            streak_credited_count: number | string;
            tracked_seconds: number | string | null;
          }> }).rows[0];
          const streakAnchorAt: Date | null = rawRow.streak_anchor_at
            ? rawRow.streak_anchor_at instanceof Date
              ? rawRow.streak_anchor_at
              : new Date(rawRow.streak_anchor_at)
            : null;
          const streakCreditedCount = Number(rawRow.streak_credited_count);
          const currentTracked = Number(rawRow.tracked_seconds ?? 0);

          const cap = screenshot.capturedAt ?? serverNow;
          const decision = creditCapture(
            cap,
            streakAnchorAt,
            streakCreditedCount,
          );

          // Mark screenshot confirmed + record credit + expected_at. The
          // WHERE confirmed=false guard provides per-row idempotency: if a
          // racing confirm beat us, this affects zero rows and we'll be a
          // no-op (the streak update below would then double-credit, so we
          // must check the returned row count).
          const confirmedRows = await tx
            .update(schema.screenshots)
            .set({
              confirmed: true,
              width,
              height,
              fileSizeBytes: fileSize,
              creditedSeconds: decision.credit,
              expectedAt: decision.expectedAt,
            })
            .where(
              and(
                eq(schema.screenshots.id, screenshotId),
                eq(schema.screenshots.confirmed, false),
              ),
            )
            .returning({ id: schema.screenshots.id });

          if (confirmedRows.length === 0) {
            // Lost the race — another confirm flipped the row. Just read the
            // current session state and return.
            return { trackedSeconds: currentTracked, decision };
          }

          // Apply streak state + advance tracked_seconds atomically.
          const newTracked = currentTracked + decision.credit;
          await tx
            .update(schema.sessions)
            .set({
              streakAnchorAt: decision.newAnchor,
              streakCreditedCount: decision.newCount,
              trackedSeconds: newTracked,
              lastScreenshotAt: serverNow,
              updatedAt: serverNow,
            })
            .where(eq(schema.sessions.id, session.id));

          return { trackedSeconds: newTracked, decision };
        });

        trackedSeconds = result.trackedSeconds;
        nextExpectedAtIso = result.decision.nextExpectedAt.toISOString();
      } else {
        // Bucket-mode: existing semantics. Flip the row, bump
        // last_screenshot_at, compute trackedSeconds from bucket count.
        await db
          .update(schema.screenshots)
          .set({
            confirmed: true,
            width,
            height,
            fileSizeBytes: fileSize,
          })
          .where(eq(schema.screenshots.id, screenshotId));

        await db
          .update(schema.sessions)
          .set({ lastScreenshotAt: serverNow, updatedAt: serverNow })
          .where(eq(schema.sessions.id, session.id));

        trackedSeconds = await getTrackedSecondsBucket(session.id);
        nextExpectedAtIso = new Date(
          serverNow.getTime() + SCREENSHOT_INTERVAL_MS,
        ).toISOString();
      }

      return {
        confirmed: true,
        trackedSeconds,
        nextExpectedAt: nextExpectedAtIso,
        serverTime: serverNow.toISOString(),
      };
    },
  );

  // Pause session
  app.post<{ Params: { token: string } }>(
    "/api/sessions/:token/pause",
    {
      schema: { params: tokenParamSchema },
    },
    async (request, reply) => {
      // Rate limit: 10 req/min per token (actions)
      const rl = checkGenericRateLimit("session-pause", request.params.token, 10);
      if (!rl.allowed) {
        reply.header(
          "Retry-After",
          String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)),
        );
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      const session = await findSession(request.params.token);
      if (!session) return reply.code(404).send({ error: "Session not found" });

      // Pending sessions: no active time to accumulate, return no-op
      if (session.status === "pending") {
        return { status: "paused" as const, totalActiveSeconds: 0 };
      }

      // Already paused: idempotent
      if (session.status === "paused") {
        return {
          status: "paused" as const,
          totalActiveSeconds: session.totalActiveSeconds,
        };
      }

      if (session.status !== "active") {
        return reply
          .code(409)
          .send({ error: `Session is ${session.status}, cannot pause` });
      }

      // Accumulate active time (with optimistic locking)
      const activeFrom =
        session.resumedAt || session.startedAt!;
      const additionalSeconds = Math.floor(
        (Date.now() - activeFrom.getTime()) / 1000,
      );

      const [updated] = await db
        .update(schema.sessions)
        .set({
          status: "paused",
          pausedAt: new Date(),
          totalActiveSeconds: session.totalActiveSeconds + additionalSeconds,
          updatedAt: new Date(),
        })
        .where(and(eq(schema.sessions.id, session.id), eq(schema.sessions.status, "active")))
        .returning({ id: schema.sessions.id });

      if (!updated) {
        return reply.code(409).send({ error: "Session state changed concurrently, please retry" });
      }

      return {
        status: "paused" as const,
        totalActiveSeconds: session.totalActiveSeconds + additionalSeconds,
      };
    },
  );

  // Resume session
  app.post<{ Params: { token: string } }>(
    "/api/sessions/:token/resume",
    {
      schema: { params: tokenParamSchema },
    },
    async (request, reply) => {
      // Rate limit: 10 req/min per token (actions)
      const rl = checkGenericRateLimit("session-resume", request.params.token, 10);
      if (!rl.allowed) {
        reply.header(
          "Retry-After",
          String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)),
        );
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      const session = await findSession(request.params.token);
      if (!session) return reply.code(404).send({ error: "Session not found" });

      if (session.status !== "paused") {
        return reply
          .code(409)
          .send({ error: `Session is ${session.status}, cannot resume` });
      }

      const resumeNow = now();

      // Credit-mode: clear the streak so the first post-resume capture
      // seeds a fresh anchor with 0 credit. Without this, the natural
      // "out-of-window resets streak" branch would burn 60s of credit on
      // every resume, even though the bucket-mode equivalent doesn't.
      const setFields: Record<string, unknown> = {
        status: "active",
        pausedAt: null,
        resumedAt: resumeNow,
        lastScreenshotAt: resumeNow,
        updatedAt: resumeNow,
      };
      if (session.trackingMode === "credit") {
        setFields.streakAnchorAt = null;
        setFields.streakCreditedCount = 0;
      }

      const [updated] = await db
        .update(schema.sessions)
        .set(setFields)
        .where(and(eq(schema.sessions.id, session.id), eq(schema.sessions.status, "paused")))
        .returning({ id: schema.sessions.id });

      if (!updated) {
        return reply.code(409).send({ error: "Session state changed concurrently, please retry" });
      }

      const nextExpectedAt = new Date(
        resumeNow.getTime() + SCREENSHOT_INTERVAL_MS,
      ).toISOString();

      return {
        status: "active" as const,
        nextExpectedAt,
        serverTime: resumeNow.toISOString(),
      };
    },
  );

  // Stop session
  app.post<{ Params: { token: string } }>(
    "/api/sessions/:token/stop",
    {
      schema: { params: tokenParamSchema },
    },
    async (request, reply) => {
      // Rate limit: 10 req/min per token (actions)
      const rl = checkGenericRateLimit("session-stop", request.params.token, 10);
      if (!rl.allowed) {
        reply.header(
          "Retry-After",
          String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)),
        );
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      const session = await findSession(request.params.token);
      if (!session) return reply.code(404).send({ error: "Session not found" });

      if (
        session.status !== "active" &&
        session.status !== "paused" &&
        session.status !== "pending"
      ) {
        return reply
          .code(409)
          .send({ error: `Session is ${session.status}, cannot stop` });
      }

      // Accumulate remaining active time
      let totalActiveSeconds = session.totalActiveSeconds;
      if (session.status === "active" && session.startedAt) {
        const activeFrom =
          session.resumedAt || session.startedAt;
        totalActiveSeconds += Math.floor(
          (Date.now() - activeFrom.getTime()) / 1000,
        );
      }

      const stopNow = now();

      // Compute tracked seconds before stopping (screenshots may be cleaned up later)
      const trackedSeconds = await getTrackedSecondsForSession(session);

      const [updated] = await db
        .update(schema.sessions)
        .set({
          status: "stopped",
          stoppedAt: stopNow,
          totalActiveSeconds,
          trackedSeconds,
          updatedAt: stopNow,
        })
        .where(and(
          eq(schema.sessions.id, session.id),
          sql`${schema.sessions.status} IN ('active', 'paused', 'pending')`,
        ))
        .returning({ id: schema.sessions.id });

      if (!updated) {
        return reply.code(409).send({ error: "Session state changed concurrently, please retry" });
      }

      // Enqueue compilation
      const screenshotCount = await getScreenshotCount(session.id);
      if (screenshotCount > 0) {
        await boss.send(COMPILE_JOB, { sessionId: session.id });
      } else {
        // No screenshots — mark failed (no video possible)
        await db
          .update(schema.sessions)
          .set({ status: "failed", updatedAt: stopNow })
          .where(eq(schema.sessions.id, session.id));
      }

      return {
        status: "stopped" as const,
        trackedSeconds,
        totalActiveSeconds,
      };
    },
  );

  // Poll compilation status
  app.get<{ Params: { token: string } }>(
    "/api/sessions/:token/status",
    {
      schema: { params: tokenParamSchema },
    },
    async (request, reply) => {
      // Rate limit: 60 req/min per token (status polling)
      const rl = checkGenericRateLimit("session-status", request.params.token, 60);
      if (!rl.allowed) {
        reply.header(
          "Retry-After",
          String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)),
        );
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      const session = await findSession(request.params.token);
      if (!session) return reply.code(404).send({ error: "Session not found" });

      const liveTrackedSeconds = await getTrackedSecondsForSession(session);
      // For credit mode, dispatcher already reads from session.trackedSeconds.
      const trackedSeconds =
        session.trackingMode === "credit"
          ? liveTrackedSeconds
          : session.trackedSeconds ?? liveTrackedSeconds;

      const baseUrl = process.env.BASE_URL || "http://localhost:3000";
      return {
        status: session.status,
        videoUrl: session.videoR2Key
          ? `${baseUrl}/api/media/${session.id}/video.mp4`
          : undefined,
        // Backwards compat: legacy clients used this to know completion + format.
        videoWebmUrl: session.videoR2Key
          ? `${baseUrl}/please-update.webm`
          : undefined,
        trackedSeconds,
      };
    },
  );

  // Get capture timings — public, token-gated.
  // Returns the ISO-8601 capture timestamps of every confirmed screenshot in
  // the session, oldest first. Uses captured_at (client-attested capture
  // moment); pre-migration rows that predate captured_at fall back to
  // requested_at so the array is never sparse.
  app.get<{ Params: { token: string } }>(
    "/api/sessions/:token/timings",
    {
      schema: { params: tokenParamSchema },
    },
    async (request, reply) => {
      // Rate limit: 30 req/min per token (read-only, potentially large body)
      const rl = checkGenericRateLimit("session-timings", request.params.token, 30);
      if (!rl.allowed) {
        reply.header(
          "Retry-After",
          String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)),
        );
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      const session = await findSession(request.params.token);
      if (!session) return reply.code(404).send({ error: "Session not found" });

      const rows = await db
        .select({
          ts: sql<Date>`coalesce(${schema.screenshots.capturedAt}, ${schema.screenshots.requestedAt})`,
        })
        .from(schema.screenshots)
        .where(
          and(
            eq(schema.screenshots.sessionId, session.id),
            eq(schema.screenshots.confirmed, true),
          ),
        )
        .orderBy(
          sql`coalesce(${schema.screenshots.capturedAt}, ${schema.screenshots.requestedAt}) ASC`,
        );

      // node-postgres may hand timestamps back as strings; coerce before toISOString.
      const timestamps = rows.map((r) =>
        (r.ts instanceof Date ? r.ts : new Date(r.ts)).toISOString(),
      );

      // first/last are convenience accessors on the already-ascending array.
      // NOTE: last − first is NOT capture duration — a paused session has gaps
      // between timestamps, so the span overstates actual recorded time.
      return {
        status: session.status,
        count: timestamps.length,
        first: timestamps[0] ?? null,
        last: timestamps[timestamps.length - 1] ?? null,
        timestamps,
      };
    },
  );

  // Get video presigned URL.
  // Legacy clients still pass ?format=webm — we no longer encode WebM, but
  // return a static "please update" WebM URL so the old player shows the
  // upgrade prompt instead of breaking.
  app.get<{ Params: { token: string }; Querystring: { format?: string } }>(
    "/api/sessions/:token/video",
    {
      schema: {
        params: tokenParamSchema,
        querystring: {
          type: "object" as const,
          properties: {
            format: { type: "string" as const, enum: ["mp4", "webm"] as const },
          },
        },
      },
    },
    async (request, reply) => {
      // Rate limit: 30 req/min per token
      const rl = checkGenericRateLimit("session-video", request.params.token, 30);
      if (!rl.allowed) {
        reply.header(
          "Retry-After",
          String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)),
        );
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      const session = await findSession(request.params.token);
      if (!session) return reply.code(404).send({ error: "Session not found" });

      if (session.status !== "complete" || !session.videoR2Key) {
        return reply.code(404).send({ error: "Video not available" });
      }

      const baseUrl = process.env.BASE_URL || "http://localhost:3000";
      if (request.query.format === "webm") {
        return { videoUrl: `${baseUrl}/please-update.webm` };
      }
      return { videoUrl: `${baseUrl}/api/media/${session.id}/video.mp4` };
    },
  );

  // Get thumbnail presigned URL
  app.get<{ Params: { token: string } }>(
    "/api/sessions/:token/thumbnail",
    {
      schema: { params: tokenParamSchema },
    },
    async (request, reply) => {
      // Rate limit: 30 req/min per token
      const rl = checkGenericRateLimit("session-thumbnail", request.params.token, 30);
      if (!rl.allowed) {
        reply.header(
          "Retry-After",
          String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)),
        );
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      const session = await findSession(request.params.token);
      if (!session) return reply.code(404).send({ error: "Session not found" });

      if (!session.thumbnailR2Key) {
        return reply.code(404).send({ error: "Thumbnail not available" });
      }

      const baseUrl = process.env.BASE_URL || "http://localhost:3000";
      const thumbnailUrl = `${baseUrl}/api/media/${session.id}/thumbnail.jpg`;

      return { thumbnailUrl };
    },
  );

  // Batch get sessions — gallery endpoint
  app.post<{ Body: { tokens: string[] } }>(
    "/api/sessions/batch",
    {
      schema: {
        body: {
          type: "object" as const,
          required: ["tokens"] as const,
          properties: {
            tokens: {
              type: "array" as const,
              items: { type: "string" as const, pattern: "^[0-9a-fA-F]{64}$" },
              minItems: 1,
              maxItems: 100,
            },
          },
          additionalProperties: false,
        },
      },
    },
    async (request, reply) => {
      // Rate limit: 30 req/min per IP
      const ip = request.ip;
      const rl = checkGenericRateLimit("batch", ip, 30);
      if (!rl.allowed) {
        reply.header(
          "Retry-After",
          String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)),
        );
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      const { tokens } = request.body;

      // All tokens are already validated by schema
      const validTokens = tokens.filter((t) =>
        typeof t === "string" && /^[a-f0-9]{64}$/i.test(t),
      );

      if (validTokens.length === 0) {
        return { sessions: [] };
      }

      const rows = await db
        .select()
        .from(schema.sessions)
        .where(inArray(schema.sessions.token, validTokens));

      // Get screenshot counts for all sessions in one query.
      // For bucket-mode sessions we still compute live tracked-seconds from
      // bucket count here; credit-mode sessions read sessions.tracked_seconds
      // directly (maintained incrementally) and skip the aggregation.
      const sessionIds = rows.map((r) => r.id);
      const counts =
        sessionIds.length > 0
          ? await db
              .select({
                sessionId: schema.screenshots.sessionId,
                bucketCount: sql<number>`count(distinct ${schema.screenshots.minuteBucket})`,
                screenshotCount: sql<number>`count(*)`,
              })
              .from(schema.screenshots)
              .where(
                and(
                  inArray(schema.screenshots.sessionId, sessionIds),
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

      // Generate permanent thumbnail URLs via redirect endpoint
      const baseUrl = process.env.BASE_URL || "http://localhost:3000";
      const sessions = rows.map((s) => {
          const c = countMap.get(s.id) ?? { bucketTrackedSeconds: 0, screenshotCount: 0 };
          const thumbnailUrl = s.thumbnailR2Key
            ? `${baseUrl}/api/media/${s.id}/thumbnail.jpg`
            : null;
          // Credit-mode: trust sessions.tracked_seconds (maintained per-credit).
          // Bucket-mode: prefer stored value (survives screenshot cleanup),
          // fall back to live screenshot bucket count for active sessions.
          const trackedSeconds =
            s.trackingMode === "credit"
              ? s.trackedSeconds ?? 0
              : s.trackedSeconds ?? c.bucketTrackedSeconds;
          return {
            token: s.token,
            name: s.name,
            status: s.status,
            trackedSeconds,
            screenshotCount: c.screenshotCount,
            startedAt: s.startedAt?.toISOString() ?? null,
            createdAt: s.createdAt.toISOString(),
            totalActiveSeconds: s.totalActiveSeconds,
            thumbnailUrl,
            videoUrl: s.videoR2Key
              ? `${baseUrl}/api/media/${s.id}/video.mp4`
              : null,
            // Backwards compat: see notes on /api/sessions/:token.
            videoWebmUrl: s.videoR2Key ? `${baseUrl}/please-update.webm` : null,
            metadata: s.metadata ?? {},
          };
        });

      // Sort newest first
      sessions.sort(
        (a, b) =>
          new Date(b.createdAt).getTime() - new Date(a.createdAt).getTime(),
      );

      return { sessions };
    },
  );

  // ── Public media redirect endpoints ─────────────────────────
  // Permanent URLs that redirect to short-lived presigned R2 URLs.
  // Use session ID (public, unguessable UUID) instead of token (secret).

  app.get<{ Params: { sessionId: string } }>(
    "/api/media/:sessionId/thumbnail.jpg",
    { schema: { params: sessionIdParamSchema } },
    async (request, reply) => {
      const rl = checkGenericRateLimit("media-thumbnail", request.params.sessionId, 60);
      if (!rl.allowed) {
        reply.header("Retry-After", String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)));
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      const session = await db.query.sessions.findFirst({
        where: eq(schema.sessions.id, request.params.sessionId),
      });
      if (!session || !session.thumbnailR2Key) {
        return reply.code(404).send({ error: "Thumbnail not available" });
      }

      const { GetObjectCommand } = await import("@aws-sdk/client-s3");
      const url = await getSignedUrl(r2Client, new GetObjectCommand({
        Bucket: R2_BUCKET,
        Key: session.thumbnailR2Key,
      }), { expiresIn: 3600 });

      reply.header("Cache-Control", "public, max-age=1800");
      return reply.redirect(url);
    },
  );

  app.get<{ Params: { sessionId: string } }>(
    "/api/media/:sessionId/video.mp4",
    { schema: { params: sessionIdParamSchema } },
    async (request, reply) => {
      const rl = checkGenericRateLimit("media-video", request.params.sessionId, 30);
      if (!rl.allowed) {
        reply.header("Retry-After", String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)));
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      const session = await db.query.sessions.findFirst({
        where: eq(schema.sessions.id, request.params.sessionId),
      });
      if (!session || session.status !== "complete" || !session.videoR2Key) {
        return reply.code(404).send({ error: "Video not available" });
      }

      const { GetObjectCommand } = await import("@aws-sdk/client-s3");
      const url = await getSignedUrl(r2Client, new GetObjectCommand({
        Bucket: R2_BUCKET,
        Key: session.videoR2Key,
      }), { expiresIn: 3600 });

      reply.header("Cache-Control", "public, max-age=1800");
      return reply.redirect(url);
    },
  );

  // Legacy: pre-MP4-only clients still hit this. Always redirect to the
  // static "please update" WebM so they show the upgrade prompt instead of
  // a broken player.
  app.get<{ Params: { sessionId: string } }>(
    "/api/media/:sessionId/video.webm",
    { schema: { params: sessionIdParamSchema } },
    async (request, reply) => {
      const rl = checkGenericRateLimit("media-video-webm", request.params.sessionId, 30);
      if (!rl.allowed) {
        reply.header("Retry-After", String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)));
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }
      const baseUrl = process.env.BASE_URL || "http://localhost:3000";
      reply.header("Cache-Control", "public, max-age=86400");
      return reply.redirect(`${baseUrl}/please-update.webm`);
    },
  );
}
