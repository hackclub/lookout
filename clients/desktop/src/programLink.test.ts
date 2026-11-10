// @vitest-environment happy-dom
import { describe, expect, it } from "vitest";
import {
  base64url,
  buildPairPageUrl,
  consentPageMissing,
  isAcceptableEndpoint,
  isLinkable,
  parsePairCallback,
  sha256,
} from "./programLink.js";

const hex = (b: Uint8Array) =>
  Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

describe("sha256", () => {
  // FIPS 180-2 test vectors.
  it("hashes the empty string", () => {
    expect(hex(sha256(new Uint8Array(0)))).toBe(
      "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    );
  });

  it('hashes "abc"', () => {
    expect(hex(sha256(new TextEncoder().encode("abc")))).toBe(
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    );
  });

  it("hashes the two-block vector", () => {
    expect(
      hex(sha256(new TextEncoder().encode(
        "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
      ))),
    ).toBe("248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
  });

  it("hashes input spanning a block boundary (63/64/65 bytes)", () => {
    // Cross-checked against `printf 'a%.0s' {1..N} | sha256sum`.
    const a = (n: number) => new TextEncoder().encode("a".repeat(n));
    expect(hex(sha256(a(63)))).toBe(
      "7d3e74a05d7db15bce4ad9ec0658ea98e3f06eeecf16b4c6fff2da457ddc2f34",
    );
    expect(hex(sha256(a(64)))).toBe(
      "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb",
    );
    expect(hex(sha256(a(65)))).toBe(
      "635361c48bb9eab14198e76ea8ab7f1a41685d6ad62aa9146d301d4f17eb0ae0",
    );
  });
});

describe("base64url", () => {
  it("encodes without padding or +/", () => {
    // 0xfb 0xef 0xff encodes to "++//" in plain base64.
    expect(base64url(new Uint8Array([0xfb, 0xef, 0xff]))).toBe("--__");
    expect(base64url(new TextEncoder().encode("f"))).toBe("Zg"); // "Zg==" padded
  });
});

describe("isAcceptableEndpoint", () => {
  it("accepts https anywhere", () => {
    expect(isAcceptableEndpoint("https://lapse.hackclub.com/desktop/pair")).toBe(true);
  });
  it("accepts http only on localhost", () => {
    expect(isAcceptableEndpoint("http://localhost:3000/pair")).toBe(true);
    expect(isAcceptableEndpoint("http://127.0.0.1:3000/pair")).toBe(true);
    expect(isAcceptableEndpoint("http://lapse.hackclub.com/pair")).toBe(false);
  });
  it("rejects garbage and other schemes", () => {
    expect(isAcceptableEndpoint("")).toBe(false);
    expect(isAcceptableEndpoint(null)).toBe(false);
    expect(isAcceptableEndpoint(undefined)).toBe(false);
    expect(isAcceptableEndpoint("not a url")).toBe(false);
    expect(isAcceptableEndpoint("ftp://x.example/pair")).toBe(false);
  });
});

describe("isLinkable", () => {
  const base = { name: "lapse", newSessionUrl: "https://lapse.hackclub.com/new" };
  it("requires both endpoints", () => {
    expect(isLinkable({ ...base })).toBe(false);
    expect(isLinkable({ ...base, pairUrl: "https://x.example/pair" })).toBe(false);
    expect(isLinkable({ ...base, startUrl: "https://x.example/start" })).toBe(false);
    expect(
      isLinkable({
        ...base,
        pairUrl: "https://x.example/pair",
        startUrl: "https://x.example/start",
      }),
    ).toBe(true);
  });
  it("rejects a pair of endpoints when either is http off-localhost", () => {
    expect(
      isLinkable({
        ...base,
        pairUrl: "https://x.example/pair",
        startUrl: "http://x.example/start",
      }),
    ).toBe(false);
  });
});

describe("consentPageMissing", () => {
  it("treats a missing or non-GET route as no consent page", () => {
    expect(consentPageMissing(404)).toBe(true);
    expect(consentPageMissing(405)).toBe(true);
  });
  it("leaves login redirects, auth challenges, and validation errors to the browser", () => {
    for (const status of [200, 204, 301, 302, 400, 401, 403, 500]) {
      expect(consentPageMissing(status)).toBe(false);
    }
  });
});

describe("buildPairPageUrl", () => {
  it("appends challenge/state/device, keeping existing query params", () => {
    const url = buildPairPageUrl("https://x.example/pair?flow=desktop", {
      challenge: "CH",
      state: "ST",
      device: "Lookout Desktop (macOS)",
    });
    const parsed = new URL(url);
    expect(parsed.searchParams.get("flow")).toBe("desktop");
    expect(parsed.searchParams.get("challenge")).toBe("CH");
    expect(parsed.searchParams.get("state")).toBe("ST");
    expect(parsed.searchParams.get("device")).toBe("Lookout Desktop (macOS)");
  });
});

describe("parsePairCallback", () => {
  it("parses a pairing callback", () => {
    expect(parsePairCallback("lookout://pair?code=abc123&state=st-1")).toEqual({
      code: "abc123",
      state: "st-1",
    });
    expect(parsePairCallback("lookout://pair/?code=abc&state=st")).toEqual({
      code: "abc",
      state: "st",
    });
  });

  it("ignores session deep links and junk", () => {
    const token = "a".repeat(64);
    expect(parsePairCallback(`lookout://session/?token=${token}`)).toBeNull();
    expect(parsePairCallback("lookout://pairing?code=a&state=b")).toBeNull();
    expect(parsePairCallback("lookout://pair")).toBeNull(); // no code/state
    expect(parsePairCallback("lookout://pair?code=a")).toBeNull(); // no state
    expect(parsePairCallback("not a url")).toBeNull();
  });

  it("bounds code/state size", () => {
    const big = "x".repeat(600);
    expect(parsePairCallback(`lookout://pair?code=${big}&state=s`)).toBeNull();
    expect(parsePairCallback(`lookout://pair?code=c&state=${big}`)).toBeNull();
  });

  it("never yields a session token from a pair link", () => {
    // A pair callback must not be treated as a session handoff even if a
    // token param is smuggled in — App handles pair links before extractToken.
    const smuggled = `lookout://pair?code=a&state=b&token=${"a".repeat(64)}`;
    expect(parsePairCallback(smuggled)).toEqual({ code: "a", state: "b" });
  });
});
