import { describe, expect, it } from "vitest";
import type { SessionSummary } from "@lookout/shared";
import { sortNewestFirst } from "./useGallery";

const s = (token: string, createdAt: string) => ({ token, createdAt }) as SessionSummary;

describe("sortNewestFirst", () => {
  it("orders merged batch chunks newest first", () => {
    // Each chunk is newest-first on its own; the second holds newer sessions.
    const merged = [
      s("b", "2025-03-01T00:00:00.000Z"),
      s("a", "2025-01-01T00:00:00.000Z"),
      s("d", "2026-11-09T12:00:00.000Z"),
      s("c", "2026-02-01T00:00:00.000Z"),
    ];
    expect(sortNewestFirst(merged).map((x) => x.token)).toEqual(["d", "c", "b", "a"]);
    expect(merged[0].token).toBe("b"); // input untouched
  });
});
