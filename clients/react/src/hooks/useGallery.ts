import { useState, useEffect, useCallback } from "react";
import type { SessionSummary } from "@lookout/shared";

export interface UseGalleryOptions {
  apiBaseUrl: string;
  tokens: string[];
}

export interface UseGallery {
  sessions: SessionSummary[];
  loading: boolean;
  error: string | null;
  refresh(): void;
}

interface CachedSession {
  summary: SessionSummary;
  thumbnailUrlFetchedAt: number;
}
const globalSessionsCache: Record<string, CachedSession> = {};

export function useGallery({ apiBaseUrl, tokens }: UseGalleryOptions): UseGallery {
  const validTokens = tokens.filter((t) => /^[a-f0-9]{64}$/i.test(t));
  
  const initialSessions = validTokens
    .map(t => globalSessionsCache[t]?.summary)
    .filter((s): s is SessionSummary => s !== undefined);

  const hasAllInCache = validTokens.length > 0 && initialSessions.length === validTokens.length;

  const [sessions, setSessions] = useState<SessionSummary[]>(initialSessions);
  const [loading, setLoading] = useState(!hasAllInCache && validTokens.length > 0);
  const [error, setError] = useState<string | null>(null);
  const [refreshCounter, setRefreshCounter] = useState(0);

  // Stable reference for the token list to avoid infinite re-renders
  const tokensKey = tokens.join(",");

  const refresh = useCallback(() => setRefreshCounter((c) => c + 1), []);

  useEffect(() => {
    if (tokens.length === 0) {
      setSessions([]);
      setLoading(false);
      setError(null);
      return;
    }

    // Only send valid hex tokens to avoid server-side validation errors
    if (validTokens.length === 0) {
      setSessions([]);
      setLoading(false);
      return;
    }

    let cancelled = false;
    // Only show loading if we don't have cached data to prevent skeleton flash
    if (!hasAllInCache) {
      setLoading(true);
    }

    const BATCH_SIZE = 100;
    const chunks: string[][] = [];
    for (let i = 0; i < validTokens.length; i += BATCH_SIZE) {
      chunks.push(validTokens.slice(i, i + BATCH_SIZE));
    }

    Promise.all(
      chunks.map((chunk) =>
        fetch(`${apiBaseUrl}/api/sessions/batch`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ tokens: chunk }),
        }).then(async (res) => {
          if (!res.ok) {
            const text = await res.text().catch(() => "");
            throw new Error(`HTTP ${res.status} ${res.statusText}\n${text.slice(0, 500)}`);
          }
          return res.json() as Promise<{ sessions: SessionSummary[] }>;
        })
      )
    )
      .then((results) => ({ sessions: results.flatMap((r) => r.sessions ?? []) }))
      .then((data: { sessions: SessionSummary[] }) => {
        if (!cancelled) {
          const now = Date.now();
          const THUMBNAIL_EXPIRY = 45 * 60 * 1000; // 45 mins
          
          const mergedSessions = (data.sessions ?? []).map(newSession => {
            const cached = globalSessionsCache[newSession.token];
            let thumbnailUrl = newSession.thumbnailUrl;
            let fetchedAt = now;
            
            if (cached && cached.summary.thumbnailUrl) {
              const isImageSame = newSession.screenshotCount === cached.summary.screenshotCount;
              const isFresh = now - cached.thumbnailUrlFetchedAt < THUMBNAIL_EXPIRY;
              
              if (isImageSame && isFresh) {
                thumbnailUrl = cached.summary.thumbnailUrl;
                fetchedAt = cached.thumbnailUrlFetchedAt;
              }
            }
            
            const resultSession = { ...newSession, thumbnailUrl };
            globalSessionsCache[newSession.token] = {
              summary: resultSession,
              thumbnailUrlFetchedAt: fetchedAt
            };
            return resultSession;
          });
          
          setSessions(mergedSessions);
          setError(null);
        }
      })
      .catch((err) => {
        if (!cancelled) {
          console.warn("Gallery fetch error:", err instanceof Error ? err.message : err);
          setError(err.message);
          // Keep showing whatever sessions we had
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [apiBaseUrl, tokensKey, refreshCounter, hasAllInCache]);

  // Re-fetch on tab focus
  useEffect(() => {
    const handler = () => {
      if (document.visibilityState === "visible") refresh();
    };
    document.addEventListener("visibilitychange", handler);
    return () => document.removeEventListener("visibilitychange", handler);
  }, [refresh]);

  return { sessions, loading, error, refresh };
}
