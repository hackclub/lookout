import type { FastifyInstance, FastifyRequest, FastifyReply } from "fastify";
import { timingSafeEqual, createHmac, createHash } from "node:crypto";
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

// ── Dashboard session cookie ───────────────────────────────────────
//
// The dashboard's only login is the browser's native basic-auth prompt, and
// whether those credentials survive a refresh is up to the browser's auth
// cache (Safari drops it constantly; some proxies strip the header) — so
// admins got re-prompted on every reload. There is deliberately NO login
// form: instead, any successfully basic-authed response piggybacks a signed
// HttpOnly session cookie, and later requests are accepted on the cookie
// alone. The prompt appears once, then refreshes ride the cookie for
// ADMIN_SESSION_MAX_AGE_S. Basic auth keeps working on every route, so
// scripts, curl, and CI are unchanged.

const ADMIN_COOKIE = "lookout_admin";
const ADMIN_SESSION_MAX_AGE_S = 30 * 24 * 3600; // 30 days

/** HMAC key derived from the admin credentials. Deliberate: rotating the
 *  password invalidates every outstanding dashboard session, which is exactly
 *  what rotating an admin password should do. */
function cookieKey(): Buffer {
  return createHash("sha256")
    .update(`lookout-admin-session:${ADMIN_USERNAME}:${ADMIN_PASSWORD}`)
    .digest();
}

function signSession(expiresAtMs: number): string {
  const sig = createHmac("sha256", cookieKey())
    .update(String(expiresAtMs))
    .digest("hex");
  return `v1.${expiresAtMs}.${sig}`;
}

function sessionCookieValid(token: string | undefined): boolean {
  if (!token) return false;
  const parts = token.split(".");
  if (parts.length !== 3 || parts[0] !== "v1") return false;
  const expiresAtMs = Number(parts[1]);
  if (!Number.isFinite(expiresAtMs) || expiresAtMs < Date.now()) return false;
  const expected = createHmac("sha256", cookieKey())
    .update(parts[1])
    .digest("hex");
  return safeEqual(parts[2], expected);
}

/** Minimal same-origin cookie read — no cookie plugin dependency for one
 *  value. Values we set are dot-separated hex, never encoded. */
function readCookie(request: FastifyRequest, name: string): string | undefined {
  const header = request.headers.cookie;
  if (typeof header !== "string") return undefined;
  for (const part of header.split(";")) {
    const eqAt = part.indexOf("=");
    if (eqAt === -1) continue;
    if (part.slice(0, eqAt).trim() === name) return part.slice(eqAt + 1).trim();
  }
  return undefined;
}

/** Whether the request should get a `Secure` cookie. Direct TLS or a
 *  TLS-terminating proxy (Coolify/traefik set x-forwarded-proto). */
function isHttps(request: FastifyRequest): boolean {
  if (request.protocol === "https") return true;
  const fwd = request.headers["x-forwarded-proto"];
  return typeof fwd === "string" && fwd.split(",")[0].trim() === "https";
}

function setSessionCookie(request: FastifyRequest, reply: FastifyReply): void {
  const attrs = [
    `${ADMIN_COOKIE}=${signSession(Date.now() + ADMIN_SESSION_MAX_AGE_S * 1000)}`,
    "Path=/",
    "HttpOnly",
    "SameSite=Lax",
    `Max-Age=${ADMIN_SESSION_MAX_AGE_S}`,
  ];
  if (isHttps(request)) attrs.push("Secure");
  reply.header("set-cookie", attrs.join("; "));
}

function validBasicAuth(request: FastifyRequest): boolean {
  const header = request.headers["authorization"];
  if (typeof header !== "string" || !header.startsWith("Basic ")) return false;
  const decoded = Buffer.from(header.slice(6), "base64").toString("utf8");
  const sep = decoded.indexOf(":");
  if (sep === -1) return false;
  const user = decoded.slice(0, sep);
  const pass = decoded.slice(sep + 1);
  // Evaluate both halves before &&-ing so a wrong username doesn't
  // short-circuit the password check.
  const okUser = safeEqual(user, ADMIN_USERNAME!);
  const okPass = safeEqual(pass, ADMIN_PASSWORD!);
  return okUser && okPass;
}

