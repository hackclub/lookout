import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AnimatePresence, motion, useIsPresent } from "motion/react";
import {
  LookoutProvider,
  LookoutRecorder,
  PageContainer,
  Spinner,
  createLookoutClient,
  colors,
  spacing,
  type CaptureMode,
  type SessionStatus,
} from "@lookout/react";
import { detectCapabilities, readEnvironment } from "./capabilities.js";
import { displayName } from "./sessionName.js";
import { readUrlOptions } from "./token.js";
import { DesktopHandoff } from "./components/DesktopHandoff.js";
import { Message } from "./components/Message.js";
import { SourceChooser, type Source } from "./components/SourceChooser.js";

/**
 * The recorder Lookout hosts itself, at /session?token=… — the URL the API
 * already hands to programs when they create a session.
 *
 * Scope is deliberately one thing: record into the session this token names.
 * There is no gallery and no session browser here. A token is a capability,
 * and a page that turns one into a browsable archive is a different product
 * with a different threat model.
 */

// Same origin as the API — this build is served by the Lookout server
// itself. The env var exists so `npm run dev` can point at a remote server.
const API_BASE = import.meta.env.VITE_API_BASE_URL ?? "";

type Load =
  | { phase: "loading" }
  | { phase: "failed"; kind: "unknown-token" | "unreachable" }
  | { phase: "ready"; name: string | null; status: SessionStatus };

/** Statuses that can still take captures. */
const RECORDABLE: readonly SessionStatus[] = ["pending", "active", "paused"];
/** Statuses where the session is over and nothing here applies. */
const FINISHED: readonly SessionStatus[] = ["complete", "failed"];

export function App() {
  const url = useMemo(() => readUrlOptions(window.location.search), []);
  const capabilities = useMemo(() => detectCapabilities(readEnvironment()), []);
  const [load, setLoad] = useState<Load>({ phase: "loading" });
  const [source, setSource] = useState<Source | null>(null);
  // Capture has actually begun. Until it has, changing your mind is free —
  // after it has, the offer to go back would be an offer to throw the
  // recording away, so it stops being made.
  const [sharing, setSharing] = useState(false);
  const callbacks = useMemo(() => ({ onShareStart: () => setSharing(true) }), []);
  const { height, measure } = useMeasuredHeight();

  const { token } = url;

  // One status read before anything mounts. It decides whether the user is
  // offered a chooser at all — asking someone to pick a capture source for
  // a session that finished last week is worse than saying so.
  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    createLookoutClient({ baseUrl: API_BASE, token })
      .getSession()
      .then((session) => {
        if (cancelled) return;
        setLoad({
          phase: "ready",
          name: displayName(session.name),
          status: session.status,
        });
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        // 404 is the interesting one: a token the server has never seen,
        // which almost always means a truncated or stale link. Everything
        // else is the network being the network.
        const status = (err as { status?: number })?.status;
        setLoad({
          phase: "failed",
          kind: status === 404 ? "unknown-token" : "unreachable",
        });
      });
    return () => {
      cancelled = true;
    };
  }, [token]);

  const view = renderView();

  return (
    <PageContainer maxWidth={640} style={{ paddingTop: spacing.xxxl }}>
      {/* Views cross-fade in place while the frame eases between their
          heights. Clipped while it moves, so the incoming view is revealed by
          the frame opening rather than overflowing it. */}
      <motion.div
        initial={false}
        animate={{ height: height ?? "auto" }}
        transition={{ duration: 0.26, ease: [0.32, 0.72, 0, 1] }}
        style={{ position: "relative", overflow: "hidden" }}
      >
        <AnimatePresence initial={false}>
          <ViewLayer key={view.key} measure={measure}>
            {view.node}
          </ViewLayer>
        </AnimatePresence>
      </motion.div>
      <Footer />
    </PageContainer>
  );

  /** The current view, plus the key that decides what counts as a
   *  transition. The recorder keeps one key across the whole recording:
   *  re-keying it would remount the provider and drop the capture loop. */
  function renderView(): { key: string; node: React.ReactNode } {
    if (!token) {
      return {
        key: "no-token",
        node: (
          <Message title="You need a session link">
            Open the link the program gave you and this page will pick things
            up from there. You can't start a new session here.
          </Message>
        ),
      };
    }

    if (load.phase === "loading") {
      return {
        key: "loading",
        node: (
          <div
            style={{
              display: "flex",
              justifyContent: "center",
              padding: spacing.xxxl,
            }}
          >
            <Spinner />
          </div>
        ),
      };
    }

    if (load.phase === "failed") {
      return {
        key: `failed:${load.kind}`,
        node:
          load.kind === "unknown-token" ? (
            <Message title="That session doesn't exist">
              The link might have been cut short when it was copied, or the
              session was removed. Ask the program that sent you here for a new
              one.
            </Message>
          ) : (
            <Message title="Can't reach Lookout">
              Something went wrong loading this session. Check your connection
              and reload the page.
            </Message>
          ),
      };
    }

    if (FINISHED.includes(load.status)) {
      return {
        key: "finished",
        node: (
          <Message
            title={
              load.status === "failed"
                ? "This session failed"
                : "This session is finished"
            }
          >
            {load.status === "failed"
              ? "Its timelapse couldn't be built. Let the program that sent you here know."
              : "Its timelapse is already built and sent back to the program that started it."}
          </Message>
        ),
      };
    }

    // stopped / compiling: the recording is over but the timelapse isn't
    // built yet. Mount the recorder — showing that progress is its job, and
    // it's the same screen the user would have been looking at had they not
    // reloaded.
    const recordable = RECORDABLE.includes(load.status);

    if (recordable && source === null) {
      return {
        key: "chooser",
        node: (
          <SourceChooser
            sessionName={load.name}
            status={load.status}
            capabilities={capabilities}
            onChoose={setSource}
          />
        ),
      };
    }

    if (source === "desktop") {
      return {
        key: "desktop",
        node: <DesktopHandoff token={token} onBack={() => setSource(null)} />,
      };
    }

    const mode: CaptureMode = source === "camera" ? "camera" : "screen";
    return {
      key: "record",
      node: (
        <>
          {recordable && !sharing && (
            <BackLink onClick={() => setSource(null)} />
          )}
          <LookoutProvider
            token={token}
            apiBaseUrl={API_BASE}
            appName={url.appName}
            capture={{ mode }}
            callbacks={callbacks}
          >
            <LookoutRecorder editing={url.editing} />
          </LookoutProvider>
        </>
      ),
    };
  }
}

