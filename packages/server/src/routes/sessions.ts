import type { FastifyInstance } from "fastify";
import { eq, sql, and, inArray, isNotNull } from "drizzle-orm";
import {
  PutObjectCommand,
  HeadObjectCommand,
  GetObjectCommand,
} from "@aws-sdk/client-s3";
import { getSignedUrl } from "@aws-sdk/s3-request-presigner";
import { randomUUID } from "node:crypto";
import { db, schema } from "../db/index.js";
import { r2Client, R2_BUCKET } from "../config/r2.js";
import { boss, COMPILE_JOB } from "../lib/queue.js";
import { publishHeldSession } from "../lib/publish.js";
import {
  computeMinuteBucket,
  checkRateLimit,
  checkGenericRateLimit,
  creditCapture,
  adoptedCapturedAt,
} from "../lib/timing.js";
import { now } from "../lib/clock.js";
import { extractJa4 } from "../lib/ja4.js";
import {
  SCREENSHOT_INTERVAL_MS,
  CLIP_FRAME_INTERVAL_MS,
  PRESIGNED_URL_EXPIRY_SECONDS,
  MAX_SCREENSHOT_BYTES,
  MAX_CLIP_BYTES,
  MAX_SCREENSHOTS_PER_SESSION,
  MAX_UPLOAD_REQUESTS_PER_SESSION,
  CLIENT_INFO_MAX_BYTES,
  CAPTURE_FORMATS,
  CAPTURE_FORMAT_CONTENT_TYPES,
  MAX_USER_RECOMPILES,
  EDIT_LEASE_SECONDS,
  EDIT_HOLD_MAX_MINUTES,
  normalizeCuts,
  isCutAt,
  countCutUnits,
  computeCutSeconds,
  type CaptureFormat,
  type CaptureRowForCuts,
  type CutInterval,
  type VideoUnit,
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

/** Reported tracked seconds: raw minus what the session's cut list removed.
 *  Cuts are user edits — they can only shrink the number, and /timings
 *  excludes the same captures, so every consumer tells one story. The raw
 *  value stays untouched in the DB (and is surfaced as
 *  uncutTrackedSeconds). */
function reportedTrackedSeconds(
  rawTrackedSeconds: number,
  session: { cutSeconds: number | null },
): number {
  return Math.max(0, rawTrackedSeconds - (session.cutSeconds ?? 0));
}

/** The session's cut list, always as an array. */
function sessionCuts(session: { cuts: unknown }): CutInterval[] {
  return Array.isArray(session.cuts) ? (session.cuts as CutInterval[]) : [];
}

/** Is the session's edit hold currently active? */
function holdActive(session: { editHoldUntil: Date | null }): boolean {
  return (
    session.editHoldUntil !== null && session.editHoldUntil.getTime() > Date.now()
  );
}

/**
 * Whether the session is CURRENTLY editable, and why not. Editing exists
 * only inside the stop-time edit hold — never after `complete`. `complete`
 * is the signal programs act on (forwarding heartbeats to Hackatime,
 * accepting submissions, firing the redirect hook), so the data they read
 * must already be final; a post-publish edit would mutate numbers someone
 * already consumed.
 */
function sessionEditability(session: {
  status: string;
  videoUnits: unknown;
  originalVideoR2Key: string | null;
  recompileCount: number;
  editHoldUntil: Date | null;
}): {
  editable: boolean;
  reason?:
    | "preparing"
    | "no_original"
    | "recompiles_exhausted"
    | "not_ready"
    | "failed"
    | "published";
} {
  if (session.status === "complete") {
    return { editable: false, reason: "published" };
  }
  if (session.status === "failed") {
    return { editable: false, reason: "failed" };
  }
  if (!holdActive(session)) {
    return { editable: false, reason: "not_ready" };
  }
  // A held session is "compiling" for most of the wait — the worker claims
  // the job within a second of the stop and only returns the session to
  // "stopped" once the preview is built. Both states are legitimate
  // waiting room; anything else means the recording isn't finished.
  if (session.status !== "stopped" && session.status !== "compiling") {
    return { editable: false, reason: "not_ready" };
  }
  // Hold is active but the preview build hasn't landed yet (the compile
  // job writes videoUnits + the original when it finishes).
  if (
    session.status === "compiling" ||
    !Array.isArray(session.videoUnits) ||
    session.videoUnits.length === 0 ||
    !session.originalVideoR2Key
  ) {
    return { editable: false, reason: "preparing" };
  }
  if (session.recompileCount >= MAX_USER_RECOMPILES) {
    return { editable: false, reason: "recompiles_exhausted" };
  }
  return { editable: true };
}

/** Confirmed capture rows in the shape the shared cut math expects —
 *  the same coalesce and rows the worker uses, so PUT /cuts previews are
 *  exactly what the cut-compile persists. */
async function getCaptureRowsForCuts(
  sessionId: string,
): Promise<CaptureRowForCuts[]> {
  const rows = await db
    .select({
      ts: sql<Date | string>`coalesce(${schema.screenshots.capturedAt}, ${schema.screenshots.requestedAt})`,
      creditedSeconds: schema.screenshots.creditedSeconds,
      minuteBucket: schema.screenshots.minuteBucket,
    })
    .from(schema.screenshots)
    .where(
      and(
        eq(schema.screenshots.sessionId, sessionId),
        eq(schema.screenshots.confirmed, true),
      ),
    );
  return rows.map((r) => ({
    timeMs: (r.ts instanceof Date ? r.ts : new Date(r.ts)).getTime(),
    creditedSeconds: r.creditedSeconds,
    minuteBucket: r.minuteBucket,
  }));
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

/** First recorded client telemetry for a session — the clientInfo string on
 *  the earliest screenshot row that has one. NULL for sessions recorded before
 *  the column existed or where no client info was ever sent. */
async function getFirstClientInfo(sessionId: string): Promise<string | null> {
  const [row] = await db
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
  return row?.clientInfo ?? null;
}

/** First recorded JA4 TLS fingerprint for a session — the ja4 on the earliest
 *  screenshot row that has one. Same "first recorded" rule as
 *  {@link getFirstClientInfo}, but resolved independently (a row may carry one
 *  telemetry field without the other). NULL when the edge never set the JA4
 *  header for this session. */
async function getFirstJa4(sessionId: string): Promise<string | null> {
  const [row] = await db
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
  return row?.ja4 ?? null;
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
      const clientInfo = await getFirstClientInfo(session.id);
      const ja4 = await getFirstJa4(session.id);
      // Prefer stored value (survives screenshot cleanup), fall back to live count.
      // For credit mode, both paths read sessions.tracked_seconds so they match.
      const rawTrackedSeconds =
        session.trackingMode === "credit"
          ? liveTrackedSeconds
          : session.trackedSeconds ?? liveTrackedSeconds;
      const trackedSeconds = reportedTrackedSeconds(rawTrackedSeconds, session);

      const baseUrl = process.env.BASE_URL || "http://localhost:3000";
      return {
        name: session.name,
        status: session.status,
        trackedSeconds,
        cuts: sessionCuts(session),
        cutSeconds: session.cutSeconds ?? 0,
        uncutTrackedSeconds: rawTrackedSeconds,
        editable: sessionEditability(session).editable,
        editHoldUntil: holdActive(session)
          ? session.editHoldUntil!.toISOString()
          : undefined,
        screenshotCount,
        clientInfo,
        ja4,
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
        // Clip capability, surfaced on the session-recovery fetch so clients
        // know BEFORE their first capture whether to record clips — the very
        // first upload of a clips session is already a clip (no static
        // opening frame in the timelapse). Old clients ignore these.
        clipsEnabled: session.clipsEnabled,
        frameIntervalMs: CLIP_FRAME_INTERVAL_MS,
        redirectUrl: session.redirectUrl,
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
    Querystring: { capturedAt?: string; clientInfo?: string; format?: CaptureFormat };
  }>(
    "/api/sessions/:token/upload-url",
    {
      schema: {
        params: tokenParamSchema,
        querystring: {
          type: "object" as const,
          properties: {
            capturedAt: { type: "string" as const, format: "date-time" },
            // Free-form client telemetry (User-Agent-like). Stored opaquely.
            // Intentionally NOT length-capped here — schema validation failure
            // would 400 the whole upload. Best-effort: truncated in the handler.
            clientInfo: { type: "string" as const },
            // Payload format. Omitted = 'jpeg' (legacy single frame). Clip
            // clients pass 'webm'/'mp4' for per-minute video clips. The
            // response echoes back the GRANTED format — requests for clip
            // formats on sessions without clips enabled are silently
            // downgraded to 'jpeg', and clients must upload what was granted.
            format: { type: "string" as const, enum: CAPTURE_FORMATS },
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
      // `let`: an out-of-envelope value is replaced with server time below
      // rather than rejected, so a wrong client clock can't cost a recording.
      let clientCapturedAt = clientCapturedAtRaw
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

      // Credit-mode: capturedAt is required and must pass the envelope.
      // Set when the client's clock was too far off to trust and server time
      // was substituted — reported back so the client can correct itself.
      let capturedAtAdopted = false;
      let nextExpectedAt: Date;
      if (trackingMode === "credit") {
        if (!clientCapturedAt) {
          return reply
            .code(400)
            .send({ error: "credit_mode_requires_captured_at" });
        }

        // Look up the latest CONFIRMED capturedAt for monotonicity. Confirmed
        // only: a row is inserted at presign time, so counting unconfirmed
        // rows made every abandoned upload a floor against its own retry.
        // That broke the clip→JPEG fallback, which deliberately reuses the
        // clip's capturedAt so the minute still credits (see the clients'
        // uploadWithFallback): the clip presign 400'd or timed out, and the
        // fallback then died here with captured_at_not_monotonic — the exact
        // minute-loss the fallback exists to prevent. A capture that never
        // landed can't be a replay target, and duplicate stamps at confirm
        // time can only RESET a streak (credit 0, see creditCapture), so
        // nothing anti-cheat is lost by ignoring unconfirmed rows.
        const [latest] = await db
          .select({ capturedAt: schema.screenshots.capturedAt })
          .from(schema.screenshots)
          .where(
            and(
              eq(schema.screenshots.sessionId, session.id),
              eq(schema.screenshots.confirmed, true),
            ),
          )
          .orderBy(sql`${schema.screenshots.capturedAt} DESC NULLS LAST`)
          .limit(1);

        // A wrong system clock must not cost the user their recording. An
        // out-of-envelope timestamp is adopted as server time rather than
        // 400'd; anything else (non-monotonic, pre-session) is still refused.
        const resolved = adoptedCapturedAt(
          clientCapturedAt,
          serverNow,
          startedAt,
          latest?.capturedAt ?? null,
        );
        if (!resolved.ok) {
          return reply.code(400).send({ error: resolved.code });
        }
        if (resolved.adopted) {
          capturedAtAdopted = true;
          request.log.warn(
            {
              sessionId: session.id,
              clientCapturedAt: clientCapturedAt.toISOString(),
              serverNow: serverNow.toISOString(),
              skewMs: clientCapturedAt.getTime() - serverNow.getTime(),
            },
            "client clock outside the trust envelope — stamping capture with server time",
          );
        }
        clientCapturedAt = resolved.capturedAt;

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
      // Payload format for this capture unit. A clip is still ONE unit per
      // minute — identical cadence, credit math, and rate limits as jpeg.
      // The clips gate is enforced HERE, per upload: sessions without
      // clips_enabled get clip-format requests silently downgraded to jpeg
      // (the presign + granted-format echo both say jpeg, so a conforming
      // client falls back without an error round-trip).
      const requestedFormat: CaptureFormat = request.query.format ?? "jpeg";
      const format: CaptureFormat =
        requestedFormat !== "jpeg" && !session.clipsEnabled
          ? "jpeg"
          : requestedFormat;
      const contentType = CAPTURE_FORMAT_CONTENT_TYPES[format];
      const ext = format === "jpeg" ? "jpg" : format;
      const r2Key = `screenshots/${session.id}/${screenshotId}.${ext}`;

      // Optional client telemetry from the query param. Stored opaquely and
      // never parsed. Truncate (don't reject) so a malformed/oversized value
      // degrades gracefully instead of breaking the upload; trim() collapses
      // an all-whitespace value to null.
      const clientInfo =
        request.query.clientInfo?.trim().slice(0, CLIENT_INFO_MAX_BYTES) || null;

      // JA4 TLS fingerprint observed at the edge and forwarded as a header
      // (see lib/ja4). Unlike clientInfo it's not client-controllable at the
      // app layer. NULL when the edge didn't set it (local dev, etc).
      const ja4 = extractJa4(request);

      // Resolve the row's `captured_at` value — populated in both modes for
      // debugging. In bucket mode it's never read for math. Read AFTER the
      // credit block so it picks up an adopted server timestamp.
      const rowCapturedAt = clientCapturedAt ?? serverNow;

      // Create screenshot record (unconfirmed)
      await db.insert(schema.screenshots).values({
        id: screenshotId,
        sessionId: session.id,
        r2Key,
        requestedAt: serverNow,
        minuteBucket,
        confirmed: false,
        capturedAt: rowCapturedAt,
        clientInfo,
        ja4,
        format,
      });

      // Generate presigned PUT URL
      // Note: Don't set ContentLength — it signs an exact size and rejects
      // anything different. Size is validated at confirmation via HeadObject.
      // Orphaned uploads are cleaned up by the unconfirmed cleanup job.
      const command = new PutObjectCommand({
        Bucket: R2_BUCKET,
        Key: r2Key,
        ContentType: contentType,
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
        // True when this capture's timestamp was replaced with server time
        // because the client's clock was outside the trust envelope. The
        // upload still succeeded; a client seeing this should re-derive its
        // offset from `serverTime` so later captures are stamped accurately.
        ...(capturedAtAdopted ? { capturedAtAdopted: true } : {}),
        trackingMode,
        format,
        clipsEnabled: session.clipsEnabled,
        frameIntervalMs: CLIP_FRAME_INTERVAL_MS,
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
      frameCount?: number;
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
            // Frames inside an uploaded clip. Informational (the worker
            // demuxes for the real count); omitted for jpeg captures.
            frameCount: { type: "integer" as const, minimum: 1, maximum: 600 },
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

      const { screenshotId, width, height, fileSize, frameCount } = request.body;

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

        // Validate ContentType matches the format the upload-url granted.
        // The presigned PUT was signed with this content type, so a mismatch
        // means the object was not uploaded through the granted URL.
        const rowFormat = (screenshot.format ?? "jpeg") as CaptureFormat;
        const expectedContentType = CAPTURE_FORMAT_CONTENT_TYPES[rowFormat];
        if (head.ContentType !== expectedContentType) {
          return reply
            .code(400)
            .send({ error: `Invalid content type — expected ${expectedContentType}` });
        }

        // Validate file size is within the per-format limit. Clips get a
        // larger budget than single frames, bounded by the client bitrate cap.
        const maxBytes =
          rowFormat === "jpeg" ? MAX_SCREENSHOT_BYTES : MAX_CLIP_BYTES;
        if (head.ContentLength && head.ContentLength > maxBytes) {
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
              frameCount: frameCount ?? null,
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
            frameCount: frameCount ?? null,
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

  // Stop session.
  // Optional body { edit: true } holds the session UNPUBLISHED after its
  // compile so the owner can cut it before programs ever observe
  // `complete`. The hold auto-publishes after EDIT_HOLD_MINUTES. Old
  // clients send no body and get today's behavior byte-for-byte.
  app.post<{ Params: { token: string }; Body: { edit?: boolean } | null }>(
    "/api/sessions/:token/stop",
    {
      schema: {
        params: tokenParamSchema,
        body: {
          type: ["object", "null"] as const,
          properties: {
            edit: { type: "boolean" as const },
          },
          // Deliberately permissive. This route accepted (and ignored) any
          // body before `edit` existed, so rejecting unknown fields would
          // turn a working custom client into a 400 for no benefit.
          additionalProperties: true,
        },
      },
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

      // Edit hold: only meaningful when there will be a video to edit.
      // This is the first lease term — the editor renews it as soon as it
      // opens, so a client that promises an editor and never shows one
      // publishes a lease later rather than stranding the session.
      const screenshotCount = await getScreenshotCount(session.id);
      const wantsEdit = request.body?.edit === true && screenshotCount > 0;
      const editHoldUntil = wantsEdit
        ? new Date(stopNow.getTime() + EDIT_LEASE_SECONDS * 1000)
        : null;

      const [updated] = await db
        .update(schema.sessions)
        .set({
          status: "stopped",
          stoppedAt: stopNow,
          totalActiveSeconds,
          trackedSeconds,
          editHoldUntil,
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
        ...(editHoldUntil
          ? { editHoldUntil: editHoldUntil.toISOString() }
          : {}),
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
      const rawTrackedSeconds =
        session.trackingMode === "credit"
          ? liveTrackedSeconds
          : session.trackedSeconds ?? liveTrackedSeconds;

      const baseUrl = process.env.BASE_URL || "http://localhost:3000";
      return {
        status: session.status,
        // Real compile progress (0..~0.95) when the worker is metering an
        // original build; absent for cut-apply compiles and pre-column
        // workers, where the client falls back to its time estimate.
        progress: session.compileProgress ?? undefined,
        videoUrl: session.videoR2Key
          ? `${baseUrl}/api/media/${session.id}/video.mp4`
          : undefined,
        // Backwards compat: legacy clients used this to know completion + format.
        videoWebmUrl: session.videoR2Key
          ? `${baseUrl}/please-update.webm`
          : undefined,
        trackedSeconds: reportedTrackedSeconds(rawTrackedSeconds, session),
        // Redirect hook — clients watching the compile open this once the
        // status flips to "complete". Absent when the session has none.
        redirectUrl: session.redirectUrl ?? undefined,
        // Edit hold. `editable` flips true when the preview build lands;
        // until then a set `editHoldUntil` means "still preparing".
        editable: sessionEditability(session).editable,
        editHoldUntil: holdActive(session)
          ? session.editHoldUntil!.toISOString()
          : undefined,
      };
    },
  );

  // Get capture timings — public, token-gated.
  // Returns the ISO-8601 capture timestamps of every confirmed screenshot in
  // the session, oldest first. Uses captured_at (client-attested capture
  // moment); pre-migration rows that predate captured_at fall back to
  // requested_at so the array is never sparse.
  //
  // Captures inside the session's cut list are EXCLUDED from `timestamps` by
  // default, so heartbeat forwarders (→ Hackatime) respect user edits with
  // no code changes. The removed points are available via ?includeCut=true.
  app.get<{ Params: { token: string }; Querystring: { includeCut?: boolean } }>(
    "/api/sessions/:token/timings",
    {
      schema: {
        params: tokenParamSchema,
        querystring: {
          type: "object" as const,
          properties: {
            includeCut: { type: "boolean" as const },
          },
          additionalProperties: false,
        },
      },
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
      const allTimestamps = rows.map((r) =>
        (r.ts instanceof Date ? r.ts : new Date(r.ts)).toISOString(),
      );

      // Partition by the cut list (kept is the default view).
      const cuts = sessionCuts(session);
      const timestamps: string[] = [];
      const cutTimestamps: string[] = [];
      for (const iso of allTimestamps) {
        if (cuts.length > 0 && isCutAt(Date.parse(iso), cuts)) {
          cutTimestamps.push(iso);
        } else {
          timestamps.push(iso);
        }
      }

      const clientInfo = await getFirstClientInfo(session.id);
      const ja4 = await getFirstJa4(session.id);

      // first/last are convenience accessors on the already-ascending array.
      // NOTE: last − first is NOT capture duration — a paused session has gaps
      // between timestamps, so the span overstates actual recorded time.
      return {
        status: session.status,
        count: timestamps.length,
        first: timestamps[0] ?? null,
        last: timestamps[timestamps.length - 1] ?? null,
        clientInfo,
        ja4,
        timestamps,
        cuts,
        cutCount: cutTimestamps.length,
        ...(request.query.includeCut ? { cutTimestamps } : {}),
      };
    },
  );

  // ── Edits (cuts) ─────────────────────────────────────────────
  // An edit is a cut list of absolute wall-clock intervals removed from
  // every output: the published video, /timings, and trackedSeconds. See
  // @lookout/shared cuts.ts for the canonical semantics and
  // docs/edit-feature-plan.md for the architecture.

  // Editor metadata: the original video's unit map (video second i ↔ wall
  // clock), current cuts, and a presigned URL for the UNCUT original.
  // Deliberately NOT the public media URL — after an edit, cut content
  // exists only in the original, which must stay reachable through the
  // secret token alone.
  app.get<{ Params: { token: string } }>(
    "/api/sessions/:token/units",
    {
      schema: { params: tokenParamSchema },
    },
    async (request, reply) => {
      const rl = checkGenericRateLimit("session-units", request.params.token, 10);
      if (!rl.allowed) {
        reply.header(
          "Retry-After",
          String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)),
        );
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      const session = await findSession(request.params.token);
      if (!session) return reply.code(404).send({ error: "Session not found" });

      const { editable, reason } = sessionEditability(session);

      let originalVideoUrl: string | null = null;
      if (editable) {
        originalVideoUrl = await getSignedUrl(
          r2Client,
          new GetObjectCommand({
            Bucket: R2_BUCKET,
            Key: session.originalVideoR2Key!,
          }),
          { expiresIn: 3600 },
        );
      }

      return {
        units: (session.videoUnits as VideoUnit[] | null) ?? [],
        cuts: sessionCuts(session),
        editable,
        ...(editable ? {} : { editableReason: reason }),
        editHoldUntil: holdActive(session)
          ? session.editHoldUntil!.toISOString()
          : null,
        // Roughly how many units the finished video will hold. Lets a
        // client waiting on the build size its progress estimate — compile
        // time scales with unit count. Minus the seed capture, which the
        // compiler excludes from the video (see dropSeedUnit): counting it
        // would make the waiting-room copy promise one minute more than
        // the finished timelapse holds.
        expectedUnits: Math.max(0, (await getScreenshotCount(session.id)) - 1),
        originalVideoUrl,
        recompilesRemaining: Math.max(
          0,
          MAX_USER_RECOMPILES - session.recompileCount,
        ),
      };
    },
  );

  // Renew the edit lease: "someone still has this open".
  //
  // The hold is a lease rather than a countdown, so an open editor keeps
  // the session unpublished for as long as it's genuinely being used, and
  // an abandoned one publishes about a lease later. Cheap and idempotent —
  // clients call it every EDIT_HEARTBEAT_SECONDS.
  app.post<{ Params: { token: string } }>(
    "/api/sessions/:token/editing",
    {
      schema: { params: tokenParamSchema },
    },
    async (request, reply) => {
      const rl = checkGenericRateLimit("session-editing", request.params.token, 20);
      if (!rl.allowed) {
        reply.header(
          "Retry-After",
          String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)),
        );
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      const session = await findSession(request.params.token);
      if (!session) return reply.code(404).send({ error: "Session not found" });

      // Already out the door — tell the caller to stop renewing.
      if (session.status === "complete" || session.status === "failed") {
        return { editHoldUntil: new Date().toISOString(), held: false };
      }
      if (session.editHoldUntil === null) {
        return { editHoldUntil: new Date().toISOString(), held: false };
      }

      // The absolute ceiling is measured from the stop, so an editor left
      // open indefinitely can't keep a program waiting forever.
      const ceiling = session.stoppedAt
        ? session.stoppedAt.getTime() + EDIT_HOLD_MAX_MINUTES * 60_000
        : Number.POSITIVE_INFINITY;
      if (Date.now() >= ceiling) {
        return { editHoldUntil: session.editHoldUntil.toISOString(), held: false };
      }

      const next = new Date(
        Math.min(ceiling, Date.now() + EDIT_LEASE_SECONDS * 1000),
      );
      // Renew even if the previous term lapsed moments ago but the expiry
      // job hasn't run: a brief network stall shouldn't end someone's edit.
      // The status guard is what makes that safe — a published session
      // can't be pulled back.
      const [updated] = await db
        .update(schema.sessions)
        .set({ editHoldUntil: next, updatedAt: new Date() })
        .where(
          and(
            eq(schema.sessions.id, session.id),
            sql`${schema.sessions.status} IN ('stopped', 'compiling')`,
            isNotNull(schema.sessions.editHoldUntil),
          ),
        )
        .returning({ id: schema.sessions.id });

      return updated
        ? { editHoldUntil: next.toISOString(), held: true }
        : { editHoldUntil: new Date().toISOString(), held: false };
    },
  );

  // Replace the session's cut list. Idempotent full replace — no patch
  // semantics (the list is small). `[]` clears all edits. Only valid during
  // an active edit hold; the cuts are baked in by POST /compile, which also
  // publishes the session.
  app.put<{
    Params: { token: string };
    Body: { cuts: Array<{ start: string; end: string }> };
  }>(
    "/api/sessions/:token/cuts",
    {
      schema: {
        params: tokenParamSchema,
        body: {
          type: "object" as const,
          required: ["cuts"] as const,
          properties: {
            cuts: {
              type: "array" as const,
              maxItems: 200,
              items: {
                type: "object" as const,
                required: ["start", "end"] as const,
                properties: {
                  start: { type: "string" as const },
                  end: { type: "string" as const },
                },
                additionalProperties: false,
              },
            },
          },
          additionalProperties: false,
        },
      },
    },
    async (request, reply) => {
      const rl = checkGenericRateLimit("session-cuts", request.params.token, 20);
      if (!rl.allowed) {
        reply.header(
          "Retry-After",
          String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)),
        );
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      const session = await findSession(request.params.token);
      if (!session) return reply.code(404).send({ error: "Session not found" });

      if (session.status === "compiling") {
        return reply
          .code(409)
          .send({ error: "Session is compiling — retry once it completes" });
      }
      const { editable, reason } = sessionEditability(session);
      if (!editable) {
        return reply
          .code(409)
          .send({ error: `Session is not editable (${reason})` });
      }

      const boundsMin = session.startedAt?.getTime();
      const boundsMax = (session.stoppedAt ?? session.updatedAt)?.getTime();
      const normalized = normalizeCuts(
        request.body.cuts,
        boundsMin !== undefined && boundsMax !== undefined
          ? { minMs: boundsMin, maxMs: boundsMax }
          : undefined,
      );
      if (!normalized.ok) {
        return reply.code(400).send({ error: normalized.error });
      }

      const videoUnits = session.videoUnits as VideoUnit[];
      const unitTimesMs = videoUnits.map((u) => Date.parse(u.capturedAt));
      const unitsCut = countCutUnits(unitTimesMs, normalized.cuts);
      if (normalized.cuts.length > 0 && unitsCut >= videoUnits.length) {
        return reply
          .code(400)
          .send({ error: "Cut list would remove the entire timelapse" });
      }

      // Same rows + same pure function as the worker's authoritative
      // cut-compile write, so this preview is exactly what lands.
      const liveTrackedSeconds = await getTrackedSecondsForSession(session);
      const rawTrackedSeconds =
        session.trackingMode === "credit"
          ? liveTrackedSeconds
          : session.trackedSeconds ?? liveTrackedSeconds;
      const cutSeconds = computeCutSeconds(
        await getCaptureRowsForCuts(session.id),
        session.trackingMode === "credit" ? "credit" : "bucket",
        rawTrackedSeconds,
        normalized.cuts,
      );

      // Guard on `stopped` + a live hold: the expiry job could have
      // published this session between our read and this write, and a
      // published session's numbers must never move.
      const [updated] = await db
        .update(schema.sessions)
        .set({
          cuts: normalized.cuts,
          cutSeconds,
          updatedAt: new Date(),
        })
        .where(
          and(
            eq(schema.sessions.id, session.id),
            eq(schema.sessions.status, "stopped"),
            sql`${schema.sessions.editHoldUntil} > now()`,
          ),
        )
        .returning({ id: schema.sessions.id });
      if (!updated) {
        return reply
          .code(409)
          .send({ error: "Edit window closed — the timelapse was already published" });
      }

      return {
        cuts: normalized.cuts,
        unitsTotal: videoUnits.length,
        unitsCut,
        trackedSeconds: Math.max(0, rawTrackedSeconds - cutSeconds),
        uncutTrackedSeconds: rawTrackedSeconds,
      };
    },
  );

  // Publish a held session, baking in its current cut list.
  //
  // This ENDS the edit hold: the session goes `complete` and programs read
  // its final numbers. With cuts, the worker slices the kept ranges out of
  // the built original (usually a lossless stream copy — seconds) and then
  // deletes the uncut original. With no cuts, the already-built original is
  // published as-is with no compile job at all ("instant").
  app.post<{ Params: { token: string } }>(
    "/api/sessions/:token/compile",
    {
      schema: { params: tokenParamSchema },
    },
    async (request, reply) => {
      const rl = checkGenericRateLimit("session-compile", request.params.token, 5);
      if (!rl.allowed) {
        reply.header(
          "Retry-After",
          String(Math.ceil((rl.retryAfterMs ?? 60_000) / 1000)),
        );
        return reply.code(429).send({ error: "Rate limit exceeded" });
      }

      const session = await findSession(request.params.token);
      if (!session) return reply.code(404).send({ error: "Session not found" });

      const recompilesRemaining = Math.max(
        0,
        MAX_USER_RECOMPILES - session.recompileCount,
      );

      // Already publishing — treat as success so client retries are safe.
      //
      // "compiling" covers two different runs, and only one of them is a
      // publish. With an original already built, the in-flight job is the
      // cut-compile that publishes, so a repeat request is a duplicate: 202.
      // With no original yet, the in-flight job is the PREVIEW build, and
      // this request means "don't bother, publish as recorded" — which the
      // hold-drop branch below handles. Without the originalVideoR2Key
      // guard this shadowed that branch, so a user who declined editing
      // mid-preview got a cheerful 202 while their session stayed held, then
      // waited for the very preview they had just declined.
      if (session.status === "compiling" && session.originalVideoR2Key) {
        return reply
          .code(202)
          .send({
            status: "compiling" as const,
            instant: false,
            recompilesRemaining,
            redirectUrl: session.redirectUrl,
          });
      }
      if (session.status === "complete") {
        // Someone (usually the hold-expiry job) published first. Idempotent
        // from the client's point of view: the timelapse is out.
        return {
          status: "complete" as const,
          instant: true,
          recompilesRemaining,
          redirectUrl: session.redirectUrl,
        };
      }

      const { editable, reason } = sessionEditability(session);

      // "Publish as recorded" while the preview is still building: just
      // drop the hold. The build re-reads it when it finishes and
      // publishes normally, so the user never has to wait for a preview
      // they said they don't want.
      if (!editable && reason === "preparing") {
        await db
          .update(schema.sessions)
          .set({ editHoldUntil: null, updatedAt: new Date() })
          .where(eq(schema.sessions.id, session.id));
        return {
          status: session.status as "stopped" | "compiling",
          instant: false,
          recompilesRemaining,
          redirectUrl: session.redirectUrl,
        };
      }

      if (!editable) {
        return reply
          .code(409)
          .send({ error: `Session is not editable (${reason})` });
      }

      const cuts = sessionCuts(session);

      if (cuts.length === 0) {
        // No cuts: publish the already-built original directly. No worker
        // round-trip, so "Save without edits" is instant.
        const published = await publishHeldSession(session.id);
        if (!published) {
          return reply
            .code(409)
            .send({ error: "Session state changed concurrently, please retry" });
        }
        return {
          status: "complete" as const,
          instant: true,
          recompilesRemaining,
          redirectUrl: session.redirectUrl,
        };
      }

      // Cuts to bake in: claim stopped → compiling and hand off to the
      // worker (whose claim accepts re-entry from 'compiling' on retry).
      const [updated] = await db
        .update(schema.sessions)
        .set({
          status: "compiling",
          recompileCount: session.recompileCount + 1,
          // Clear the hold: this session is being published now, so the
          // expiry job must not race in behind us.
          editHoldUntil: null,
          updatedAt: new Date(),
        })
        .where(
          and(
            eq(schema.sessions.id, session.id),
            eq(schema.sessions.status, "stopped"),
          ),
        )
        .returning({ id: schema.sessions.id });
      if (!updated) {
        return reply
          .code(409)
          .send({ error: "Session state changed concurrently, please retry" });
      }

      await boss.send(COMPILE_JOB, { sessionId: session.id });

      return {
        status: "compiling" as const,
        instant: false,
        recompilesRemaining: Math.max(
          0,
          MAX_USER_RECOMPILES - (session.recompileCount + 1),
        ),
        redirectUrl: session.redirectUrl,
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
          const rawTrackedSeconds =
            s.trackingMode === "credit"
              ? s.trackedSeconds ?? 0
              : s.trackedSeconds ?? c.bucketTrackedSeconds;
          return {
            token: s.token,
            name: s.name,
            status: s.status,
            trackedSeconds: reportedTrackedSeconds(rawTrackedSeconds, s),
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

      // Stream the bytes instead of redirecting to a presigned URL: the
      // presigned URL changes on every request, which defeats the browser
      // HTTP cache entirely. Thumbnails are the session's first frame, so
      // they almost never change — a stable URL + ETag makes repeat app
      // opens a disk-cache hit or a 304.
      const cacheControl = "public, max-age=86400, stale-while-revalidate=604800";
      const { GetObjectCommand } = await import("@aws-sdk/client-s3");
      const ifNoneMatch = request.headers["if-none-match"];
      try {
        const obj = await r2Client.send(new GetObjectCommand({
          Bucket: R2_BUCKET,
          Key: session.thumbnailR2Key,
          IfNoneMatch: ifNoneMatch,
        }));
        reply.header("Cache-Control", cacheControl);
        reply.header("Content-Type", "image/jpeg");
        if (obj.ETag) reply.header("ETag", obj.ETag);
        if (obj.ContentLength !== undefined) {
          reply.header("Content-Length", String(obj.ContentLength));
        }
        return reply.send(obj.Body);
      } catch (err) {
        const status = (err as { $metadata?: { httpStatusCode?: number } })
          .$metadata?.httpStatusCode;
        if (status === 304) {
          reply.header("Cache-Control", cacheControl);
          if (ifNoneMatch) reply.header("ETag", ifNoneMatch);
          return reply.code(304).send();
        }
        if (status === 404) {
          return reply.code(404).send({ error: "Thumbnail not available" });
        }
        throw err;
      }
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
