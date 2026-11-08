import { useState, useEffect, useRef } from "react";
import type { CaptureSource } from "../recorder.js";

const sharedPreviewUrlCache = new Map<string, string>();
const previewListeners = new Map<string, Set<(url: string) => void>>();

function setSharedPreviewUrl(key: string, url: string) {
  const oldUrl = sharedPreviewUrlCache.get(key);
  // Strictly manually free the old blob from memory to prevent memory leaks
  // during high-framerate image swapping
  if (oldUrl && oldUrl !== url && oldUrl.startsWith("blob:")) {
    URL.revokeObjectURL(oldUrl);
  }
  
  sharedPreviewUrlCache.set(key, url);
  const subs = previewListeners.get(key);
  if (subs) {
    for (const cb of subs) cb(url);
  }
}

/**
 * Periodically captures a high-res preview screenshot from the given source.
 * Uses a Tauri custom protocol to completely bypass the JS bridge/Base64 overhead.
 * Target 20fps but waits for the previous frame to load to prevent request pile-up.
 */
export function useScreenPreview(
  source: CaptureSource | null,
  targetFps = 20,
  live = true,
) {
  const [previewUrl, setPreviewUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const sourceRef = useRef(source);
  sourceRef.current = source;

  const sourceKey = source ? `${source.type}:${source.id}` : "";

  useEffect(() => {
    if (!sourceKey) return;

    const cached = sharedPreviewUrlCache.get(sourceKey);
    if (cached) setPreviewUrl(cached);

    const handler = (url: string) => setPreviewUrl(url);
    
    let subs = previewListeners.get(sourceKey);
    if (!subs) {
      subs = new Set();
      previewListeners.set(sourceKey, subs);
    }
    subs.add(handler);

    return () => {
      subs?.delete(handler);
    };
  }, [sourceKey]);

  useEffect(() => {
    if (!source) {
      setPreviewUrl(null);
      setError(null);
      return;
    }

    let cancelled = false;
    let timerId: ReturnType<typeof setTimeout>;
    let removeVisibilityListener: (() => void) | null = null;

    // Convert fps to ms interval, min 16ms (60fps)
    const intervalMs = Math.max(16, Math.floor(1000 / targetFps));

    console.debug(`[preview] starting preview loop for ${source.type} id=${source.id} at ~${targetFps}fps`);

    const loop = async () => {
      if (cancelled) return;

      const s = sourceRef.current;
      if (!s) return;

      // Each preview frame is a full native screen capture + JPEG encode.
      // Nobody can see the result while the window is hidden/minimized, so
      // park the loop until the document becomes visible again.
      if (document.hidden) {
        const onVisibility = () => {
          document.removeEventListener("visibilitychange", onVisibility);
          removeVisibilityListener = null;
          if (!cancelled) loop();
        };
        document.addEventListener("visibilitychange", onVisibility);
        removeVisibilityListener = () =>
          document.removeEventListener("visibilitychange", onVisibility);
        return;
      }

      const startTime = performance.now();
      const scheduleNext = () => {
        if (cancelled) return;
        if (!live) return;
        const elapsed = performance.now() - startTime;
        const delay = Math.max(0, intervalMs - elapsed);
        timerId = setTimeout(loop, delay);
      };

      try {
        const isWindows = navigator.userAgent.includes("Windows");
        const baseUrl = isWindows ? "http://lookout-preview.localhost" : "lookout-preview://localhost";
        const url = `${baseUrl}/${s.type}/${s.id}?maxWidth=854&maxHeight=480&jpegQuality=65&t=${Date.now()}`;
        
        // Use fetch + blob to manually control memory allocation.
        // Directly assigning URL to img.src can cause DOM rendering cache memory leaks
        // in WebKit at high framerates.
        const res = await fetch(url);
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        
        const blob = await res.blob();
        if (cancelled) return;

        const objectUrl = URL.createObjectURL(blob);
        
        // Preload image to avoid blinking, then swap
        const img = new Image();
        img.onload = () => {
          if (!cancelled) {
            setSharedPreviewUrl(sourceKey, objectUrl);
            setError(null);
          } else {
            URL.revokeObjectURL(objectUrl);
          }
          scheduleNext();
        };
        
        img.onerror = () => {
          URL.revokeObjectURL(objectUrl);
          if (!cancelled) setError("Failed to decode preview");
          scheduleNext();
        };
        
        img.src = objectUrl;
      } catch (err) {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : String(err));
        }
        scheduleNext();
      }
    };

    const cached = sharedPreviewUrlCache.get(sourceKey);
    if (live || !cached) {
      loop();
    }

    return () => {
      cancelled = true;
      clearTimeout(timerId);
      removeVisibilityListener?.();
      console.debug("[preview] stopping preview loop");
    };
  }, [sourceKey, targetFps, live]);

  return { previewUrl, error };
}
