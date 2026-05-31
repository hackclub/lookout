import { useState, useEffect } from "react";
import { api } from "../api/client.js";
import { formatTime } from "@lookout/react";
import { VideoPlayer } from "@lookout/react";
import type { SessionStatus } from "@lookout/shared";

interface ResultProps {
  status: SessionStatus;
  trackedSeconds: number;
}

export function Result({ status, trackedSeconds }: ResultProps) {
  const [videoUrl, setVideoUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (status === "complete") {
      api
        .getVideo()
        .then((data) => {
          if (data.videoUrl && !data.videoUrl.startsWith("https://")) {
            throw new Error("Invalid video URL: must be HTTPS.");
          }
          setVideoUrl(data.videoUrl);
        })
        .catch((err) =>
          setError(err instanceof Error ? err.message : "Failed to load video"),
        );
    }
  }, [status]);

  if (status === "stopped" || status === "compiling") {
    return (
      <div style={styles.container}>
        <div style={styles.spinner} />
        <h2 style={styles.title}>Compiling your timelapse...</h2>
        <p style={styles.subtitle}>
          This may take a moment. Tracked time: {formatTime(trackedSeconds)}
        </p>
      </div>
    );
  }

  if (status === "failed") {
    return (
      <div style={styles.container}>
        <h2 style={{ ...styles.title, color: "#ef4444" }}>
          Compilation failed
        </h2>
        <p style={styles.subtitle}>
          Something went wrong while creating your timelapse. The team has been
          notified and it will be retried automatically.
        </p>
      </div>
    );
  }

  if (status === "complete") {
    return (
      <div style={styles.container}>
        <h2 style={styles.title}>Your timelapse is ready!</h2>
        <p style={styles.subtitle}>
          Tracked time: {formatTime(trackedSeconds)}
        </p>
        {error && <p style={{ color: "#ef4444" }}>{error}</p>}
        {videoUrl && (
          <div style={styles.videoContainer}>
            <VideoPlayer src={videoUrl} />
          </div>
        )}
      </div>
    );
  }

  return null;
}

const styles: Record<string, React.CSSProperties> = {
  container: {
    width: "100%",
    margin: "40px auto",
    padding: 24,
    textAlign: "center",
  },
  title: {
    fontSize: 24,
    fontWeight: 700,
    color: "#fff",
    marginBottom: 8,
  },
  subtitle: {
    fontSize: 16,
    color: "#888",
    marginBottom: 24,
  },
  videoContainer: {
    width: "100%",
    borderRadius: 8,
    background: "#000",
    overflow: "hidden",
  },
  spinner: {
    width: 40,
    height: 40,
    border: "4px solid #333",
    borderTop: "4px solid #3b82f6",
    borderRadius: "50%",
    margin: "0 auto 16px",
    animation: "spin 1s linear infinite",
  },
};
