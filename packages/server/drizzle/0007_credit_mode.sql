ALTER TABLE "screenshots" ADD COLUMN "captured_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "screenshots" ADD COLUMN "credited_seconds" integer;--> statement-breakpoint
ALTER TABLE "screenshots" ADD COLUMN "expected_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "sessions" ADD COLUMN "tracking_mode" text DEFAULT 'bucket' NOT NULL;--> statement-breakpoint
ALTER TABLE "sessions" ADD COLUMN "streak_anchor_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "sessions" ADD COLUMN "streak_credited_count" integer DEFAULT 0 NOT NULL;--> statement-breakpoint
CREATE INDEX "idx_screenshots_session_captured_at" ON "screenshots" USING btree ("session_id","captured_at");--> statement-breakpoint
ALTER TABLE "sessions" ADD CONSTRAINT "chk_sessions_tracking_mode" CHECK (tracking_mode IN ('bucket', 'credit'));--> statement-breakpoint
ALTER TABLE "screenshots" ADD CONSTRAINT "chk_screenshots_credited_seconds" CHECK (credited_seconds IS NULL OR credited_seconds IN (0, 60));