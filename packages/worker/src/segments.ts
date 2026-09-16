// Segment building: normalizing capture units (legacy JPEGs and per-minute
// clips) into uniform 1-second video segments that the compiler can
// stream-copy concatenate into the final timelapse. Also the cut step of
// the edit feature, which exploits the same pinned grid. Kept free of DB/R2
// imports so the ffmpeg contracts are testable in isolation.

import { execFile } from "node:child_process";
import { promisify } from "node:util";
import * as fs from "node:fs/promises";
import * as path from "node:path";
import type { KeptRange } from "@lookout/shared";

const execFileAsync = promisify(execFile);

/**
 * Drop a BUCKET-MODE session's seed capture from the units that become
 * video. `rows` must be ordered by minute bucket ascending (the
 * `DISTINCT ON` contract in the compiler), so the seed is the first entry.
 *
 * Positional because bucket-mode rows carry no per-row credit to recognise
 * a seed by, which also means this only ever catches the session's FIRST
 * one. Credit-mode sessions drop seeds in the sampler instead, by credit
 * signature — that is what catches the ones a resume plants mid-session.
 *
 * The seed is the capture that STARTS the recording rather than closing a
 * recorded minute, and it is special in two measurable ways:
 *
 *  - It credits 0 tracked seconds. Both tracking modes agree: bucket mode
 *    reports `(distinct buckets - 1) * 60`, and in credit mode the seed
 *    capture is explicitly worth 0. Every other unit is worth 60s. Giving
 *    the seed a video second is therefore the exact reason a session
 *    reported N seconds of video against N-1 minutes of tracked time.
 *  - In clips mode it covers a fraction of a minute. The recorder cuts the
 *    opening clip after CLIP_FIRST_CUT_DELAY_MS (~8s) so the session
 *    activates quickly, so that clip holds ~8s of wall clock where every
 *    later clip holds 60s. Rendered as an equal one-second segment it plays
 *    at ~8x while the rest of the timelapse plays at 60x — a visible
 *    slow-motion lurch at the head of every video.
 *
 * Excluding it makes the rule uniform: a capture earns video time exactly
 * when it earns tracked time. The timelapse still opens on motion (the
 * first shown unit is a full ~15-frame clip), which is what the dense
 * opening cadence was originally for.
 *
 * A single-unit session keeps its one unit — a zero-length video is worse
 * than an imprecise one.
 */
export function dropSeedUnit<T>(rows: T[]): T[] {
  return rows.length > 1 ? rows.slice(1) : rows;
}

/**
 * Which of the two compile tiers a build belongs to.
 *
 * - `publish` — the video that actually goes out. Full resolution, visually
 *   lossless. This is the only tier a non-edited session ever builds.
 * - `preview` — a throwaway scrubbing copy built ONLY to open the editor
 *   quickly, then deleted when the session publishes. Nothing derives from
 *   it: the published video is re-encoded from the capture units, so this
 *   tier's quality never reaches a viewer and can be as cheap as remains
 *   useful for choosing cuts.
 *
 * Both tiers keep the pinned 1-second closed GOP (see SEGMENT_GOP_ARGS): the
 * editor maps video seconds to capture units and seeks by second, so the
 * grid is load-bearing for the preview too.
 */
export type SegmentQuality = "publish" | "preview";

/** Preview tier resolution. 720p is a quarter of 1080p's pixels — the
 *  dominant term in encode cost — while still showing enough of a code
 *  editor or browser window to tell one minute from another, which is the
 *  only judgement the cut UI asks of it. */
export const PREVIEW_WIDTH = 1280;
export const PREVIEW_HEIGHT = 720;

/** Scale-with-pillarbox filter for a tier. The range/matrix options make
 *  swscale convert rather than relabel — a jpeg decodes full-range BT.601, a
 *  clip limited BT.709, and segments have to agree (SEGMENT_COLOR_ARGS).
 *  Padding after the conversion keeps the pillarbox black at Y=16. */
