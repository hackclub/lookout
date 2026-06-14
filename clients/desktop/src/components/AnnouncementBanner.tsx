import { Button, colors, spacing, fontSize, radii } from "@lookout/react";
import { invoke } from "../logger.js";
import type { Announcement } from "../hooks/useAnnouncement.js";

const LEVEL_COLOR: Record<Announcement["level"], string> = {
  info: colors.status.info,
  success: colors.status.success,
  warning: colors.status.warning,
  danger: colors.status.danger,
};

/**
 * Gallery banner for an admin-authored announcement. Same shape as
 * UpdateBanner, tinted by level; if a URL is set, an "Open" button launches it
 * in the OS browser.
 */
export function AnnouncementBanner({ announcement }: { announcement: Announcement }) {
  const color = LEVEL_COLOR[announcement.level] ?? colors.status.info;

  const handleOpen = async () => {
    if (!announcement.url) return;
    try {
      await invoke("open_external_url", { url: announcement.url });
    } catch (e) {
      console.error("[announcement] failed to open url:", e);
    }
  };

  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        gap: spacing.md,
        padding: `${spacing.sm}px ${spacing.md}px`,
        background: color + "1a",
        border: `1px solid ${color}`,
        borderRadius: radii.md,
      }}
    >
      <span
        style={{
          fontSize: fontSize.sm,
          color: colors.text.primary,
          minWidth: 0,
          lineHeight: 1.4,
        }}
      >
        {announcement.message}
      </span>
      {announcement.url && (
        <Button variant="secondary" size="sm" onClick={handleOpen} style={{ flexShrink: 0 }}>
          Open
        </Button>
      )}
    </div>
  );
}
