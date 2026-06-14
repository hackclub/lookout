CREATE TYPE "public"."announcement_level" AS ENUM('info', 'success', 'warning', 'danger');--> statement-breakpoint
CREATE TABLE "announcements" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"level" "announcement_level" DEFAULT 'info' NOT NULL,
	"message" text NOT NULL,
	"url" text,
	"active" boolean DEFAULT true NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL
);
