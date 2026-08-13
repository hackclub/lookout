import { describe, it, expect } from "vitest";
import { displayName } from "./sessionName.js";

describe("displayName", () => {
  it("keeps a name the program actually chose", () => {
    expect(displayName("Building my Neopixel clock")).toBe(
      "Building my Neopixel clock",
    );
  });

  it("drops the server's untitled-<date> default", () => {
    // What every session created without a name is called.
    expect(displayName("untitled-2026-08-14")).toBeNull();
    expect(displayName("UNTITLED-2026-01-02")).toBeNull();
  });

  it("keeps a real name that merely starts with untitled", () => {
    expect(displayName("untitled-song-project")).toBe("untitled-song-project");
  });

  it("treats empty and whitespace as no name", () => {
    expect(displayName("")).toBeNull();
    expect(displayName("   ")).toBeNull();
    expect(displayName(null)).toBeNull();
    expect(displayName(undefined)).toBeNull();
  });

  it("trims", () => {
    expect(displayName("  Clock build  ")).toBe("Clock build");
  });
});
