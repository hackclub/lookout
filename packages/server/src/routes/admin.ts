import type { FastifyInstance, FastifyRequest, FastifyReply } from "fastify";
import { timingSafeEqual } from "node:crypto";
import { eq, sql, or, desc } from "drizzle-orm";
import { parseClientInfo } from "@lookout/shared";
import { db, schema } from "../db/index.js";
import { ADMIN_PAGE_HTML } from "./adminPage.js";

const ADMIN_USERNAME = process.env.ADMIN_USERNAME;
const ADMIN_PASSWORD = process.env.ADMIN_PASSWORD;
const ADMIN_ENABLED = Boolean(ADMIN_USERNAME && ADMIN_PASSWORD);

function safeEqual(a: string, b: string): boolean {
  const ab = Buffer.from(a);
  const bb = Buffer.from(b);
  return ab.length === bb.length && timingSafeEqual(ab, bb);
}

// Basic-auth gate for the whole admin plugin. Returns true when the request is
// authorized; otherwise it has already sent the response.
function requireBasicAuth(request: FastifyRequest, reply: FastifyReply): boolean {
  if (!ADMIN_ENABLED) {
    reply.code(503).send({ error: "admin disabled" });
    return false;
  }

  const header = request.headers["authorization"];
  if (typeof header === "string" && header.startsWith("Basic ")) {
    const decoded = Buffer.from(header.slice(6), "base64").toString("utf8");
    const sep = decoded.indexOf(":");
    if (sep !== -1) {
      const user = decoded.slice(0, sep);
      const pass = decoded.slice(sep + 1);
      // Evaluate both halves before &&-ing so a wrong username doesn't
      // short-circuit the password check.
      const okUser = safeEqual(user, ADMIN_USERNAME!);
      const okPass = safeEqual(pass, ADMIN_PASSWORD!);
      if (okUser && okPass) return true;
    }
  }

  reply
    .code(401)
    .header("WWW-Authenticate", 'Basic realm="Lookout Admin"')
    .send({ error: "Unauthorized" });
  return false;
}

// Light URL validation for a program's new-session URL. Empty/whitespace means
// "unset" (NULL). Anything else must look like an http(s) URL.
function normalizeNewSessionUrl(raw: unknown): string | null | undefined {
  if (raw === undefined) return undefined; // not provided → leave unchanged
  if (raw === null) return null;
  if (typeof raw !== "string") return undefined;
  const trimmed = raw.trim();
  if (!trimmed) return null;
  if (!/^https?:\/\//i.test(trimmed)) {
    throw new Error("newSessionUrl must be an http(s) URL");
  }
  return trimmed;
}

// Trim a display name; empty/whitespace means "unset" (NULL → falls back to
// the raw program name). `undefined` means "leave unchanged" on patch.
function normalizeDisplayName(raw: unknown): string | null | undefined {
  if (raw === undefined) return undefined;
  if (raw === null) return null;
  if (typeof raw !== "string") return undefined;
  const trimmed = raw.trim();
  return trimmed ? trimmed : null;
}

const createProgramBodySchema = {
  type: "object" as const,
  properties: {
    name: { type: "string" as const, minLength: 1, maxLength: 255 },
    displayName: { type: "string" as const, maxLength: 255 },
    newSessionUrl: { type: "string" as const, maxLength: 2048 },
  },
  required: ["name"] as const,
  additionalProperties: false,
};

const patchProgramBodySchema = {
  type: "object" as const,
  properties: {
    // Pass "" to clear the URL (program drops out of the desktop picker).
    newSessionUrl: { type: ["string", "null"] as const, maxLength: 2048 },
    // Pass "" to clear the display name (UIs fall back to the raw name).
    displayName: { type: ["string", "null"] as const, maxLength: 255 },
  },
  additionalProperties: false,
};

const ANNOUNCEMENT_LEVELS = ["info", "success", "warning", "danger"] as const;
type AnnouncementLevel = (typeof ANNOUNCEMENT_LEVELS)[number];

const setAnnouncementBodySchema = {
  type: "object" as const,
  properties: {
    level: { type: "string" as const, enum: ANNOUNCEMENT_LEVELS },
    message: { type: "string" as const, minLength: 1, maxLength: 500 },
    url: { type: "string" as const, maxLength: 2048 },
  },
  required: ["level", "message"] as const,
  additionalProperties: false,
};

const programIdParamSchema = {
  type: "object" as const,
  properties: {
    id: { type: "string" as const, format: "uuid" },
  },
  required: ["id"] as const,
};