// Auth gate for the whole admin plugin. Returns true when the request is
// authorized; otherwise it has already sent the 401 (whose WWW-Authenticate
// makes the browser show its native prompt — the only login there is).
// Successful Basic auth refreshes the session cookie on the way out, so the
// prompt is a once-per-ADMIN_SESSION_MAX_AGE_S event, not a per-refresh one.
function requireAdminAuth(request: FastifyRequest, reply: FastifyReply): boolean {
  if (!ADMIN_ENABLED) {
    reply.code(503).send({ error: "admin disabled" });
    return false;
  }
  if (validBasicAuth(request)) {
    setSessionCookie(request, reply);
    return true;
  }
  if (sessionCookieValid(readCookie(request, ADMIN_COOKIE))) {
    return true;
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

// Same validation for a program's icon URL: empty/whitespace clears it,
// anything else must be http(s).
function normalizeIconUrl(raw: unknown): string | null | undefined {
  if (raw === undefined) return undefined; // not provided → leave unchanged
  if (raw === null) return null;
  if (typeof raw !== "string") return undefined;
  const trimmed = raw.trim();
  if (!trimmed) return null;
  if (!/^https?:\/\//i.test(trimmed)) {
    throw new Error("iconUrl must be an http(s) URL");
  }
  return trimmed;
}

// Validation for the desktop pairing endpoints (pairUrl/startUrl). These URLs
// receive device credentials in Authorization headers, so unlike the other
// program URLs they must be https — plain http on localhost/127.0.0.1 is
// allowed purely for development. Empty/whitespace clears (NULL).
function normalizeDesktopUrl(raw: unknown, field: string): string | null | undefined {
  if (raw === undefined) return undefined; // not provided → leave unchanged
  if (raw === null) return null;
  if (typeof raw !== "string") return undefined;
  const trimmed = raw.trim();
  if (!trimmed) return null;
  let parsed: URL;
  try {
    parsed = new URL(trimmed);
  } catch {
    throw new Error(`${field} must be a valid URL`);
  }
  const isLocalhost =
    parsed.hostname === "localhost" || parsed.hostname === "127.0.0.1";
  if (parsed.protocol !== "https:" && !(parsed.protocol === "http:" && isLocalhost)) {
    throw new Error(`${field} must be an https URL (http is allowed only on localhost)`);
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
    iconUrl: { type: "string" as const, maxLength: 2048 },
    pairUrl: { type: "string" as const, maxLength: 2048 },
    startUrl: { type: "string" as const, maxLength: 2048 },
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
    // Pass "" to clear the icon (pickers fall back to a generic glyph).
    iconUrl: { type: ["string", "null"] as const, maxLength: 2048 },
    // Desktop instant-start endpoints. Pass "" to clear; both must end up
    // set (or both unset) or the request is rejected.
    pairUrl: { type: ["string", "null"] as const, maxLength: 2048 },
    startUrl: { type: ["string", "null"] as const, maxLength: 2048 },
  },
  additionalProperties: false,
};

const ANNOUNCEMENT_LEVELS = ["info", "success", "warning", "danger"] as const;
type AnnouncementLevel = (typeof ANNOUNCEMENT_LEVELS)[number];

// Dotted numeric version, optionally "v"-prefixed: "0.3", "v1.2.3".
const VERSION_PATTERN = "^v?\\d+(\\.\\d+)*$";

const setAnnouncementBodySchema = {
  type: "object" as const,
  properties: {
    level: { type: "string" as const, enum: ANNOUNCEMENT_LEVELS },
    message: { type: "string" as const, minLength: 1, maxLength: 500 },
    url: { type: "string" as const, maxLength: 2048 },
    // Optional version targeting, both bounds inclusive against the version
    // clients report on the announcement fetch. Clients that report none
    // (builds predating version reporting) count as version 0 — so
    // maxVersion-only reaches exactly the old builds, minVersion never
    // shows to them. Empty string = unset (the admin form sends "").
    minVersion: {
      anyOf: [
        { type: "string" as const, pattern: VERSION_PATTERN, maxLength: 64 },
        { type: "string" as const, maxLength: 0 },
      ],
    },
    maxVersion: {
      anyOf: [
        { type: "string" as const, pattern: VERSION_PATTERN, maxLength: 64 },
        { type: "string" as const, maxLength: 0 },
      ],
    },
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
    if (!requireAdminAuth(request, reply)) {
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
        minVersion: schema.announcements.minVersion,
        maxVersion: schema.announcements.maxVersion,
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
  app.post<{
    Body: {
      level: AnnouncementLevel;
      message: string;
      url?: string;
      minVersion?: string;
      maxVersion?: string;
    };
  }>(
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
      const minVersion = request.body.minVersion?.trim() || null;
      const maxVersion = request.body.maxVersion?.trim() || null;

      const announcement = await db.transaction(async (tx) => {
        await tx
          .update(schema.announcements)
          .set({ active: false })
          .where(eq(schema.announcements.active, true));
        const [created] = await tx
          .insert(schema.announcements)
          .values({
            level: request.body.level,
            message,
            url,
            minVersion,
            maxVersion,
          })
          .returning({
            id: schema.announcements.id,
            level: schema.announcements.level,
            message: schema.announcements.message,
            url: schema.announcements.url,
            minVersion: schema.announcements.minVersion,
            maxVersion: schema.announcements.maxVersion,
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
        iconUrl: schema.programs.iconUrl,
        pairUrl: schema.programs.pairUrl,
        startUrl: schema.programs.startUrl,
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

    // Client-info breakdown for the dashboard graphs, keyed off each
    // session's first reported clientInfo (sessions.first_client_info, a copy
    // kept so this never has to scan screenshots), with identical strings
    // grouped so each distinct string is parsed just once.
    const clientInfoResult = await db.execute(sql`
      select ${schema.sessions.firstClientInfo} as client_info, count(*)::int as n
      from ${schema.sessions}
      where ${schema.sessions.firstClientInfo} is not null
      group by 1
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
        iconUrl: p.iconUrl,
        pairUrl: p.pairUrl,
        startUrl: p.startUrl,
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
  app.post<{
    Body: {
      name: string;
      displayName?: string;
      newSessionUrl?: string;
      iconUrl?: string;
      pairUrl?: string;
      startUrl?: string;
    };
  }>(
    "/api/admin/programs",
    { schema: { body: createProgramBodySchema } },
    async (request, reply) => {
      const name = request.body.name.trim();
      if (!name) {
        return reply.code(400).send({ error: "name is required" });
      }
      const displayName = normalizeDisplayName(request.body.displayName) ?? null;
      let newSessionUrl: string | null;
      let iconUrl: string | null;
      let pairUrl: string | null;
      let startUrl: string | null;
      try {
        newSessionUrl = normalizeNewSessionUrl(request.body.newSessionUrl) ?? null;
        iconUrl = normalizeIconUrl(request.body.iconUrl) ?? null;
        pairUrl = normalizeDesktopUrl(request.body.pairUrl, "pairUrl") ?? null;
        startUrl = normalizeDesktopUrl(request.body.startUrl, "startUrl") ?? null;
      } catch (e) {
        return reply
          .code(400)
          .send({ error: e instanceof Error ? e.message : "invalid URL" });
      }
      // Half a capability is worse than none: a pair URL with nowhere to
      // start from (or vice versa) would strand paired devices.
      if (!!pairUrl !== !!startUrl) {
        return reply
          .code(400)
          .send({ error: "pairUrl and startUrl must be set together" });
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
          .values({ name, displayName, newSessionUrl, iconUrl, pairUrl, startUrl })
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
        iconUrl: result.program.iconUrl,
        pairUrl: result.program.pairUrl,
        startUrl: result.program.startUrl,
        key: result.key.key,
      });
    },
  );

  // Update a program's display name and/or URLs (set or clear each).
  app.patch<{
    Params: { id: string };
    Body: {
      newSessionUrl?: string | null;
      displayName?: string | null;
      iconUrl?: string | null;
      pairUrl?: string | null;
      startUrl?: string | null;
    };
  }>(
    "/api/admin/programs/:id",
    { schema: { params: programIdParamSchema, body: patchProgramBodySchema } },
    async (request, reply) => {
      let newSessionUrl: string | null | undefined;
      let iconUrl: string | null | undefined;
      let pairUrl: string | null | undefined;
      let startUrl: string | null | undefined;
      try {
        newSessionUrl = normalizeNewSessionUrl(request.body.newSessionUrl);
        iconUrl = normalizeIconUrl(request.body.iconUrl);
        pairUrl = normalizeDesktopUrl(request.body.pairUrl, "pairUrl");
        startUrl = normalizeDesktopUrl(request.body.startUrl, "startUrl");
      } catch (e) {
        return reply
          .code(400)
          .send({ error: e instanceof Error ? e.message : "invalid URL" });
      }
      const displayName = normalizeDisplayName(request.body.displayName);

      // Build a partial update from only the fields the caller provided.
      const set: {
        newSessionUrl?: string | null;
        displayName?: string | null;
        iconUrl?: string | null;
        pairUrl?: string | null;
        startUrl?: string | null;
      } = {};
      if (newSessionUrl !== undefined) set.newSessionUrl = newSessionUrl;
      if (displayName !== undefined) set.displayName = displayName;
      if (iconUrl !== undefined) set.iconUrl = iconUrl;
      if (pairUrl !== undefined) set.pairUrl = pairUrl;
      if (startUrl !== undefined) set.startUrl = startUrl;
      if (Object.keys(set).length === 0) {
        return reply
          .code(400)
          .send({ error: "Provide newSessionUrl, displayName, iconUrl, pairUrl and/or startUrl" });
      }

      // The pair/start pair must stay both-set or both-unset AFTER the patch,
      // so validate against the merged state, not just the request body.
      if (pairUrl !== undefined || startUrl !== undefined) {
        const current = await db.query.programs.findFirst({
          where: eq(schema.programs.id, request.params.id),
          columns: { pairUrl: true, startUrl: true },
        });
        if (!current) {
          return reply.code(404).send({ error: "Program not found" });
        }
        const nextPair = pairUrl !== undefined ? pairUrl : current.pairUrl;
        const nextStart = startUrl !== undefined ? startUrl : current.startUrl;
        if (!!nextPair !== !!nextStart) {
          return reply
            .code(400)
            .send({ error: "pairUrl and startUrl must be set together" });
        }
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
          iconUrl: schema.programs.iconUrl,
          pairUrl: schema.programs.pairUrl,
          startUrl: schema.programs.startUrl,
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
