import type { ReactNode } from "react";
import { colors, fontSize, fontWeight, spacing } from "@lookout/react";

export interface MessageProps {
  title: string;
  children: ReactNode;
}

/** A dead end, stated plainly. Used for the states where there is nothing
 *  to record: no token, a token the server doesn't know, a finished
 *  session. */
export function Message({ title, children }: MessageProps) {
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
        {title}
      </h1>
      <div
        style={{
          fontSize: fontSize.lg,
          color: colors.text.secondary,
          lineHeight: 1.6,
        }}
      >
        {children}
      </div>
    </div>
  );
}
