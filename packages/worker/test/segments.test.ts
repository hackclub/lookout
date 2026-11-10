/**
 * Integration tests for the segment pipeline — the contract that makes the
 * compiler's stream-copy assembly safe:
 *
 *   every capture unit (legacy JPEG still, VP8 webm clip, H.264 mp4 clip,
 *   any resolution) → exactly ONE second of 30fps video with pinned,
 *   bit-compatible encoder parameters, concatenable with `-c copy`.
 *
 * Uses real ffmpeg with synthetic inputs (no DB, no R2). Skipped when
 * ffmpeg isn't installed — CI installs it explicitly.
 */
import { describe, expect, it, beforeAll } from "vitest";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import {
  buildSegment,
  probeFrameCount,
  probeDimensions,
  cutVideoToKeptRanges,
  SEGMENT_FPS,
  PREVIEW_WIDTH,
  PREVIEW_HEIGHT,
} from "../src/segments.js";

const execFileAsync = promisify(execFile);

async function hasFfmpeg(): Promise<boolean> {
  try {
    await execFileAsync("ffmpeg", ["-version"], { timeout: 10_000 });
    await execFileAsync("ffprobe", ["-version"], { timeout: 10_000 });
    return true;
  } catch {
    return false;
  }
}

const ffmpegAvailable = await hasFfmpeg();

async function probeResolution(
  filePath: string,
): Promise<{ width: number; height: number }> {
  const { stdout } = await execFileAsync(
    "ffprobe",
    [
      "-v", "error",
      "-select_streams", "v:0",
      "-show_entries", "stream=width,height",
      "-of", "csv=p=0",
      filePath,
    ],
    { timeout: 30_000 },
  );
  // ffprobe lists the stream twice for MPEG-TS ("1280,720\n\n1280,720"), so
  // take the first non-empty line rather than splitting the whole output.
  const line = stdout
    .split("\n")
    .map((l) => l.trim())
    .find((l) => l.length > 0)!;
  const [width, height] = line.split(",").map(Number);
  return { width, height };
}

async function probeDurationSeconds(filePath: string): Promise<number> {
  const { stdout } = await execFileAsync(
    "ffprobe",
    [
      "-v", "error",
      "-show_entries", "format=duration",
      "-of", "csv=p=0",
      filePath,
    ],
    { timeout: 30_000 },
  );
  return parseFloat(stdout.trim());
}

