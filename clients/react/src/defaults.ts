import {
  SCREENSHOT_INTERVAL_MS,
  JPEG_QUALITY,
  MAX_WIDTH,
  MAX_HEIGHT,
  MAX_UPLOAD_RETRIES,
  UPLOAD_RETRY_DELAYS_MS,
  MAX_PENDING_BUFFER,
} from "@lookout/shared";
import type { LookoutConfig, ResolvedConfig } from "./types.js";

export function resolveConfig(config: LookoutConfig): ResolvedConfig {
  return {
    token: config.token,
    apiBaseUrl: config.apiBaseUrl ?? "",
    capture: {
      intervalMs: config.capture?.intervalMs ?? SCREENSHOT_INTERVAL_MS,
      jpegQuality: config.capture?.jpegQuality ?? JPEG_QUALITY,
      maxWidth: config.capture?.maxWidth ?? MAX_WIDTH,
      maxHeight: config.capture?.maxHeight ?? MAX_HEIGHT,
      displayMediaConstraints: config.capture?.displayMediaConstraints,
      mode: config.capture?.mode ?? "screen",
      camera: {
        deviceId: config.capture?.camera?.deviceId,
        userMediaConstraints: config.capture?.camera?.userMediaConstraints,
      },
    },
    retry: {
      maxRetries: config.retry?.maxRetries ?? MAX_UPLOAD_RETRIES,
      retryDelays: config.retry?.retryDelays ?? UPLOAD_RETRY_DELAYS_MS,
      maxPendingBuffer: config.retry?.maxPendingBuffer ?? MAX_PENDING_BUFFER,
    },
    callbacks: config.callbacks ?? {},
    statusPollIntervalMs: config.statusPollIntervalMs ?? 3000,
    autoStart: config.autoStart ?? false,
    appName: config.appName,
  };
}