export function scaleFilter(quality: SegmentQuality = "publish"): string {
  const [w, h] =
    quality === "preview" ? [PREVIEW_WIDTH, PREVIEW_HEIGHT] : [1920, 1080];
  return (
    `scale=${w}:${h}:force_original_aspect_ratio=decrease` +
    `:in_range=auto:out_range=tv:out_color_matrix=bt709` +
    `,pad=${w}:${h}:(ow-iw)/2:(oh-ih)/2`
  );
}

/** Shared video filter: scale to 1920x1080 with pillarboxing. */
export const SCALE_FILTER = scaleFilter("publish");

/** Output framerate of the compiled timelapse. Every capture unit (one
 *  recorded minute) becomes exactly one second of output at this rate.
 *
 *  6 matches the capture cadence (CLIP_FRAME_INTERVAL_MS = 10s → 6 frames
 *  per recorded minute), so each output frame is one captured frame — no
 *  duplication padding. The distinct-image rate is identical to the old
 *  30fps output (a unit still shows its ~6 captured images per second);
 *  what changes is container overhead (5x fewer packets) and that a unit
 *  carrying MORE than 6 frames (both boundary tick frames, or a stalled
 *  clip that kept recording) gets resampled down to 6 instead of showing
 *  every frame.
 *
 *  Originals compiled before this change are 30fps. Everything that touches
 *  an EXISTING artifact must therefore ask the file for its rate
 *  (`probeFps`) rather than trusting this constant — `cutVideoToKeptRanges`
 *  does — while fresh builds derive uniformly from here. */
export const SEGMENT_FPS = 6;

/** How many segment builds run concurrently. Each build is a small
 *  single-threaded ffmpeg (see -threads 1 below), so the parallelism
 *  lives here rather than inside x264. */
export const SEGMENT_CONCURRENCY = 8;

/** Timeout for one segment build (demux + 30-frame encode). */
export const SEGMENT_TIMEOUT_MS = 120_000;

/** Timeout for final assembly. The happy path is a stream-copy remux
 *  (seconds even for 12h sessions); the budget covers the re-encode
 *  fallback too. */
export const ASSEMBLE_TIMEOUT_MS = 1_800_000;

/** Every x264 key in one string: a second `-x264-params` replaces the first
 *  instead of merging, so splitting these up silently drops `open-gop=0`.
 *  colorprim/transfer are here because `-color_primaries`/`-color_trc` never
 *  reach the VUI on their own. */
export const SEGMENT_X264_PARAMS =
  "open-gop=0:colorprim=bt709:transfer=bt709:colormatrix=bt709";

/** GOP pinning shared by every encode that produces (part of) a compiled
 *  timelapse: a closed GOP of exactly one segment (1 second) starting on an
 *  IDR frame, scene-cut keyframes disabled. This grid is ALSO what makes
 *  compiled videos losslessly cuttable at second boundaries by the edit
 *  feature's stream-copy cut — every encode path (segments, assembly
 *  fallback, edited-video re-encode fallback) must keep it. */
export const SEGMENT_GOP_ARGS = [
  "-g", String(SEGMENT_FPS),
  "-keyint_min", String(SEGMENT_FPS),
  "-sc_threshold", "0",
  // Carries the colour keys too — see SEGMENT_X264_PARAMS.
  "-x264-params", SEGMENT_X264_PARAMS,
];

/** Segments stream-copy concat into a file that carries only the FIRST
 *  segment's colour tags, so every builder has to write the same ones.
 *  `-pix_fmt yuv420p` alone doesn't do it: it picks a format, not a range,
 *  and jpeg units were coming out full-range (white Y=255 against a clip's
 *  235). scaleFilter converts the pixels; these declare the result. */
export const SEGMENT_COLOR_ARGS = [
  "-color_range", "tv",
  "-colorspace", "bt709",
  "-color_primaries", "bt709",
  "-color_trc", "bt709",
];

