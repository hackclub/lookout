-- A key that can only read. A program can hand one to a service that reviews
-- its sessions (Lapse's to Assay) without that service being able to create,
-- stop, delete or restore anything. Every existing key keeps full access.
--
-- IF NOT EXISTS as 0028 explains: the drizzle meta snapshot has drifted.
ALTER TABLE "api_keys" ADD COLUMN IF NOT EXISTS "read_only" boolean DEFAULT false NOT NULL;
