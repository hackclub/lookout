import type { FastifyRequest, FastifyReply } from "fastify";
import { eq } from "drizzle-orm";
import { db, schema } from "../db/index.js";

// Internal routes authenticate with per-program API keys (managed at /admin).
// The legacy single shared secret (INTERNAL_API_KEY / GLOBAL_API_KEY) has been
// fully retired — every caller must present its own program key.

declare module "fastify" {
  interface FastifyRequest {
    // Name of the program whose key authorized this request. Set by
    // requireApiKey; read by routes that tag created data (e.g. sessions).
    program: string | null;
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
  // Fire-and-forget last-used bookkeeping — never block or fail the request.
  void db
    .update(schema.apiKeys)
    .set({ lastUsedAt: new Date() })
    .where(eq(schema.apiKeys.id, row.id))
    .catch(() => {});
}
