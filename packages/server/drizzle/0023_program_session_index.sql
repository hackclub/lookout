-- Index for listing a program's sessions (GET /api/internal/sessions).
--
-- Exactly the query's shape: one program, newest first, walked by a
-- createdAt cursor. Without it a busy program scans and sorts its whole
-- history on every page.
--
-- Partial, because sessions created with the legacy global key have no
-- program and are never listed by that endpoint. Keeping them out of the
-- index costs nothing and keeps it to the rows it serves.
--
-- Written by hand rather than taken from `drizzle-kit generate`: the
-- snapshots in this repo lag the hand-written migrations from 0018 on, so
-- generate re-derives columns that already shipped. Running that output
-- would ADD COLUMN over existing columns and, since the container migrates
-- before it boots, take the server down with it.
CREATE INDEX IF NOT EXISTS "idx_sessions_program_created"
  ON "sessions" USING btree ("program_id", "created_at" DESC NULLS LAST)
  WHERE program_id IS NOT NULL;
