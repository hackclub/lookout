import { Button, colors, spacing, fontSize, fontWeight, radii } from "@lookout/react";

interface UpdateBannerProps {
  version: string;
  restarting?: boolean;
  onRestart: () => void;
}

/** Gallery banner shown when a newer app version is available. */
export function UpdateBanner({ version, restarting, onRestart }: UpdateBannerProps) {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        gap: spacing.md,
        padding: `${spacing.sm}px ${spacing.md}px`,
        background: colors.status.info + "1a",
        border: `1px solid ${colors.status.info}`,
        borderRadius: radii.md,
      }}
    >
      <div style={{ display: "flex", flexDirection: "column", minWidth: 0 }}>
        <span style={{ fontSize: fontSize.sm, fontWeight: fontWeight.bold, color: colors.text.primary }}>
          Update available
        </span>
        <span style={{ fontSize: fontSize.xs, color: colors.text.secondary }}>
          Version {version} is ready — restart to update.
        </span>
      </div>
      <Button
        variant="primary"
        size="sm"
        loading={restarting}
        onClick={onRestart}
        style={{ flexShrink: 0 }}
      >
        {restarting ? "Updating…" : "Restart"}
      </Button>
    </div>
  );
}
