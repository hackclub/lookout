import {
  pgTable,
  pgEnum,
  uuid,
  text,
  timestamp,
  integer,
  real,
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

// Severity/style of an admin announcement banner shown in the desktop app.
export const announcementLevelEnum = pgEnum("announcement_level", [
  "info",
  "success",
  "warning",
  "danger",
]);

// Admin-authored banner shown in the desktop app's gallery. At most one row is
// `active` at a time (the desktop app reads the latest active one); older rows
// are kept inactive as history. `url`, when set, is opened in the OS browser.
export const announcements = pgTable("announcements", {
  id: uuid("id").primaryKey().defaultRandom(),
  level: announcementLevelEnum("level").notNull().default("info"),
  message: text("message").notNull(),
  // Optional http(s) URL the banner's action button opens. NULL = no button.
  url: text("url"),
  // Only the latest active row is surfaced; clearing sets this false rather
  // than deleting, so the announcement history is preserved.
  active: boolean("active").notNull().default(true),
  createdAt: timestamp("created_at", { withTimezone: true })
    .notNull()
    .defaultNow(),
  updatedAt: timestamp("updated_at", { withTimezone: true })
    .notNull()
    .defaultNow(),
});

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
  // URL of a small square logo shown next to the program in pickers (e.g. the
  // desktop's + menu). NULL means clients fall back to a generic glyph.
  iconUrl: text("icon_url"),
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
    // Whether this session accepts per-minute video clip uploads (~6
    // frames/min) instead of single JPEGs. Enforced on every upload-url
    // (disallowed formats are downgraded to jpeg) and immutable thereafter —
    // a session's capture character never changes mid-recording.
    //
    // Defaults TRUE: clips are the normal capture mode, and a program opts
    // OUT with `clips: false` on the internal create endpoint. Existing rows
    // were deliberately NOT backfilled when the default flipped — a session's
    // mode is immutable, so in-flight sessions keep the mode they started
    // with. Clients that can't record clips are unaffected either way: they
    // keep uploading JPEGs to the same session, which stays fully valid.
    clipsEnabled: boolean("clips_enabled").notNull().default(true),
    // Redirect hook: http(s) URL the recording client sends the user to once
    // the timelapse finishes compiling. Set at creation by the program's
    // backend (internal API `redirectUrl`), immutable thereafter. NULL = no
    // redirect.
    redirectUrl: text("redirect_url"),
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
    // Real compile progress (0..~0.95), written by the worker's per-unit
    // download+encode loop so /status can report ground truth instead of the
    // client's time estimate. NULL when not compiling, when the worker
    // predates this column, or for cut-apply compiles (no per-unit stage to
    // meter) — the client falls back to the time estimate in every such case.
    // Capped below 1: assembly/thumbnail/upload still run after the last
    // unit, so the ring must never reach 100% while the user is still waiting.
    compileProgress: real("compile_progress"),
    // ── Edits (cuts) ──
    // Normalized cut list: [{start, end}] ISO wall-clock intervals removed
    // from every output (video, /timings, trackedSeconds). NULL/[] = no
    // edits. Canonical semantics live in @lookout/shared cuts.ts.
    cuts: jsonb("cuts").$type<{ start: string; end: string }[]>(),
    // Credited seconds removed by `cuts`. Reported trackedSeconds is
    // tracked_seconds − cut_seconds (raw value stays untouched as the audit
    // trail). Recomputed on every cuts write; authoritative at cut-compile.
    cutSeconds: integer("cut_seconds"),
    // Units that actually made it into the compiled ORIGINAL video, in
    // output order: [{capturedAt, screenshotId}]. Array index = video
    // second = real-world minute — THE video-time ↔ wall-clock map (sampled
    // rows alone can't provide it: compile skips undecodable units). NULL
    // for sessions compiled before edit support (not editable).
    videoUnits: jsonb("video_units").$type<
      { capturedAt: string; screenshotId: string }[]
    >(),
    // The UNCUT compiled video. Equal to video_r2_key until an edited
    // compile repoints video_r2_key at edited.mp4. Cut-compiles always
    // start from this file, so edits never compound quality loss. NULLed by
    // the retention job once an edited session's edit window closes (the
    // cut content must eventually be truly gone).
    originalVideoR2Key: text("original_video_r2_key"),
    // True when assembly used the stream-copy path, guaranteeing the pinned
    // 1s closed-GOP grid that makes lossless second-boundary cutting
    // possible. False → the cut-compile re-encodes instead.
    videoCopyAligned: boolean("video_copy_aligned"),
    // True when original_video_r2_key holds a PREVIEW-grade build: reduced
    // resolution, cheap encoder settings, made only so the editor can open
    // promptly on a long session. Such a file must never be published — the
    // publish step re-encodes from the capture units at full quality instead
    // of stream-copying it.
    //
    // NULL/false means the original is publish-grade, which is both the
    // legacy shape (every session compiled before the two-tier split) and
    // what a session that never entered the edit flow still builds. That
    // makes the flag safe to read as "false unless proven otherwise".
    originalIsPreview: boolean("original_is_preview").notNull().default(false),
    // User-initiated cut-compiles, capped at MAX_USER_RECOMPILES.
    recompileCount: integer("recompile_count").notNull().default(0),
    // When the last cut-compile finished; anchors the EDIT_WINDOW_DAYS
    // original-video retention backstop for edited sessions.
    lastEditCompileAt: timestamp("last_edit_compile_at", {
      withTimezone: true,
    }),
    // Edit hold: while set and in the future, a stopped session's compiled
    // video stays UNPUBLISHED (status remains "stopped", video_r2_key null)
    // so the owner can cut it before programs ever see `complete`. Set by
    // POST /stop {edit: true}; cleared by the finalize call or the expiry
    // job (which auto-publishes uncut). Editing is only possible during
    // this hold — never after complete, because programs act on complete.
    editHoldUntil: timestamp("edit_hold_until", { withTimezone: true }),
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
    // Exactly the shape of the program session list: one program's
    // sessions, newest first, walked by a createdAt cursor. Without the
    // pair a busy program scans and sorts its whole history per page.
    index("idx_sessions_program_created")
      .on(table.programId, table.createdAt.desc())
      .where(sql`program_id IS NOT NULL`),
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
    // Payload format of this capture unit: 'jpeg' (legacy single frame) or
    // 'webm'/'mp4' (per-minute clip of ~6 frames). Decided per upload by the
    // client's `format` query param, gated by sessions.clips_enabled —
    // sessions may mix formats (e.g. a clip client falling back to jpeg
    // mid-session); the compiler handles both per row.
    format: text("format").notNull().default("jpeg"),
    // Client-reported frame count inside a clip. Informational/telemetry —
    // the compiler derives the real count by demuxing. NULL for jpeg rows.
    frameCount: integer("frame_count"),
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
    // JA4 TLS client fingerprint of the connection that made the upload-url
    // request. Unlike clientInfo (client-supplied and freely spoofable) this is
    // observed at the TLS layer, so it's a harder-to-forge abuse signal: a
    // genuine Lookout client has a stable JA4, and it drifting mid-session (or
    // disagreeing with the claimed clientInfo) is suspicious. The Node origin
    // can't compute JA4 — it's set by the edge proxy (e.g. a Cloudflare
    // Transform Rule from `cf.bot_management.ja4`) as a request header and read
    // via JA4_HEADER. Stored opaquely. NULL when the header is absent (local
    // dev, direct-to-origin) or predates this column. Like clientInfo, the
    // session's "first recorded" JA4 is the earliest row that has one.
    ja4: text("ja4"),
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
