/**
 * Vitest setup. Runs before any test imports, so the env vars and module
 * mocks here are visible to all subsequent imports.
 *
 * - Forces DATABASE_URL to point at the docker test PG (port 5434) unless
 *   already set in the environment.
 * - Stubs the R2 credentials with non-empty placeholders so the config
 *   module doesn't throw on import.
 * - Replaces the AWS S3 client + pg-boss queue with in-memory test doubles
 *   so routes never touch external services.
 */
import { vi } from "vitest";

process.env.DATABASE_URL =
  process.env.TEST_DATABASE_URL ??
  process.env.DATABASE_URL ??
  "postgresql://lookout:lookout@localhost:5434/lookout_test";

process.env.R2_ACCOUNT_ID = process.env.R2_ACCOUNT_ID ?? "test-account";
process.env.R2_ACCESS_KEY_ID = process.env.R2_ACCESS_KEY_ID ?? "test-key";
process.env.R2_SECRET_ACCESS_KEY =
  process.env.R2_SECRET_ACCESS_KEY ?? "test-secret";
process.env.R2_BUCKET_NAME = process.env.R2_BUCKET_NAME ?? "test-bucket";
process.env.BASE_URL = process.env.BASE_URL ?? "http://localhost:3000";
process.env.GLOBAL_API_KEY =
  process.env.GLOBAL_API_KEY ?? "test-global-key";
process.env.ADMIN_USERNAME = process.env.ADMIN_USERNAME ?? "admin";
process.env.ADMIN_PASSWORD = process.env.ADMIN_PASSWORD ?? "admin-secret";

// AWS S3 / R2 — make every HEAD / PUT succeed silently. The integration
// tests don't care about R2; they care about session + screenshot state.
vi.mock("@aws-sdk/client-s3", async (orig) => {
  const real: any = await orig();
  class FakeS3Client {
    async send(command: any) {
      const name = command?.constructor?.name ?? "";
      if (name === "HeadObjectCommand") {
        return { ContentType: "image/jpeg", ContentLength: 1024 };
      }
      if (name === "PutObjectCommand" || name === "DeleteObjectCommand" || name === "GetObjectCommand") {
        return {};
      }
      return {};
    }
  }
  return { ...real, S3Client: FakeS3Client };
});

// Presigner — just echo a fake URL so the route doesn't have to call out.
vi.mock("@aws-sdk/s3-request-presigner", () => ({
  getSignedUrl: vi.fn(async () => "https://r2.test/fake-upload-url"),
}));

// pg-boss — `boss.send`, `boss.work`, `boss.schedule`, `boss.createQueue`,
// `boss.start`, `boss.stop` all become no-ops. No background jobs run.
vi.mock("pg-boss", () => {
  class FakePgBoss {
    async start() {}
    async stop() {}
    async send() {
      return "fake-job-id";
    }
    async work() {}
    async schedule() {}
    async createQueue() {}
  }
  return { default: FakePgBoss };
});
