/**
 * The "pick a source again" prompt.
 *
 * When a screencast ends underneath a live recording — the user hits Stop in
 * their compositor's sharing indicator, or PipeWire restarts — the app cannot
 * quietly reconnect. Portal sessions are opened with `PersistMode::DoNot`, so
 * there is no restore token, and re-opening a cast means going back through
 * the picker. The recording itself is fine; every frame already uploaded is
 * saved and the session keeps its tracked time.
 *
 * So this asks, rather than leaving a red banner next to a UI that still looks
 * like it's recording. It has no dismiss on purpose: behind it sits a recorder
 * that can't record, and both ways out of that state are buttons here.
 */

import { Button, Overlay, colors, spacing, fontSize, fontWeight } from "@lookout/react";
import type { CaptureSourceLoss } from "../recorder.js";

interface SourceLostPromptProps {
  loss: CaptureSourceLoss;
  /** Back to the source picker. The session stays open. */
  onReselect: () => void;
  /** Give up and end the recording. */
  onStop: () => void;
  /** Show the raw capture error. Only offered when there is one. */
  onDetails?: () => void;
  /** True while the stop is in flight. */
  stopping?: boolean;
}

export function SourceLostPrompt({
  loss,
  onReselect,
  onStop,
  onDetails,
  stopping = false,
}: SourceLostPromptProps) {
  const title =
    loss.reason === "revoked" ? "Screen sharing was stopped" : "Lost the screen being captured";

  return (
    <Overlay label={title} width="min(420px, 92vw)">
      <div style={{ display: "flex", flexDirection: "column", padding: spacing.xxl, gap: spacing.lg }}>
        <div>
          <h2 style={{ margin: 0, fontSize: fontSize.xxl, fontWeight: fontWeight.semibold, color: colors.text.primary }}>
            {title}
          </h2>
          <p style={{ margin: `${spacing.sm}px 0 0 0`, fontSize: fontSize.lg, lineHeight: 1.5, color: colors.text.secondary }}>
            {loss.message} Pick a source again to carry on recording — everything captured so far is
            saved, and the session keeps its time.
          </p>
        </div>

        <div style={{ display: "flex", flexDirection: "column", gap: spacing.sm }}>
          <Button variant="primary" size="lg" fullWidth onClick={onReselect} disabled={stopping}>
            Select source again
          </Button>
          <div style={{ display: "flex", gap: spacing.sm }}>
            <Button
              variant="secondary"
              size="md"
              loading={stopping}
              onClick={onStop}
              style={{ flex: 1 }}
            >
              Stop recording
            </Button>
            {onDetails && (
              <Button variant="ghost" size="md" onClick={onDetails} disabled={stopping} style={{ flexShrink: 0 }}>
                Details
              </Button>
            )}
          </div>
        </div>
      </div>
    </Overlay>
  );
}
