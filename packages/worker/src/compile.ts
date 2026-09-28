import { execFile } from "node:child_process";
import { promisify } from "node:util";
import * as fs from "node:fs/promises";
import * as path from "node:path";
import { randomBytes } from "node:crypto";
import {
  S3Client,
  GetObjectCommand,
  PutObjectCommand,
  DeleteObjectCommand,
  HeadObjectCommand,
} from "@aws-sdk/client-s3";
import { drizzle } from "drizzle-orm/node-postgres";
import pg from "pg";
import { eq, and, sql } from "drizzle-orm";
import {
  computeKeptRanges,
  computeCutSeconds,
  type CaptureRowForCuts,
  type CutInterval,
  type KeptRange,
  type VideoUnit,
  type MaskRegion,
} from "@lookout/shared";
import * as schema from "./schema.js";
import {
  buildSegment,
  cutVideoToKeptRanges,
  type VideoMask,
  maskToVideoMask,
  masksToVideoMasks,
  dropSeedUnit,
  segmentEncodeArgs,
  PREVIEW_WIDTH,
  PREVIEW_HEIGHT,
  SEGMENT_CONCURRENCY,
  SEGMENT_FPS,
  SEGMENT_GOP_ARGS,
  ASSEMBLE_TIMEOUT_MS,
  type SegmentQuality,
} from "./segments.js";

const execFileAsync = promisify(execFile);

/**
 * R2 key for a session's UNCUT original — deliberately unguessable.
 *
 * This file is the one piece of a session that is not meant to be shareable:
 * during the edit hold it holds every minute the user is about to cut out, and
 * it stays readable until the publish deletes it. The key used to be
 * `timelapses/<sessionId>/original.mp4`, and sessionId is public — it appears
 * in the `/api/media/:sessionId/...` URLs handed out with any shared timelapse.
 * So anyone holding a share link could reconstruct the original's URL and, if
 * the bucket is readable at all (R2_PUBLIC_DOMAIN fronts it publicly in the
 * documented setup), fetch the footage the user had cut — bypassing the token
 * gate that `/units` presigns behind.
 *
 * 128 bits of randomness in the key closes that whether or not the bucket is
 * public, which is the property worth having: it doesn't depend on an ACL
 * staying right. The published video keeps a predictable key — it is meant to
 * be fetched — and every reader gets this key from
 * `sessions.original_video_r2_key` rather than rebuilding it.
 */
function uncutOriginalKey(sessionId: string): string {
  return `timelapses/${sessionId}/original-${randomBytes(16).toString("hex")}.mp4`;
}

/** Whether a session is currently inside its edit hold. */
function holdActiveOn(session: { editHoldUntil: Date | null }): boolean {
  return (
    session.editHoldUntil != null &&
    session.editHoldUntil.getTime() > Date.now()
  );
}

export { maskToVideoMask, masksToVideoMasks };

/**
 * Post-build capture cleanup: drop the R2 objects for units that didn't make
 * it into the video, and the rows for uploads that were never confirmed.
 *
 * SAMPLED units are deliberately kept. They were always kept (so /timings and
 * the credit history stay queryable), and the two-tier split makes it load-
 * bearing: a preview-grade original can't be published, so the publish step
 * re-encodes from exactly these objects. Deleting them here would strand a
 * held session with nothing to publish from.
 */
async function cleanUpCaptureLeftovers(sessionId: string): Promise<void> {
  const unsampled = await db
    .select({ r2Key: schema.screenshots.r2Key, id: schema.screenshots.id })
    .from(schema.screenshots)
    .where(
      and(
        eq(schema.screenshots.sessionId, sessionId),
        eq(schema.screenshots.confirmed, true),
        eq(schema.screenshots.sampled, false),
      ),
    );

  for (const ss of unsampled) {
    await deleteObjectQuiet(ss.r2Key);
  }

  await db
    .delete(schema.screenshots)
    .where(
      and(
        eq(schema.screenshots.sessionId, sessionId),
        eq(schema.screenshots.confirmed, false),
      ),
    );
}

const DATABASE_URL = process.env.DATABASE_URL;
if (!DATABASE_URL) {
  throw new Error("DATABASE_URL environment variable must be set");
}

const pool = new pg.Pool({ connectionString: DATABASE_URL });
const db = drizzle(pool, { schema });

const r2Client = new S3Client({
  region: "auto",
  // R2_ENDPOINT is the local-development escape hatch (an S3-compatible
  // server instead of real R2); unset in production. Must stay in step with
  // the server's config/r2.ts — the two read and write the same objects.
  endpoint:
    process.env.R2_ENDPOINT ||
    `https://${process.env.R2_ACCOUNT_ID}.r2.cloudflarestorage.com`,
  credentials: {
    accessKeyId: process.env.R2_ACCESS_KEY_ID!,
    secretAccessKey: process.env.R2_SECRET_ACCESS_KEY!,
  },
  forcePathStyle: true,
  requestChecksumCalculation: "WHEN_REQUIRED",
  responseChecksumValidation: "WHEN_REQUIRED",
});

const R2_BUCKET = process.env.R2_BUCKET_NAME || "lookout";
const R2_PUBLIC_DOMAIN = process.env.R2_PUBLIC_DOMAIN || "";

const UUID_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** Ceiling for reported per-unit progress. The unit loop is the only metered
 *  stage; assembly, thumbnail and upload still run after the last unit lands,
 *  so the ring must stop short of 100% — only the status flip to
 *  complete/editable ends the wait. Mirrors the client's asymptotic estimate. */
