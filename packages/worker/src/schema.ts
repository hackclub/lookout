// Re-export the DB schema from server package.
// In production the schema is shared; for the worker we duplicate the definition
// to avoid a direct dependency on the server package.

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
import type { MaskRegion } from "@lookout/shared";

export const sessionStatusEnum = pgEnum("session_status", [
  "pending",
  "active",
  "paused",
  "stopped",
  "compiling",
  "complete",
  "failed",
]);

export const sessions = pgTable(
  "sessions",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    token: text("token").notNull().unique(),
    metadata: jsonb("metadata").$type<Record<string, unknown>>().default({}),
    status: sessionStatusEnum("status").notNull().default("pending"),
    startedAt: timestamp("started_at", { withTimezone: true }),
    stoppedAt: timestamp("stopped_at", { withTimezone: true }),
    pausedAt: timestamp("paused_at", { withTimezone: true }),
    lastScreenshotAt: timestamp("last_screenshot_at", { withTimezone: true }),
    resumedAt: timestamp("resumed_at", { withTimezone: true }),
    totalActiveSeconds: integer("total_active_seconds").notNull().default(0),
    trackedSeconds: integer("tracked_seconds"),
    // 'bucket' (legacy distinct-minute count) or 'credit' (per-capture
    // acceptance window). Needed for cut-seconds math at cut-compile.
    trackingMode: text("tracking_mode").notNull().default("bucket"),
    videoUrl: text("video_url"),
    videoR2Key: text("video_r2_key"),
    thumbnailUrl: text("thumbnail_url"),
    thumbnailR2Key: text("thumbnail_r2_key"),
    compileAttempts: integer("compile_attempts").notNull().default(0),
    // Real per-unit compile progress (0..~0.95). See the server schema for
    // full docs; the worker's compile loop writes it, /status reports it.
    compileProgress: real("compile_progress"),
    // ── Edits (cuts) — see the server schema for full docs ──
    cuts: jsonb("cuts").$type<{ start: string; end: string }[]>(),
    masks: jsonb("masks").$type<MaskRegion[]>(),
    cutSeconds: integer("cut_seconds"),
    videoUnits: jsonb("video_units").$type<
      { capturedAt: string; screenshotId: string }[]
    >(),
    originalVideoR2Key: text("original_video_r2_key"),
    videoCopyAligned: boolean("video_copy_aligned"),
    // True when original_video_r2_key is a throwaway PREVIEW build (reduced
    // resolution, cheap encoder settings) made only so the editor opens
    // promptly. Such a file must never be published — publishing re-encodes
    // from the capture units instead. Mirrors the server schema.
    originalIsPreview: boolean("original_is_preview").notNull().default(false),
    recompileCount: integer("recompile_count").notNull().default(0),
    lastEditCompileAt: timestamp("last_edit_compile_at", {
      withTimezone: true,
    }),
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
      .where(sql`status IN ('active', 'paused')`),
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
    // 'jpeg' (legacy single frame) or 'webm'/'mp4' (per-minute clip).
    format: text("format").notNull().default("jpeg"),
    // Client-reported frames per clip; the compiler demuxes for the truth.
    frameCount: integer("frame_count"),
    // Client-attested capture time; NULL for pre-migration rows (fall back
    // to requestedAt — same coalesce the timings endpoint uses).
    capturedAt: timestamp("captured_at", { withTimezone: true }),
    // The pause/stop flush capture. Excluded from the unit sampler.
    isFinal: boolean("is_final").notNull().default(false),
    // Credit-mode only: 0 or 60. NULL for bucket-mode rows.
    creditedSeconds: integer("credited_seconds"),
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
  ],
);
