import type { FastifyRequest, FastifyReply } from "fastify";
import { timingSafeEqual } from "node:crypto";
import { eq } from "drizzle-orm";
import { db, schema } from "../db/index.js";

// Backward compat: the single shared secret used to be INTERNAL_API_KEY. It is
// now GLOBAL_API_KEY, but we still accept the old name so existing deployments
// don't break.
const GLOBAL_API_KEY: string | undefined =
  process.env.GLOBAL_API_KEY ?? process.env.INTERNAL_API_KEY;
if (!GLOBAL_API_KEY) {
  throw new Error(
    "GLOBAL_API_KEY (or legacy INTERNAL_API_KEY) environment variable is required but not set",
  );
}

declare module "fastify" {
  interface FastifyRequest {
    // Name of the program whose key authorized this request, or null for the
    // global key. Set by requireApiKey; read by routes that tag created data.
    program: string | null;
  }
}

function matchesGlobalKey(key: string): boolean {
  return (
    key.length === GLOBAL_API_KEY!.length &&
    timingSafeEqual(Buffer.from(key), Buffer.from(GLOBAL_API_KEY!))
  );
}

export async function requireApiKey(
  request: FastifyRequest,
  reply: FastifyReply,
) {
  const key = request.headers["x-api-key"];
  if (typeof key !== "string" || key.length === 0) {
    return reply.code(401).send({ error: "Invalid API key" });
  }

  // Global key: full access, tags nothing.
  if (matchesGlobalKey(key)) {
    request.program = null;
    return;
  }

  // Program key: direct indexed lookup. Same access as the global key; the
  // session-create route uses request.program to tag the session.
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
