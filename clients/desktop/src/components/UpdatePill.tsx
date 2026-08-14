import { useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import NumberFlow from "@number-flow/react";
import { colors, fontSize, fontWeight } from "@lookout/react";
import type { UpdatePhase } from "../hooks/useAppUpdate.js";

interface UpdatePillProps {
  phase: UpdatePhase;
  onRestart: () => void;
  /** Which screen edge the pill is anchored to — sets the slide direction. */
  origin?: "top" | "bottom";
}

/** Tiny circular progress ring for the downloading state. */
function ProgressRing({ progress }: { progress: number }) {
  const r = 5;
  const c = 2 * Math.PI * r;
  return (
    <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true" style={{ display: "block", flexShrink: 0 }}>
      <circle cx="6" cy="6" r={r} fill="none" stroke={colors.border.default} strokeWidth="1.5" />
      <circle
        cx="6"
        cy="6"
        r={r}
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeDasharray={c}
        strokeDashoffset={c * (1 - progress / 100)}
        transform="rotate(-90 6 6)"
        style={{ transition: "stroke-dashoffset 0.2s ease" }}
      />
    </svg>
  );
}

function PowerIcon() {
  return (
    <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" style={{ display: "block", flexShrink: 0, marginTop: 0.5 }}>
      <path d="M12 2v10" />
      <path d="M18.4 6.6a9 9 0 1 1-12.77.04" />
    </svg>
  );
}

/**
 * Ghostty-style update pill that lives in the titlebar. Shows download
 * progress while an update streams in, then becomes a "Restart to Complete
 * Update" button. Renders nothing when no update is in flight.
 */
export function UpdatePill({ phase, onRestart, origin = "top" }: UpdatePillProps) {
  const [hovered, setHovered] = useState(false);
  const clickable = phase.state === "ready";
  // Slide in from whichever edge the pill is anchored to.
  const offset = origin === "bottom" ? 8 : -8;
  // Ready/installing render as a solid, borderless capsule (inverted colors);
  // downloading stays a quiet outlined pill.
  const solid = phase.state === "ready" || phase.state === "installing";

  return (
    <AnimatePresence>
      {phase.state !== "idle" && (
        <motion.button
          initial={{ opacity: 0, y: offset, scale: 0.95 }}
          animate={{ opacity: 1, y: 0, scale: 1 }}
          exit={{ opacity: 0, y: offset, scale: 0.95, transition: { duration: 0.15 } }}
          transition={{ type: "spring", stiffness: 450, damping: 32 }}
          disabled={!clickable}
          onClick={onRestart}
          onMouseEnter={() => setHovered(true)}
          onMouseLeave={() => setHovered(false)}
          title={
            phase.state === "downloading"
              ? `Downloading v${phase.version}`
              : phase.state === "installing"
                ? `Installing v${phase.version}`
                : `Restart to update to v${phase.version}`
          }
          style={{
            display: "inline-flex",
            alignItems: "center",
            gap: 6,
            height: 22,
            padding: "0 10px",
            borderRadius: 999,
            border: `1px solid ${solid ? "transparent" : colors.border.default}`,
            background: solid
              ? clickable && hovered
                ? `color-mix(in srgb, ${colors.text.primary} 19%, transparent)`
                : `color-mix(in srgb, ${colors.text.primary} 16%, transparent)`
              : colors.bg.surface,
            color: solid ? colors.text.primary : colors.text.secondary,
            fontSize: fontSize.xs,
            fontWeight: fontWeight.medium,
            fontFamily: "inherit",
            whiteSpace: "nowrap",
            cursor: "default",
            transition: "background 0.15s ease, color 0.15s ease, border-color 0.15s ease",
            WebkitAppRegion: "no-drag",
          } as React.CSSProperties}
        >
          {phase.state === "downloading" ? (
            <>
              <ProgressRing progress={phase.progress} />
              <span>
                Downloading Update…{" "}
                <NumberFlow
                  value={phase.progress}
                  suffix="%"
                  style={{ fontVariantNumeric: "tabular-nums" }}
                />
              </span>
            </>
          ) : (
            <>
              <PowerIcon />
              <span>
                {phase.state === "installing"
                  ? "Installing Update…"
                  : "Restart to Complete Update"}
              </span>
            </>
          )}
        </motion.button>
      )}
    </AnimatePresence>
  );
}
