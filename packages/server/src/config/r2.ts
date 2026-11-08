import { S3Client } from "@aws-sdk/client-s3";

const R2_ACCOUNT_ID = process.env.R2_ACCOUNT_ID;
const R2_ACCESS_KEY_ID = process.env.R2_ACCESS_KEY_ID;
const R2_SECRET_ACCESS_KEY = process.env.R2_SECRET_ACCESS_KEY;
const R2_BUCKET_NAME = process.env.R2_BUCKET_NAME;

if (!R2_ACCOUNT_ID) {
  throw new Error("R2_ACCOUNT_ID environment variable is required but not set");
}
if (!R2_ACCESS_KEY_ID) {
  throw new Error("R2_ACCESS_KEY_ID environment variable is required but not set");
}
if (!R2_SECRET_ACCESS_KEY) {
  throw new Error("R2_SECRET_ACCESS_KEY environment variable is required but not set");
}
if (!R2_BUCKET_NAME) {
  throw new Error("R2_BUCKET_NAME environment variable is required but not set");
}

// Local development escape hatch: point the S3 client (and therefore the
// presigned URLs it hands clients) at any S3-compatible endpoint instead of
// real R2. Unset in production, where the account-derived R2 host is used.
const R2_ENDPOINT =
  process.env.R2_ENDPOINT || `https://${R2_ACCOUNT_ID}.r2.cloudflarestorage.com`;

export const r2Client = new S3Client({
  region: "auto",
  endpoint: R2_ENDPOINT,
  credentials: {
    accessKeyId: R2_ACCESS_KEY_ID,
    secretAccessKey: R2_SECRET_ACCESS_KEY,
  },
  forcePathStyle: true,
  requestChecksumCalculation: "WHEN_REQUIRED",
  responseChecksumValidation: "WHEN_REQUIRED",
});

export const R2_BUCKET = R2_BUCKET_NAME;

/**
 * Public URL for an object when the bucket is fronted by R2_PUBLIC_DOMAIN,
 * or null when it isn't and the caller must presign instead.
 *
 * Presigned URLs only work on the account's r2.cloudflarestorage.com
 * endpoint — Cloudflare doesn't honour them on custom domains — so a public
 * domain is the only way to hand out a stable, cacheable URL for published
 * media. Read at call time rather than module load so tests can exercise
 * both modes against the same app.
 */
export function publicObjectUrl(key: string): string | null {
  const domain = (process.env.R2_PUBLIC_DOMAIN || "")
    .replace(/^https?:\/\//, "")
    .replace(/\/+$/, "");
  return domain ? `https://${domain}/${key}` : null;
}