/**
 * Pinned x264 parameters for a segment encode. Segments within one build
 * must be bit-compatible so final assembly can stream-copy concatenate them:
 * fixed profile/level, the pinned GOP grid above, single-threaded (the
 * parallelism is at the segment level). Changing any of these breaks
 * copy-concat — keep in lockstep with the assembly fallback and the
 * mixed-session compile test.
 *
 * `publish` tier: CRF 18 = visually lossless. The compile step must not be a
 * quality event — the clip bitrate is the only intended quality dial. (The
 * legacy pipeline used CRF 28, which added a visible second generation of
 * loss on top of already-compressed clips.) Costs ~2.5-3x the output size of
 * CRF 28; timelapses are short, so absolute sizes stay modest.
 *
 * `preview` tier: as cheap as stays useful for choosing cuts, because the
 * file is deleted at publish and no viewer ever sees it. Measured per unit
 * through buildSegment on a 10-core box, 6fpm clip, against 431ms/55KB for
 * the publish tier:
 *
 *      720p ultrafast crf30    72 ms   58 KB
 *      720p superfast crf30    83 ms   29 KB   <- chosen
 *      720p veryfast  crf30   103 ms   25 KB
 *
 * `superfast` rather than `ultrafast`: 15% slower for HALF the bytes, and the
 * preview is not just encoded — the worker uploads it and the editor streams
 * it back, so its size is part of the latency this tier exists to reduce.
 * ultrafast's output is actually LARGER than the 1080p publish tier's, which
 * would have made the editor slower to load in exchange for the faster
 * encode. Net: ~5x faster to build and half the size to move.
 */
export function segmentEncodeArgs(
  quality: SegmentQuality = "publish",
  opts: { singleThreaded?: boolean } = {},
): string[] {
  const { singleThreaded = true } = opts;
  const tier =
    quality === "preview"
      ? ["-preset", "superfast", "-crf", "30"]
      : ["-preset", "fast", "-crf", "18"];
  return [
    "-c:v", "libx264",
    "-profile:v", "high",
    "-level:v", "4.0",
    ...tier,
    "-pix_fmt", "yuv420p",
    ...SEGMENT_COLOR_ARGS,
    ...SEGMENT_GOP_ARGS,
    // Segment builds are single-threaded because the parallelism lives at the
    // segment level (SEGMENT_CONCURRENCY). Whole-file encodes — the assembly
    // fallback, the cut re-encode — are one process at a time and should use
    // the box.
    ...(singleThreaded ? ["-threads", "1"] : []),
  ];
}

/** Publish-tier segment parameters. See segmentEncodeArgs. */
export const SEGMENT_ENCODE_ARGS = segmentEncodeArgs("publish");

/** Probe a video's frame rate (rounded to integer fps). Used wherever the
 *  code touches an artifact that may predate the current SEGMENT_FPS —
 *  originals compiled before the 6fps change are 30fps, and cutting one
 *  with the wrong frames-per-unit would silently produce a video 5x too
 *  short while the frame-count verify (built from the same wrong constant)
 *  still passed. Falls back to SEGMENT_FPS if the probe is unusable. */
export async function probeFps(filePath: string): Promise<number> {
  try {
    const { stdout } = await execFileAsync(
      "ffprobe",
      [
        "-v", "error",
        "-select_streams", "v:0",
        "-show_entries", "stream=r_frame_rate",
        "-of", "csv=p=0",
        filePath,
      ],
      { timeout: 30_000 },
    );
    const [num, den] = stdout.trim().split("/").map(Number);
    const fps = Math.round(num / (den || 1));
    if (Number.isFinite(fps) && fps >= 1) return fps;
  } catch {
    // fall through to the constant
  }
  return SEGMENT_FPS;
}

/** Count the video frames in a file with ffprobe. */
export async function probeFrameCount(filePath: string): Promise<number> {
  const { stdout } = await execFileAsync(
    "ffprobe",
    [
      "-v", "error",
      "-count_packets",
      "-select_streams", "v:0",
      "-show_entries", "stream=nb_read_packets",
      "-of", "csv=p=0",
      filePath,
    ],
    { timeout: 30_000 },
  );
  return parseInt(stdout.trim(), 10);
}

/** Assert a built segment holds exactly SEGMENT_FPS frames. Strict — a
 *  short segment would silently desync every later minute of the video. */
async function verifySegmentFrameCount(filePath: string): Promise<void> {
  const frames = await probeFrameCount(filePath);
  if (frames !== SEGMENT_FPS) {
    throw new Error(`segment has ${frames} frames, expected ${SEGMENT_FPS}`);
  }
}

