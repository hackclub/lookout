CREATE TABLE "programs" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"name" text NOT NULL,
	"new_session_url" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "programs_name_unique" UNIQUE("name")
);
--> statement-breakpoint
-- Backfill: one program per existing api key name.
INSERT INTO "programs" ("name") SELECT DISTINCT "name" FROM "api_keys" ON CONFLICT ("name") DO NOTHING;--> statement-breakpoint
-- Backfill: also capture program names from sessions whose key was since
-- deleted, so historical attribution isn't lost.
INSERT INTO "programs" ("name") SELECT DISTINCT "program" FROM "sessions" WHERE "program" IS NOT NULL ON CONFLICT ("name") DO NOTHING;--> statement-breakpoint
ALTER TABLE "api_keys" ADD COLUMN "program_id" uuid;--> statement-breakpoint
ALTER TABLE "sessions" ADD COLUMN "program_id" uuid;--> statement-breakpoint
ALTER TABLE "api_keys" ADD CONSTRAINT "api_keys_program_id_programs_id_fk" FOREIGN KEY ("program_id") REFERENCES "public"."programs"("id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "sessions" ADD CONSTRAINT "sessions_program_id_programs_id_fk" FOREIGN KEY ("program_id") REFERENCES "public"."programs"("id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
-- Backfill the new FK columns by matching the retained name/program text.
UPDATE "api_keys" SET "program_id" = "programs"."id" FROM "programs" WHERE "programs"."name" = "api_keys"."name";--> statement-breakpoint
UPDATE "sessions" SET "program_id" = "programs"."id" FROM "programs" WHERE "programs"."name" = "sessions"."program" AND "sessions"."program" IS NOT NULL;