const PROGRESS_UNIT_CAP = 0.95;

/** Write real compile progress for /status to report. `greatest(...)` keeps it
 *  monotonic in the DB even if a pg-boss retry re-claims and re-counts from 0,
 *  and never rewinds a value a prior attempt already reached. */
async function writeCompileProgress(
  sessionId: string,
  fraction: number,
): Promise<void> {
  const clamped = Math.max(0, Math.min(PROGRESS_UNIT_CAP, fraction));
  await db
    .update(schema.sessions)
    .set({
      compileProgress: sql`greatest(coalesce(${schema.sessions.compileProgress}, 0), ${clamped})`,
    })
    .where(eq(schema.sessions.id, sessionId));
}

/** Verify a video file with ffprobe: check file size > 0 and frame count within tolerance. */
async function verifyVideo(
  filePath: string,
  expectedInputFrames: number,
  outputFps: number,
  label: string,
): Promise<number> {
  const stat = await fs.stat(filePath);
  if (stat.size === 0)
    throw new Error(`${label}: ffmpeg produced empty output`);

  const { stdout: frameCountStr } = await execFileAsync(
    "ffprobe",
    [
      "-v",
      "error",
      "-count_packets",
      "-select_streams",
      "v:0",
      "-show_entries",
      "stream=nb_read_packets",
      "-of",
      "csv=p=0",
      filePath,
    ],
    { timeout: 60_000 },
  );

  const frameCount = parseInt(frameCountStr.trim(), 10);
  const expectedFrames = expectedInputFrames * outputFps;
  const tolerance = Math.max(outputFps, Math.round(expectedFrames * 0.02));
  if (isNaN(frameCount) || Math.abs(frameCount - expectedFrames) > tolerance) {
    throw new Error(
      `${label}: frame count mismatch: expected ~${expectedFrames} (±${tolerance}), got ${frameCount}`,
    );
  }

  return stat.size;
}

/** Upload a file to R2 and verify the upload with HeadObject. */
async function uploadAndVerify(
  localPath: string,
  r2Key: string,
  contentType: string,
  expectedSize: number,
  label: string,
): Promise<void> {
  const bytes = await fs.readFile(localPath);
  await r2Client.send(
    new PutObjectCommand({
      Bucket: R2_BUCKET,
      Key: r2Key,
      Body: bytes,
      ContentType: contentType,
    }),
  );

  const headResponse = await r2Client.send(
    new HeadObjectCommand({ Bucket: R2_BUCKET, Key: r2Key }),
  );
  if (headResponse.ContentLength !== expectedSize) {
    throw new Error(
      `${label}: R2 upload size mismatch: expected ${expectedSize}, got ${headResponse.ContentLength}`,
    );
  }
}

/** Download an R2 object to a local file, with retries. */
async function downloadObject(r2Key: string, destPath: string): Promise<void> {
  let lastErr: unknown;
  for (let attempt = 0; attempt < 3; attempt++) {
    try {
      const response = await r2Client.send(
        new GetObjectCommand({ Bucket: R2_BUCKET, Key: r2Key }),
      );
      const body = await response.Body!.transformToByteArray();
      await fs.writeFile(destPath, body);
      return;
    } catch (err) {
      lastErr = err;
    }
  }
  throw new Error(`Failed to download ${r2Key} after 3 attempts`, {
    cause: lastErr,
  });
}

/** Extract the session thumbnail (first frame) from a compiled video. */
async function extractThumbnail(
  videoPath: string,
  thumbnailPath: string,
): Promise<void> {
  await execFileAsync(
    "ffmpeg",
    [
      "-i", videoPath,
      "-vframes", "1",
      "-vf", "scale=480:-1",
      "-q:v", "5",
      "-y",
      thumbnailPath,
    ],
    { timeout: 30_000 },
  );
}

/** Best-effort R2 delete (orphans are acceptable; jobs must not fail on it). */
async function deleteObjectQuiet(r2Key: string): Promise<void> {
  try {
    await r2Client.send(
      new DeleteObjectCommand({ Bucket: R2_BUCKET, Key: r2Key }),
    );
  } catch {
    // Non-fatal: orphaned R2 objects can be cleaned up later
  }
}

/** Confirmed capture rows in the shape the shared cut/tracked-time math
 *  expects. The coalesce mirrors the timings endpoint exactly. */
async function getCaptureRowsForCuts(
  sessionId: string,
): Promise<CaptureRowForCuts[]> {
  const rows = await db.execute<{
    ts: Date | string;
    credited_seconds: number | string | null;
    minute_bucket: number | string;
  }>(sql`
    SELECT coalesce(captured_at, requested_at) AS ts,
           credited_seconds,
           minute_bucket
    FROM screenshots
    WHERE session_id = ${sessionId} AND confirmed = true
  `);
  return rows.rows.map((r) => ({
    timeMs: (r.ts instanceof Date ? r.ts : new Date(r.ts)).getTime(),
    creditedSeconds:
      r.credited_seconds === null ? null : Number(r.credited_seconds),
    minuteBucket: Number(r.minute_bucket),
  }));
}