/**
 * Normalize one capture unit into a 1-second, SEGMENT_FPS MPEG-TS segment
 * with the pinned encoder parameters.
 *
 * - jpeg unit: the still is held for the full second — identical to the
 *   legacy one-frame-per-minute output.
 * - webm/mp4 clip: transcoded in ONE ffmpeg pass — decode, retime the
 *   REAL frame count evenly across the second (clips are VFR; the
 *   client's claimed frameCount is never trusted, ffprobe counts), scale,
 *   encode. No intermediate JPEG round-trip: that cost an extra encode+
 *   decode generation (visible softness on top of the clip's own
 *   compression) and an extra process per unit.
 *
 * Returns the segment path; throws if the unit is undecodable.
 */
export async function buildSegment(
  tmpDir: string,
  index: number,
  unitPath: string,
  format: string,
  quality: SegmentQuality = "publish",
): Promise<string> {
  const segmentPath = path.join(
    tmpDir,
    `segment_${String(index).padStart(5, "0")}.ts`,
  );
  const encodeArgs = segmentEncodeArgs(quality);
  const scale = scaleFilter(quality);

  if (format === "jpeg") {
    // -framerate 1 over one still = exactly one second of input; fps
    // duplicates it onto the SEGMENT_FPS grid, -frames:v hard-caps the
    // length.
    await execFileAsync(
      "ffmpeg",
      [
        "-framerate", "1",
        "-i", unitPath,
        "-vf", `${scale},fps=${SEGMENT_FPS}`,
        "-frames:v", String(SEGMENT_FPS),
        ...encodeArgs,
        "-f", "mpegts",
        "-y",
        segmentPath,
      ],
      { timeout: SEGMENT_TIMEOUT_MS },
    );
  } else {
    const frames = await probeFrameCount(unitPath);
    if (!Number.isFinite(frames) || frames < 1) {
      throw new Error("clip contained no decodable frames");
    }
    // setpts spreads the N decoded frames evenly across [0, 1s); fps
    // resamples onto the SEGMENT_FPS grid (a clip carrying more frames
    // than that — boundary tick frames, or a stall that kept recording —
    // is downsampled); tpad clone-extends the last frame so PTS rounding
    // can never come up a frame short; -frames:v caps at exactly one
    // segment.
    await execFileAsync(
      "ffmpeg",
      [
        "-i", unitPath,
        "-vf",
        `setpts=N/(${frames}*TB),${scale},fps=${SEGMENT_FPS},tpad=stop_mode=clone:stop=-1`,
        "-frames:v", String(SEGMENT_FPS),
        ...encodeArgs,
        "-f", "mpegts",
        "-y",
        segmentPath,
      ],
      { timeout: SEGMENT_TIMEOUT_MS },
    );
  }

  // Frame-count verification costs an ffprobe per unit (~22ms measured), and
  // it exists because a short segment silently desyncs every later minute of
  // the PUBLISHED video. The preview is a scrubbing aid that gets deleted, so
  // a one-frame drift in it is invisible and not worth the process — the
  // publish tier is still checked strictly.
  if (quality === "publish") {
    await verifySegmentFrameCount(segmentPath);
  }
  return segmentPath;
}

/**
 * The edit feature's cut step: produce a video containing only the kept
 * ranges of a compiled original.
 *
 * Fast path (`aligned`): every second of the original starts on an IDR
 * frame in a closed GOP (the pinned grid above), and one second = one
 * capture unit. Each kept range is extracted losslessly with an input seek
 * to its IDR (`-ss` lands exactly on the second boundary) plus an exact
 * packet count (`-frames:v` applies to copied packets), into an MPEG-TS
 * intermediate; the intermediates stream-copy concat into the edited MP4.
 * NOT the concat demuxer's inpoint/outpoint — outpoint is dts-based, and
 * B-frame dts offsets leak ~2 frames of the CUT region past each boundary.
 * This path is I/O-bound (seconds even for a 12-hour session) and adds
 * zero generation loss.
 *
 * Fallback (originals that predate GOP pinning, or a failed copy): one
 * frame-exact re-encode of the whole original through a `select` filter
 * keeping pts ∈ [start, end) per range, with the pinned parameters — so
 * the output is itself aligned for future edits.
 */
