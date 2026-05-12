import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    globals: false,
    setupFiles: ["./test/setup.ts"],
    // Integration tests start a real Postgres connection — keep them serial
    // so we don't deadlock when concurrent suites both truncate tables.
    fileParallelism: false,
    testTimeout: 15_000,
  },
});
