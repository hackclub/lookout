import React, { useRef, useState, useEffect, useCallback } from "react";
import type { SessionSummary } from "@lookout/shared";
import { SessionCard } from "./SessionCard.js";
import { Button } from "../ui/Button.js";
import { ErrorDisplay } from "../ui/ErrorDisplay.js";
import { GallerySkeleton } from "../ui/Skeleton.js";
import { colors, spacing, fontSize, fontWeight, radii } from "../ui/theme.js";

export interface GalleryProps {
  sessions: SessionSummary[];
  loading: boolean;
  error: string | null;
  onSessionClick?: (token: string) => void;
  onArchive?: (token: string) => void;
  onRefresh?: () => void;
  onAdd?: () => void;
  onSettings?: () => void;
  /** Optional content rendered just below the header (e.g. an update banner). */
  banner?: React.ReactNode;
}

const addButtonStyle: React.CSSProperties = {
  borderRadius: radii.md,
  fontSize: fontSize.xxl,
  width: 36,
  height: 36,
  padding: 0,
};

function GalleryHeader({ onAdd, onSettings }: { onAdd?: () => void; onSettings?: () => void }) {
  return (
    <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", padding: spacing.lg, paddingBottom: 0, flexShrink: 0 }}>
      <h2 style={{ fontSize: fontSize.heading, fontWeight: fontWeight.bold, color: colors.text.primary, margin: 0 }}>Your Timelapses</h2>
      <div style={{ display: "flex", alignItems: "center", gap: spacing.xs }}>
        {onSettings && (
          <Button variant="ghost" size="sm" onClick={onSettings} title="Filter Apps" aria-label="Filter Apps" style={addButtonStyle}>
            <svg width="26" height="26" viewBox="0 0 32 32" fill="currentColor" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
              <path d="M1.64062 15.9375C1.64062 13.9271 2.00521 12.0573 2.73438 10.3281C3.46354 8.58854 4.47396 7.07292 5.76562 5.78125C7.06771 4.47917 8.58333 3.46354 10.3125 2.73438C12.0521 2.00521 13.9219 1.64062 15.9219 1.64062V30.2188C13.9219 30.2188 12.0521 29.8542 10.3125 29.125C8.58333 28.4062 7.06771 27.3958 5.76562 26.0938C4.47396 24.7917 3.46354 23.276 2.73438 21.5469C2.00521 19.8177 1.64062 17.9479 1.64062 15.9375ZM15.125 3.23438V1.1875H20.6875V3.23438H15.125ZM15.125 6.59375V4.85938H26.4375V6.59375H15.125ZM15.125 9.9375V8.20312H28.6719V9.9375H15.125ZM15.125 13.2969V11.5625H30.3438V13.2969H15.125ZM15.125 16.6406V14.9062H30.3438V16.6406H15.125ZM15.125 20V18.2656H30.3438V20H15.125ZM15.125 23.3594V21.625H28.6719V23.3594H15.125ZM15.125 27.0156V24.9688H26.4375V27.0156H15.125ZM15.125 30.6875V28.6406H20.6875V30.6875H15.125ZM15.9375 31.875C13.7396 31.875 11.6771 31.4583 9.75 30.625C7.82292 29.8021 6.13021 28.6615 4.67188 27.2031C3.21354 25.7448 2.06771 24.0521 1.23438 22.125C0.411458 20.1979 0 18.1354 0 15.9375C0 13.7396 0.411458 11.6771 1.23438 9.75C2.06771 7.82292 3.21354 6.13021 4.67188 4.67188C6.13021 3.20312 7.82292 2.05729 9.75 1.23438C11.6771 0.411458 13.7396 0 15.9375 0C18.1354 0 20.1979 0.411458 22.125 1.23438C24.0521 2.05729 25.7448 3.20312 27.2031 4.67188C28.6615 6.13021 29.8021 7.82292 30.625 9.75C31.4583 11.6771 31.875 13.7396 31.875 15.9375C31.875 18.1354 31.4583 20.1979 30.625 22.125C29.8021 24.0521 28.6615 25.7448 27.2031 27.2031C25.7448 28.6615 24.0521 29.8021 22.125 30.625C20.1979 31.4583 18.1354 31.875 15.9375 31.875ZM15.9375 29.2188C17.7708 29.2188 19.4896 28.875 21.0938 28.1875C22.6979 27.5 24.1094 26.5469 25.3281 25.3281C26.5469 24.1094 27.5 22.6979 28.1875 21.0938C28.875 19.4896 29.2188 17.7708 29.2188 15.9375C29.2188 14.1042 28.875 12.3854 28.1875 10.7812C27.5 9.16667 26.5469 7.75521 25.3281 6.54688C24.1094 5.32812 22.6979 4.375 21.0938 3.6875C19.4896 3 17.7708 2.65625 15.9375 2.65625C14.1042 2.65625 12.3854 3 10.7812 3.6875C9.17708 4.375 7.76562 5.32812 6.54688 6.54688C5.32812 7.75521 4.375 9.16667 3.6875 10.7812C3 12.3854 2.65625 14.1042 2.65625 15.9375C2.65625 17.7708 3 19.4896 3.6875 21.0938C4.375 22.6979 5.32812 24.1094 6.54688 25.3281C7.76562 26.5469 9.17708 27.5 10.7812 28.1875C12.3854 28.875 14.1042 29.2188 15.9375 29.2188Z" />
            </svg>
          </Button>
        )}
        {onAdd && (
          <Button variant="ghost" size="sm" onClick={onAdd} title="Start" style={addButtonStyle}>
            +
          </Button>
        )}
      </div>
    </div>
  );
}

