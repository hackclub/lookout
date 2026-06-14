import {
  pgTable,
  pgEnum,
  uuid,
  text,
  timestamp,
  integer,
  boolean,
  jsonb,
  index,
} from "drizzle-orm/pg-core";
import { sql } from "drizzle-orm";
import { randomBytes } from "node:crypto";

function randomHex(bytes: number): string {
  return randomBytes(bytes).toString("hex");
}

export const sessionStatusEnum = pgEnum("session_status", [
  "pending",
  "active",
  "paused",
  "stopped",
  "compiling",
  "complete",
  "failed",
]);

// A program is a brand/integration that issues recording sessions (e.g.
// "Fallout"). It owns a public new-session URL used by the desktop app's
// program picker, and is the canonical entity that api_keys belong to (one
// program → many keys). Session attribution points here via program_id.
export const programs = pgTable("programs", {
  id: uuid("id").primaryKey().defaultRandom(),
  name: text("name").notNull().unique(),
  // Human-friendly label shown to users (e.g. "Fallout"). `name` is the raw
  // slug-like identifier (often lowercase/dashed) used for attribution; this is
  // what the desktop picker and other UIs display. NULL falls back to `name`.
  displayName: text("display_name"),
  // Full URL the desktop app opens to start a session for this program (e.g.
  // https://fallout.hackclub.com/lookout_session/new?desktop=true). NULL means
  // the program isn't listed in the desktop picker.
  newSessionUrl: text("new_session_url"),
  createdAt: timestamp("created_at", { withTimezone: true })
    .notNull()
    .defaultNow(),
});

export const sessions = pgTable(
  "sessions",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    token: text("token")
      .notNull()
      .unique()
      .$defaultFn(() => randomHex(32)),
    name: text("name")
      .notNull()
      .$defaultFn(
        () =>
          `untitled-${new Date().toISOString().slice(0, 10)}`,
      ),
    metadata: jsonb("metadata").$type<Record<string, unknown>>().default({}),
    // Legacy program name (api_keys.name) whose key created this session.
    // Attribution/tracking only — NOT access control. NULL when created with
    // the global/legacy key. Superseded by programId; kept in sync via
    // dual-write so it can be removed once nothing reads it.
    program: text("program"),
    // Canonical program attribution. Nullable while existing rows and any
    // pre-FK callers may not set it. NULL = global/legacy key.
    programId: uuid("program_id").references(() => programs.id),
    status: sessionStatusEnum("status").notNull().default("pending"),
    startedAt: timestamp("started_at", { withTimezone: true }),
    stoppedAt: timestamp("stopped_at", { withTimezone: true }),
    pausedAt: timestamp("paused_at", { withTimezone: true }),
    lastScreenshotAt: timestamp("last_screenshot_at", { withTimezone: true }),
    resumedAt: timestamp("resumed_at", { withTimezone: true }),
    totalActiveSeconds: integer("total_active_seconds").notNull().default(0),
    trackedSeconds: integer("tracked_seconds"),
    // Credit-mode tracking state. 'bucket' (default) is the legacy
    // distinct-minute-bucket count; 'credit' is the server-authoritative
    // wall-clock acceptance window. Mode is decided by the first upload
    // (presence of capturedAt) and is sticky for the session's lifetime.
    trackingMode: text("tracking_mode").notNull().default("bucket"),
    streakAnchorAt: timestamp("streak_anchor_at", { withTimezone: true }),
    streakCreditedCount: integer("streak_credited_count").notNull().default(0),
    // Set when the retention job has deleted this session's screenshot R2
    // objects (after SCREENSHOT_RETENTION_DAYS). The screenshot *rows* are
    // kept so capture timings stay queryable; this flag stops the job from
    // reprocessing already-purged sessions. NULL = R2 objects still present.
    screenshotsPurgedAt: timestamp("screenshots_purged_at", {
      withTimezone: true,
    }),
    videoUrl: text("video_url"),
    videoR2Key: text("video_r2_key"),
    thumbnailUrl: text("thumbnail_url"),
    thumbnailR2Key: text("thumbnail_r2_key"),
    compileAttempts: integer("compile_attempts").notNull().default(0),
    createdAt: timestamp("created_at", { withTimezone: true })
      .notNull()
      .defaultNow(),
    updatedAt: timestamp("updated_at", { withTimezone: true })
      .notNull()
      .defaultNow(),
  },
  (table) => [
    index("idx_sessions_status").on(table.status),
    index("idx_sessions_active_last_screenshot")
      .on(table.lastScreenshotAt)
      .where(sql`status IN ('active', 'paused', 'pending')`),
  ],
);

