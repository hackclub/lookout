import { describe, expect, it, beforeAll } from "vitest";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import {
  buildSegment,
  cutVideoToKeptRanges,
  probeFrameCount,
  SEGMENT_FPS,
  type VideoMask,
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

async function frameHashes(filePath: string): Promise<string[]> {
  const { stdout } = await execFileAsync(
    "ffmpeg",
    ["-v", "error", "-i", filePath, "-an", "-f", "framemd5", "-"],
    { timeout: 180_000, maxBuffer: 64 * 1024 * 1024 },
  );
  return stdout
    .split("\n")
    .filter((l) => l && !l.startsWith("#"))
    .map((l) => l.trim().split(/[,\s]+/).pop() as string)
    .filter(Boolean);
}

const UNITS = 4;

describe.skipIf(!ffmpegAvailable)("maskVideo with cutVideoToKeptRanges", () => {
  let tmpDir: string;
  let originalPath: string;
  let originalHashes: string[];

  beforeAll(async () => {
    tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), "lookout-mask-"));

    const segments: string[] = [];
    for (let i = 0; i < UNITS; i++) {
      const jpeg = path.join(tmpDir, `unit_${i}.jpg`);
      // Use testsrc with time/clock display for distinct pixel content per frame
      await execFileAsync(
        "ffmpeg",
        [
          "-f", "lavfi",
          "-i", `testsrc=size=640x360:rate=1:duration=1`,
          "-frames:v", "1",
          "-y", jpeg,
        ],
        { timeout: 60_000 },
      );
      segments.push(await buildSegment(tmpDir, i, jpeg, "jpeg"));
    }
    const listPath = path.join(tmpDir, "segments.txt");
    await fs.writeFile(
      listPath,
      segments.map((p) => `file '${p}'`).join("\n") + "\n",
    );
    originalPath = path.join(tmpDir, "original.mp4");
    await execFileAsync(
      "ffmpeg",
      [
        "-f", "concat",
        "-safe", "0",
        "-i", listPath,
        "-c", "copy",
        "-movflags", "+faststart",
        "-y", originalPath,
      ],
      { timeout: 120_000 },
    );
    expect(await probeFrameCount(originalPath)).toBe(UNITS * SEGMENT_FPS);
    originalHashes = await frameHashes(originalPath);
  }, 300_000);

  it("re-encodes video with pixelated mask box over specified duration", async () => {
    const dir = await fs.mkdtemp(path.join(os.tmpdir(), "lookout-mask-a-"));
    const masks: VideoMask[] = [
      {
        startSec: 1,
        endSec: 3,
        x: 0.2,
        y: 0.2,
        width: 0.4,
        height: 0.4,
      },
    ];

    const edited = await cutVideoToKeptRanges(
      dir,
      originalPath,
      [{ start: 0, end: UNITS }],
      true,
      masks,
    );

    const frameCount = await probeFrameCount(edited);
    expect(frameCount).toBe(UNITS * SEGMENT_FPS);

    const editedHashes = await frameHashes(edited);
    expect(editedHashes.length).toBe(originalHashes.length);

    // Frames during unit 1 & 2 (seconds 1 to 3) MUST have altered hashes because of pixelation
    const middleFrameIndex = 1 * SEGMENT_FPS + 5;
    expect(editedHashes[middleFrameIndex]).not.toBe(originalHashes[middleFrameIndex]);
  }, 120_000);

  it("supports multiple and simultaneous mask boxes", async () => {
    const dir = await fs.mkdtemp(path.join(os.tmpdir(), "lookout-mask-b-"));
    const masks: VideoMask[] = [
      {
        startSec: 1,
        endSec: 2,
        x: 0.1,
        y: 0.1,
        width: 0.3,
        height: 0.3,
      },
      {
        startSec: 1,
        endSec: 2,
        x: 0.6,
        y: 0.6,
        width: 0.3,
        height: 0.3,
      },
    ];

    const edited = await cutVideoToKeptRanges(
      dir,
      originalPath,
      [{ start: 0, end: UNITS }],
      true,
      masks,
    );

    const frameCount = await probeFrameCount(edited);
    expect(frameCount).toBe(UNITS * SEGMENT_FPS);
  }, 120_000);

  it("combines cuts and masks correctly", async () => {
    const dir = await fs.mkdtemp(path.join(os.tmpdir(), "lookout-mask-c-"));
    // Cut unit 0, keep units 1..4 (3 units = 90 frames), mask unit 1
    const masks: VideoMask[] = [
      {
        startSec: 1,
        endSec: 2,
        x: 0.2,
        y: 0.2,
        width: 0.5,
        height: 0.5,
      },
    ];

    const edited = await cutVideoToKeptRanges(
      dir,
      originalPath,
      [{ start: 1, end: 4 }],
      true,
      masks,
    );

    const frameCount = await probeFrameCount(edited);
    expect(frameCount).toBe(3 * SEGMENT_FPS);
  }, 120_000);
});
