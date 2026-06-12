import type { FastifyInstance, FastifyRequest, FastifyReply } from "fastify";
import { timingSafeEqual } from "node:crypto";
import { eq, sql } from "drizzle-orm";
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

const createKeyBodySchema = {
  type: "object" as const,
  properties: {
    name: { type: "string" as const, minLength: 1, maxLength: 255 },
  },
  required: ["name"] as const,
  additionalProperties: false,
};

const keyIdParamSchema = {
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

  // List keys (plaintext storage — full key returned), each enriched with its
  // program's session aggregates for the dashboard.
  app.get("/api/admin/keys", async () => {
    const keys = await db
      .select({
        id: schema.apiKeys.id,
        name: schema.apiKeys.name,
        key: schema.apiKeys.key,
        lastUsedAt: schema.apiKeys.lastUsedAt,
        createdAt: schema.apiKeys.createdAt,
      })
      .from(schema.apiKeys)
      .orderBy(schema.apiKeys.createdAt);

    // Per-program session aggregates: total count, cumulative tracked time, and
    // a per-status breakdown. tracked_seconds is the authoritative tracked
    // length but is NULL for bucket-mode sessions, so fall back to
    // total_active_seconds. Sessions attribute to a key via
    // sessions.program = api_keys.name; NULL-program rows (global/legacy key)
    // aren't shown. count(*) returns bigint, so cast the scalars to a JS number.
    const status = schema.sessions.status;
    const aggCols = {
      sessionCount: sql<number>`count(*)::int`,
      trackedSeconds: sql<number>`coalesce(sum(coalesce(${schema.sessions.trackedSeconds}, ${schema.sessions.totalActiveSeconds})), 0)::float8`,
      pending: sql<number>`(count(*) filter (where ${status} = 'pending'))::int`,
      active: sql<number>`(count(*) filter (where ${status} = 'active'))::int`,
      paused: sql<number>`(count(*) filter (where ${status} = 'paused'))::int`,
      stopped: sql<number>`(count(*) filter (where ${status} = 'stopped'))::int`,
      compiling: sql<number>`(count(*) filter (where ${status} = 'compiling'))::int`,
      complete: sql<number>`(count(*) filter (where ${status} = 'complete'))::int`,
      failed: sql<number>`(count(*) filter (where ${status} = 'failed'))::int`,
    };
    const statsRows = await db
      .select({ program: schema.sessions.program, ...aggCols })
      .from(schema.sessions)
      .where(sql`${schema.sessions.program} is not null`)
      .groupBy(schema.sessions.program);

    // Global totals across ALL sessions, including those with no program
    // (created with the global/legacy key).
    const [totals] = await db.select(aggCols).from(schema.sessions);

    const statsByProgram = new Map(statsRows.map((s) => [s.program, s]));
    const enriched = keys.map((k) => {
      const s = statsByProgram.get(k.name);
      return {
        ...k,
        sessionCount: s?.sessionCount ?? 0,
        trackedSeconds: s?.trackedSeconds ?? 0,
        statusCounts: {
          pending: s?.pending ?? 0,
          active: s?.active ?? 0,
          paused: s?.paused ?? 0,
          stopped: s?.stopped ?? 0,
          compiling: s?.compiling ?? 0,
          complete: s?.complete ?? 0,
          failed: s?.failed ?? 0,
        },
      };
    });

    return {
      keys: enriched,
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
          failed: totals?.failed ?? 0,
        },
      },
    };
  });

  // Create a key for a program
  app.post<{ Body: { name: string } }>(
    "/api/admin/keys",
    { schema: { body: createKeyBodySchema } },
    async (request, reply) => {
      const name = request.body.name.trim();
      if (!name) {
        return reply.code(400).send({ error: "name is required" });
      }

      const existing = await db.query.apiKeys.findFirst({
        where: eq(schema.apiKeys.name, name),
      });
      if (existing) {
        return reply
          .code(409)
          .send({ error: `A key named "${name}" already exists` });
      }

      const [created] = await db
        .insert(schema.apiKeys)
        .values({ name })
        .returning();

      return reply
        .code(201)
        .send({ id: created.id, name: created.name, key: created.key });
    },
  );

  // Delete a key
  app.delete<{ Params: { id: string } }>(
    "/api/admin/keys/:id",
    { schema: { params: keyIdParamSchema } },
    async (request, reply) => {
      const [deleted] = await db
        .delete(schema.apiKeys)
        .where(eq(schema.apiKeys.id, request.params.id))
        .returning({ id: schema.apiKeys.id });

      if (!deleted) {
        return reply.code(404).send({ error: "Key not found" });
      }
      return { deleted: true };
    },
  );
}
