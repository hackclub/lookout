import { useEffect, useState } from "react";

const API_BASE = "https://lookout.hackclub.com";
// Re-check for a new/cleared announcement while the app stays open.
const CHECK_INTERVAL_MS = 15 * 60_000; // 15 minutes

export interface Announcement {
  level: "info" | "success" | "warning" | "danger";
  message: string;
  url: string | null;
}

/**
 * Polls the server's public announcement endpoint — once on open and every
 * ~15 min thereafter — so the gallery can show an admin-authored banner.
 * Failures and the no-announcement case both resolve to null; the banner is
 * simply not shown. Never blocks or interrupts the user.
 */
export function useAnnouncement(): Announcement | null {
  const [announcement, setAnnouncement] = useState<Announcement | null>(null);

  useEffect(() => {
    let cancelled = false;

    const fetchOnce = async () => {
      try {
        const res = await fetch(`${API_BASE}/api/announcement`);
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const data = await res.json();
        if (!cancelled) setAnnouncement(data.announcement ?? null);
      } catch (e) {
        // Non-fatal — keep whatever we last had (or null) and try again later.
        console.warn("[announcement] fetch failed:", e);
      }
    };

    fetchOnce();
    const id = setInterval(fetchOnce, CHECK_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, []);

  return announcement;
}
