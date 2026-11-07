import React, { useEffect, useRef, useState } from "react";
import { createPlayer } from "@videojs/react";
import { VideoSkin, Video, videoFeatures } from "@videojs/react/video";
import "@videojs/react/video/skin.css";
import "./VideoPlayer.css";

interface VideoPlayerProps {
  src: string;
}

type PlaybackError =
  | { kind: "codec"; detail: string }
  | { kind: "network"; detail: string }
  | { kind: "unknown"; detail: string };

const { Player } = createPlayer({ features: videoFeatures });

export function VideoPlayer({ src }: VideoPlayerProps) {
  const [platform, setPlatform] = useState("");
  const [error, setError] = useState<PlaybackError | null>(null);
  const wrapperRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (typeof navigator !== "undefined") {
      const ua = navigator.userAgent.toLowerCase();
      if (ua.includes("linux") && !ua.includes("android")) {
        setPlatform("linux");
      } else if (ua.includes("win")) {
        setPlatform("windows");
      } else if (ua.includes("mac")) {
        setPlatform("mac");
      }
    }
  }, []);

  // Listen for playback errors on the underlying <video> element so we can
  // tell the user it's a system-level codec issue, not a corrupt timelapse.
  useEffect(() => {
    setError(null);
    const wrapper = wrapperRef.current;
    if (!wrapper) return;

    const onVideoError = (event: Event) => {
      const el = event.target as HTMLVideoElement | null;
      const code = el?.error?.code;
      // 4 = MEDIA_ERR_SRC_NOT_SUPPORTED, 3 = MEDIA_ERR_DECODE
      if (code === 4 || code === 3) {
        setError({
          kind: "codec",
          detail:
            "Your system can't decode this video (H.264). On Linux, install gst-plugins-bad / OpenH264 to enable playback.",
        });
      } else if (code === 2) {
        setError({
          kind: "network",
          detail: "Network error while loading the video. Check your connection and retry.",
        });
      } else {
        setError({
          kind: "unknown",
          detail: el?.error?.message || "Playback failed for an unknown reason.",
        });
      }
    };

    // Attach to any <video> inside the wrapper (videojs/react renders one).
    let videoEl: HTMLVideoElement | null = wrapper.querySelector("video");
    if (videoEl) videoEl.addEventListener("error", onVideoError, true);

    // The library may swap the <video> element on src change — observe and rebind.
    const observer = new MutationObserver(() => {
      const next = wrapper.querySelector("video");
      if (next && next !== videoEl) {
        if (videoEl) videoEl.removeEventListener("error", onVideoError, true);
        videoEl = next;
        videoEl.addEventListener("error", onVideoError, true);
      }
    });
    observer.observe(wrapper, { childList: true, subtree: true });

    return () => {
      observer.disconnect();
      if (videoEl) videoEl.removeEventListener("error", onVideoError, true);
    };
  }, [src]);

  return (
    <div
      ref={wrapperRef}
      className={`lookout-video-player platform-${platform}`}
      style={
        {
          width: "100%",
          height: "100%",
          display: "flex",
          position: "relative",
          "--media-border-radius": "8px",
          "--media-video-border-radius": "8px",
        } as React.CSSProperties
      }
    >
      <Player>
        <VideoSkin style={{ width: "100%", height: "100%" }}>
          <Video src={src} muted playsInline autoPlay={false} />
        </VideoSkin>
      </Player>
      {error && (
        <div className="lookout-video-player__error" role="alert">
          <h3>Can't play this timelapse</h3>
          <p>{error.detail}</p>
          <p className="lookout-video-player__error-note">
            Your timelapse saved successfully. You can{" "}
            <a href={src} target="_blank" rel="noopener noreferrer">
              download the file
            </a>{" "}
            and play it in another app.
          </p>
        </div>
      )}
    </div>
  );
}
