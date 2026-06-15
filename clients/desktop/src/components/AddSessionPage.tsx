import { useState, useEffect, type CSSProperties } from "react";
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
  // Human-friendly label to show users; the server falls back to `name` when
  // unset, so this is always present, but guard anyway for older servers.
  displayName?: string;
  newSessionUrl: string;
}

interface AddSessionPageProps {
  onBack: () => void;
  onStart: (token: string) => void;
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

  const programLabel = (p: Program) => p.displayName || p.name;

  const handleOpenProgram = async (program: Program) => {
    setError(null);
    setLaunched(programLabel(program));
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

  const note: CSSProperties = {
    fontSize: fontSize.sm,
    color: colors.text.tertiary,
    margin: 0,
    textAlign: "center",
    lineHeight: 1.5,
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
            <p style={{ ...note, color: colors.status.danger }}>{error}</p>
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
      <div
        style={{
          width: "100%",
          marginTop: spacing.lg,
          display: "flex",
          flexDirection: "column",
          gap: spacing.md,
        }}
      >
        {programsLoading ? (
          <Spinner size="md" />
        ) : hasPrograms ? (
          <>
            <div
              style={{
                display: "flex",
                flexDirection: "column",
                gap: spacing.sm,
              }}
            >
              {programs.map((p) => (
                <Button
                  key={p.name}
                  variant="primary"
                  size="lg"
                  fullWidth
                  loading={launched === programLabel(p)}
                  disabled={loading || (launched !== null && launched !== programLabel(p))}
                  onClick={() => handleOpenProgram(p)}
                >
                  {programLabel(p)}
                </Button>
              ))}
            </div>
            {launched ? (
              <p style={{ ...note, color: colors.text.secondary }}>
                Opened <strong>{launched}</strong> in your browser — finish there and
                you'll be sent back here to start recording.
              </p>
            ) : (
              <p style={note}>
                Not all programs support this. You may need to start the session on
                the program's website.
              </p>
            )}
          </>
        ) : (
          <p style={note}>
            No programs available right now. Open Lookout from a Hack Club site, or
            paste a link below.
          </p>
        )}
      </div>
    </PageLayout>
  );
}
