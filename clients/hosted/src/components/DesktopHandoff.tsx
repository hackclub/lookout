import { useEffect, useRef, useState } from "react";
import {
  Button,
  colors,
  fontSize,
  fontWeight,
  radii,
  spacing,
} from "@lookout/react";
import { desktopHandoffUrl } from "../token.js";

export interface DesktopHandoffProps {
  token: string;
  /** Back to the chooser. */
  onBack: () => void;
}

/** How long to look like we're opening the app before admitting it might
 *  not be installed. Long enough that the OS handoff usually wins the
 *  race; short enough that a user without the app isn't left staring. */
const FALLBACK_DELAY_MS = 2500;

/**
 * Hands the session to the installed desktop app over `lookout://`, then
 * gets out of the way.
 *
 * There is deliberately no detection of whether it worked. Every trick for
 * that (blur races, timing heuristics) is wrong often enough to accuse a
 * working install of being missing, so instead the page just offers the
 * two escape hatches — retry, or record here — and lets the user say.
 */
export function DesktopHandoff({ token, onBack }: DesktopHandoffProps) {
  const [showFallback, setShowFallback] = useState(false);
  // The chooser click is the user activation that lets a protocol handler
  // fire. Spend it immediately, once — StrictMode double-invokes effects
  // in dev and a second navigation would re-prompt.
  const launched = useRef(false);

  useEffect(() => {
    if (!launched.current) {
      launched.current = true;
      open(token);
    }
    const timer = setTimeout(() => setShowFallback(true), FALLBACK_DELAY_MS);
    return () => clearTimeout(timer);
  }, [token]);

  return (
    <div style={{ textAlign: "center", paddingTop: spacing.xxxl }}>
      <h1
        style={{
          fontSize: fontSize.heading,
          fontWeight: fontWeight.semibold,
          color: colors.text.primary,
          marginBottom: spacing.sm,
        }}
      >
        Opening the Lookout app…
      </h1>
      <p
        style={{
          fontSize: fontSize.lg,
          color: colors.text.secondary,
          lineHeight: 1.5,
        }}
      >
        Your browser might ask for permission to open it. Once the app has the
        session you can close this page.
      </p>

      {showFallback && (
        <div
          style={{
            marginTop: spacing.xxl,
            padding: spacing.lg,
            background: colors.bg.sunken,
            border: `1px solid ${colors.border.default}`,
            borderRadius: radii.lg,
            textAlign: "left",
          }}
        >
          <div
            style={{
              fontSize: fontSize.lg,
              fontWeight: fontWeight.medium,
              color: colors.text.primary,
              marginBottom: spacing.xs,
            }}
          >
            Nothing happened?
          </div>
          <p
            style={{
              fontSize: fontSize.md,
              color: colors.text.secondary,
              lineHeight: 1.5,
              marginBottom: spacing.lg,
            }}
          >
            You might not have the app installed yet. Install it and open this
            page again, or record in this browser instead.
          </p>
          <div style={{ display: "flex", gap: spacing.sm, flexWrap: "wrap" }}>
            <Button onClick={() => open(token)} variant="secondary" size="sm">
              Try again
            </Button>
            {/* The origin root is the download page. */}
            <Button
              onClick={() => {
                window.location.href = "/";
              }}
              variant="secondary"
              size="sm"
            >
              Download Lookout
            </Button>
            <Button onClick={onBack} variant="ghost" size="sm">
              Record here instead
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

function open(token: string) {
  window.location.href = desktopHandoffUrl(token);
}
