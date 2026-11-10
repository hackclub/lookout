-- Interaction metadata per capture: pointer position (as a fraction of its
-- display), the frontmost app and captured-window focus, sampled by native
-- clients at every clip frame and sent with the confirm. Internal only — see
-- the `activity` column comment in schema.ts.
--
-- IF NOT EXISTS as 0028 explains: the drizzle meta snapshot has drifted, so
-- this has to be right on fresh and already-migrated databases both. No
-- backfill — older captures never had it.
ALTER TABLE "screenshots" ADD COLUMN IF NOT EXISTS "activity" jsonb;
