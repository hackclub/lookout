import { and, eq, lte, ne, isNotNull } from "drizzle-orm";
import {
  DeleteObjectsCommand,
  ListObjectsV2Command,
} from "@aws-sdk/client-s3";
import { db, schema } from "../db/index.js";
import { r2Client, R2_BUCKET } from "../config/r2.js";

// Sessions purged per run. The job runs every minute, so a large erasure
// drains over a few runs instead of holding one job open for its whole size.
const PURGE_BATCH_SIZE = 20;

// Every object a session can own lives under one of these. Listing them
// (rather than trusting the row's keys alone) also catches what the row no
// longer points at: superseded originals, old edited.mp4s, and captures
// whose rows were cleaned up but whose bytes weren't.
const SESSION_PREFIXES = ["screenshots", "timelapses"];

/**
 * Destroy sessions whose deletion grace period has run out: every R2 object
 * first, then the row, which cascades to its screenshots.
 *
 * Objects go first so that a failure leaves the row — and so the session's
 * place in this queue — intact for the next run, rather than a row-less
 * pile of bytes nobody will ever find again. A session still `compiling`
 * waits: the claim guard stops new builds of a scheduled session, but one
 * already running would otherwise upload its output after the sweep.
 */
export async function purgeDeletedSessions(): Promise<void> {
  const due = await db
    .select({
      id: schema.sessions.id,
      videoR2Key: schema.sessions.videoR2Key,
      originalVideoR2Key: schema.sessions.originalVideoR2Key,
      thumbnailR2Key: schema.sessions.thumbnailR2Key,
    })
    .from(schema.sessions)
    .where(
      and(
        isNotNull(schema.sessions.deleteAfter),
        lte(schema.sessions.deleteAfter, new Date()),
        ne(schema.sessions.status, "compiling"),
      ),
    )
    .limit(PURGE_BATCH_SIZE);

  for (const session of due) {
    try {
      await purgeSession(session);
    } catch (err) {
      console.warn(
        `[deletion] failed to purge session ${session.id}, will retry next run:`,
        err,
      );
    }
  }
}

async function purgeSession(session: {
  id: string;
  videoR2Key: string | null;
  originalVideoR2Key: string | null;
  thumbnailR2Key: string | null;
}): Promise<void> {
  const screenshots = await db
    .select({ r2Key: schema.screenshots.r2Key })
    .from(schema.screenshots)
    .where(eq(schema.screenshots.sessionId, session.id));

  const keys = new Set<string>(screenshots.map((s) => s.r2Key));
  for (const key of [
    session.videoR2Key,
    session.originalVideoR2Key,
    session.thumbnailR2Key,
  ]) {
    if (key) keys.add(key);
  }

  for (const prefix of SESSION_PREFIXES) {
    for (const key of await listKeys(`${prefix}/${session.id}/`)) {
      keys.add(key);
    }
  }

  await deleteKeys([...keys]);

  // Restore refuses anything past its deadline, so a due session can't come
  // back; the conditions are repeated so the delete never depends on that.
  const [purged] = await db
    .delete(schema.sessions)
    .where(
      and(
        eq(schema.sessions.id, session.id),
        isNotNull(schema.sessions.deleteAfter),
        lte(schema.sessions.deleteAfter, new Date()),
        ne(schema.sessions.status, "compiling"),
      ),
    )
    .returning({ id: schema.sessions.id });

  if (purged) {
    console.log(
      `[deletion] purged session ${session.id} (${keys.size} object(s))`,
    );
  }
}

async function listKeys(prefix: string): Promise<string[]> {
  const keys: string[] = [];
  let continuationToken: string | undefined;

  do {
    const page = await r2Client.send(
      new ListObjectsV2Command({
        Bucket: R2_BUCKET,
        Prefix: prefix,
        ContinuationToken: continuationToken,
      }),
    );

    for (const object of page.Contents ?? []) {
      if (object.Key) keys.push(object.Key);
    }

    continuationToken = page.IsTruncated
      ? page.NextContinuationToken
      : undefined;
  } while (continuationToken);

  return keys;
}

async function deleteKeys(keys: string[]): Promise<void> {
  // DeleteObjects takes at most 1000 keys per request.
  for (let start = 0; start < keys.length; start += 1000) {
    const batch = keys.slice(start, start + 1000);
    const response = await r2Client.send(
      new DeleteObjectsCommand({
        Bucket: R2_BUCKET,
        Delete: { Objects: batch.map((Key) => ({ Key })), Quiet: true },
      }),
    );

    if (response.Errors && response.Errors.length > 0) {
      throw new Error(
        `R2 refused ${response.Errors.length} of ${batch.length} deletes`,
      );
    }
  }
}