export async function adminRoutes(app: FastifyInstance) {
  app.addHook("onRequest", async (request, reply) => {
    if (!requireBasicAuth(request, reply)) {
      // Response already sent; signal Fastify to stop processing this request.
      return reply;
    }
  });

  // Dashboard page
  app.get("/admin", async (_request, reply) => {
    return reply.type("text/html").send(ADMIN_PAGE_HTML);
  });

  // Current announcement (latest active), or null.
  app.get("/api/admin/announcement", async () => {
    const [a] = await db
      .select({
        id: schema.announcements.id,
        level: schema.announcements.level,
        message: schema.announcements.message,
        url: schema.announcements.url,
        updatedAt: schema.announcements.updatedAt,
      })
      .from(schema.announcements)
      .where(eq(schema.announcements.active, true))
      .orderBy(desc(schema.announcements.updatedAt))
      .limit(1);

    return { announcement: a ?? null };
  });

  // Set the announcement. Deactivates any prior active one and inserts the new
  // one, so there's always at most one active row (history is preserved).
  app.post<{ Body: { level: AnnouncementLevel; message: string; url?: string } }>(
    "/api/admin/announcement",
    { schema: { body: setAnnouncementBodySchema } },
    async (request, reply) => {
      const message = request.body.message.trim();
      if (!message) {
        return reply.code(400).send({ error: "message is required" });
      }
      let url: string | null;
      try {
        url = normalizeNewSessionUrl(request.body.url) ?? null;
      } catch {
        return reply.code(400).send({ error: "url must be an http(s) URL" });
      }

      const announcement = await db.transaction(async (tx) => {
        await tx
          .update(schema.announcements)
          .set({ active: false })
          .where(eq(schema.announcements.active, true));
        const [created] = await tx
          .insert(schema.announcements)
          .values({ level: request.body.level, message, url })
          .returning({
            id: schema.announcements.id,
            level: schema.announcements.level,
            message: schema.announcements.message,
            url: schema.announcements.url,
          });
        return created;
      });

      return reply.code(201).send(announcement);
    },
  );

  // Clear the announcement (deactivate the active one). Idempotent.
  app.delete("/api/admin/announcement", async () => {
    await db
      .update(schema.announcements)
      .set({ active: false })
      .where(eq(schema.announcements.active, true));
    return { cleared: true };
  });

  // List programs, each with its API keys and session aggregates.
  app.get("/api/admin/programs", async () => {
    const programs = await db
      .select({
        id: schema.programs.id,
        name: schema.programs.name,
        displayName: schema.programs.displayName,
        newSessionUrl: schema.programs.newSessionUrl,
        createdAt: schema.programs.createdAt,
      })
      .from(schema.programs)
      .orderBy(schema.programs.createdAt);

    const keys = await db
      .select({
        id: schema.apiKeys.id,
        programId: schema.apiKeys.programId,
        name: schema.apiKeys.name,
        key: schema.apiKeys.key,
        lastUsedAt: schema.apiKeys.lastUsedAt,
        createdAt: schema.apiKeys.createdAt,
      })
      .from(schema.apiKeys)
      .orderBy(schema.apiKeys.createdAt);

    // Per-program session aggregates. Grouped by the sessions.program text
    // (every session carries it via dual-write) and matched to programs by
    // name, so attribution is complete regardless of which writer created the
    // session. tracked_seconds is authoritative but NULL for bucket-mode, so
    // fall back to total_active_seconds.
    const status = schema.sessions.status;
    // The DB lumps two outcomes under 'failed': real compile failures and
    // sessions that never captured a confirmed screenshot. Split them in the
    // admin stats only — a 'failed' row with no confirmed shots is "empty".
    const hasConfirmedShot = sql`exists (select 1 from ${schema.screenshots} where ${schema.screenshots.sessionId} = ${schema.sessions.id} and ${schema.screenshots.confirmed})`;
    const aggCols = {
      sessionCount: sql<number>`count(*)::int`,
      trackedSeconds: sql<number>`coalesce(sum(coalesce(${schema.sessions.trackedSeconds}, ${schema.sessions.totalActiveSeconds})), 0)::float8`,
      pending: sql<number>`(count(*) filter (where ${status} = 'pending'))::int`,
      active: sql<number>`(count(*) filter (where ${status} = 'active'))::int`,
      paused: sql<number>`(count(*) filter (where ${status} = 'paused'))::int`,
      stopped: sql<number>`(count(*) filter (where ${status} = 'stopped'))::int`,
      compiling: sql<number>`(count(*) filter (where ${status} = 'compiling'))::int`,
      complete: sql<number>`(count(*) filter (where ${status} = 'complete'))::int`,
      empty: sql<number>`(count(*) filter (where ${status} = 'failed' and not ${hasConfirmedShot}))::int`,
      failed: sql<number>`(count(*) filter (where ${status} = 'failed' and ${hasConfirmedShot}))::int`,
    };
    const statsRows = await db
      .select({ program: schema.sessions.program, ...aggCols })
      .from(schema.sessions)
      .where(sql`${schema.sessions.program} is not null`)
      .groupBy(schema.sessions.program);

    // Global totals across ALL sessions, including program-less ones.
    const [totals] = await db.select(aggCols).from(schema.sessions);

    // Client-info breakdown for the dashboard graphs. For each session we take
    // its first reported clientInfo (earliest screenshot by requested_at that
    // carried one — same "first recorded" rule as getFirstClientInfo), then
    // group identical strings so each distinct string is parsed just once.
    const clientInfoResult = await db.execute(sql`
      select client_info, count(*)::int as n
      from (
        select distinct on (${schema.screenshots.sessionId})
          ${schema.screenshots.clientInfo} as client_info
        from ${schema.screenshots}
        where ${schema.screenshots.clientInfo} is not null
        order by ${schema.screenshots.sessionId}, ${schema.screenshots.requestedAt} asc
      ) t
      group by client_info
    `);
    const clientInfoRows = (
      clientInfoResult as unknown as {
        rows: Array<{ client_info: string; n: number }>;
      }
    ).rows;

    // Tally distinct strings into named buckets (type / OS / version). A string
    // that fails to parse — or parses but lacks a dimension (e.g. a desktop
    // client reports no OS segment) — feeds that dimension's "other" seed, so
    // the dashboard's "Other" bar stays honest instead of silently dropping it.
    const typeCounts = new Map<string, number>();
    const osCounts = new Map<string, number>();
    const versionCounts = new Map<string, number>();
    let typesOther = 0;
    let osesOther = 0;
    let versionsOther = 0;
    let clientTotal = 0;
    const bump = (m: Map<string, number>, k: string, n: number) =>
      m.set(k, (m.get(k) ?? 0) + n);
    const titleCase = (s: string) => (s ? s[0].toUpperCase() + s.slice(1) : s);
    const typeLabel = (t: string) => (t === "sdk" ? "SDK" : titleCase(t));
    for (const r of clientInfoRows) {
      clientTotal += r.n;
      const parts = parseClientInfo(r.client_info);
      if (!parts) {
        typesOther += r.n;
        osesOther += r.n;
        versionsOther += r.n;
        continue;
      }
      bump(typeCounts, typeLabel(parts.type), r.n);
      bump(versionCounts, parts.version, r.n);
      if (parts.osType) bump(osCounts, parts.osType, r.n);
      else osesOther += r.n;
    }
    const toPairs = (m: Map<string, number>): Array<[string, number]> =>
      [...m.entries()].sort((a, b) => b[1] - a[1]);
    const clientStats = {
      total: clientTotal,
      types: toPairs(typeCounts),
      typesOther,
      oses: toPairs(osCounts),
      osesOther,
      versions: toPairs(versionCounts),
      versionsOther,
    };

    const statsByName = new Map(statsRows.map((s) => [s.program, s]));
    const keysByProgram = new Map<string, typeof keys>();
    for (const k of keys) {
      if (!k.programId) continue; // not yet linked to a program — skip
      const list = keysByProgram.get(k.programId) ?? [];
      list.push(k);
      keysByProgram.set(k.programId, list);
    }

    const enriched = programs.map((p) => {
      const s = statsByName.get(p.name);
      return {
        id: p.id,
        name: p.name,
        displayName: p.displayName,
        newSessionUrl: p.newSessionUrl,
        createdAt: p.createdAt,
        keys: (keysByProgram.get(p.id) ?? []).map((k) => ({
          id: k.id,
          key: k.key,
          lastUsedAt: k.lastUsedAt,
          createdAt: k.createdAt,
        })),
        sessionCount: s?.sessionCount ?? 0,
        trackedSeconds: s?.trackedSeconds ?? 0,
        statusCounts: {
          pending: s?.pending ?? 0,
          active: s?.active ?? 0,
          paused: s?.paused ?? 0,
          stopped: s?.stopped ?? 0,
          compiling: s?.compiling ?? 0,
          complete: s?.complete ?? 0,
          empty: s?.empty ?? 0,
          failed: s?.failed ?? 0,
        },
      };
    });

    return {
      programs: enriched,
      totals: {
        sessionCount: totals?.sessionCount ?? 0,
        trackedSeconds: totals?.trackedSeconds ?? 0,
        statusCounts: {
          pending: totals?.pending ?? 0,
          active: totals?.active ?? 0,
          paused: totals?.paused ?? 0,
          stopped: totals?.stopped ?? 0,
          compiling: totals?.compiling ?? 0,
          complete: totals?.complete ?? 0,
          empty: totals?.empty ?? 0,
          failed: totals?.failed ?? 0,
        },
        clientStats,
      },
    };
  });

  // Create a program and its first API key.
  app.post<{ Body: { name: string; displayName?: string; newSessionUrl?: string } }>(
    "/api/admin/programs",
    { schema: { body: createProgramBodySchema } },
    async (request, reply) => {
      const name = request.body.name.trim();
      if (!name) {
        return reply.code(400).send({ error: "name is required" });
      }
      const displayName = normalizeDisplayName(request.body.displayName) ?? null;
      let newSessionUrl: string | null;
      try {
        newSessionUrl = normalizeNewSessionUrl(request.body.newSessionUrl) ?? null;
      } catch (e) {
        return reply
          .code(400)
          .send({ error: e instanceof Error ? e.message : "invalid newSessionUrl" });
      }

      const existing = await db.query.programs.findFirst({
        where: eq(schema.programs.name, name),
      });
      if (existing) {
        return reply
          .code(409)
          .send({ error: `A program named "${name}" already exists` });
      }

      // Program + its first key in one transaction. The key's `name` mirrors
      // the program name (still unique) so session attribution via
      // sessions.program stays correct for callers that haven't moved to
      // programId yet.
      const result = await db.transaction(async (tx) => {
        const [program] = await tx
          .insert(schema.programs)
          .values({ name, displayName, newSessionUrl })
          .returning();
        const [key] = await tx
          .insert(schema.apiKeys)
          .values({ name, programId: program.id })
          .returning();
        return { program, key };
      });

      return reply.code(201).send({
        id: result.program.id,
        name: result.program.name,
        displayName: result.program.displayName,
        newSessionUrl: result.program.newSessionUrl,
        key: result.key.key,
      });
    },
  );

  // Update a program's display name and/or new-session URL (set or clear each).
  app.patch<{
    Params: { id: string };
    Body: { newSessionUrl?: string | null; displayName?: string | null };
  }>(
    "/api/admin/programs/:id",
    { schema: { params: programIdParamSchema, body: patchProgramBodySchema } },
    async (request, reply) => {
      let newSessionUrl: string | null | undefined;
      try {
        newSessionUrl = normalizeNewSessionUrl(request.body.newSessionUrl);
      } catch (e) {
        return reply
          .code(400)
          .send({ error: e instanceof Error ? e.message : "invalid newSessionUrl" });
      }
      const displayName = normalizeDisplayName(request.body.displayName);

      // Build a partial update from only the fields the caller provided.
      const set: { newSessionUrl?: string | null; displayName?: string | null } = {};
      if (newSessionUrl !== undefined) set.newSessionUrl = newSessionUrl;
      if (displayName !== undefined) set.displayName = displayName;
      if (Object.keys(set).length === 0) {
        return reply
          .code(400)
          .send({ error: "Provide newSessionUrl and/or displayName" });
      }

      const [updated] = await db
        .update(schema.programs)
        .set(set)
        .where(eq(schema.programs.id, request.params.id))
        .returning({
          id: schema.programs.id,
          name: schema.programs.name,
          displayName: schema.programs.displayName,
          newSessionUrl: schema.programs.newSessionUrl,
        });

      if (!updated) {
        return reply.code(404).send({ error: "Program not found" });
      }
      return updated;
    },
  );

  // Delete a program (and its keys). Blocked if any session is attributed to
  // it, so historical attribution is never orphaned.
  app.delete<{ Params: { id: string } }>(
    "/api/admin/programs/:id",
    { schema: { params: programIdParamSchema } },
    async (request, reply) => {
      const program = await db.query.programs.findFirst({
        where: eq(schema.programs.id, request.params.id),
      });
      if (!program) {
        return reply.code(404).send({ error: "Program not found" });
      }

      // Match sessions by either the canonical FK or the retained text name.
      const [{ count }] = await db
        .select({ count: sql<number>`count(*)::int` })
        .from(schema.sessions)
        .where(
          or(
            eq(schema.sessions.programId, program.id),
            eq(schema.sessions.program, program.name),
          ),
        );
      if (count > 0) {
        return reply.code(409).send({
          error: `Program "${program.name}" has ${count} session(s); cannot delete`,
        });
      }

      await db.transaction(async (tx) => {
        await tx
          .delete(schema.apiKeys)
          .where(eq(schema.apiKeys.programId, program.id));
        await tx
          .delete(schema.programs)
          .where(eq(schema.programs.id, program.id));
      });

      return { deleted: true };
    },
  );
}
