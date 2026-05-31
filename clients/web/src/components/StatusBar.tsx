import { useSessionTimer, formatTime } from "@lookout/react";

interface StatusBarProps {
  trackedSeconds: number;
  isActive: boolean;
  screenshotCount: number;
  uploadPending: number;
  uploadCompleted: number;
  uploadFailed: number;
}

export function StatusBar({
  trackedSeconds,
  isActive,
  screenshotCount,
  uploadPending,
  uploadCompleted,
  uploadFailed,
}: StatusBarProps) {
  const displayTime = useSessionTimer(trackedSeconds, isActive);

  return (
    <div style={styles.bar}>
      <div style={styles.time}>{formatTime(displayTime)}</div>
      <div style={styles.stats}>
        <span>{screenshotCount + uploadCompleted} screenshots</span>
        {uploadPending > 0 && (
          <span style={styles.pending}>{uploadPending} uploading...</span>
        )}
        {uploadFailed > 0 && (
          <span style={styles.failed}>{uploadFailed} failed</span>
        )}
      </div>
    </div>
  );
}

const styles: Record<string, React.CSSProperties> = {
  bar: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    padding: "12px 20px",
    background: "#1a1a1a",
    borderRadius: 8,
    marginBottom: 16,
  },
  time: {
    fontSize: 32,
    fontWeight: 700,
    fontVariantNumeric: "tabular-nums",
    color: "#fff",
  },
  stats: {
    display: "flex",
    gap: 16,
    fontSize: 14,
    color: "#888",
  },
  pending: { color: "#f59e0b" },
  failed: { color: "#ef4444" },
};
