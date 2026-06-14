import type { FastifyInstance, FastifyRequest, FastifyReply } from "fastify";
import { timingSafeEqual } from "node:crypto";
import { eq, sql, or } from "drizzle-orm";
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