export const screenshots = pgTable(
  "screenshots",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    sessionId: uuid("session_id")
      .notNull()
      .references(() => sessions.id, { onDelete: "cascade" }),
    r2Key: text("r2_key").notNull(),
    requestedAt: timestamp("requested_at", { withTimezone: true }).notNull(),
    minuteBucket: integer("minute_bucket").notNull(),
    confirmed: boolean("confirmed").notNull().default(false),
    width: integer("width"),
    height: integer("height"),
    fileSizeBytes: integer("file_size_bytes"),
    sampled: boolean("sampled").notNull().default(false),
    // Client-attested (or server-fallback) capture time. Populated for ALL
    // new rows post-migration 0007 regardless of mode — credit-mode rows use
    // it for streak math, bucket-mode rows store it as debug-only data.
    // NULL for pre-migration rows; never backfilled.
    capturedAt: timestamp("captured_at", { withTimezone: true }),
    // Free-form client telemetry string reported on the upload-url request
    // (query param `clientInfo`). NOT the HTTP User-Agent — a User-Agent-like
    // string with Lookout-specific info (type, version, OS, browser, host app).
    // Stored opaquely (never parsed). NULL for rows created before this column
    // existed or when the client sent nothing. The session's "first recorded"
    // clientInfo is derived from the earliest row that has one.
    clientInfo: text("client_info"),
    // Credit-mode only. 0 or 60. NULL for bucket-mode rows.
    creditedSeconds: integer("credited_seconds"),
    // Credit-mode only. Server-predicted capture time at confirm; lets us
    // compute the design-invariant delta (capturedAt - expectedAt) per row.
    // NULL for bucket rows and for the seed capture of a credit streak.
    expectedAt: timestamp("expected_at", { withTimezone: true }),
    createdAt: timestamp("created_at", { withTimezone: true })
      .notNull()
      .defaultNow(),
  },
  (table) => [
    index("idx_screenshots_session_id").on(table.sessionId),
    index("idx_screenshots_session_bucket").on(
      table.sessionId,
      table.minuteBucket,
    ),
    index("idx_screenshots_unconfirmed")
      .on(table.sessionId)
      .where(sql`confirmed = false`),
    index("idx_screenshots_session_captured_at").on(
      table.sessionId,
      table.capturedAt,
    ),
  ],
);

// Per-program API keys. Each row is one program's credential, granting the
// same access as the global key; the only difference is that sessions created
// with a program key are tagged with `name` (see sessions.program). Keys are
// stored in plaintext — this data isn't highly sensitive and the admin
// dashboard displays/copies them on demand.
export const apiKeys = pgTable("api_keys", {
  id: uuid("id").primaryKey().defaultRandom(),
  // Legacy program identifier. The canonical program now lives in `programs`;
  // `name` is retained (and still unique) for backward compatibility until
  // everything reads programId. Dropping its unique constraint later lets one
  // program own many keys, with `name` becoming an optional per-key label.
  name: text("name").notNull().unique(),
  // The program this key belongs to. Nullable until all keys are backfilled
  // and all writers set it.
  programId: uuid("program_id").references(() => programs.id),
  key: text("key")
    .notNull()
    .unique()
    .$defaultFn(() => `lk_${randomHex(24)}`),
  lastUsedAt: timestamp("last_used_at", { withTimezone: true }),
  createdAt: timestamp("created_at", { withTimezone: true })
    .notNull()
    .defaultNow(),
});
