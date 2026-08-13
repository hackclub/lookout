import type { ReactNode } from "react";
import {
  Card,
  colors,
  fontSize,
  fontWeight,
  radii,
  spacing,
  type SessionStatus,
} from "@lookout/react";
import type { Capabilities } from "../capabilities.js";

export type Source = "desktop" | "screen" | "camera";

export interface SourceChooserProps {
  /** The session's name, or null when it only has the server's generated
   *  placeholder. */
  sessionName: string | null;
  status: SessionStatus;
  capabilities: Capabilities;
  onChoose: (source: Source) => void;
}

/**
 * The one screen this app adds on top of the SDK: pick where the frames
 * come from before the recorder mounts.
 *
 * It has to exist here rather than inside `LookoutRecorder` because the
 * capture mode is provider-level config — the recorder reads
 * `capture.mode` and adapts, it doesn't switch. So the choice is made
 * above the provider, and the provider is mounted with the answer.
 */
export function SourceChooser({
  sessionName,
  status,
  capabilities,
  onChoose,
}: SourceChooserProps) {
  const resuming = status === "active" || status === "paused";
  const heading = resuming ? "Continue recording" : "Start recording";

  return (
    <div>
      <header style={{ marginBottom: spacing.xxl }}>
        {/* With a real name the heading is the name and the action becomes
            the small line above it. Without one there is only the action. */}
        {sessionName && (
          <div
            style={{
              fontSize: fontSize.sm,
              fontWeight: fontWeight.medium,
              color: colors.text.tertiary,
              textTransform: "uppercase",
              letterSpacing: 0.6,
              marginBottom: spacing.xs,
            }}
          >
            {heading}
          </div>
        )}
        <h1
          style={{
            fontSize: fontSize.display,
            fontWeight: fontWeight.semibold,
            color: colors.text.primary,
            lineHeight: 1.25,
          }}
        >
          {sessionName ?? heading}
        </h1>
        {/* Only when it says something the user doesn't already know. There
            is no line explaining what Lookout is: they got here because a
            program sent them here to record, and the options below describe
            themselves. */}
        {resuming && (
          <p
            style={{
              marginTop: spacing.sm,
              fontSize: fontSize.lg,
              color: colors.text.secondary,
              lineHeight: 1.5,
            }}
          >
            This session is already running. Anything you record now gets added
            to it.
          </p>
        )}
      </header>

      <div style={{ display: "grid", gap: spacing.md }}>
        {capabilities.desktopApp && (
          <Option
            icon={<DesktopIcon />}
            title="Lookout desktop app"
            description="Records your whole screen and keeps going while you work in other apps. Best quality, and it carries on if you close this page."
            badge="Recommended"
            onChoose={() => onChoose("desktop")}
          />
        )}

        <Option
          icon={<MonitorIcon />}
          title="This browser"
          description="Pick a screen or window to share. This tab has to stay open while you record."
          onChoose={() => onChoose("screen")}
          unavailable={
            capabilities.screen
              ? undefined
              : "This browser can't share a screen. Try the desktop app or use a camera."
          }
        />

        <Option
          icon={<CameraIcon />}
          title="Camera"
          description="Use a webcam instead of a screen. Good for projects you build away from a computer."
          onChoose={() => onChoose("camera")}
          unavailable={
            capabilities.camera ? undefined : "No camera is available to this browser."
          }
        />
      </div>
    </div>
  );
}

interface OptionProps {
  icon: ReactNode;
  title: string;
  description: string;
  badge?: string;
  onChoose: () => void;
  /** When set, the option renders inert and explains itself instead. */
  unavailable?: string;
}

function Option({
  icon,
  title,
  description,
  badge,
  onChoose,
  unavailable,
}: OptionProps) {
  const body = (
    <div
      style={{
        display: "flex",
        gap: spacing.lg,
        alignItems: "flex-start",
        padding: spacing.lg,
        opacity: unavailable ? 0.55 : 1,
      }}
    >
      <div
        style={{
          flexShrink: 0,
          width: 40,
          height: 40,
          borderRadius: radii.md,
          background: colors.bg.sunken,
          border: `1px solid ${colors.border.default}`,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          color: colors.text.secondary,
        }}
      >
        {icon}
      </div>
      <div style={{ minWidth: 0 }}>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: spacing.sm,
            marginBottom: spacing.xs,
          }}
        >
          <span
            style={{
              fontSize: fontSize.xl,
              fontWeight: fontWeight.semibold,
              color: colors.text.primary,
            }}
          >
            {title}
          </span>
          {badge && !unavailable && (
            <span
              style={{
                fontSize: fontSize.xs,
                fontWeight: fontWeight.medium,
                // Neutral, not the SDK's green badge: green reads as a
                // status here, and this is a suggestion.
                color: colors.text.primary,
                background: colors.bg.selected,
                borderRadius: radii.sm,
                padding: "2px 6px",
                whiteSpace: "nowrap",
              }}
            >
              {badge}
            </span>
          )}
        </div>
        <p
          style={{
            fontSize: fontSize.md,
            color: colors.text.secondary,
            lineHeight: 1.5,
          }}
        >
          {unavailable ?? description}
        </p>
      </div>
    </div>
  );

  if (unavailable) {
    return <Card>{body}</Card>;
  }
  return <Card onClick={onChoose}>{body}</Card>;
}

// ── Icons ──────────────────────────────────────────────────
// Inline rather than pulled from an icon package: three glyphs is not
// worth a dependency, and this page should ship as little JS as it can.

const iconProps = {
  width: 20,
  height: 20,
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.8,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
  "aria-hidden": true,
};

/** A laptop. Deliberately a different silhouette from the browser window
 *  below — the two top options are the ones a user has to tell apart at a
 *  glance, and two rectangles-on-a-stand would not help them. */
function DesktopIcon() {
  return (
    <svg {...iconProps}>
      <rect x="4" y="4" width="16" height="11" rx="1.5" />
      <path d="M2 19h20" />
      <path d="M4 15h16" />
    </svg>
  );
}

/** A browser window: chrome bar and all. */
function MonitorIcon() {
  return (
    <svg {...iconProps}>
      <rect x="2" y="4" width="20" height="16" rx="2" />
      <path d="M2 9h20" />
      <path d="M5.5 6.5h.01" />
      <path d="M8.5 6.5h.01" />
    </svg>
  );
}

function CameraIcon() {
  return (
    <svg {...iconProps}>
      <path d="M23 7l-7 5 7 5V7z" />
      <rect x="1" y="5" width="15" height="14" rx="2" />
    </svg>
  );
}
