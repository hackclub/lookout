import { describe, expect, it } from "vitest";
import { redactDeep, redactTokens, redactedReqSerializer } from "./redact.js";

const TOKEN = "a".repeat(32) + "0123456789ABCDEF0123456789abcdef";

describe("redactTokens", () => {
  it("scrubs tokens from paths and query strings", () => {
    expect(redactTokens(`/api/sessions/${TOKEN}/upload-url?x=1`)).toBe(
      "/api/sessions/[token]/upload-url?x=1",
    );
    expect(redactTokens(`/session?token=${TOKEN}`)).toBe("/session?token=[token]");
  });

  it("leaves session ids alone", () => {
    const id = "/api/media/3f2b8c1e-1d2a-4c5b-9e8f-0a1b2c3d4e5f/video.mp4";
    expect(redactTokens(id)).toBe(id);
  });
});

describe("redactDeep", () => {
  it("scrubs a Sentry-shaped event everywhere a URL can surface", () => {
    const event = {
      transaction: `GET /api/sessions/${TOKEN}`,
      request: { url: `https://lookout/api/sessions/${TOKEN}`, query_string: `token=${TOKEN}` },
      spans: [{ description: `GET /api/sessions/${TOKEN}`, data: { "url.full": `/x/${TOKEN}` } }],
      breadcrumbs: [{ message: TOKEN }],
      level: 2,
    };
    const out = JSON.stringify(redactDeep(event));
    expect(out).not.toContain(TOKEN);
    expect(redactDeep(event).level).toBe(2);
  });
});

describe("redactedReqSerializer", () => {
  it("logs the URL without the token", () => {
    const out = redactedReqSerializer({ method: "GET", url: `/api/sessions/${TOKEN}` });
    expect(out.url).toBe("/api/sessions/[token]");
  });
});