export async function compileTimelapse(sessionId: string): Promise<{
  videoUrl: string;
  videoR2Key: string;
  thumbnailUrl: string;
  thumbnailR2Key: string;
}> {
  // Validate sessionId is a proper UUID to prevent path traversal
  if (!UUID_RE.test(sessionId)) {
    throw new Error(`Invalid sessionId format: ${sessionId}`);
  }

  const session = await db.query.sessions.findFirst({
    where: eq(schema.sessions.id, sessionId),
  });

  if (!session) throw new Error(`Session ${sessionId} not found`);

  // Atomically claim the compilation (concurrency guard).
  // Allow re-entry from 'compiling' so pg-boss retries can re-claim after a crash.
  const [claimed] = await db
    .update(schema.sessions)
    // Clear any progress a prior attempt left behind, so a cut-apply compile
    // (which never meters) reports NULL → estimate, and a re-run of a
    // half-built original starts the metered value fresh.
    .set({ status: "compiling", compileProgress: null, updatedAt: new Date() })
    .where(
      and(
        eq(schema.sessions.id, sessionId),
        sql`${schema.sessions.status} IN ('stopped', 'compiling')`,
      ),
    )
    .returning({ id: schema.sessions.id });

  if (!claimed) {
    throw new Error(
      `Session ${sessionId} cannot be compiled (status: ${session.status})`,
    );
  }

  const tmpDir = `/tmp/compile-${sessionId}`;
  await fs.mkdir(tmpDir, { recursive: true });

  try {
    const cuts: CutInterval[] = Array.isArray(session.cuts)
      ? (session.cuts as CutInterval[])
      : [];

    // ── Half B: cut apply ─────────────────────────────────────
    // The original video already exists with its unit map — this is a
    // user-initiated edit (or an un-cut). Never touches capture units, so
    // it works even after the screenshot retention purge.
    if (session.originalVideoR2Key && (session.videoUnits?.length ?? 0) > 0) {
      return await applyCutCompile(session, cuts, tmpDir);
    }

    // ── Half A: original build (the pre-existing pipeline) ───────

    // Two-tier decision, made BEFORE any encoding.
    //
    // A session stopped with `{edit: true}` carries an edit hold, which means
    // the only consumer of this build is the editor: the published video is
    // re-encoded from the capture units when the user publishes (see
    // publishFromUnits). So build the cheap tier and skip the quality this
    // file will never deliver. A session with no hold publishes THIS file
    // directly, so it must be publish-grade — that path is unchanged.
    const buildQuality: SegmentQuality = holdActiveOn(session)
      ? "preview"
      : "publish";
    if (buildQuality === "preview") {
      console.log(
        `Session ${sessionId}: building PREVIEW-grade original ` +
          `(${PREVIEW_WIDTH}x${PREVIEW_HEIGHT}) — the editor opens on this, ` +
          `and publishing re-encodes from capture units at full quality.`,
      );
    }

    // Step 1: Sample selection — pick the best capture per minute of
    // CAPTURE time. Raw SQL for DISTINCT ON, which Drizzle doesn't support.
    //
    // The bucket is derived from captured_at, NOT arrival time. The stored
    // minute_bucket column is arrival-based (computeMinuteBucket at upload
    // time), and sampling on it made the video jitter-dependent: an upload
    // landing >60s after its capture (multi-MB clip, slow uplink, a
    // retried tick) slid into the NEXT minute's arrival bucket, collided
    // with that minute's own capture, and DISTINCT ON silently dropped one
    // — a whole minute of video, present or missing depending on network
    // weather. captured_at is the cut moment: it sits on the capture grid
    // however long the upload took. COALESCE keeps pre-credit-mode rows
    // (captured_at NULL) on the old arrival-based behavior, bit for bit.
    //
    // The stored minute_bucket column itself is deliberately untouched —
    // legacy bucket-mode CREDITING counts distinct values of it; this
    // change is sampling only. Mirrored (SQL kept in step by hand) by
    // packages/server/test/unitSampling.integration.test.ts.
    const sampledScreenshots = await db.execute<{
      id: string;
      r2_key: string;
      minute_bucket: number;
      requested_at: Date | string;
      captured_at: Date | string | null;
      format: string;
      frame_count: number | null;
    }>(sql`
      SELECT DISTINCT ON (sample_bucket)
        id, r2_key, sample_bucket AS minute_bucket, requested_at, captured_at, format, frame_count
      FROM (
        SELECT *, bool_or(NOT skipped) OVER () AS has_ordinary_unit
        FROM (
          SELECT id, r2_key, requested_at, captured_at, format, frame_count,
            -- A seed credits 0 and has no expected mark. A drift RESET also
            -- credits 0 but records the mark it missed, and covers real
            -- screen time, so it keeps its second. IS TRUE for the NULLs on
            -- bucket-mode rows, which are neither.
            (is_final OR (credited_seconds = 0 AND expected_at IS NULL))
              IS TRUE AS skipped,
            FLOOR(EXTRACT(EPOCH FROM (
              COALESCE(captured_at, requested_at) - ${session.startedAt!}::timestamptz
            )) / 60)::int AS sample_bucket
          FROM screenshots
          WHERE session_id = ${sessionId} AND confirmed = true
        ) base
      ) units
      -- A capture earns a video second exactly when it earned a full minute
      -- of tracked time. Seeds credit 0 — the session's first capture AND
      -- every resume's — and the pause/stop flush credits a partial minute,
      -- so neither can hold a second the editor maps to a whole minute.
      -- Ordinary jpeg units are whole minutes and stay. A session with
      -- nothing else keeps what it has, rather than compiling to nothing.
      WHERE NOT skipped OR NOT has_ordinary_unit
      ORDER BY sample_bucket,
        -- Motion beats stills within a bucket. Credit-mode seeds and
        -- flushes never reach here now; this still covers the bucket-mode
        -- ones, which carry no credit to recognise them by.
        (format = 'jpeg')::int,
        ABS(EXTRACT(EPOCH FROM (COALESCE(captured_at, requested_at) - (
          ${session.startedAt!}::timestamptz
          + (sample_bucket * interval '1 minute')
          + interval '30 seconds'
        ))))
    `);

    if (sampledScreenshots.rows.length === 0) {
      // No screenshots — mark failed (no video possible)
      await db
        .update(schema.sessions)
        .set({ status: "failed", compileProgress: null, updatedAt: new Date() })
        .where(eq(schema.sessions.id, sessionId));
      return {
        videoUrl: "",
        videoR2Key: "",
        thumbnailUrl: "",
        thumbnailR2Key: "",
      };
    }

    // Credit-mode seeds are already gone above, identified by their credit
    // signature rather than their position — that is what catches the ones a
    // RESUME plants mid-session. Bucket-mode rows carry no per-row credit to
    // recognise, so their seed is still dropped positionally.
    const unitRows =
      session.trackingMode === "credit"
        ? sampledScreenshots.rows
        : dropSeedUnit(sampledScreenshots.rows);

    // Mark sampled screenshots. The seed is deliberately NOT marked: it is
    // not in the video, so its R2 object is cleaned up with the other
    // unsampled captures (the row itself stays, so /timings and the credit
    // history are untouched).
    const sampledIds = unitRows.map((s) => s.id);
    for (const id of sampledIds) {
      await db
        .update(schema.screenshots)
        .set({ sampled: true })
        .where(eq(schema.screenshots.id, id));
    }

    // Steps 2+3, pipelined: one worker pool downloads each unit and
    // immediately builds its 1-second normalized segment — no barrier
    // between the stages, so early units encode while later units are
    // still downloading. Every unit — legacy JPEG or clip — becomes
    // exactly one second of SEGMENT_FPS output with identical pinned encoder
    // parameters, so the final timelapse is a stream-copy concatenation
    // instead of one giant whole-session encode. Wall clock scales with
    // units/SEGMENT_CONCURRENCY, and a corrupt unit is caught and skipped
    // per-minute rather than poisoning the full encode.
    const total = unitRows.length;
    const unitExt = (format: string) => (format === "jpeg" ? "jpg" : format);
    const segmentPaths: (string | null)[] = new Array(total).fill(null);
    let downloadFailures = 0;
    let buildFailures = 0;
    {
      let next = 0;
      // Real progress: units finished (built OR skipped — a skip still
      // advances the wait) over total, capped and written throttled. The
      // event loop is single-threaded, so the shared counters need no lock.
      let done = 0;
      let lastWrittenFrac = 0;
      const reportUnitDone = async () => {
        done++;
        const frac = PROGRESS_UNIT_CAP * (done / total);
        // Write at most once per 1% of movement (≤ ~95 writes even for a
        // 12-hour session), always flushing the final unit.
        if (frac - lastWrittenFrac < 0.01 && done < total) return;
        lastWrittenFrac = frac;
        try {
          await writeCompileProgress(sessionId, frac);
        } catch {
          // Progress is cosmetic; a failed write must never fail the compile.
        }
      };
      const worker = async () => {
        while (next < total) {
          const i = next++;
          const ss = unitRows[i];
          const unitPath = path.join(tmpDir, `dl_${i}.${unitExt(ss.format)}`);

          let downloadedUnit = false;
          for (let attempt = 0; attempt < 3; attempt++) {
            try {
              const response = await r2Client.send(
                new GetObjectCommand({ Bucket: R2_BUCKET, Key: ss.r2_key }),
              );
              const body = await response.Body!.transformToByteArray();
              await fs.writeFile(unitPath, body);
              downloadedUnit = true;
              break;
            } catch {
              if (attempt === 2) {
                console.warn(
                  `Skipping unit ${i + 1}: download failed after 3 attempts (${ss.r2_key})`,
                );
              }
            }
          }
          if (!downloadedUnit) {
            downloadFailures++;
            await reportUnitDone();
            continue;
          }

          try {
            segmentPaths[i] = await buildSegment(
              tmpDir,
              i,
              unitPath,
              ss.format,
              buildQuality,
            );
          } catch (err) {
            buildFailures++;
            console.warn(
              `Skipping unit ${i + 1}: segment build failed (${ss.r2_key})`,
              err,
            );
          }
          await reportUnitDone();
        }
      };
      await Promise.all(
        Array.from({ length: Math.min(SEGMENT_CONCURRENCY, total) }, worker),
      );
    }

    const segments = segmentPaths.filter((p): p is string => p !== null);
    const unitsIncluded = segments.length;
    if (downloadFailures > 5) {
      throw new Error(
        `Too many failed unit downloads: ${downloadFailures}/${total} failed`,
      );
    }
    if (unitsIncluded === 0) {
      throw new Error("No usable capture units after segment build");
    }
    if (buildFailures > 5) {
      throw new Error(
        `Too many failed segment builds: ${buildFailures}/${total - downloadFailures} failed`,
      );
    }
    if (downloadFailures + buildFailures > 0) {
      console.warn(
        `${downloadFailures} download / ${buildFailures} build failures out of ${total} units, continuing`,
      );
    }

    // The units that actually made it in, in output order. Array index =
    // video second = real-world minute — the exact video-time ↔ wall-clock
    // map the edit feature scrubs and cuts against. Built from the segment
    // list (not the sampled rows) so build-failure holes never desync it.
    const videoUnits: VideoUnit[] = [];
    for (let i = 0; i < total; i++) {
      if (segmentPaths[i] === null) continue;
      const ss = unitRows[i];
      const ts = ss.captured_at ?? ss.requested_at;
      videoUnits.push({
        capturedAt: (ts instanceof Date ? ts : new Date(ts)).toISOString(),
        screenshotId: ss.id,
        frameCount: ss.frame_count ?? 1,
      });
    }

    // Step 4: Assemble — stream-copy concat of the segments, remuxed to MP4.
    // No re-encoding on the happy path: segments share pinned parameters and
    // each starts on an IDR frame, so this is I/O-bound (seconds, even for a
    // 12-hour session).
    const concatListPath = path.join(tmpDir, "segments.txt");
    await fs.writeFile(
      concatListPath,
      segments.map((p) => `file '${p}'`).join("\n") + "\n",
    );

    const originalPath = path.join(tmpDir, "original.mp4");
    let originalSize: number;
    try {
      await execFileAsync(
        "ffmpeg",
        [
          "-f", "concat",
          "-safe", "0",
          "-i", concatListPath,
          "-c", "copy",
          "-movflags", "+faststart",
          "-y",
          originalPath,
        ],
        { timeout: ASSEMBLE_TIMEOUT_MS },
      );
      originalSize = await verifyVideo(
        originalPath,
        unitsIncluded,
        SEGMENT_FPS,
        "MP4",
      );
    } catch (err) {
      // Safety net: if the copied stream doesn't verify (e.g. an encoder
      // parameter drifted between segments), re-encode the already-built
      // segments into one uniform stream. One extra ffmpeg pass over 1s
      // segments — not a second pipeline. The GOP args keep the fallback
      // output on the same 1s IDR grid as the copy path, so the video stays
      // losslessly cuttable by the edit feature.
      console.warn("Stream-copy assembly failed, re-encoding segments:", err);
      await execFileAsync(
        "ffmpeg",
        [
          "-f", "concat",
          "-safe", "0",
          "-i", concatListPath,
          // Match the segment encoder for this TIER — the fallback must not
          // be a quality downgrade on the publish tier, and must not be an
          // expensive upgrade on the throwaway preview tier.
          ...segmentEncodeArgs(buildQuality, { singleThreaded: false }),
          "-r", String(SEGMENT_FPS),
          "-movflags", "+faststart",
          "-y",
          originalPath,
        ],
        { timeout: ASSEMBLE_TIMEOUT_MS },
      );
      originalSize = await verifyVideo(
        originalPath,
        unitsIncluded,
        SEGMENT_FPS,
        "MP4 (re-encoded)",
      );
    }
    // Both assembly paths land on the pinned 1s closed-GOP grid.
    const videoCopyAligned = true;

    // Step 4.25: apply cuts and masks, if any. A first compile normally has none —
    // cuts/masks are authored during the edit hold, after this build hands the
    // user a preview. This branch covers a re-run that lost its original
    // (e.g. an internal recompile of a failed edited compile).
    const unitTimesMs = videoUnits.map((u) => Date.parse(u.capturedAt));
    const keptRanges = computeKeptRanges(unitTimesMs, cuts);
    const hasEffectiveCuts =
      cuts.length > 0 &&
      keptRanges.reduce((n, r) => n + (r.end - r.start), 0) < videoUnits.length;
    if (cuts.length > 0 && keptRanges.length === 0) {
      throw new Error("Cut list removes every capture unit — refusing to compile an empty video");
    }

    const masks = (session.masks ?? []) as MaskRegion[];
    const videoMasks = masksToVideoMasks(masks, videoUnits);
    const hasEffectiveMasks = videoMasks.length > 0;
    const needsEdit = hasEffectiveCuts || hasEffectiveMasks;

    let publishPath = originalPath;
    let publishSize = originalSize;
    // Reuse the existing key on a recompile so the old object is overwritten
    // rather than orphaned; mint a fresh unguessable one otherwise.
    const originalR2Key =
      session.originalVideoR2Key ?? uncutOriginalKey(sessionId);
    let publishR2Key = originalR2Key;

    if (needsEdit) {
      publishPath = await cutVideoToKeptRanges(
        tmpDir,
        originalPath,
        keptRanges,
        videoCopyAligned && !hasEffectiveMasks,
        videoMasks,
      );
      publishSize = (await fs.stat(publishPath)).size;
      publishR2Key = `timelapses/${sessionId}/edited.mp4`;
    }

    // Step 4.5: Extract thumbnail from the PUBLISHED video's first frame
    // (post-cut when edited, so a cut first minute never leaks a stale frame).
    const thumbnailPath = path.join(tmpDir, "thumbnail.jpg");
    await extractThumbnail(publishPath, thumbnailPath);

    // Step 5: Upload all artifacts to R2 and verify
    const thumbnailR2Key = `timelapses/${sessionId}/thumbnail.jpg`;
    const thumbnailBytes = await fs.readFile(thumbnailPath);
    await r2Client.send(
      new PutObjectCommand({
        Bucket: R2_BUCKET,
        Key: thumbnailR2Key,
        Body: thumbnailBytes,
        ContentType: "image/jpeg",
        CacheControl: "public, max-age=31536000, immutable",
      }),
    );

    await uploadAndVerify(
      originalPath,
      originalR2Key,
      "video/mp4",
      originalSize,
      "MP4 (original)",
    );
    if (needsEdit) {
      await uploadAndVerify(
        publishPath,
        publishR2Key,
        "video/mp4",
        publishSize,
        "MP4 (edited)",
      );
    }

    // Authoritative cut-seconds for the tracked-time subtraction.
    const cutSeconds = hasEffectiveCuts
      ? computeCutSeconds(
          await getCaptureRowsForCuts(sessionId),
          session.trackingMode === "credit" ? "credit" : "bucket",
          session.trackedSeconds ?? 0,
          cuts,
        )
      : 0;

    // Step 6: Publish — or hold.
    //
    // A session stopped with `{edit: true}` must NOT reach `complete` yet:
    // that status is what programs act on (forwarding heartbeats, accepting
    // submissions, firing the redirect hook), so it may only appear once
    // the user's cuts are baked in. Such a session goes back to `stopped`
    // with everything built but `video_r2_key` still null — the editor
    // opens on the original, and either the user's publish call or the
    // hold-expiry job flips it to `complete`.
    const thumbnailUrl = R2_PUBLIC_DOMAIN
      ? `https://${R2_PUBLIC_DOMAIN}/${thumbnailR2Key}`
      : thumbnailR2Key;

    const videoUrl = R2_PUBLIC_DOMAIN
      ? `https://${R2_PUBLIC_DOMAIN}/${publishR2Key}`
      : publishR2Key;

    // Re-read the hold: the user may have let it lapse (or the expiry job
    // may have cleared it) during the minutes this build was running.
    const [current] = await db
      .select({ editHoldUntil: schema.sessions.editHoldUntil })
      .from(schema.sessions)
      .where(eq(schema.sessions.id, sessionId));
    const holdActive =
      current?.editHoldUntil != null &&
      current.editHoldUntil.getTime() > Date.now();

    // A PREVIEW-grade build may never publish, hold or no hold — the file is
    // low-resolution and exists only for the editor. So it always records
    // itself as the unpublished original, and if the hold lapsed while we
    // were encoding (the one race the two-tier split introduces) it hands
    // straight over to the publish path, which re-encodes from the capture
    // units at full quality. That keeps exactly one implementation of
    // "produce the published video" instead of a second copy here.
    if (buildQuality === "preview") {
      await db
        .update(schema.sessions)
        .set({
          status: "stopped",
          compileProgress: null,
          videoUrl: null,
          videoR2Key: null,
          originalVideoR2Key: originalR2Key,
          originalIsPreview: true,
          videoUnits,
          videoCopyAligned,
          cutSeconds,
          thumbnailUrl,
          thumbnailR2Key,
          updatedAt: new Date(),
        })
        .where(eq(schema.sessions.id, sessionId));

      if (holdActive) {
        console.log(
          `Session ${sessionId} preview built, held for editing until ` +
            `${current!.editHoldUntil!.toISOString()}`,
        );
        await cleanUpCaptureLeftovers(sessionId);
        return {
          videoUrl,
          videoR2Key: publishR2Key,
          thumbnailUrl,
          thumbnailR2Key,
        };
      }

      console.warn(
        `Session ${sessionId}: edit hold lapsed during the preview build — ` +
          `publishing at full quality from capture units instead.`,
      );
      const fresh = await db.query.sessions.findFirst({
        where: eq(schema.sessions.id, sessionId),
      });
      return await applyCutCompile(fresh!, cuts, tmpDir);
    }

    await db
      .update(schema.sessions)
      .set({
        status: holdActive ? "stopped" : "complete",
        // The build is done — the wait now hinges on the status flip, not a
        // fraction. Clear it so a later reopen doesn't show stale progress.
        compileProgress: null,
        videoUrl: holdActive ? null : videoUrl,
        videoR2Key: holdActive ? null : publishR2Key,
        originalVideoR2Key: originalR2Key,
        videoUnits,
        videoCopyAligned,
        cutSeconds,
        ...(needsEdit ? { lastEditCompileAt: new Date() } : {}),
        thumbnailUrl,
        thumbnailR2Key,
        updatedAt: new Date(),
      })
      .where(eq(schema.sessions.id, sessionId));

    if (holdActive) {
      console.log(
        `Session ${sessionId} built and held for editing until ${current!.editHoldUntil!.toISOString()}`,
      );
    }

    // Step 7: Cleanup unsampled screenshots from R2
    await cleanUpCaptureLeftovers(sessionId);

    return {
      videoUrl,
      videoR2Key: publishR2Key,
      thumbnailUrl,
      thumbnailR2Key,
    };
  } finally {
    // Always clean up temp directory
    await fs.rm(tmpDir, { recursive: true, force: true }).catch(() => {});
  }
}