describe.skipIf(!ffmpegAvailable)("segment pipeline", () => {
  let tmpDir: string;
  let jpegPath: string;
  let webmPath: string;
  let vp9Path: string;
  let mp4Path: string;

  beforeAll(async () => {
    tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), "lookout-segments-"));

    // Legacy unit: a single JPEG still (odd size on purpose — the scale
    // filter must normalize it).
    jpegPath = path.join(tmpDir, "unit_jpeg.jpg");
    await execFileAsync(
      "ffmpeg",
      [
        "-f", "lavfi",
        "-i", "testsrc2=size=1280x801:rate=1:duration=1",
        "-frames:v", "1",
        "-y", jpegPath,
      ],
      { timeout: 60_000 },
    );

    // Clip unit: VP8/WebM. A deliberately odd 20 frames — nothing in the
    // pipeline may assume the nominal count, since clips are VFR and the
    // real count comes from demuxing.
    webmPath = path.join(tmpDir, "unit_clip.webm");
    await execFileAsync(
      "ffmpeg",
      [
        "-f", "lavfi",
        "-i", "testsrc2=size=1668x1080:rate=1/3:duration=60",
        "-c:v", "libvpx",
        "-b:v", "133k",
        "-y", webmPath,
      ],
      { timeout: 120_000 },
    );

    // Clip unit: VP9/WebM. The desktop client's Linux fallback, for the
    // (common) machines with no x264 encoder installed — same container as
    // the browser's VP8 but a different codec, and the worker must not care
    // which. 6 frames over 60s, the shipping desktop cadence.
    vp9Path = path.join(tmpDir, "unit_clip_vp9.webm");
    await execFileAsync(
      "ffmpeg",
      [
        "-f", "lavfi",
        "-i", "testsrc2=size=1620x1080:rate=1/10:duration=60",
        "-c:v", "libvpx-vp9",
        "-b:v", "320k",
        "-y", vp9Path,
      ],
      { timeout: 120_000 },
    );

    // Clip unit: H.264/MP4 at a DIFFERENT resolution — Safari's format,
    // and simulates a mid-session display change. 12 frames over 60s.
    mp4Path = path.join(tmpDir, "unit_clip.mp4");
    await execFileAsync(
      "ffmpeg",
      [
        "-f", "lavfi",
        "-i", "testsrc2=size=1920x1080:rate=1/5:duration=60",
        "-c:v", "libx264",
        "-pix_fmt", "yuv420p",
        "-y", mp4Path,
      ],
      { timeout: 120_000 },
    );
  }, 300_000);

  it("normalizes a JPEG still to exactly one second of 30fps video", async () => {
    const seg = await buildSegment(tmpDir, 0, jpegPath, "jpeg");
    expect(await probeFrameCount(seg)).toBe(SEGMENT_FPS);
  }, 120_000);

  it("normalizes a VP8 webm clip to exactly one second of 30fps video", async () => {
    const seg = await buildSegment(tmpDir, 1, webmPath, "webm");
    expect(await probeFrameCount(seg)).toBe(SEGMENT_FPS);
  }, 120_000);

  it("normalizes a VP9 webm clip to exactly one second of 30fps video", async () => {
    const seg = await buildSegment(tmpDir, 3, vp9Path, "webm");
    expect(await probeFrameCount(seg)).toBe(SEGMENT_FPS);
  }, 120_000);

  it("normalizes an H.264 mp4 clip to exactly one second of 30fps video", async () => {
    const seg = await buildSegment(tmpDir, 2, mp4Path, "mp4");
    expect(await probeFrameCount(seg)).toBe(SEGMENT_FPS);
  }, 120_000);

  it("stream-copy concatenates a mixed session into a coherent MP4", async () => {
    // Same order a real mixed session could produce: clip, jpeg fallback,
    // clip in another format/resolution, clip in another codec. Every
    // combination has to copy-concat, since one session can switch payload
    // between minutes (encoder failure, and now codec availability).
    const segments = [
      await buildSegment(tmpDir, 10, webmPath, "webm"),
      await buildSegment(tmpDir, 11, jpegPath, "jpeg"),
      await buildSegment(tmpDir, 12, mp4Path, "mp4"),
      await buildSegment(tmpDir, 13, vp9Path, "webm"),
    ];
    const listPath = path.join(tmpDir, "segments.txt");
    await fs.writeFile(
      listPath,
      segments.map((p) => `file '${p}'`).join("\n") + "\n",
    );
    const outPath = path.join(tmpDir, "timelapse.mp4");
    await execFileAsync(
      "ffmpeg",
      [
        "-f", "concat",
        "-safe", "0",
        "-i", listPath,
        "-c", "copy",
        "-movflags", "+faststart",
        "-y", outPath,
      ],
      { timeout: 120_000 },
    );

    // 3 units → exactly 3 seconds, 90 frames, one decodable H.264 stream.
    expect(await probeFrameCount(outPath)).toBe(segments.length * SEGMENT_FPS);
    expect(await probeDurationSeconds(outPath)).toBeCloseTo(segments.length, 1);

    // The copied stream must actually DECODE end to end (a bad splice can
    // still carry a plausible frame count).
    const { stderr } = await execFileAsync(
      "ffmpeg",
      ["-v", "error", "-i", outPath, "-f", "null", "-"],
      { timeout: 120_000 },
    );
    expect(stderr.trim()).toBe("");
  }, 300_000);

  describe("portrait canvas", () => {
    let portraitJpeg: string;
    let rotatedClip: string;

    beforeAll(async () => {
      portraitJpeg = path.join(tmpDir, "portrait.jpg");
      await execFileAsync("ffmpeg", [
        "-f", "lavfi", "-i", "color=red:size=360x640:rate=1",
        "-frames:v", "1", "-y", portraitJpeg,
      ]);
      rotatedClip = path.join(tmpDir, "rotated.mp4");
      await execFileAsync("ffmpeg", [
        "-i", mp4Path, "-c", "copy", "-metadata:s:v:0", "rotate=90",
        "-y", rotatedClip,
      ]);
      // ffmpeg 8 stopped translating the rotate tag into a display matrix.
      // Older CI versions accept the tag; newer ones expose this input option.
      const rotated = await probeDimensions(rotatedClip);
      const original = await probeDimensions(mp4Path);
      if (rotated.width === original.width) {
        await execFileAsync("ffmpeg", [
          "-display_rotation", "90", "-i", mp4Path,
          "-c", "copy", "-y", rotatedClip,
        ]);
      }
    });

    it("uses display dimensions for rotated phone clips", async () => {
      const original = await probeDimensions(mp4Path);
      expect(await probeDimensions(rotatedClip)).toEqual({
        width: original.height, height: original.width,
      });
      expect(await probeDimensions(portraitJpeg)).toEqual({ width: 360, height: 640 });
    });

    it.each(["preview", "publish"] as const)(
      "keeps portrait stills, rotated clips and later landscape captures compatible at %s quality",
      async (quality) => {
        const base = quality === "preview" ? 300 : 310;
        const segments = [
          await buildSegment(tmpDir, base, portraitJpeg, "jpeg", quality, true),
          await buildSegment(tmpDir, base + 1, rotatedClip, "mp4", quality, true),
          await buildSegment(tmpDir, base + 2, webmPath, "webm", quality, true),
        ];
        const expected = quality === "preview"
          ? { width: PREVIEW_HEIGHT, height: PREVIEW_WIDTH }
          : { width: 1080, height: 1920 };
        for (const segment of segments) {
          expect(await probeResolution(segment)).toEqual(expected);
          expect(await probeFrameCount(segment)).toBe(SEGMENT_FPS);
        }
        // A 9:16 red still fills the canvas: the left edge is red, not the
        // black pillarbox the old landscape-only compiler introduced.
        const { stdout: pixel } = await execFileAsync("ffmpeg", [
          "-v", "error", "-i", segments[0], "-vf", "crop=2:2:0:100",
          "-frames:v", "1", "-pix_fmt", "rgb24", "-f", "rawvideo", "-",
        ], { encoding: "buffer" });
        expect(pixel[0]).toBeGreaterThan(200);
        expect(pixel[1]).toBeLessThan(30);

        const list = path.join(tmpDir, `portrait_${quality}.txt`);
        const output = path.join(tmpDir, `portrait_${quality}.mp4`);
        await fs.writeFile(list, segments.map((p) => `file '${p}'`).join("\n"));
        await execFileAsync("ffmpeg", [
          "-f", "concat", "-safe", "0", "-i", list,
          "-c", "copy", "-y", output,
        ]);
        expect(await probeFrameCount(output)).toBe(3 * SEGMENT_FPS);
        const { stderr } = await execFileAsync("ffmpeg", [
          "-v", "error", "-i", output, "-f", "null", "-",
        ]);
        expect(stderr.trim()).toBe("");

        // Removing the portrait first minute must preserve the canvas even
        // though the surviving sources include a landscape capture.
        const cut = await cutVideoToKeptRanges(tmpDir, output, [{ start: 1, end: 3 }], true);
        expect(await probeResolution(cut)).toEqual(expected);
        expect(await probeFrameCount(cut)).toBe(2 * SEGMENT_FPS);
      }, 300_000,
    );
  });

  it("rejects an undecodable clip instead of emitting a bad segment", async () => {
    const garbagePath = path.join(tmpDir, "garbage.webm");
    await fs.writeFile(garbagePath, Buffer.from("not a webm file at all"));
    await expect(buildSegment(tmpDir, 99, garbagePath, "webm")).rejects.toThrow();
  }, 120_000);

  /**
   * The two-tier contract. The preview tier exists only to open the editor
   * quickly and is deleted at publish, so it may be small and cheap — but it
   * must still be one second on the same 30fps grid, because the editor maps
   * video seconds to capture units.
   */
  describe("preview tier", () => {
    it("keeps the 1-second grid while being smaller and cheaper", async () => {
      const publishSeg = await buildSegment(tmpDir, 200, mp4Path, "mp4", "publish");
      const previewSeg = await buildSegment(tmpDir, 201, mp4Path, "mp4", "preview");

      // Same timeline shape — this is what the cut UI depends on.
      expect(await probeFrameCount(previewSeg)).toBe(SEGMENT_FPS);
      expect(await probeFrameCount(publishSeg)).toBe(SEGMENT_FPS);

      // Reduced resolution is where the speed comes from.
      expect(await probeResolution(previewSeg)).toEqual({
        width: PREVIEW_WIDTH,
        height: PREVIEW_HEIGHT,
      });
      expect(await probeResolution(publishSeg)).toEqual({
        width: 1920,
        height: 1080,
      });

      // The preview must also be cheaper to MOVE, not just to encode: the
      // worker uploads it and the editor streams it back. This is what rules
      // out the very fastest presets, whose output is bigger than the 1080p
      // publish tier's — see segmentEncodeArgs.
      const previewBytes = (await fs.stat(previewSeg)).size;
      const publishBytes = (await fs.stat(publishSeg)).size;
      expect(previewBytes).toBeLessThan(publishBytes);
    }, 300_000);

    it("still decodes cleanly, so the editor can scrub it", async () => {
      const seg = await buildSegment(tmpDir, 202, mp4Path, "mp4", "preview");
      const { stderr } = await execFileAsync(
        "ffmpeg",
        ["-v", "error", "-i", seg, "-f", "null", "-"],
        { timeout: 120_000 },
      );
      expect(stderr.trim()).toBe("");
    }, 120_000);
  });
});

describe.skipIf(ffmpegAvailable)("segment pipeline (skipped)", () => {
  it("skipped because ffmpeg/ffprobe are not installed", () => {
    console.warn("ffmpeg not found — segment pipeline tests were skipped");
  });
});