/**
 * One view in the cross-fade.
 *
 * Both views are mounted at once so their fades overlap. Only the incoming
 * one is in flow; the one on its way out is taken out of flow and laid over
 * the same spot, so it can't push the layout around on its way to zero
 * opacity, and the frame's height animation is driven purely by the view
 * that's arriving.
 */
function ViewLayer({
  children,
  measure,
}: {
  children: React.ReactNode;
  measure: (el: HTMLDivElement | null) => void;
}) {
  const isPresent = useIsPresent();
  return (
    <motion.div
      ref={isPresent ? measure : undefined}
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: 0.18, ease: "easeOut" }}
      style={{
        // Keeps the clip edge inside empty space: while the frame is
        // resizing, what gets cut off is padding, not a line of text sliced
        // in half by the footer rule.
        paddingBottom: spacing.xxl,
        ...(isPresent
          ? { position: "relative" as const }
          : { position: "absolute" as const, top: 0, left: 0, right: 0 }),
      }}
    >
      {children}
    </motion.div>
  );
}

/**
 * The height of the view currently in flow, tracked live.
 *
 * A callback ref rather than an effect, so it follows whichever element
 * AnimatePresence has on screen. Detaches are ignored on purpose: during a
 * swap React can null this out and hand over the new element in either
 * order, and ignoring the null makes the result the same either way. The
 * recorder resizes on its own too (a preview appears, an error shows up),
 * and this picks that up for free.
 */
function useMeasuredHeight() {
  const [height, setHeight] = useState<number | null>(null);
  const observer = useRef<ResizeObserver | null>(null);

  const measure = useCallback((el: HTMLDivElement | null) => {
    if (!el) return;
    observer.current?.disconnect();
    setHeight(el.getBoundingClientRect().height);
    observer.current = new ResizeObserver(() =>
      setHeight(el.getBoundingClientRect().height),
    );
    observer.current.observe(el);
  }, []);

  useEffect(() => () => observer.current?.disconnect(), []);

  return { height, measure };
}

function BackLink({ onClick }: { onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      style={{
        display: "block",
        marginBottom: spacing.md,
        padding: 0,
        background: "none",
        border: "none",
        cursor: "pointer",
        font: "inherit",
        fontSize: 13,
        color: colors.text.tertiary,
      }}
    >
      ← Record a different way
    </button>
  );
}

function Footer() {
  return (
    <div
      style={{
        // The view above already ends in padding (see ViewLayer), so this
        // only has to add the rest of the gap.
        marginTop: spacing.sm,
        paddingTop: spacing.lg,
        borderTop: `1px solid ${colors.border.default}`,
        display: "flex",
        justifyContent: "center",
      }}
    >
      {/* Inverted in dark mode by the `hc-flag` rule in index.html, since
          the stock mark is a black flag with the wordmark knocked out. */}
      <img
        className="hc-flag"
        src="/session/hack-club-flag.svg"
        alt="Hack Club"
        width={110}
      />
    </div>
  );
}
