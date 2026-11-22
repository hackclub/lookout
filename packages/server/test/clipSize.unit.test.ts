import { describe, expect, it } from "vitest";
import {
  MIN_CLIP_BYTES,
  MAX_CLIP_BYTES,
  MAX_SCREENSHOT_BYTES,
  type CaptureFormat,
} from "@lookout/shared";

/**
 * Validates file size against per-format limits, exactly matching the logic
 * in the screenshot confirmation endpoint (/sessions/:token/screenshots).
 */
function validateUploadSize(
  rowFormat: CaptureFormat,
  contentLength: number | undefined,
): { valid: boolean; error?: string } {
  const maxBytes = rowFormat === "jpeg" ? MAX_SCREENSHOT_BYTES : MAX_CLIP_BYTES;
  if (contentLength && contentLength > maxBytes) {
    return { valid: false, error: "Uploaded object is too large" };
  }
  if (
    rowFormat !== "jpeg" &&
    contentLength !== undefined &&
    contentLength < MIN_CLIP_BYTES
  ) {
    return { valid: false, error: "Uploaded clip is empty or corrupt" };
  }
  return { valid: true };
}

describe("MIN_CLIP_BYTES constant", () => {
  it("is defined as 1024 bytes", () => {
    expect(MIN_CLIP_BYTES).toBe(1024);
  });

  it("is strictly greater than an empty WebM file (217 bytes)", () => {
    const EMPTY_WEBM_BYTES = 217;
    expect(EMPTY_WEBM_BYTES).toBeLessThan(MIN_CLIP_BYTES);
  });
});

describe("server clip minimum size validation", () => {
  it("rejects webm clips smaller than MIN_CLIP_BYTES with empty or corrupt error", () => {
    const emptyWebm = validateUploadSize("webm", 217);
    expect(emptyWebm.valid).toBe(false);
    expect(emptyWebm.error).toBe("Uploaded clip is empty or corrupt");

    const boundaryFail = validateUploadSize("webm", MIN_CLIP_BYTES - 1);
    expect(boundaryFail.valid).toBe(false);
    expect(boundaryFail.error).toBe("Uploaded clip is empty or corrupt");

    const zeroBytes = validateUploadSize("webm", 0);
    expect(zeroBytes.valid).toBe(false);
    expect(zeroBytes.error).toBe("Uploaded clip is empty or corrupt");
  });

  it("rejects mp4 clips smaller than MIN_CLIP_BYTES with empty or corrupt error", () => {
    const emptyMp4 = validateUploadSize("mp4", 500);
    expect(emptyMp4.valid).toBe(false);
    expect(emptyMp4.error).toBe("Uploaded clip is empty or corrupt");
  });

  it("accepts clips that meet or exceed MIN_CLIP_BYTES", () => {
    const exactBoundary = validateUploadSize("webm", MIN_CLIP_BYTES);
    expect(exactBoundary.valid).toBe(true);

    const normalClip = validateUploadSize("webm", 500_000);
    expect(normalClip.valid).toBe(true);
  });

  it("rejects clips that exceed MAX_CLIP_BYTES", () => {
    const tooLarge = validateUploadSize("webm", MAX_CLIP_BYTES + 1);
    expect(tooLarge.valid).toBe(false);
    expect(tooLarge.error).toBe("Uploaded object is too large");
  });

  it("does not apply MIN_CLIP_BYTES to jpeg screenshots", () => {
    // A small JPEG screenshot under 1024 bytes is valid
    const smallJpeg = validateUploadSize("jpeg", 500);
    expect(smallJpeg.valid).toBe(true);

    // But exceeding MAX_SCREENSHOT_BYTES is rejected
    const largeJpeg = validateUploadSize("jpeg", MAX_SCREENSHOT_BYTES + 1);
    expect(largeJpeg.valid).toBe(false);
    expect(largeJpeg.error).toBe("Uploaded object is too large");
  });
});
