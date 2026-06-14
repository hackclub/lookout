import type { FastifyRequest, FastifyReply } from "fastify";
import { eq } from "drizzle-orm";
import { db, schema } from "../db/index.js";

// Internal routes authenticate with per-program API keys (managed at /admin).
// The legacy single shared secret (INTERNAL_API_KEY / GLOBAL_API_KEY) has been
// fully retired — every caller must present its own program key.

declare module "fastify" {
  interface FastifyRequest {
    // Legacy program name whose key authorized this request. Set by
    // requireApiKey; kept for backward compat (dual-written), superseded by
    // programId.
    program: string | null;
    // Canonical program id whose key authorized this request. Set by
    // requireApiKey. May be null for keys that predate the FK and aren't yet
    // backfilled.
    programId: string | null;
  }
}

export async function requireApiKey(
  request: FastifyRequest,
  reply: FastifyReply,
) {
  const key = request.headers["x-api-key"];
  if (typeof key !== "string" || key.length === 0) {
    return reply.code(401).send({ error: "Invalid API key" });
  }

  // Direct indexed lookup of the program key.
  const row = await db.query.apiKeys.findFirst({
    where: eq(schema.apiKeys.key, key),
  });
  if (!row) {
    return reply.code(401).send({ error: "Invalid API key" });
  }

  request.program = row.name;
  request.programId = row.programId;
  // Fire-and-forget last-used bookkeeping — never block or fail the request.
  void db
    .update(schema.apiKeys)
    .set({ lastUsedAt: new Date() })
    .where(eq(schema.apiKeys.id, row.id))
    .catch(() => {});
}
