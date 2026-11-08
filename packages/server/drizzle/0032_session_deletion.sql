-- Program-requested session deletion with a grace period. See the
-- `delete_after` column comment in schema.ts.
--
-- IF NOT EXISTS as 0028 explains: the drizzle meta snapshot has drifted, so
-- this has to be right on fresh and already-migrated databases both.
ALTER TABLE "sessions" ADD COLUMN IF NOT EXISTS "delete_after" timestamp with time zone;--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "idx_sessions_delete_after" ON "sessions" USING btree ("delete_after") WHERE delete_after IS NOT NULL;