/**
 * Half B of the compile job: bake the user's cuts into the already-built
 * original and PUBLISH the session. Downloads a single MP4, cuts it
 * (usually a lossless stream copy), regenerates the thumbnail, and flips
 * the session `complete` — no capture units involved.
 *
 * This is the end of the edit hold, so the uncut original is deleted right
 * after: the cut minutes are gone the moment the timelapse goes out, not
 * seven days later. Recovering an edited session afterwards (admin
 * recompile) falls back to Half A, which rebuilds from capture units and
 * applies the same cut list.
 */
/**
 * Build the PUBLISHED video from the session's capture units at full quality,
 * including only the kept ranges.
 *
 * This is the second tier of the two-tier compile: the editor ran against a
 * cheap preview, and this is where the timelapse that actually goes out is
 * made. Cuts are applied by simply not encoding the removed units, so no
 * separate cut step (and no generation of loss) is involved, and a session
 * with half its minutes cut costs half as much to publish.
 *
 * Returns null when the units can no longer be read — the caller decides how
 * to degrade rather than having a failure imposed on it.
 */
async function buildPublishFromUnits(
  sessionId: string,
  tmpDir: string,
  keptRanges: KeptRange[],
  totalUnits: number,
): Promise<{ path: string; size: number } | null> {
  // The unit map's index space is what keptRanges refer to: index i is the
  // i-th unit in the compiled video, which is the i-th sampled screenshot in
  // minute-bucket order (the same DISTINCT ON contract Half A uses, minus the
  // dropped seed unit).
  const sampled = await db.execute<{ r2_key: string; format: string }>(sql`
    SELECT DISTINCT ON (minute_bucket) r2_key, format
    FROM screenshots
    WHERE session_id = ${sessionId} AND confirmed = true AND sampled = true
    ORDER BY minute_bucket ASC, captured_at ASC NULLS LAST, requested_at ASC
  `);
  const rows = sampled.rows;
  if (rows.length !== totalUnits) {
    console.warn(
      `Session ${sessionId}: expected ${totalUnits} sampled units for a ` +
        `publish re-encode, found ${rows.length} — falling back.`,
    );
    return null;
  }

  const keptIndices: number[] = [];
  for (const r of keptRanges) {
    for (let i = r.start; i < r.end; i++) keptIndices.push(i);
  }
  if (keptIndices.length === 0) return null;

  const segmentPaths: (string | null)[] = new Array(keptIndices.length).fill(null);
  let failures = 0;
  let next = 0;
  const worker = async () => {
    while (next < keptIndices.length) {
      const slot = next++;
      const unitIndex = keptIndices[slot];
      const row = rows[unitIndex];
      const ext = row.format === "jpeg" ? "jpg" : row.format;
      const unitPath = path.join(tmpDir, `pub_${slot}.${ext}`);
      try {
        const response = await r2Client.send(
          new GetObjectCommand({ Bucket: R2_BUCKET, Key: row.r2_key }),
        );
        await fs.writeFile(
          unitPath,
          await response.Body!.transformToByteArray(),
        );
      } catch {
        failures++;
        continue;
      }
      try {
        // `pub_` index space, so these never collide with the preview run's
        // segment files still sitting in tmpDir.
        segmentPaths[slot] = await buildSegment(
          tmpDir,
          10_000 + slot,
          unitPath,
          row.format,
          "publish",
        );
      } catch (err) {
        failures++;
        console.warn(`Session ${sessionId}: publish segment ${slot} failed`, err);
      }
    }
  };
  await Promise.all(
    Array.from(
      { length: Math.min(SEGMENT_CONCURRENCY, keptIndices.length) },
      worker,
    ),
  );

  const segments = segmentPaths.filter((p): p is string => p !== null);
  // A gap would silently shorten the timelapse and desync the unit map the
  // editor and /timings both read, so this is all-or-nothing.
  if (failures > 0 || segments.length !== keptIndices.length) {
    console.warn(
      `Session ${sessionId}: ${failures} unit(s) unavailable for the publish ` +
        `re-encode (${segments.length}/${keptIndices.length} built).`,
    );
    return null;
  }

  const concatListPath = path.join(tmpDir, "publish_concat.txt");
  await fs.writeFile(
    concatListPath,
    segments.map((p) => `file '${p}'`).join("\n") + "\n",
  );
  const outPath = path.join(tmpDir, "publish.mp4");
  try {
    await execFileAsync(
      "ffmpeg",
      [
        "-f", "concat",
        "-safe", "0",
        "-i", concatListPath,
        "-c", "copy",
        "-movflags", "+faststart",
        "-y",
        outPath,
      ],
      { timeout: ASSEMBLE_TIMEOUT_MS },
    );
    const size = await verifyVideo(
      outPath,
      segments.length,
      SEGMENT_FPS,
      "Published MP4 (from units)",
    );
    return { path: outPath, size };
  } catch (err) {
    // Same safety net as Half A's assembly: re-encode the segments into one
    // uniform stream, keeping the pinned grid so the result stays cuttable.
    console.warn(
      `Session ${sessionId}: stream-copy assembly of the published video ` +
        `failed, re-encoding segments:`,
      err,
    );
    await execFileAsync(
      "ffmpeg",
      [
        "-f", "concat",
        "-safe", "0",
        "-i", concatListPath,
        ...segmentEncodeArgs("publish", { singleThreaded: false }),
        "-r", String(SEGMENT_FPS),
        "-movflags", "+faststart",
        "-y",
        outPath,
      ],
      { timeout: ASSEMBLE_TIMEOUT_MS },
    );
    const size = await verifyVideo(
      outPath,
      segments.length,
      SEGMENT_FPS,
      "Published MP4 (from units, re-encoded)",
    );
    return { path: outPath, size };
  }
}

