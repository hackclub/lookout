import { useState, useEffect } from "react";
import {
  Button,
  Spinner,
  colors,
  spacing,
  fontSize,
  fontWeight,
  radii,
} from "@lookout/react";

import { invoke } from "../logger.js";
import { extractToken } from "../utils.js";
import { PageLayout } from "./PageLayout.js";

const API_BASE = "https://lookout.hackclub.com";

interface Program {
  name: string;
  newSessionUrl: string;
}

interface AddSessionPageProps {
  onBack: () => void;
  onStart: (token: string) => void;
}

/** Small info "i" with a hover tooltip. */
function InfoTooltip({ text }: { text: string }) {
  const [show, setShow] = useState(false);
  return (
    <span
      style={{ position: "relative", display: "inline-flex", verticalAlign: "middle" }}
      onMouseEnter={() => setShow(true)}
      onMouseLeave={() => setShow(false)}
    >
      <span
        aria-label={text}
        style={{
          width: 16,
          height: 16,
          borderRadius: "50%",
          border: `1px solid ${colors.border.default}`,
          color: colors.text.tertiary,
          fontSize: 11,
          fontWeight: fontWeight.bold,
          fontStyle: "italic",
          display: "inline-flex",
          alignItems: "center",
          justifyContent: "center",
          cursor: "help",
          userSelect: "none",
        }}
      >
        i
      </span>
      {show && (
        <span
          role="tooltip"
          style={{
            position: "absolute",
            bottom: "calc(100% + 6px)",
            left: "50%",
            transform: "translateX(-50%)",
            width: 240,
            padding: `${spacing.sm}px ${spacing.md}px`,
            background: colors.bg.surface,
            border: `1px solid ${colors.border.default}`,
            borderRadius: radii.md,
            fontSize: fontSize.xs,
            fontWeight: fontWeight.normal,
            fontStyle: "normal",
            color: colors.text.secondary,
            lineHeight: 1.5,
            textAlign: "center",
            boxShadow: "0 4px 16px rgba(0,0,0,0.18)",
            zIndex: 10,
            pointerEvents: "none",
          }}
        >
          {text}
        </span>
      )}
    </span>
  );
}

export function AddSessionPage({ onBack, onStart }: AddSessionPageProps) {
  const [programs, setPrograms] = useState<Program[]>([]);
  const [programsLoading, setProgramsLoading] = useState(true);
  const [launched, setLaunched] = useState<string | null>(null);

  const [link, setLink] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [showLink, setShowLink] = useState(false);

  // Fetch the program registry. Failures and empty lists are non-fatal — the
  // paste-a-link backup always remains available.
  useEffect(() => {
    let cancelled = false;
    fetch(`${API_BASE}/api/programs`)
      .then((r) => (r.ok ? r.json() : Promise.reject(new Error(`HTTP ${r.status}`))))
      .then((d) => {
        if (!cancelled) setPrograms(Array.isArray(d.programs) ? d.programs : []);
      })
      .catch((e) => {
        // Don't surface as a blocking error — just fall back to manual entry.
        console.warn("[programs] failed to load registry:", e);
        if (!cancelled) setPrograms([]);
      })
      .finally(() => {
        if (!cancelled) setProgramsLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const handleOpenProgram = async (program: Program) => {
    setError(null);
    setLaunched(program.name);
    try {
      await invoke("open_external_url", { url: program.newSessionUrl });
    } catch (e) {
      console.error("[programs] failed to open url:", e);
      setError("Couldn't open your browser. Try the link option below.");
      setLaunched(null);
    }
  };

  const handleStart = () => {
    const trimmed = link.trim();
    if (!trimmed) return;
    const token = extractToken(trimmed);
    if (!token) {
      setError("Couldn't find a valid session token in that link.");
      return;
    }
    setError(null);
    setLoading(true);
    onStart(token);
  };

  const hasPrograms = programs.length > 0;

  return (
    <PageLayout
      onBack={onBack}
      title="Start a recording"
      subtitle="Pick a program to start a timelapse"
      actions={
        <>
          {error && (
            <p style={{ fontSize: fontSize.sm, color: colors.status.danger, margin: 0, textAlign: "center" }}>
              {error}
            </p>
          )}
          {/* Backup: paste a lookout:// link, for when the deep link doesn't fire. */}
          {showLink ? (
            <>
              <input
                autoFocus
                type="text"
                value={link}
                onChange={(e) => {
                  setLink(e.target.value);
                  setError(null);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !loading) handleStart();
                }}
                placeholder="Paste a lookout:// link here"
                disabled={loading}
                style={{
                  width: "100%",
                  padding: `${spacing.md}px ${spacing.lg}px`,
                  fontSize: fontSize.md,
                  fontWeight: fontWeight.medium,
                  color: colors.text.primary,
                  background: colors.bg.sunken,
                  border: `1px solid ${error ? colors.status.danger : colors.border.default}`,
                  borderRadius: radii.lg,
                  outline: "none",
                  boxSizing: "border-box",
                  height: 48,
                  opacity: loading ? 0.5 : 1,
                }}
              />
              <Button
                variant="primary"
                size="lg"
                fullWidth
                disabled={!link.trim() || loading}
                loading={loading}
                onClick={handleStart}
              >
                Start from link
              </Button>
            </>
          ) : (
            <button
              onClick={() => setShowLink(true)}
              style={{
                background: "none",
                border: "none",
                color: colors.text.tertiary,
                fontSize: fontSize.sm,
                cursor: "pointer",
                padding: spacing.xs,
                textDecoration: "underline",
                alignSelf: "center",
              }}
            >
              Deep link not working? Paste a link instead
            </button>
          )}
        </>
      }
    >
      <div style={{ width: "100%", marginTop: spacing.lg, display: "flex", flexDirection: "column", gap: spacing.md }}>
        {programsLoading ? (
          <div style={{ display: "flex", justifyContent: "center", padding: spacing.xl }}>
            <Spinner size="md" />
          </div>
        ) : hasPrograms ? (
          <>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "center", gap: spacing.xs }}>
              <span style={{ fontSize: fontSize.sm, color: colors.text.tertiary }}>Programs</span>
              <InfoTooltip text="Not all programs support this feature. You may need to start the session on the program's website." />
            </div>
            <div style={{ display: "flex", flexDirection: "column", gap: spacing.sm, maxHeight: 280, overflowY: "auto" }}>
              {programs.map((p) => (
                <Button
                  key={p.name}
                  variant="primary"
                  size="lg"
                  fullWidth
                  loading={launched === p.name}
                  disabled={loading || (launched !== null && launched !== p.name)}
                  onClick={() => handleOpenProgram(p)}
                >
                  {p.name}
                </Button>
              ))}
            </div>
            {launched && (
              <p style={{ fontSize: fontSize.sm, color: colors.text.secondary, margin: 0, textAlign: "center", lineHeight: 1.5 }}>
                Opened <strong>{launched}</strong> in your browser — finish there and
                you'll be sent back here to start recording.
              </p>
            )}
          </>
        ) : (
          <p style={{ fontSize: fontSize.sm, color: colors.text.tertiary, margin: 0, textAlign: "center", lineHeight: 1.5 }}>
            No programs available right now. Open Lookout from a Hack Club site, or
            paste a link below.
          </p>
        )}
      </div>
    </PageLayout>
  );
}
