ALTER TABLE "sessions" ADD COLUMN "screenshots_purged_at" timestamp with time zone;--> statement-breakpoint
-- Backfill: the pre-0008 retention job DELETED screenshot rows once a session
-- was >7 days past stop. Those timings are gone and unrecoverable. Mark every
-- such already-purged session (complete, has a video, stopped >7 days ago, and
-- no screenshot rows remain) so:
--   1. the new cleanup job skips them instead of re-scanning every run, and
--   2. callers can distinguish "timings pruned" (purged_at set, count 0) from
--      "never recorded" (purged_at null, count 0).
-- 7 days mirrors SCREENSHOT_RETENTION_DAYS at the time of this migration.
UPDATE "sessions" s
SET "screenshots_purged_at" = now()
WHERE s."status" = 'complete'
  AND s."video_r2_key" IS NOT NULL
  AND s."stopped_at" IS NOT NULL
  AND s."stopped_at" < now() - interval '7 days'
  AND NOT EXISTS (
    SELECT 1 FROM "screenshots" sc WHERE sc."session_id" = s."id"
  );
