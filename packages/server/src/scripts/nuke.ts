/**
 * Permanently remove a session's recorded media: compiled videos, thumbnail,
 * and every raw screenshot/clip, then the session row itself (screenshot rows
 * cascade). For takedowns of content that must not exist anywhere.
 *
 *   npm run nuke -- <sessionId | media URL> [--yes]
 *
 * Accepts a bare session UUID or any URL containing one, e.g.
 * https://lookout.hackclub-assets.com/timelapses/<id>/original-….mp4 or
 * /api/media/<id>/video.mp4. Without --yes it only prints what it would
 * delete. In a deployed container run `node dist/scripts/nuke.js …`.
 *
 * Needs DATABASE_URL and the R2_* variables. If CLOUDFLARE_API_TOKEN and
 * CLOUDFLARE_ZONE_ID are set, also purges the public URLs from the CDN cache;
 * otherwise prints the URLs so they can be purged by hand.
 */
import { eq } from "drizzle-orm";
import {
  ListObjectsV2Command,
  DeleteObjectsCommand,
} from "@aws-sdk/client-s3";

const UUID_RE = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i;

async function main() {
  const args = process.argv.slice(2);
  const confirmed = args.includes("--yes");
  const target = args.find((a) => !a.startsWith("--"));
  const sessionId = target?.match(UUID_RE)?.[0]?.toLowerCase();
  if (!sessionId) {
    console.error("Usage: npm run nuke -- <sessionId | media URL> [--yes]");
    process.exitCode = 1;
    return;
  }

  // Imported lazily so the usage message works without credentials.
  const { db, schema } = await import("../db/index.js");
  const { r2Client, R2_BUCKET, publicObjectUrl } = await import("../config/r2.js");

  const session = await db.query.sessions.findFirst({
    where: eq(schema.sessions.id, sessionId),
  });
  const screenshotKeys = session
    ? (
        await db
          .select({ r2Key: schema.screenshots.r2Key })
          .from(schema.screenshots)
          .where(eq(schema.screenshots.sessionId, sessionId))
      ).map((r) => r.r2Key)
    : [];

  // Everything under the session's prefixes, plus any key the rows point at
  // directly, in case one ever lives outside the usual layout.
  const keys = new Set<string>(screenshotKeys);
  for (const k of [session?.videoR2Key, session?.originalVideoR2Key, session?.thumbnailR2Key]) {
    if (k) keys.add(k);
  }
  for (const prefix of [`timelapses/${sessionId}/`, `screenshots/${sessionId}/`]) {
    let token: string | undefined;
    do {
      const page = await r2Client.send(
        new ListObjectsV2Command({ Bucket: R2_BUCKET, Prefix: prefix, ContinuationToken: token }),
      );
      for (const obj of page.Contents ?? []) if (obj.Key) keys.add(obj.Key);
      token = page.IsTruncated ? page.NextContinuationToken : undefined;
    } while (token);
  }

  if (session) {
    console.log("Session (save this for the conduct record):");
    console.log(
      JSON.stringify(
        {
          id: session.id,
          program: session.program,
          programId: session.programId,
          metadata: session.metadata,
          status: session.status,
          name: session.name,
          createdAt: session.createdAt,
          videoR2Key: session.videoR2Key,
          originalVideoR2Key: session.originalVideoR2Key,
        },
        null,
        2,
      ),
    );
  } else {
    console.log(`No session row for ${sessionId}; deleting any orphaned objects.`);
  }
  console.log(`${keys.size} R2 object(s) to delete.`);

  if (!confirmed) {
    console.log("\nDry run. Re-run with --yes to delete.");
    return;
  }

  const all = [...keys];
  for (let i = 0; i < all.length; i += 1000) {
    const batch = all.slice(i, i + 1000);
    const res = await r2Client.send(
      new DeleteObjectsCommand({
        Bucket: R2_BUCKET,
        Delete: { Objects: batch.map((Key) => ({ Key })), Quiet: true },
      }),
    );
    if (res.Errors?.length) {
      console.error("Some R2 deletes failed; the row is left in place so you can re-run:");
      for (const e of res.Errors) console.error(`  ${e.Key}: ${e.Code} ${e.Message}`);
      process.exitCode = 1;
      return;
    }
  }
  console.log(`Deleted ${all.length} R2 object(s).`);

  if (session) {
    await db.delete(schema.sessions).where(eq(schema.sessions.id, sessionId));
    console.log("Deleted session row (screenshot rows cascaded).");
  }

  // The public domain sits behind Cloudflare's cache, which can keep serving
  // a deleted object until it expires.
  const urls = all
    .filter((k) => k.startsWith("timelapses/"))
    .map((k) => publicObjectUrl(k))
    .filter((u): u is string => !!u);
  if (urls.length === 0) return;

  const token = process.env.CLOUDFLARE_API_TOKEN;
  const zone = process.env.CLOUDFLARE_ZONE_ID;
  if (token && zone) {
    for (let i = 0; i < urls.length; i += 30) {
      const res = await fetch(`https://api.cloudflare.com/client/v4/zones/${zone}/purge_cache`, {
        method: "POST",
        headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
        body: JSON.stringify({ files: urls.slice(i, i + 30) }),
      });
      if (!res.ok) {
        console.error(`CDN purge failed (${res.status}): ${await res.text()}`);
        process.exitCode = 1;
      }
    }
    if (!process.exitCode) console.log(`Purged ${urls.length} URL(s) from the CDN cache.`);
  } else {
    console.log("\nPurge these from the Cloudflare cache (Caching → Purge Custom URLs):");
    for (const u of urls) console.log(`  ${u}`);
  }
}

main()
  .catch((e) => {
    console.error(e);
    process.exitCode = 1;
  })
  // The db module opens a pg pool at import, keeping the process alive.
  .finally(() => process.exit(process.exitCode ?? 0));