async function applyCutCompile(
  session: typeof schema.sessions.$inferSelect,
  cuts: CutInterval[],
  tmpDir: string,
): Promise<{
  videoUrl: string;
  videoR2Key: string;
  thumbnailUrl: string;
  thumbnailR2Key: string;
}> {
  const sessionId = session.id;
  const originalR2Key = session.originalVideoR2Key!;
  const editedR2Key = `timelapses/${sessionId}/edited.mp4`;
  const videoUnits = session.videoUnits as VideoUnit[];

  const unitTimesMs = videoUnits.map((u) => Date.parse(u.capturedAt));
  const keptRanges = computeKeptRanges(unitTimesMs, cuts);
  const keptUnits = keptRanges.reduce((n, r) => n + (r.end - r.start), 0);
  const hasEffectiveCuts = cuts.length > 0 && keptUnits < videoUnits.length;

  const masks = (session.masks ?? []) as MaskRegion[];
  const videoMasks = masksToVideoMasks(masks, videoUnits);
  const hasEffectiveMasks = videoMasks.length > 0;
  const needsEdit = hasEffectiveCuts || hasEffectiveMasks;
  const hasEdits = hasEffectiveCuts || (masks && masks.length > 0);

  if (cuts.length > 0 && keptRanges.length === 0) {
    throw new Error(
      "Cut list removes every capture unit — refusing to compile an empty video",
    );
  }

  let publishPath: string;
  let publishR2Key: string;

  if (session.originalIsPreview) {
    // The original is the throwaway preview: it is low-resolution, so it can
    // neither be published nor cut-copied. Build the published video from the
    // capture units at full quality, encoding ONLY the kept ones — which
    // makes a heavily-cut session cheaper here than an uncut one, not dearer.
    const rangesToBuild = hasEffectiveMasks
      ? [{ start: 0, end: videoUnits.length }]
      : keptRanges;
    const built = await buildPublishFromUnits(
      sessionId,
      tmpDir,
      rangesToBuild,
      videoUnits.length,
    );
    if (built && !hasEffectiveMasks) {
      publishPath = built.path;
      publishR2Key = hasEffectiveCuts ? editedR2Key : originalR2Key;
    } else {
      const originalPath = built ? built.path : path.join(tmpDir, "original.mp4");
      if (!built) {
        // The units are gone (retention purge, or an R2 outage that outlasted
        // the retries). Publishing the preview is a visible quality drop, but a
        // held session that can never publish is worse — the user's recording
        // would be lost. Take the copy path and say so loudly.
        console.error(
          `Session ${sessionId}: cannot re-encode from capture units — ` +
            `publishing the PREVIEW-grade original instead. The timelapse will ` +
            `be ${PREVIEW_WIDTH}x${PREVIEW_HEIGHT} rather than full resolution.`,
        );
        await downloadObject(originalR2Key, originalPath);
      }
      publishPath = originalPath;
      publishR2Key = originalR2Key;
      if (needsEdit) {
        publishPath = await cutVideoToKeptRanges(
          tmpDir,
          originalPath,
          keptRanges,
          session.videoCopyAligned === true && !hasEffectiveMasks,
          videoMasks,
        );
        publishR2Key = editedR2Key;
      }
    }
  } else {
    // Publish-grade original (a legacy session, or one that never entered the
    // edit flow): cut it losslessly, exactly as before.
    const originalPath = path.join(tmpDir, "original.mp4");
    await downloadObject(originalR2Key, originalPath);
    publishPath = originalPath;
    publishR2Key = originalR2Key;

    if (needsEdit) {
      publishPath = await cutVideoToKeptRanges(
        tmpDir,
        originalPath,
        keptRanges,
        session.videoCopyAligned === true && !hasEffectiveMasks,
        videoMasks,
      );
      publishR2Key = editedR2Key;
    }
  }

  // Thumbnail follows the published video: a cut first minute must not leak
  // a stale first frame, and un-cutting must restore the original's.
  const thumbnailPath = path.join(tmpDir, "thumbnail.jpg");
  await extractThumbnail(publishPath, thumbnailPath);

  const thumbnailR2Key = `timelapses/${sessionId}/thumbnail.jpg`;
  const thumbnailBytes = await fs.readFile(thumbnailPath);
  await r2Client.send(
    new PutObjectCommand({
      Bucket: R2_BUCKET,
      Key: thumbnailR2Key,
      Body: thumbnailBytes,
      ContentType: "image/jpeg",
      CacheControl: "public, max-age=86400",
    }),
  );

  if (needsEdit) {
    const publishSize = (await fs.stat(publishPath)).size;
    await uploadAndVerify(
      publishPath,
      publishR2Key,
      "video/mp4",
      publishSize,
      "MP4 (edited)",
    );
  } else if (session.videoR2Key && session.videoR2Key !== originalR2Key) {
    // Un-cut: the original becomes the published video again; drop the now
    // stale edited artifact.
    await deleteObjectQuiet(session.videoR2Key);
  }

  const cutSeconds = hasEffectiveCuts
    ? computeCutSeconds(
        await getCaptureRowsForCuts(sessionId),
        session.trackingMode === "credit" ? "credit" : "bucket",
        session.trackedSeconds ?? 0,
        cuts,
      )
    : 0;

  const thumbnailUrl = R2_PUBLIC_DOMAIN
    ? `https://${R2_PUBLIC_DOMAIN}/${thumbnailR2Key}`
    : thumbnailR2Key;
  const videoUrl = R2_PUBLIC_DOMAIN
    ? `https://${R2_PUBLIC_DOMAIN}/${publishR2Key}`
    : publishR2Key;

  await db
    .update(schema.sessions)
    .set({
      status: "complete",
      videoUrl,
      videoR2Key: publishR2Key,
      cutSeconds,
      // The hold ends here — the session is published and its numbers are
      // final for every program reading them.
      editHoldUntil: null,
      // Cut/mask content must not outlive the publish: once the edited video is
      // out, the uncut original is deleted below and its key cleared.
      ...(hasEdits ? { originalVideoR2Key: null } : {}),
      lastEditCompileAt: new Date(),
      thumbnailUrl,
      thumbnailR2Key,
      updatedAt: new Date(),
    })
    .where(eq(schema.sessions.id, sessionId));

  // Delete the uncut original only AFTER the edited video is published and
  // the row committed — if this ordering flipped, a crash in between would
  // leave a session pointing at bytes that no longer exist.
  if (hasEdits) {
    await deleteObjectQuiet(originalR2Key);
  }

  return { videoUrl, videoR2Key: publishR2Key, thumbnailUrl, thumbnailR2Key };
}
