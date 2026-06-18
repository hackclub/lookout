import React from "react";
import { motion } from "motion/react";
import { colors, radii, fontSize, fontWeight, spacing } from "./theme.js";
import { Button } from "./Button.js";

export interface ErrorDisplayProps {
  error: string;
  variant?: "banner" | "inline" | "page";
  title?: string;
  /** Calm, plain-language note shown under the technical error (e.g. "Your
   *  earlier progress is saved."). Kept separate so the copyable error text
   *  stays purely diagnostic. */
  reassurance?: string;
  onDismiss?: () => void;
  onCopy?: () => void;
  action?: { label: string; onClick: () => void };
}

export function ErrorDisplay({ error, variant = "banner", title, reassurance, onDismiss, onCopy, action }: ErrorDisplayProps) {
  if (variant === "page") {
    return (
      <div style={{ display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", padding: spacing.xxl, textAlign: "center" }}>
        <h2 style={{ fontSize: fontSize.heading, fontWeight: fontWeight.bold, color: colors.status.danger, marginBottom: spacing.sm }}>{title || "Error"}</h2>
        <pre style={{ margin: 0, fontSize: fontSize.xs, fontFamily: "monospace", whiteSpace: "pre-wrap", wordBreak: "break-all", maxWidth: 400, maxHeight: 150, overflowY: "auto", color: colors.text.error, marginBottom: spacing.lg }}>{error}</pre>
        <div style={{ display: "flex", gap: spacing.sm, justifyContent: "center" }}>
          {action && <Button variant="primary" size="md" onClick={action.onClick}>{action.label}</Button>}
          {onCopy && <Button variant="secondary" size="md" onClick={onCopy}>Copy Error</Button>}
        </div>
      </div>
    );
  }

  if (variant === "inline") {
    return (
      <pre style={{ margin: 0, fontSize: fontSize.xs, fontFamily: "monospace", whiteSpace: "pre-wrap", wordBreak: "break-all", color: colors.text.error, maxHeight: 150, overflowY: "auto" }}>{error}</pre>
    );
  }

  // banner (default)
  return (
    <div style={{ padding: `${spacing.md}px ${spacing.lg}px`, marginBottom: spacing.md, background: "rgba(239,68,68,0.15)", border: `1px solid ${colors.status.danger}`, borderRadius: radii.md, color: colors.text.error, fontSize: fontSize.md, display: "flex", alignItems: "flex-start", gap: spacing.sm }}>
      <div style={{ flex: 1, margin: 0, fontSize: fontSize.xs, whiteSpace: "pre-wrap", wordBreak: "break-all", maxHeight: 120, overflowY: "auto" }}>
        {title && <strong style={{ display: "block", marginBottom: spacing.xs }}>{title}</strong>}
        <span style={{ fontFamily: "monospace" }}>{error}</span>
        {reassurance && <span style={{ opacity: 0.8 }}>{`\n\n${reassurance}`}</span>}
      </div>
      {onCopy && (
        <motion.button 
          onClick={onCopy} 
          whileTap="active"
          initial="idle"
          style={{ background: "transparent", border: "none", color: colors.text.error, cursor: "pointer", fontSize: fontSize.xs, lineHeight: 1, padding: "2px 8px", borderRadius: radii.sm, whiteSpace: "nowrap" as const, position: "relative" }}
        >
          <motion.div
            variants={{ idle: { scale: 1 }, active: { scale: 0.96 } }}
            transition={{ type: "spring", stiffness: 1500, damping: 60 }}
            style={{ position: "absolute", inset: 0, borderRadius: radii.sm, border: "1px solid " + colors.text.error, zIndex: 0 }}
          />
          <span style={{ position: "relative", zIndex: 1, display: "inline-block" }}>
            Copy
          </span>
        </motion.button>
      )}
      {onDismiss && (
        <motion.button 
          onClick={onDismiss} 
          whileTap="active"
          initial="idle"
          style={{ background: "none", border: "none", color: colors.text.error, cursor: "pointer", fontSize: fontSize.xl, lineHeight: 1, padding: 0, position: "relative" }}
        >
          <motion.div
            variants={{ idle: { scale: 1 }, active: { scale: 0.96 } }}
            transition={{ type: "spring", stiffness: 1500, damping: 60 }}
            style={{ position: "absolute", inset: -2, borderRadius: "50%", background: "transparent", zIndex: 0 }}
          />
          <span style={{ position: "relative", zIndex: 1, display: "inline-block" }}>
            &times;
          </span>
        </motion.button>
      )}
    </div>
  );
}
