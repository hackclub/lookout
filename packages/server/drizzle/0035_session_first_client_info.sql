-- Read-side copy of a session's first reported clientInfo, so the admin
-- dashboard's client breakdown groups sessions instead of running DISTINCT ON
-- over every screenshot (which timed out /api/admin/programs at ~5.7M rows).
-- Filled by the upload-url route from now on; older sessions are backfilled
-- out of band in batches, not here, so the boot-time migrate stays instant.
ALTER TABLE "sessions" ADD COLUMN IF NOT EXISTS "first_client_info" text;
