import { useState, useEffect, useCallback } from "react";
import { AnimatePresence, motion } from "motion/react";
import { invoke } from "../logger.js";
import {
  LookoutProvider,
  Button,
  Skeleton,
  colors,
  spacing,
  radii,
  fontSize,
  fontWeight,
} from "@lookout/react";
import { isLinux } from "../platform.js";
import type { CaptureSource } from "../hooks/useNativeCapture.js";
import { SourcePicker } from "./SourcePicker.js";
import { DesktopRecorder } from "./DesktopRecorder.js";
import { NamingModal } from "./NamingModal.js";
import { openEditorWindow } from "./EditorWindow.js";
import { PageLayout, cardButtonStyle } from "./PageLayout.js";

import { getApiBase } from "../serverConfig.js";

// Read once per webview load; Settings → Server reloads the view on change.
const API_BASE = getApiBase();

interface RecordPageProps {
  token: string;
  onBack: () => void;
  onViewSession: (token: string) => void;
}

export function RecordPage({ token, onBack, onViewSession }: RecordPageProps) {
  const isMacOS = navigator.userAgent.includes("Mac");
  const [captureSource, setCaptureSource] = useState<CaptureSource[] | null>(null);
  const [captureFlowDirection, setCaptureFlowDirection] = useState(1);
  const [stopping, setStopping] = useState(false);
  const [sessionCheck, setSessionCheck] = useState<"loading" | "ok" | "finished" | "error">("loading");
  const [sessionStatus, setSessionStatus] = useState<string | null>(null);
  const [checkError, setCheckError] = useState<string | null>(null);
  const [isPrompting, setIsPrompting] = useState(false);

  // Check if the session is still recordable before showing source picker
  useEffect(() => {
    (async () => {
      console.log(`[record] checking session status for token: ${token.slice(0, 8)}...`);
      try {
        const res = await fetch(`${API_BASE}/api/sessions/${token}/status`);
        if (!res.ok) {
          const errText = `HTTP ${res.status} ${await res.text().catch(() => "")}`;
          console.error(`[record] session check failed: ${errText}`);
          setCheckError(errText);
          setSessionCheck("error");
          return;
        }
        const data = await res.json();
        console.log(`[record] session status: ${data.status}`);
        setSessionStatus(data.status);
        if (["stopped", "compiling", "complete", "failed"].includes(data.status)) {
          setSessionCheck("finished");
        } else {
          setSessionCheck("ok");
        }
      } catch (err: any) {
        console.error("[record] session check error:", err);
        setCheckError(err.message || String(err));
        setSessionCheck("error");
      }
    })();
  }, [token]);

  const handleStopClick = useCallback(() => {
    setIsPrompting(true);
  }, []);

  /** Hand the screen back to the compositor.
   *
   *  Wayland casts through the XDG portal, and the portal keeps streaming
   *  until the session is explicitly closed — leaving the "screen is being
   *  shared" indicator up long after the timelapse ended. Stopping the
   *  capture loop is deliberately not enough: pause goes through that too,
   *  and a paused session has to keep its cast. So the release happens here,
   *  where the recording genuinely ends. */
  const releaseScreencast = useCallback(() => {
    invoke("release_screencast").catch(console.error);
  }, []);

  // Backstop for every other way out of a recording — a server-side auto-stop
  // navigates straight to the session view without passing through
  // `stopSession`, and that must not leave the screen being cast either.
  useEffect(() => releaseScreencast, [releaseScreencast]);

  const stopSession = useCallback(async (name: string | null, edit: boolean) => {
    setStopping(true);
    console.log(
      `[record] stopping session, name: ${name?.trim() || "(none)"}, edit: ${edit}`,
    );
    if (name && name.trim()) {
      try {
        await fetch(`${API_BASE}/api/sessions/${token}/name`, {
          method: "PATCH",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ name: name.trim() }),
        });
      } catch (e) {
        console.warn("[record] rename failed:", e);
      }
    }
    try {
      // `edit: true` holds the timelapse unpublished after it compiles so
      // the user can cut it first — programs only ever see it finished.
      await fetch(`${API_BASE}/api/sessions/${token}/stop`, {
        method: "POST",
        ...(edit
          ? {
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({ edit: true }),
            }
          : {}),
      });
      console.log("[record] session stopped");
    } catch (e) {
      console.error("[record] stop failed:", e);
    }
    // Either way the user lands on the session view; when held, it shows
    // the review panel (and the editor window opens from there).
    if (edit) {
      void openEditorWindow(token);
    }
    releaseScreencast();
    onViewSession(token);
  }, [token, onViewSession, releaseScreencast]);

  const handleConfirmStop = useCallback(
    (name: string | null) => stopSession(name, false),
    [stopSession],
  );

  const handleEditAndSave = useCallback(
    (name: string | null) => stopSession(name, true),
    [stopSession],
  );

  const handleResumeFromModal = useCallback(() => {
    setIsPrompting(false);
  }, []);

  const handleSelectSource = useCallback((source: CaptureSource | CaptureSource[]) => {
    setCaptureFlowDirection(1);
    setCaptureSource(Array.isArray(source) ? source : [source]);
  }, []);

  const handleChangeSource = useCallback(() => {
    setCaptureFlowDirection(-1);
    setCaptureSource(null);
    // Back to the picker: whatever was being cast isn't being recorded any
    // more, and the next pick opens its own session.
    releaseScreencast();
  }, [releaseScreencast]);

  // Loading skeleton that matches the SourcePicker layout
  if (sessionCheck === "loading") {
    return (
      <div style={{ display: "flex", flexDirection: "column", height: "100%" }}>
        <div style={{ maxWidth: 480, margin: "0 auto", padding: spacing.lg, width: "100%", boxSizing: "border-box", flexShrink: 0, display: "flex", justifyContent: "space-between", alignItems: "center" }}>
          <Skeleton width={80} height={32} borderRadius={radii.md} />
          <Skeleton width={180} height={24} borderRadius={radii.md} style={{ margin: "0 auto" }} />
          <div style={{ width: 80 }}></div> {/* placeholder for balance */}
        </div>
        <div style={{ maxWidth: 480, margin: "0 auto", padding: spacing.lg, paddingTop: 0, flex: 1, width: "100%", boxSizing: "border-box" }}>
          <Skeleton aspectRatio="2/1" borderRadius={radii.lg} style={{ marginBottom: spacing.lg }} />
          <Skeleton height={36} borderRadius={radii.md} style={{ marginBottom: spacing.md }} />
          {[1, 2, 3].map((i) => (
            <Skeleton key={i} height={48} borderRadius={radii.md} style={{ marginBottom: spacing.xs }} />
          ))}
          <Skeleton height={48} borderRadius={radii.lg} style={{ marginTop: spacing.lg }} />
        </div>
      </div>
    );
  }

  if (sessionCheck === "error") {
    return (
      <PageLayout
        onBack={onBack}
        icon={
          <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke={colors.status.danger} strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
            <circle cx="12" cy="12" r="10" />
            <line x1="15" y1="9" x2="9" y2="15" />
            <line x1="9" y1="9" x2="15" y2="15" />
          </svg>
        }
        title="Session Error"
        subtitle={checkError || "Unknown error"}
      />
    );
  }

  if (sessionCheck === "finished") {
    const label = sessionStatus === "complete" ? "Complete" : sessionStatus === "compiling" ? "Compiling" : sessionStatus === "failed" ? "Failed" : "Stopped";
    return (
      <PageLayout
        onBack={onBack}
        icon={
          <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke={colors.text.tertiary} strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
            <polyline points="20 6 9 17 4 12" />
          </svg>
        }
        title={`Session Already ${label}`}
        subtitle="This session is no longer recordable."
        actions={
          <Button variant="primary" size="lg" fullWidth onClick={() => onViewSession(token)}>
            View Timelapse
          </Button>
        }
      />
    );
  }

  return (
    <div style={{ position: "relative", height: "100%", overflow: "hidden" }}>
      <AnimatePresence mode="sync" initial={false} custom={captureFlowDirection}>
        {!captureSource ? (
          <motion.div
            key="source-picker"
            custom={captureFlowDirection}
            initial="enter"
            animate="center"
            exit="exit"
            variants={{
              enter: (direction: number) => ({ opacity: 0, x: direction > 0 ? 14 : -14 }),
              center: {
                opacity: 1,
                x: 0,
                transition: {
                  x: { type: "spring", stiffness: 460, damping: 36, mass: 0.7 },
                  opacity: { duration: 0.16, delay: 0.04, ease: "easeOut" },
                },
              },
              exit: (direction: number) => ({
                opacity: 0,
                x: direction > 0 ? -14 : 14,
                transition: {
                  x: { type: "spring", stiffness: 460, damping: 36, mass: 0.7 },
                  opacity: { duration: 0.14, ease: "easeOut" },
                },
              }),
            }}
            style={{ position: "absolute", inset: 0, display: "flex", flexDirection: "column", height: "100%" }}
          >
            <div style={{ maxWidth: 480, margin: "0 auto", padding: spacing.lg, paddingBottom: 0, display: "flex", justifyContent: "space-between", alignItems: "center", flexShrink: 0, width: "100%", boxSizing: "border-box", position: "relative" }}>
              {!isLinux && (
              <Button variant="secondary" size="sm" onClick={onBack} style={{...cardButtonStyle, zIndex: 1}}>
                {isMacOS ? (
                  <span>&larr; Gallery</span>
                ) : (
                  <span style={{ display: "inline-flex", alignItems: "center", gap: spacing.xs }}>
                    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                      <path d="m15 18-6-6 6-6" />
                    </svg>
                    <span>Gallery</span>
                  </span>
                )}
              </Button>
              )}
              <h2 style={{ position: "absolute", left: sessionStatus !== "pending" ? 100 : 0, right: sessionStatus !== "pending" ? 100 : 0, textAlign: "center", fontSize: fontSize.lg, fontWeight: fontWeight.bold, color: colors.text.primary, margin: 0, pointerEvents: "none" }}>
                {sessionStatus !== "pending" ? "Select source" : "What should Lookout capture?"}
              </h2>
              {sessionStatus !== "pending" && (
                <Button variant="danger" size="md" loading={stopping} onClick={handleStopClick} style={{ zIndex: 1 }}>
                  Stop Session
                </Button>
              )}
            </div>
            <div style={{ flex: 1, minHeight: 0 }}>
              <SourcePicker
                onSelect={handleSelectSource}
                submitLabel={sessionStatus === "active" || sessionStatus === "paused" ? "Resume Session" : "Start Capture"}
              />
            </div>
            {isPrompting && (
              <NamingModal
                loading={stopping}
                onConfirm={handleConfirmStop}
                onEditAndSave={handleEditAndSave}
                onResume={handleResumeFromModal}
              />
            )}
          </motion.div>
        ) : (
          <motion.div
            key={`recorder:${captureSource.map(s => `${s.type}:${s.id}`).join(',')}`}
            custom={captureFlowDirection}
            initial="enter"
            animate="center"
            exit="exit"
            variants={{
              enter: (direction: number) => ({ opacity: 0, x: direction > 0 ? 14 : -14 }),
              center: {
                opacity: 1,
                x: 0,
                transition: {
                  x: { type: "spring", stiffness: 460, damping: 36, mass: 0.7 },
                  opacity: { duration: 0.16, delay: 0.04, ease: "easeOut" },
                },
              },
              exit: (direction: number) => ({
                opacity: 0,
                x: direction > 0 ? -14 : 14,
                transition: {
                  x: { type: "spring", stiffness: 460, damping: 36, mass: 0.7 },
                  opacity: { duration: 0.14, ease: "easeOut" },
                },
              }),
            }}
            style={{ position: "absolute", inset: 0, height: "100%" }}
          >
            <LookoutProvider token={token} apiBaseUrl={API_BASE}>
              <DesktopRecorder
                token={token}
                source={captureSource}
                onChangeSource={handleChangeSource}
                onBack={onBack}
                onViewSession={onViewSession}
              />
            </LookoutProvider>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}
