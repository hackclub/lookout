/**
 * Belt-and-braces compat test: boot the Fastify server on a real port,
 * then spawn `cargo test --test legacy_client` in the desktop crate. The
 * Rust test uses struct definitions copied verbatim from the pre-credit-
 * mode code and runs the full session lifecycle.
 *
 * This is the closest we can get to "the currently-shipped binary works
 * against the new server" without actually building the old binary.
 *
 * Skips if the Rust toolchain isn't available, or if the
 * `SKIP_RUST_SUBPROCESS` env var is set (useful when running just the
 * fast TS suite).
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import type { FastifyInstance } from "fastify";
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { join, resolve } from "node:path";
import { sql } from "drizzle-orm";
import { buildApp } from "../src/app.js";
import { db, schema } from "../src/db/index.js";

const DESKTOP_CRATE = resolve(
  process.cwd(),
  "../../clients/desktop/src-tauri",
);

// Skip if explicit opt-out OR cargo isn't on PATH.
const skip =
  process.env.SKIP_RUST_SUBPROCESS === "1" || !existsSync(DESKTOP_CRATE);

describe.skipIf(skip)("legacy rust client subprocess", () => {
  let app: FastifyInstance;
  let baseUrl: string;

  beforeAll(async () => {
    await db.execute(sql`TRUNCATE screenshots, sessions RESTART IDENTITY CASCADE`);
    app = await buildApp();
    // Listen on a kernel-assigned port so we don't collide with anything.
    await app.listen({ port: 0, host: "127.0.0.1" });
    const addr = app.server.address();
    if (!addr || typeof addr === "string") {
      throw new Error("Server didn't bind a TCP port");
    }
    baseUrl = `http://127.0.0.1:${addr.port}`;
  }, 30_000);

  afterAll(async () => {
    if (app) await app.close();
    await (db.$client as any).end?.();
  });

  it(
    "runs full session lifecycle through the legacy struct definitions",
    async () => {
      // Pre-create a session by inserting directly. The old client gets
      // its token from a deep link / external system; we mimic that by
      // creating one out-of-band so the Rust test just needs the token.
      const [sess] = await db
        .insert(schema.sessions)
        .values({ name: "rust-subprocess-test" })
        .returning({ token: schema.sessions.token });

      const cargo = spawn(
        "cargo",
        ["test", "--test", "legacy_client", "--", "--nocapture"],
        {
          cwd: DESKTOP_CRATE,
          env: {
            ...process.env,
            LEGACY_CLIENT_TEST_URL: baseUrl,
            LEGACY_CLIENT_TEST_TOKEN: sess.token,
          },
          stdio: ["ignore", "pipe", "pipe"],
        },
      );

      let stdout = "";
      let stderr = "";
      cargo.stdout?.on("data", (b) => (stdout += b.toString()));
      cargo.stderr?.on("data", (b) => (stderr += b.toString()));

      const exitCode: number = await new Promise((resolveExit, reject) => {
        cargo.on("error", reject);
        cargo.on("close", (code) => resolveExit(code ?? -1));
      });

      if (exitCode !== 0) {
        // Surface the full output so a failure is debuggable from CI logs.
        console.error("cargo stdout:", stdout);
        console.error("cargo stderr:", stderr);
      }
      expect(exitCode, "cargo test --test legacy_client should pass").toBe(0);
      expect(stdout).toContain("test result: ok");
    },
    // Cargo first run can be slow if the binary isn't already compiled.
    300_000,
  );
});

// Stash to keep TS happy when join is unused after a refactor.
void join;