// Global cache for gallery scroll position
let galleryScrollPosition = 0;

export function Gallery({
  sessions,
  loading,
  error,
  onSessionClick,
  onArchive,
  onRefresh,
  onAdd,
  onSettings,
  banner,
}: GalleryProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [showTopMask, setShowTopMask] = useState(false);
  const [showBottomMask, setShowBottomMask] = useState(false);

  const handleScroll = useCallback(() => {
    if (!scrollRef.current) return;
    const { scrollTop, scrollHeight, clientHeight } = scrollRef.current;
    galleryScrollPosition = scrollTop;
    setShowTopMask(scrollTop > 0);
    setShowBottomMask(Math.ceil(scrollTop + clientHeight) < scrollHeight);
  }, []);

  // Restore scroll position when sessions load or component mounts
  useEffect(() => {
    if (scrollRef.current && sessions.length > 0 && !loading) {
      scrollRef.current.scrollTop = galleryScrollPosition;
      handleScroll();
    }
  }, [sessions.length, loading, handleScroll]);

  useEffect(() => {
    handleScroll();
    window.addEventListener('resize', handleScroll);
    return () => window.removeEventListener('resize', handleScroll);
  }, [sessions, handleScroll]);

  if (loading && sessions.length === 0) {
    return <GallerySkeleton />;
  }

  if (error && sessions.length === 0) {
    return (
      <div style={{ display: "flex", flexDirection: "column", height: "100%" }}>
        <GalleryHeader onAdd={onAdd} onSettings={onSettings} />
      {banner && <div style={{ padding: spacing.lg, paddingBottom: 0 }}>{banner}</div>}
        <div style={{ flex: 1, display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", padding: spacing.xxl }}>
          <ErrorDisplay error={error} variant="inline" />
          {onRefresh && (
            <Button variant="primary" size="md" onClick={onRefresh} style={{ marginTop: spacing.md }}>
              Retry
            </Button>
          )}
        </div>
      </div>
    );
  }

  if (sessions.length === 0) {
    return (
      <div style={{ display: "flex", flexDirection: "column", height: "100%" }}>
        <GalleryHeader onAdd={onAdd} onSettings={onSettings} />
      {banner && <div style={{ padding: spacing.lg, paddingBottom: 0 }}>{banner}</div>}
        <div style={{ flex: 1, display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", padding: spacing.xxl }}>
          <p style={{ marginBottom: spacing.md }}>
            <svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke={colors.text.primary} strokeWidth="1.5" style={{ opacity: 0.2 }}>
              <rect x="2" y="3" width="20" height="14" rx="2" ry="2" />
              <line x1="8" y1="21" x2="16" y2="21" />
              <line x1="12" y1="17" x2="12" y2="21" />
            </svg>
          </p>
          <p style={{ fontSize: fontSize.lg, color: colors.text.primary, opacity: 0.5, textAlign: "center" }}>No timelapses yet</p>
          <p style={{ fontSize: fontSize.sm, color: colors.text.primary, opacity: 0.3, marginTop: spacing.xs, textAlign: "center" }}>
            Start a recording session to see it here.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%" }}>
      <GalleryHeader onAdd={onAdd} onSettings={onSettings} />
      {banner && <div style={{ padding: spacing.lg, paddingBottom: 0 }}>{banner}</div>}
      <div
        ref={scrollRef}
        onScroll={handleScroll}
        style={{
          flex: 1,
          overflowY: "auto",
          padding: spacing.lg,
          maskImage: `linear-gradient(to bottom, ${showTopMask ? 'transparent 0%, black 20px' : 'black 0%, black 20px'}, ${showBottomMask ? 'black calc(100% - 20px), transparent 100%' : 'black calc(100% - 20px), black 100%'})`,
          WebkitMaskImage: `linear-gradient(to bottom, ${showTopMask ? 'transparent 0%, black 20px' : 'black 0%, black 20px'}, ${showBottomMask ? 'black calc(100% - 20px), transparent 100%' : 'black calc(100% - 20px), black 100%'})`,
        }}
      >
        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(240px, 1fr))", gap: spacing.md }}>
          {sessions.map((s) => (
            <SessionCard
              key={s.token}
              session={s}
              onClick={() => onSessionClick?.(s.token)}
              onArchive={onArchive ? () => onArchive(s.token) : undefined}
            />
          ))}
        </div>
      </div>
    </div>
  );
}
