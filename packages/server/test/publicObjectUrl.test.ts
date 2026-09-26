/**
 * Unit tests for the public-domain URL helper. No database needed.
 *
 * The helper is the single point that decides whether published media is
 * handed out on R2_PUBLIC_DOMAIN or has to be presigned, so its edge cases
 * (unset, empty, sloppy values with a scheme or trailing slash) are pinned
 * here rather than re-derived in every route test.
 */
import { afterEach, describe, expect, it } from "vitest";
import { publicObjectUrl } from "../src/config/r2.js";

const saved = process.env.R2_PUBLIC_DOMAIN;
afterEach(() => {
  if (saved === undefined) delete process.env.R2_PUBLIC_DOMAIN;
  else process.env.R2_PUBLIC_DOMAIN = saved;
});

describe("publicObjectUrl", () => {
  it("is null when no public domain is configured", () => {
    delete process.env.R2_PUBLIC_DOMAIN;
    expect(publicObjectUrl("timelapses/x/edited.mp4")).toBeNull();
  });

  it("treats an empty value as unset", () => {
    process.env.R2_PUBLIC_DOMAIN = "";
    expect(publicObjectUrl("timelapses/x/edited.mp4")).toBeNull();
  });

  it("builds the URL from a bare hostname", () => {
    process.env.R2_PUBLIC_DOMAIN = "cdn.example.com";
    expect(publicObjectUrl("timelapses/x/edited.mp4")).toBe(
      "https://cdn.example.com/timelapses/x/edited.mp4",
    );
  });

  it("tolerates a scheme and trailing slash in the configured value", () => {
    process.env.R2_PUBLIC_DOMAIN = "https://cdn.example.com/";
    expect(publicObjectUrl("timelapses/x/edited.mp4")).toBe(
      "https://cdn.example.com/timelapses/x/edited.mp4",
    );
  });
});
