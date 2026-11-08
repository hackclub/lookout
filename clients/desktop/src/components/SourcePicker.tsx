import React, { useEffect, useState, useCallback, useRef } from "react";
import { invoke, getReport } from "../logger.js";
import { AnimatePresence, LayoutGroup, motion } from "motion/react";
import {
  Button,
  ErrorDisplay,
  Spinner,
  colors,
  spacing,
  radii,
  fontSize,
  fontWeight,
} from "@lookout/react";
import { CaptureErrorDialog } from "./CaptureErrorDialog.js";
import { isCancelledCaptureError } from "../captureDiagnosis.js";
import type { CaptureSource } from "../recorder.js";
import { useScreenPreview } from "../hooks/useScreenPreview.js";
import { enumerateCameras } from "../hooks/useCameraCapture.js";

interface MonitorInfo {
  id: number;
  name: string;
  width: number;
  height: number;
  isPrimary: boolean;
  isBuiltin: boolean;
  scaleFactor: number;
}

interface WindowInfo {
  id: number;
  appName: string;
  title: string;
  width: number;
  height: number;
  isMinimized: boolean;
  isFocused: boolean;
}

interface CaptureSourceList {
  monitors: MonitorInfo[];
  windows: WindowInfo[];
}

interface SourcePickerProps {
  onSelect: (source: CaptureSource | CaptureSource[]) => void;
  submitLabel?: string;
}

function sourcesEqual(a: CaptureSource | null, b: CaptureSource | null): boolean {
  if (!a || !b) return false;
  return a.type === b.type && a.id === b.id;
}

// Most people record the same screen(s) every session — remember the last
// monitor selection so it's preselected next time. Only monitors: window ids
// aren't stable across app launches, and cameras load asynchronously.
const LAST_MONITORS_KEY = "lookout-last-monitor-selection";

function loadLastMonitorIds(): number[] {
  try {
    const raw = localStorage.getItem(LAST_MONITORS_KEY);
    if (raw) {
      const parsed = JSON.parse(raw);
      if (Array.isArray(parsed) && parsed.every((id) => typeof id === "number")) {
        return parsed;
      }
    }
  } catch { /* corrupted entry — fall back to the primary-monitor default */ }
  return [];
}

function saveLastMonitorSelection(sources: CaptureSource[]) {
  const ids = sources.filter((s) => s.type === "monitor").map((s) => s.id);
  try {
    if (ids.length > 0) {
      localStorage.setItem(LAST_MONITORS_KEY, JSON.stringify(ids));
    }
  } catch { /* storage unavailable — non-fatal */ }
}

type TabId = "screens" | "windows" | "cameras" | "cast";

function PreviewImage({
  source,
  isMulti,
  layoutId,
  isWindow,
  fallbackUrl
}: {
  source: CaptureSource,
  isMulti: boolean,
  layoutId?: string,
  isWindow: boolean,
  fallbackUrl?: string | null
}) {
  const { previewUrl } = useScreenPreview(source, 1);
  const finalUrl = previewUrl || fallbackUrl;

  if (!finalUrl) {
    return (
      <motion.div layout style={{ flex: 1, display: "flex", alignItems: "center", justifyContent: "center", height: "100%", minWidth: 0 }}>
         <Spinner size="sm" />
      </motion.div>
    );
  }

  return (
    <motion.img
      layout
      src={finalUrl}
      alt="Preview"
      layoutId={layoutId}
      initial={{ opacity: 0, scale: isWindow ? 1.08 : 1.02 }}
      animate={{ opacity: 1, scale: 1 }}
      exit={{ opacity: 0, scale: 0.98 }}
      transition={{
        opacity: { duration: 0.22, ease: "easeOut" },
        scale: { type: "spring", stiffness: 360, damping: 32, mass: 0.7 },
        layout: { type: "spring", stiffness: 420, damping: 34, mass: 0.75 },
      }}
      style={{
        flex: isMulti ? 1 : undefined,
        width: "100%",
        height: "100%",
        objectFit: isMulti ? "cover" : "contain",
        display: "block",
        minWidth: 0, // prevents flex overflow
        position: isMulti ? "relative" : "absolute",
        inset: isMulti ? undefined : 0,
      }}
    />
  );
}