// Mask region in video seconds with normalized coordinates.
export interface VideoMask {
  startSec: number;
  endSec: number;
  x: number;      // 0..1
  y: number;      // 0..1
  width: number;  // 0..1
  height: number; // 0..1
}

// Read video dimensions using ffprobe.
export async function probeDimensions(
  filePath: string,
): Promise<{ width: number; height: number }> {
  try {
    const { stdout } = await execFileAsync(
      "ffprobe",
      [
        "-v", "error",
        "-select_streams", "v:0",
        "-show_entries", "stream=width,height",
        "-of", "json",
        filePath,
      ],
      { timeout: 10_000 },
    );
    const data = JSON.parse(stdout) as {
      streams?: Array<{ width?: number; height?: number }>;
    };
    const stream = data.streams?.[0];
    return {
      width: stream?.width ?? 1920,
      height: stream?.height ?? 1080,
    };
  } catch {
    return { width: 1920, height: 1080 };
  }
}

export async function cutVideoToKeptRanges(
  tmpDir: string,
  originalPath: string,
  keptRanges: KeptRange[],
  aligned: boolean,
  masks?: VideoMask[],
): Promise<string> {
  if (keptRanges.length === 0) {
    throw new Error("cutVideoToKeptRanges: no kept ranges");
  }

  const editedPath = path.join(tmpDir, "edited.mp4");
  const keptUnits = keptRanges.reduce((n, r) => n + (r.end - r.start), 0);
  // The ORIGINAL's rate, not SEGMENT_FPS: originals compiled before the
  // 6fps change are 30fps, and their 1s-per-unit grid means frames-per-unit
  // equals their fps. Using the constant against an old original would copy
  // a fifth of every kept range — and the verify below, built from the same
  // constant, would pass it. The edited output keeps the original's rate so
  // the artifact stays self-consistent for future re-edits.
  const fps = await probeFps(originalPath);
  const expectedFrames = keptUnits * fps;

  const verify = async (label: string, toleranceFrames: number) => {
    const stat = await fs.stat(editedPath);
    if (stat.size === 0) throw new Error(`${label}: ffmpeg produced empty output`);
    const frames = await probeFrameCount(editedPath);
    if (
      !Number.isFinite(frames) ||
      Math.abs(frames - expectedFrames) > toleranceFrames
    ) {
      throw new Error(
        `${label}: frame count mismatch: expected ${expectedFrames} (±${toleranceFrames}), got ${frames}`,
      );
    }
  };

  const hasMasks = masks != null && masks.length > 0;

  if (aligned && !hasMasks) {
    try {
      // 1. Extract each kept range losslessly into a TS intermediate.
      const rangePaths: string[] = [];
      for (const [i, r] of keptRanges.entries()) {
        const rangePath = path.join(tmpDir, `kept_${i}.ts`);
        await execFileAsync(
          "ffmpeg",
          [
            "-ss", String(r.start),
            "-i", originalPath,
            "-c", "copy",
            "-frames:v", String((r.end - r.start) * fps),
            "-avoid_negative_ts", "make_zero",
            "-f", "mpegts",
            "-y",
            rangePath,
          ],
          { timeout: ASSEMBLE_TIMEOUT_MS },
        );
        rangePaths.push(rangePath);
      }

      // 2. Stream-copy concat the ranges (same mechanism as assembly).
      const listPath = path.join(tmpDir, "kept_ranges.txt");
      await fs.writeFile(
        listPath,
        rangePaths.map((p) => `file '${p}'`).join("\n") + "\n",
      );
      await execFileAsync(
        "ffmpeg",
        [
          "-f", "concat",
          "-safe", "0",
          "-i", listPath,
          "-c", "copy",
          "-movflags", "+faststart",
          "-y",
          editedPath,
        ],
        { timeout: ASSEMBLE_TIMEOUT_MS },
      );
      // The copy path must be frame-EXACT — that's the whole point.
      await verify("Edited MP4 (copy)", 0);
      return editedPath;
    } catch (err) {
      // The only way a cut costs quality. The copy path is bit-exact
      // (proven in cutVideo.test.ts by comparing decoded frame hashes),
      // so falling through here means the user's timelapse takes a
      // generation of loss it shouldn't have. Loud, not a debug aside.
      console.error(
        "Stream-copy cut FAILED — falling back to a re-encode, so this " +
          "timelapse loses a generation of quality. Investigate: the " +
          "original was expected to be on the pinned 1s IDR grid.",
        err,
      );
    }
  }

  // Frame-exact single-pass re-encode: keep frames whose pts falls in any
  // kept [start, end) range, then retime onto a contiguous grid at the
  // original's own rate (see `fps` above). The GOP stays one second — pinned
  // by frame count matching the fps — so the output remains aligned for
  // future edits.
  const keepExpr = keptRanges
    .map((r) => `(gte(t\\,${r.start})*lt(t\\,${r.end}))`)
    .join("+");
  if (!hasMasks) {
    await execFileAsync(
      "ffmpeg",
      [
        "-i", originalPath,
        "-vf", `select='${keepExpr}',setpts=N/(${fps}*TB)`,
        "-r", String(fps),
        "-c:v", "libx264",
        "-preset", "fast",
        "-crf", "18",
        "-pix_fmt", "yuv420p",
        "-g", String(fps),
        "-keyint_min", String(fps),
        "-sc_threshold", "0",
        "-x264-params", "open-gop=0",
        "-movflags", "+faststart",
        "-y",
        editedPath,
      ],
      { timeout: ASSEMBLE_TIMEOUT_MS },
    );
  } else {
    // Probe video dimensions to compute pixel coordinates
    const { width: vidW, height: vidH } = await probeDimensions(originalPath);

    // Build FFmpeg filter graph to overlay pixelated boxes
    const filterParts: string[] = [];
    let currentIn = "0:v";

    for (let i = 0; i < masks.length; i++) {
      const m = masks[i];
      const rawX = Math.round(m.x * vidW);
      const rawY = Math.round(m.y * vidH);
      const rawW = Math.round(m.width * vidW);
      const rawH = Math.round(m.height * vidH);

      const X = Math.max(0, Math.min(vidW - 2, rawX & ~1));
      const Y = Math.max(0, Math.min(vidH - 2, rawY & ~1));
      const W = Math.max(2, Math.min(vidW - X, rawW & ~1));
      const H = Math.max(2, Math.min(vidH - Y, rawH & ~1));

      // Scale down 16x and scale back up with nearest-neighbor to pixelate
      const downW = Math.max(2, Math.floor(W / 16 / 2) * 2);
      const downH = Math.max(2, Math.floor(H / 16 / 2) * 2);

      const outTag = `v_mask_${i}`;
      filterParts.push(
        `[${currentIn}]split=2[m_base_${i}][m_crop_${i}]`,
        `[m_crop_${i}]crop=${W}:${H}:${X}:${Y},scale=${downW}:${downH}:flags=neighbor,scale=${W}:${H}:flags=neighbor[m_pix_${i}]`,
        `[m_base_${i}][m_pix_${i}]overlay=${X}:${Y}:enable='gte(t\\,${m.startSec})*lt(t\\,${m.endSec})'[${outTag}]`,
      );
      currentIn = outTag;
    }

    filterParts.push(
      `[${currentIn}]select='${keepExpr}',setpts=N/(${fps}*TB)[vout]`,
    );

    await execFileAsync(
      "ffmpeg",
      [
        "-threads", "0",
        "-i", originalPath,
        "-filter_complex", filterParts.join(";"),
        "-map", "[vout]",
        "-r", String(fps),
        "-c:v", "libx264",
        "-preset", "fast",
        "-crf", "18",
        "-pix_fmt", "yuv420p",
        "-g", String(fps),
        "-keyint_min", String(fps),
        "-sc_threshold", "0",
        "-x264-params", "open-gop=0",
        "-movflags", "+faststart",
        "-y",
        editedPath,
      ],
      { timeout: ASSEMBLE_TIMEOUT_MS },
    );
  }
  await verify("Edited MP4 (re-encoded)", 1);
  return editedPath;
}
