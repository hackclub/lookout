import { describe, it, expect } from "vitest";
import { desktopHandoffUrl, isValidToken, readUrlOptions } from "./token.js";

const TOKEN = "a".repeat(64);

describe("isValidToken", () => {
  it("accepts a 64-char hex token in either case", () => {
    expect(isValidToken(TOKEN)).toBe(true);
    expect(isValidToken("A1B2".repeat(16))).toBe(true);
  });

  it("rejects anything else", () => {
    expect(isValidToken(null)).toBe(false);
    expect(isValidToken("")).toBe(false);
    expect(isValidToken("a".repeat(63))).toBe(false);
    expect(isValidToken("a".repeat(65))).toBe(false);
    expect(isValidToken("g".repeat(64))).toBe(false);
  });
});

describe("readUrlOptions", () => {
  it("reads a valid token", () => {
    expect(readUrlOptions(`?token=${TOKEN}`).token).toBe(TOKEN);
  });

  it("treats a malformed token as no token at all", () => {
    // A truncated copy/paste should land on "this needs a session link",
    // not on a recorder that 404s at the first upload.
    expect(readUrlOptions("?token=nope").token).toBeNull();
    expect(readUrlOptions("").token).toBeNull();
  });

  it("defaults editing on, and honours ?edit=false", () => {
    expect(readUrlOptions(`?token=${TOKEN}`).editing).toBe(true);
    expect(readUrlOptions(`?token=${TOKEN}&edit=false`).editing).toBe(false);
    expect(readUrlOptions(`?token=${TOKEN}&edit=true`).editing).toBe(true);
  });

  it("passes ?app= through, ignoring blank values", () => {
    expect(readUrlOptions(`?token=${TOKEN}&app=Fallout`).appName).toBe("Fallout");
    expect(readUrlOptions(`?token=${TOKEN}&app=%20%20`).appName).toBeUndefined();
    expect(readUrlOptions(`?token=${TOKEN}`).appName).toBeUndefined();
  });
});

describe("desktopHandoffUrl", () => {
  it("matches the shape the desktop app parses", () => {
    // Contract with clients/desktop/src-tauri/src/lib.rs — changing this
    // string breaks the handoff for every already-installed copy.
    expect(desktopHandoffUrl(TOKEN)).toBe(`lookout://session/?token=${TOKEN}`);
  });
});
