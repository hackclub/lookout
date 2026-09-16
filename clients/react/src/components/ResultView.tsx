import React, { useState, useEffect } from "react";
import { useLookoutContext } from "../LookoutProvider.js";
import { ProcessingState } from "./ProcessingState.js";
import type { RecorderStatus } from "../types.js";

export interface ResultViewProps {
  status: RecorderStatus;
  trackedSeconds: number;
}

export function ResultView({ status, trackedSeconds }: ResultViewProps) {
  const { client, config } = useLookoutContext();
  const [videoUrl, setVideoUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (status === "complete") {
      client
        .getVideo()
        .then((data) => {
          if (
            data.videoUrl &&
            !data.videoUrl.startsWith("https://") &&
            !data.videoUrl.startsWith("http://localhost") &&
            !data.videoUrl.startsWith("http://127.0.0.1")
          ) {
            throw new Error("Invalid video URL: must be HTTPS.");
          }
          setVideoUrl(data.videoUrl);
          config.callbacks.onComplete?.({ videoUrl: data.videoUrl });
        })
        .catch((err) =>
          setError(err instanceof Error ? err.message : "Failed to load video"),
        );
    }
  }, [status, client, config.callbacks]);

  return (
    <ProcessingState
      status={status}
      trackedSeconds={trackedSeconds}
      videoUrl={videoUrl}
      error={error}
    />
  );
}