export function SourcePicker({ onSelect, submitLabel = "Start Capture" }: SourcePickerProps) {
  const [sources, setSources] = useState<CaptureSourceList | null>(null);
  const [cameras, setCameras] = useState<MediaDeviceInfo[]>([]);
  const [error, setError] = useState<string | null>(null);
  /** Capture failure currently being explained in the modal. Separate from
   *  `error`, which is the card the picker falls back to once it's closed. */
  const [dialogError, setDialogError] = useState<string | null>(null);
  const [tab, setTab] = useState<TabId>("screens");
  const [selected, setSelected] = useState<CaptureSource[]>([]);
  const [hoveredWindow, setHoveredWindow] = useState<CaptureSource | null>(null);
  const [hoverAspectRatio, setHoverAspectRatio] = useState(16 / 9);
  const [isWayland, setIsWayland] = useState(false);

  const scrollRef = useRef<HTMLDivElement>(null);
  const [showTopMask, setShowTopMask] = useState(false);
  const [showBottomMask, setShowBottomMask] = useState(false);

  // Camera preview stream (only while a camera is selected in the picker)
  const [cameraPreviewStream, setCameraPreviewStream] = useState<MediaStream | null>(null);
  const cameraPreviewRef = useRef<MediaStream | null>(null);
  const videoPreviewRef = useRef<HTMLVideoElement>(null);

  const handleScroll = useCallback(() => {
    if (!scrollRef.current) return;
    const { scrollTop, scrollHeight, clientHeight } = scrollRef.current;
    setShowTopMask(scrollTop > 0);
    setShowBottomMask(Math.ceil(scrollTop + clientHeight) < scrollHeight);
  }, []);

  const { previewUrl: hoverPreviewUrl } = useScreenPreview(hoveredWindow, 5, false);

  // Derive camera selection state for preview
  const selectedCameraId = selected.length === 1 && selected[0].type === "camera" ? String(selected[0].id) : null;

  // Start/stop camera preview when a camera is selected/deselected
  useEffect(() => {
    if (!selectedCameraId) {
      // Stop camera preview if we switched away
      if (cameraPreviewRef.current) {
        console.log("[sources] stopping camera preview (source changed)");
        cameraPreviewRef.current.getTracks().forEach((t) => t.stop());
        cameraPreviewRef.current = null;
        setCameraPreviewStream(null);
      }
      return;
    }

    let cancelled = false;

    (async () => {
      // Stop any existing preview
      if (cameraPreviewRef.current) {
        cameraPreviewRef.current.getTracks().forEach((t) => t.stop());
      }

      console.log(`[sources] starting camera preview for device ${selectedCameraId}...`);
      try {
        const stream = await navigator.mediaDevices.getUserMedia({
          video: { deviceId: { exact: selectedCameraId } },
          audio: false,
        });
        if (cancelled) {
          stream.getTracks().forEach((t) => t.stop());
          return;
        }
        cameraPreviewRef.current = stream;
        setCameraPreviewStream(stream);
        console.log("[sources] camera preview started");
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        console.error(`[sources] camera preview failed: ${msg}`);
        if (!cancelled) {
          cameraPreviewRef.current = null;
          setCameraPreviewStream(null);
        }
      }
    })();

    return () => {
      cancelled = true;
      if (cameraPreviewRef.current) {
        cameraPreviewRef.current.getTracks().forEach((t) => t.stop());
        cameraPreviewRef.current = null;
        setCameraPreviewStream(null);
      }
    };
  }, [selectedCameraId]);

  // Attach stream to video element
  useEffect(() => {
    if (videoPreviewRef.current) {
      videoPreviewRef.current.srcObject = cameraPreviewStream;
    }
  }, [cameraPreviewStream]);

  // Enumerate cameras on mount and on device changes
  useEffect(() => {
    const loadCameras = async () => {
      const devices = await enumerateCameras();
      setCameras(devices);
    };
    loadCameras();

    // Listen for camera connect/disconnect
    const handleDeviceChange = () => {
      console.log("[sources] device change detected, re-enumerating cameras...");
      loadCameras();
    };

    try {
      navigator.mediaDevices?.addEventListener("devicechange", handleDeviceChange);
    } catch {
      console.warn("[sources] devicechange event not supported");
    }

    return () => {
      try {
        navigator.mediaDevices?.removeEventListener("devicechange", handleDeviceChange);
      } catch { /* ignore */ }
    };
  }, []);

  const refresh = useCallback(async () => {
    console.log("[sources] listing capture sources...");
    try {
      const wayland = await invoke<boolean>("is_wayland").catch(() => false);
      setIsWayland(wayland);
      if (wayland && tab !== "cast" && tab !== "cameras") {
        setTab("cast");
      }

      if (wayland) {
        // On Wayland, xcap can't enumerate sources (no X11).
        // Set an empty source list so the UI renders the Cast tab directly.
        setSources({ monitors: [], windows: [] });
        setError(null);
        setTimeout(handleScroll, 10);
        return;
      }

      const result = await invoke<CaptureSourceList>("list_capture_sources");
      console.log(`[sources] found ${result.monitors.length} monitors, ${result.windows.length} windows`);
      setSources(result);
      setError(null);
      // Wait for render then check scroll
      setTimeout(handleScroll, 10);

      // Auto-select if nothing selected yet: prefer the monitors used last
      // session (when every one of them is still connected), else the primary.
      if (selected.length === 0 && !wayland) {
        const lastIds = loadLastMonitorIds();
        const remembered = lastIds.filter((id) => result.monitors.some((m) => m.id === id));
        if (lastIds.length > 0 && remembered.length === lastIds.length) {
          console.log(`[sources] auto-selected remembered monitors: ${remembered.join(", ")}`);
          setSelected(remembered.map((id) => ({ type: "monitor" as const, id })));
        } else {
          const primary = result.monitors.find((m) => m.isPrimary) ?? result.monitors[0];
          if (primary) {
            console.log(`[sources] auto-selected: monitor id=${primary.id} (${primary.name})`);
            setSelected([{ type: "monitor", id: primary.id }]);
          }
        }
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error(`[sources] failed to list sources: ${msg}`);
      setError(msg);
      setDialogError(msg);
    }
  }, [selected.length, handleScroll, tab]);

  useEffect(() => { refresh(); }, [refresh]);

  useEffect(() => {
    handleScroll();
    window.addEventListener('resize', handleScroll);
    return () => window.removeEventListener('resize', handleScroll);
  }, [tab, sources, handleScroll]);

  const handleSelect = (src: CaptureSource, shiftKey: boolean) => {
    if (shiftKey) {
      setSelected(prev => {
        // Enforce same type: only allow multiple selection of the same type (either all monitors or all windows)
        if (prev.length > 0 && prev[0].type !== src.type) {
          // If they shift-click a different type, we just replace the whole selection with the new item
          return [src];
        }

        const exists = prev.some(p => sourcesEqual(p, src));
        if (exists) {
          return prev.filter(p => !sourcesEqual(p, src));
        } else {
          return [...prev, src];
        }
      });
    } else {
      setSelected([src]);
    }
  };

  /** Portal failures are their own thing: they aren't a failure to detect
   *  displays, and dismissing the portal's picker isn't a failure at all. */
  const handleCastError = (e: unknown) => {
    const msg = e instanceof Error ? e.message : String(e);
    if (isCancelledCaptureError(msg)) {
      console.log(`[sources] screencast dismissed by user: ${msg}`);
      return;
    }
    console.error(`[sources] screencast failed: ${msg}`);
    setDialogError(msg);
  };

  const handleAddCast = async () => {
    try {
      const streams = await invoke<{ node_id: number }[]>("request_screencast");
      if (streams && streams.length > 0) {
        setSelected(streams.map(s => ({ type: "pipewire", id: s.node_id })));
      }
    } catch (e) {
      handleCastError(e);
    }
  };

  const handleAddToCast = async () => {
    try {
      const streams = await invoke<{ node_id: number }[]>("add_screencast");
      if (streams && streams.length > 0) {
        setSelected(prev => {
          const newSources = streams
            .map(s => ({ type: "pipewire" as const, id: s.node_id }))
            .filter(ns => !prev.some(p => sourcesEqual(p, ns)));
          return [...prev, ...newSources];
        });
      }
    } catch (e) {
      handleCastError(e);
    }
  };

  // Portals to the body, so it rides along with whichever state renders.
  const errorDialog = dialogError ? (
    <CaptureErrorDialog
      error={dialogError}
      fallbackTitle={error ? "Couldn't detect your displays" : "Couldn't start the capture"}
      onRetry={() => { setDialogError(null); refresh(); }}
      onDismiss={() => setDialogError(null)}
    />
  ) : null;

  if (error) {
    return (
      <div style={{
        display: "flex", flexDirection: "column", alignItems: "center",
        justifyContent: "center", minHeight: 200, padding: spacing.xxl,
      }}>
        <ErrorDisplay
          variant="page"
          title="Failed to detect displays"
          error={error}
          action={{ label: "Retry", onClick: refresh }}
          onCopy={() => navigator.clipboard.writeText(getReport(error))}
        />
        <div style={{ marginTop: spacing.md }}>
          <Button variant="ghost" size="sm" onClick={() => setDialogError(error)}>
            Troubleshoot
          </Button>
        </div>
        {errorDialog}
      </div>
    );
  }

  if (!sources) {
    return (
      <div style={{
        display: "flex", flexDirection: "column", alignItems: "center",
        justifyContent: "center", minHeight: 200, padding: spacing.xxl, gap: spacing.md,
      }}>
        <Spinner size="md" />
        <p style={{ fontSize: fontSize.lg, color: colors.text.secondary, textAlign: "center" }}>
          Detecting displays...
        </p>
        {errorDialog}
      </div>
    );
  }

  const hasWindows = sources.windows.length > 0;
  const hasCameras = cameras.length > 0;
  const hasTabs = hasWindows || hasCameras || isWayland;

  // Build tab list dynamically
  const tabs: { id: TabId; label: string; count?: number }[] = [];
  if (isWayland) {
    tabs.push({ id: "cast", label: "Cast" });
  } else {
    tabs.push({ id: "screens", label: "Screens", count: sources.monitors.length });
    if (hasWindows) tabs.push({ id: "windows", label: "Windows", count: sources.windows.length });
  }
  if (hasCameras) tabs.push({ id: "cameras", label: "Cameras", count: cameras.length });

  const isSingleSelected = selected.length === 1;
  const isHoveredAlreadySelected = !!hoveredWindow && selected.some(s => sourcesEqual(s, hoveredWindow));
  const showHoverPreview = tab === "windows" && hoveredWindow && !isHoveredAlreadySelected;
  const isCameraSelected = selected.length === 1 && selected[0].type === "camera";
  const showCameraPreview = isCameraSelected && cameraPreviewStream;

  return (
    <div style={{
      maxWidth: 480,
      margin: "0 auto",
      padding: spacing.lg,
      display: "flex",
      flexDirection: "column",
      height: "100%", // Fill parent RecordPage container
      maxHeight: "100%",
    }}>
      <div style={{ flexShrink: 0 }}>
        {/* Live preview */}
        <div style={{
          position: "relative",
          borderRadius: radii.lg, overflow: "hidden", background: colors.bg.sunken,
          border: `1px solid ${colors.border.default}`, marginBottom: spacing.lg, aspectRatio: "2/1",
        }}>
          {showCameraPreview ? (
            <video
              ref={videoPreviewRef}
              autoPlay
              muted
              playsInline
              style={{ width: "100%", height: "100%", objectFit: "contain", display: "block", transform: "scaleX(-1)" }}
            />
          ) : (
            <LayoutGroup id="preview-handoff">
              <div style={{ width: "100%", height: "100%", display: "flex", flexDirection: "row", overflow: "hidden", position: "absolute", inset: 0, gap: selected.length > 1 ? spacing.xs : 0 }}>
                <AnimatePresence mode="popLayout" initial={false}>
                  {selected.length > 0 && !isCameraSelected ? (
                    selected.map((src) => {
                      const isWindow = src.type === "window";
                      const isHoverMatch = !!hoveredWindow && sourcesEqual(hoveredWindow, src);
                      const handoffLayoutId = isWindow && isHoverMatch ? `hover-to-main-preview-${src.id}` : undefined;
                      return (
                        <PreviewImage
                          key={`${src.type}:${src.id}`}
                          source={src}
                          isMulti={selected.length > 1}
                          layoutId={handoffLayoutId}
                          isWindow={isWindow}
                          fallbackUrl={isHoverMatch ? hoverPreviewUrl : null}
                        />
                      );
                    })
                  ) : (
                    <motion.div
                      key="preview-empty"
                      initial={{ opacity: 0 }}
                      animate={{ opacity: 1 }}
                      exit={{ opacity: 0 }}
                      style={{ width: "100%", height: "100%", display: "flex", alignItems: "center", justifyContent: "center" }}
                    >
                      <p style={{ fontSize: fontSize.md, color: colors.text.quaternary, textAlign: "center" }}>
                        {isCameraSelected ? "Starting camera preview..." : "Select a source below"}
                      </p>
                    </motion.div>
                  )}
                </AnimatePresence>
              </div>

              <AnimatePresence>
                {showHoverPreview && (
                  <motion.div
                    key={hoveredWindow ? `hover-${hoveredWindow.id}` : "hover-empty"}
                    initial={{ opacity: 0, scale: 0.92 }}
                    animate={{ opacity: 1, scale: 1 }}
                    exit={{ opacity: 0 }}
                    transition={{ type: "spring", stiffness: 420, damping: 32, mass: 0.7 }}
                    style={{
                      position: "absolute",
                      right: spacing.md,
                      top: spacing.xl,
                      width: 240,
                      maxHeight: 180,
                      aspectRatio: String(hoverAspectRatio),
                      borderRadius: radii.md,
                      overflow: "hidden",
                      background: colors.bg.surface,
                      border: `1px solid ${colors.border.default}`,
                      boxShadow: "0 10px 30px rgba(0,0,0,0.22)",
                      pointerEvents: "none",
                      zIndex: 2,
                    }}
                  >
                    {hoverPreviewUrl ? (
                      <motion.img
                        src={hoverPreviewUrl}
                        alt="Window hover preview"
                        layoutId={`hover-to-main-preview-${hoveredWindow.id}`}
                        style={{ width: "100%", height: "100%", objectFit: "contain", display: "block" }}
                      />
                    ) : (
                      <div style={{ width: "100%", height: "100%", display: "flex", alignItems: "center", justifyContent: "center" }}>
                        <p style={{ fontSize: fontSize.xs, color: colors.text.tertiary }}>Loading preview...</p>
                      </div>
                    )}
                  </motion.div>
                )}
              </AnimatePresence>
            </LayoutGroup>
          )}
        </div>

        {/* Tabs */}
        {hasTabs && (
          <LayoutGroup>
            <div style={{
              display: "flex", gap: spacing.xs, marginBottom: spacing.md, background: colors.bg.surface,
              borderRadius: radii.md, padding: 4, position: "relative", alignItems: "stretch"
            }}>
              {tabs.map((t) => (
                <button
                  key={t.id}
                  style={{
                    flex: 1, padding: `6px ${spacing.sm}px`, fontSize: fontSize.sm, fontWeight: tab === t.id ? fontWeight.semibold : fontWeight.medium,
                    background: "transparent",
                    color: tab === t.id ? colors.text.primary : colors.text.secondary,
                    border: "none", borderRadius: radii.sm, cursor: "pointer",
                    position: "relative",
                    display: "flex", alignItems: "center", justifyContent: "center"
                  }}
                  onClick={() => setTab(t.id)}
                >
                  {tab === t.id && (
                    <motion.div
                      layoutId="activeTabIndicator"
                      transition={{ type: "spring", stiffness: 350, damping: 35 }}
                      style={{
                        position: "absolute", inset: 0, borderRadius: radii.sm,
                        background: colors.border.default, zIndex: 0,
                      }}
                    />
                  )}
                  <span style={{ position: "relative", zIndex: 1, display: "inline-block" }}>
                    {t.label} {t.count !== undefined ? `(${t.count})` : ''}
                  </span>
                </button>
              ))}
            </div>
          </LayoutGroup>
        )}
      </div>

      {/* Source list */}
      <div style={{ flex: 1, position: "relative", minHeight: 0, display: "flex", flexDirection: "column" }}>
        <div
          ref={scrollRef}
          onScroll={handleScroll}
          style={{
          display: "flex",
          flexDirection: "column",
          gap: spacing.xs,
          flex: 1, // Let it fill available space
          minHeight: 0, // Critical for nested flex scrolling
          overflowY: "auto",
          // Add a smooth fade out mask at top/bottom of scroll area
          maskImage: `linear-gradient(to bottom, ${showTopMask ? 'transparent 0%, black 12px' : 'black 0%, black 12px'}, ${showBottomMask ? 'black calc(100% - 12px), transparent 100%' : 'black calc(100% - 12px), black 100%'})`,
          WebkitMaskImage: `linear-gradient(to bottom, ${showTopMask ? 'transparent 0%, black 12px' : 'black 0%, black 12px'}, ${showBottomMask ? 'black calc(100% - 12px), transparent 100%' : 'black calc(100% - 12px), black 100%'})`,
          paddingBottom: spacing.xs,
        }}>
          {tab === "cast" && (
            <div style={{ display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", height: "100%", gap: spacing.md, padding: spacing.xl }}>
              <p style={{ color: colors.text.secondary, textAlign: "center", fontSize: fontSize.md }}>
                Wayland requires explicit permission to capture screens or windows.
              </p>
              
              {selected.filter(s => s.type === "pipewire").length > 0 && (
                <div style={{ width: "100%", display: "flex", flexDirection: "column", gap: spacing.xs, margin: `${spacing.sm}px 0` }}>
                  {selected.filter(s => s.type === "pipewire").map((src, i) => (
                    <div key={src.id} style={{
                      display: "flex", alignItems: "center", gap: spacing.md,
                      padding: `${spacing.md}px ${spacing.md}px`, background: colors.bg.selected,
                      border: `1px solid ${colors.border.selected}`,
                      borderRadius: radii.md,
                      width: "100%"
                    }}>
                      <div style={{
                        width: 18, height: 18, borderRadius: "50%",
                        border: `2px solid ${colors.icon.selected}`,
                        flexShrink: 0, display: "flex", alignItems: "center", justifyContent: "center",
                      }}>
                        <div style={{ width: 8, height: 8, borderRadius: "50%", background: colors.icon.selected }} />
                      </div>
                      <span style={{ fontSize: fontSize.md, fontWeight: fontWeight.semibold, color: colors.text.primary }}>
                        Cast Stream {i + 1}
                      </span>
                    </div>
                  ))}
                </div>
              )}

              {selected.some(s => s.type === "pipewire") ? (
                <div style={{ display: "flex", gap: spacing.xs, width: "100%" }}>
                  <Button variant="secondary" size="md" onClick={handleAddCast} style={{ flex: 1 }}>
                    Select new sets of windows
                  </Button>
                  <Button variant="secondary" size="md" onClick={handleAddToCast} style={{ width: 40, padding: 0, flexShrink: 0 }}>
                    +
                  </Button>
                </div>
              ) : (
                <Button variant="secondary" size="md" onClick={handleAddCast}>
                  + Add new Screen/Window
                </Button>
              )}
            </div>
          )}

          {(tab === "screens" || (!isWayland && !hasWindows && !hasCameras)) &&
            sources.monitors.map((m) => {
              const src: CaptureSource = { type: "monitor", id: m.id };
              const isSelected = selected.some(p => sourcesEqual(p, src));
              return (
                <motion.button
                  key={`m-${m.id}`}
                  whileTap="active"
                  initial="idle"
                  style={{
                    display: "flex", alignItems: "center", gap: spacing.md,
                    padding: `${spacing.md}px ${spacing.md}px`, background: "transparent",
                    border: "none", outline: "none", userSelect: "none",
                    WebkitUserSelect: "none",
                    borderRadius: radii.md, cursor: "pointer", textAlign: "left" as const,
                    width: "100%", position: "relative",
                  }}
                  onClick={(e) => handleSelect(src, e.shiftKey)}
                >
                  <motion.div
                    variants={{ idle: { scale: 1 }, active: { scale: 0.98 } }}
                    transition={{ type: "spring", stiffness: 1500, damping: 60 }}
                    style={{
                      position: "absolute", inset: 0,
                      background: isSelected ? colors.bg.selected : colors.bg.surface,
                      border: `1px solid ${isSelected ? colors.border.selected : colors.border.default}`,
                      borderRadius: radii.md,
                      zIndex: 0,
                    }}
                  />
                  <div
                    style={{ display: "flex", alignItems: "center", gap: spacing.md, width: "100%", position: "relative", zIndex: 1 }}
                  >
                    <div style={{
                      width: 18, height: 18, borderRadius: "50%",
                      border: `2px solid ${isSelected ? colors.icon.selected : colors.text.quaternary}`,
                      flexShrink: 0, display: "flex", alignItems: "center", justifyContent: "center",
                    }}>
                      {isSelected && <div style={{ width: 8, height: 8, borderRadius: "50%", background: colors.icon.selected }} />}
                    </div>
                    <div style={{ flex: 1, display: "flex", flexDirection: "column" as const, gap: 2, minWidth: 0 }}>
                      <span style={{ fontSize: fontSize.md, fontWeight: fontWeight.semibold, color: colors.text.primary, display: "flex", alignItems: "center", gap: spacing.sm }}>
                        {m.name}
                        {m.isPrimary && (
                          <span style={{
                            fontSize: fontSize.xs - 1, fontWeight: fontWeight.semibold, color: colors.badge.primaryText,
                            background: colors.badge.primaryBg, padding: "1px 6px", borderRadius: radii.sm,
                          }}>
                            Primary
                          </span>
                        )}
                      </span>
                      <span style={{ fontSize: fontSize.xs, color: colors.text.tertiary, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" as const }}>
                        {m.width}x{m.height}
                        {m.scaleFactor > 1 && ` @ ${m.scaleFactor}x`}
                      </span>
                    </div>
                  </div>
                </motion.button>
              );
            })}

          {tab === "windows" && hasWindows &&
            sources.windows.map((w) => {
              const src: CaptureSource = { type: "window", id: w.id };
              const isSelected = selected.some(p => sourcesEqual(p, src));
              return (
                <motion.button
                  key={`w-${w.id}`}
                  whileTap="active"
                  initial="idle"
                  style={{
                    display: "flex", alignItems: "center", gap: spacing.md,
                    padding: `${spacing.md}px ${spacing.md}px`, background: "transparent",
                    border: "none", outline: "none", userSelect: "none",
                    WebkitUserSelect: "none",
                    borderRadius: radii.md, cursor: "pointer", textAlign: "left" as const,
                    width: "100%", position: "relative",
                    ...(w.isMinimized ? { opacity: 0.5 } : {}),
                  }}
                  onClick={(e) => handleSelect(src, e.shiftKey)}
                  onMouseEnter={() => {
                    setHoveredWindow(src);
                    const ratio = w.height > 0 ? w.width / w.height : 16 / 9;
                    setHoverAspectRatio(Math.min(3, Math.max(0.5, ratio)));
                  }}
                  onMouseLeave={() => {
                    setHoveredWindow(null);
                  }}
                >
                  <motion.div
                    variants={{ idle: { scale: 1 }, active: { scale: 0.98 } }}
                    transition={{ type: "spring", stiffness: 1500, damping: 60 }}
                    style={{
                      position: "absolute", inset: 0,
                      background: isSelected ? colors.bg.selected : colors.bg.surface,
                      border: `1px solid ${isSelected ? colors.border.selected : colors.border.default}`,
                      borderRadius: radii.md,
                      zIndex: 0,
                    }}
                  />
                  <div
                    style={{ display: "flex", alignItems: "center", gap: spacing.md, width: "100%", position: "relative", zIndex: 1 }}
                  >
                    <div style={{
                      width: 18, height: 18, borderRadius: "50%",
                      border: `2px solid ${isSelected ? colors.icon.selected : colors.text.quaternary}`,
                      flexShrink: 0, display: "flex", alignItems: "center", justifyContent: "center",
                    }}>
                      {isSelected && <div style={{ width: 8, height: 8, borderRadius: "50%", background: colors.icon.selected }} />}
                    </div>
                    <div style={{ flex: 1, display: "flex", flexDirection: "column" as const, gap: 2, minWidth: 0 }}>
                      <span style={{ fontSize: fontSize.md, fontWeight: fontWeight.semibold, color: colors.text.primary, display: "flex", alignItems: "center", gap: spacing.sm }}>
                        {w.appName || w.title}
                        {w.isMinimized && (
                          <span style={{
                            fontSize: fontSize.xs - 1, fontWeight: fontWeight.medium, color: colors.text.secondary,
                            background: `${colors.text.secondary}26`, padding: "1px 6px", borderRadius: radii.sm,
                          }}>
                            Minimized
                          </span>
                        )}
                      </span>
                      <span style={{ fontSize: fontSize.xs, color: colors.text.tertiary, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" as const }}>
                        {w.title && w.appName ? w.title + " \u2014 " : ""}
                        {w.width}x{w.height}
                      </span>
                    </div>
                  </div>
                </motion.button>
              );
            })}

          {tab === "cameras" && hasCameras &&
            cameras.map((cam, index) => {
              const src: CaptureSource = { type: "camera", id: cam.deviceId };
              const isSelected = selected.some(p => sourcesEqual(p, src));
              return (
                <motion.button
                  key={`c-${cam.deviceId}`}
                  whileTap="active"
                  initial="idle"
                  style={{
                    display: "flex", alignItems: "center", gap: spacing.md,
                    padding: `${spacing.md}px ${spacing.md}px`, background: "transparent",
                    border: "none", outline: "none", userSelect: "none",
                    WebkitUserSelect: "none",
                    borderRadius: radii.md, cursor: "pointer", textAlign: "left" as const,
                    width: "100%", position: "relative",
                  }}
                  onClick={() => setSelected([src])}
                >
                  <motion.div
                    variants={{ idle: { scale: 1 }, active: { scale: 0.98 } }}
                    transition={{ type: "spring", stiffness: 1500, damping: 60 }}
                    style={{
                      position: "absolute", inset: 0,
                      background: isSelected ? colors.bg.selected : colors.bg.surface,
                      border: `1px solid ${isSelected ? colors.border.selected : colors.border.default}`,
                      borderRadius: radii.md,
                      zIndex: 0,
                    }}
                  />
                  <div
                    style={{ display: "flex", alignItems: "center", gap: spacing.md, width: "100%", position: "relative", zIndex: 1 }}
                  >
                    <div style={{
                      width: 18, height: 18, borderRadius: "50%",
                      border: `2px solid ${isSelected ? colors.icon.selected : colors.text.quaternary}`,
                      flexShrink: 0, display: "flex", alignItems: "center", justifyContent: "center",
                    }}>
                      {isSelected && <div style={{ width: 8, height: 8, borderRadius: "50%", background: colors.icon.selected }} />}
                    </div>
                    <div style={{ flex: 1, display: "flex", flexDirection: "column" as const, gap: 2, minWidth: 0 }}>
                      <span style={{ fontSize: fontSize.md, fontWeight: fontWeight.semibold, color: colors.text.primary }}>
                        {cam.label || `Camera ${index + 1}`}
                      </span>
                    </div>
                  </div>
                </motion.button>
              );
            })}
        </div>

        <AnimatePresence>
          {((tab === "screens" && sources.monitors.length > 1) || (tab === "windows" && sources.windows.length > 1)) && (
            <motion.div
              initial={{ opacity: 0, y: 5 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: 10 }}
              transition={{ type: "spring", stiffness: 900, damping: 30 }}
              style={{
                position: "absolute",
                bottom: '-0px',
                right: spacing.lg,
                background: "rgba(0, 0, 0, 0.7)",
                backdropFilter: "blur(8px)",
                WebkitBackdropFilter: "blur(8px)",
                color: "#fff",
                padding: "6px 10px",
                borderRadius: radii.md,
                fontSize: fontSize.xs,
                display: "flex",
                alignItems: "center",
                pointerEvents: "none",
                boxShadow: "0 4px 12px rgba(0,0,0,0.15)",
                zIndex: 10,
              }}
            >
              <kbd style={{ border: "1px solid rgba(255,255,255,0.3)", background: "rgba(255,255,255,0.1)", borderRadius: 4, padding: "2px 4px", margin: "0 4px 0 0", fontFamily: "inherit" }}>shift</kbd>
              +
              <kbd style={{ border: "1px solid rgba(255,255,255,0.3)", background: "rgba(255,255,255,0.1)", borderRadius: 4, padding: "2px 4px", margin: "0 6px 0 4px", fontFamily: "inherit" }}>click</kbd>
              to select multiple
            </motion.div>
          )}
        </AnimatePresence>
      </div>

      {/* Start button */}
      <div style={{ flexShrink: 0 }}>
        {selected.length > 0 && (
          <Button
            variant="primary"
            size="lg"
            fullWidth
            onClick={() => {
              saveLastMonitorSelection(selected);
              onSelect(selected);
            }}
            style={{ marginTop: spacing.lg }}
          >
            {submitLabel}
          </Button>
        )}
      </div>

      {errorDialog}
    </div>
  );
}
