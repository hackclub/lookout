import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { AnimatePresence, motion } from "motion/react";
import {
  countCutUnits,
  type ApplyCutsResponse,
  type MaskRegion,
  type CutInterval,
  type UnitsResponse,
  type VideoShot,
} from "@lookout/shared";
import { createLookoutClient, type LookoutClient } from "../api/client.js";
import {
  cutsToRegions,
  gapIndices,
  normalizeRegions,
  regionAtTime,
  regionsToCuts,
  elapsedLabel,
  shotRulerLabel,
  rulerStep,
  rulerTicks,
  unitAtTime,
  unitClockLabel,
  unitMasksToMasks,
  masksToUnitMasks,
  findShotAtTime,
  snapToNearestShotBoundary,
  assignMaskTracks,
  canAddMaskAtTime,
  activeMasksAtUnit,
  isMaskActiveAtTime,
  computeSafeCursorTime,
  TRACK_PRESETS,
  MAX_MASK_TRACKS,
  type UnitRegion,
  type UnitMaskRegion,
} from "../hooks/editorMath.js";
import {
  openDecoderFrames,
  openVideoFrames,
  prefersDecoderFrames,
  type FilmstripFrames,
} from "../hooks/filmstripFrames.js";
import { useEditLease } from "../hooks/useEditLease.js";
import {
  compileEstimateMs,
  estimateBuildProgress,
  interpolateBuildProgress,
  PROGRESS_CAP,
} from "../hooks/buildProgress.js";
import { injectEditorStyles } from "./editorStyles.js";
import { Button } from "../ui/Button.js";
import { MinutesFlow } from "../ui/MinutesFlow.js";
import NumberFlow from "@number-flow/react";
import { Spinner } from "../ui/Spinner.js";
import { ProgressRing } from "../ui/ProgressRing.js";
import { ErrorDisplay } from "../ui/ErrorDisplay.js";
import { colors, fontSize, fontWeight, radii, spacing } from "../ui/theme.js";

export interface TimelapseEditorProps {
  token: string;
  apiBaseUrl: string;
  /** Bring your own API client (see `LookoutProviderProps.client`).
   *  Defaults to the fetch client for `apiBaseUrl` + `token`. */
  client?: LookoutClient;
  /** The timelapse was published — with cuts baked in, or without them.
   *  The caller should return to its detail view and poll status. The
   *  publish response is passed through: `instant`/`complete` means it's
   *  already done (fire any redirect now), otherwise a compile is running. */
  onApplied?: (result: ApplyCutsResponse) => void;
  /** Dismiss the editor. Only offered when it can't load — there is no
   *  "leave without deciding" exit, because closing the editor is itself
   *  the decision: the session publishes. */
  onCancel?: () => void;
  /** Fired whenever the cut list changes, with the normalized list and
   *  whether it differs from what's saved. Lets a host (the desktop
   *  window) publish the current edit when the user closes it. */
  onCutsChange?: (cuts: CutInterval[], dirty: boolean) => void;
  /** Fired whenever the mask list changes. */
  onMasksChange?: (masks: MaskRegion[], dirty: boolean) => void;
  /** Alias for onMasksChange. */
  onBlursChange?: (masks: MaskRegion[], dirty: boolean) => void;
}

const STRIP_HEIGHT = 56;
/** Diagonal hatch marking removed stretches on the timeline — the
 *  conventional "excluded" texture, and a second channel beyond colour
 *  alone. Kept faint: it should register as texture, not as content
 *  competing with the thumbnails underneath. */
const hatch = (periodPx: number) =>
  `repeating-linear-gradient(45deg, ${colors.editor.cutStripe} 0 ${
    periodPx / 2
  }px, transparent ${periodPx / 2}px ${periodPx}px)`;

const RULER_HEIGHT = 22;
/** Playhead cap: a slim pill, bottom-aligned to the ruler so it tucks
 *  under the labels instead of covering them. Small on purpose — it marks
 *  a position, it isn't a control that should dominate the timeline. */
const HEAD_W = 9;
const HEAD_H = 13;
/** Invisible grab area around the cap. The cap is too small to hit
 *  comfortably; the target isn't. */
const HEAD_HIT = 22;
/** Upper bound on filmstrip tiles. The real count comes from the track
 *  width; this only stops an ultra-wide display from queueing hundreds of
 *  decodes. */
const FILMSTRIP_MAX_TILES = 48;
/** Faulty tiles tolerated before a frame source is written off. One can
 *  be the recording's own fault; two in a row is the source. */
const MAX_TILE_FAULTS = 2;
/** Canvases are sized in device pixels and scaled down by CSS — without
 *  this a 2x display renders every thumbnail at half resolution, which
 *  reads as a blurry, low-quality preview. Capped at 2 because 3x gains
 *  nothing visible here and triples the decode cost. */
const pixelRatio = () =>
  Math.min(2, typeof window === "undefined" ? 1 : window.devicePixelRatio || 1);

type DragState =
  | { kind: "maybe"; downUnitF: number }
  | { kind: "scrub" }
  | {
      kind: "region";
      index: number;
      mode: "move" | "start" | "end";
      grabOffset: number;
      anchorUnit: number;
    }
  | {
      kind: "mask";
      id: string;
      mode: "move" | "start" | "end";
      grabOffset: number;
      initialWidth: number;
    }
  | null;

/**
 * The "Edit & save" step of stopping a recording. The session is compiled
 * but deliberately UNPUBLISHED (held), so nothing downstream has consumed
 * it yet; this view previews that video (1 second = 1 capture unit = 1
 * real-world minute), lets the user drag out cut regions, and publishes —
 * with the cuts baked in, or without them.
 *
 * Layout is a three-row shell: a fixed transport bar, a stage that shrinks
 * (the only flexible row), and a dock pinned to the bottom. Every ancestor
 * of the stage carries `min-height: 0` so the video letterboxes down
 * instead of shoving the timeline out of the window.
 */
export function TimelapseEditor({
  token,
  apiBaseUrl,
  client: clientProp,
  onApplied,
  onCancel,
  onCutsChange,
  onMasksChange,
  onBlursChange,
}: TimelapseEditorProps) {
  const client = useMemo<LookoutClient>(
    () => clientProp ?? createLookoutClient({ baseUrl: apiBaseUrl, token }),
    [clientProp, apiBaseUrl, token],
  );

  useEffect(() => injectEditorStyles(), []);

  const [data, setData] = useState<UnitsResponse | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  /** Unit count while the preview video is still compiling; null once it's
   *  ready. Deliberately a NUMBER, not an object: the poll below re-sets it
   *  every 1.5s, and a fresh object literal would change identity on every
   *  poll, re-running the progress effect and restarting the ring at 0. */
  const [preparingUnits, setPreparingUnits] = useState<number | null>(null);
  const [buildProgress, setBuildProgress] = useState(0);
  /** Real compile progress from /status, when the worker reports it; null
   *  until the first metered poll (or forever, for cut-apply/old workers). */
  const [realProgress, setRealProgress] = useState<number | null>(null);
  /** Latest real value and when it landed. Real progress ANCHORS the ring
   *  rather than owning it outright: it arrives once per 2s poll and only when
   *  the worker has moved another 1%, so displaying it directly lurches in
   *  steps with long dead pauses. The tick eases between anchors. */
  const realAnchorRef = useRef<{ value: number; atMs: number } | null>(null);
  /** Anchored once per preparing spell, so even a genuine change in the
   *  unit count can't restart the estimate. */
  const prepareStartRef = useRef<number | null>(null);
  const videoRef = useRef<HTMLVideoElement>(null);
  const videoBoxRef = useRef<HTMLDivElement>(null);
  const overlayRef = useRef<HTMLDivElement>(null);
  const maskTrackRef = useRef<HTMLDivElement>(null);
  const timelineRef = useRef<HTMLDivElement>(null);
  const timelineScrollRef = useRef<HTMLDivElement>(null);
  const timelineAreaRef = useRef<HTMLDivElement>(null);
  const maskMenuRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const activeScanTokenRef = useRef(0);
  const probeVideoRef = useRef<HTMLVideoElement | null>(null);

  const [mode, setMode] = useState<"cut" | "mask">("cut");
  const [regions, setRegions] = useState<UnitRegion[]>([]);
  const [selected, setSelected] = useState<number | null>(null);
  const [masks, setMasks] = useState<UnitMaskRegion[]>([]);
  const [selectedMaskId, setSelectedMaskId] = useState<string | null>(null);
  const [drawingMask, setDrawingMask] = useState<{
    startX: number;
    startY: number;
    currentX: number;
    currentY: number;
  } | null>(null);
  const [maskDrag, setMaskDrag] = useState<{
    id: string;
    handle: "move" | "nw" | "ne" | "sw" | "se";
    startX: number;
    startY: number;
    initial: UnitMaskRegion;
  } | null>(null);
  const [maskMenuOpen, setMaskMenuOpen] = useState(false);
  const [time, setTime] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [filmstrip, setFilmstrip] = useState<string[]>([]);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [expandingMaskId, setExpandingMaskId] = useState<string | null>(null);
  const [dynamicShots, setDynamicShots] = useState<VideoShot[] | null>(null);
  const [maskLimitNotice, setMaskLimitNotice] = useState<string | null>(null);
  const [videoAspect, setVideoAspect] = useState<number | null>(null);
  const [zoom, setZoom] = useState(1);
  const zoomAnchorRef = useRef<{ trackFrac: number; anchorXInViewport: number } | null>(null);
  const [isSmoothSeek, setIsSmoothSeek] = useState(false);
  const smoothSeekTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [isTimelineHovered, setIsTimelineHovered] = useState(false);

  const activeShots = useMemo(
    () => (dynamicShots && dynamicShots.length > 0 ? dynamicShots : (data?.shots ?? [])),
    [dynamicShots, data?.shots],
  );

  const trackAllocation = useMemo(() => assignMaskTracks(masks), [masks]);

  const drawingTrackIdx = useMemo(() => {
    if (!drawingMask) return 0;
    const curTime = videoRef.current?.currentTime ?? time;
    const active = activeMasksAtUnit(curTime, masks);
    const usedTracks = new Set(active.map((b) => trackAllocation.assignments[b.id]));
    for (let t = 0; t < MAX_MASK_TRACKS; t++) {
      if (!usedTracks.has(t)) return t;
    }
    return 0;
  }, [drawingMask, masks, trackAllocation, time]);

  const [containerSize, setContainerSize] = useState({ width: 900, height: 620 });

  useEffect(() => {
    const el = containerRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(([entry]) => {
      if (!entry) return;
      const { width, height } = entry.contentRect;
      setContainerSize({ width, height });
    });
    ro.observe(el);
    const rect = el.getBoundingClientRect();
    if (rect.width > 0 && rect.height > 0) {
      setContainerSize({ width: rect.width, height: rect.height });
    }
    return () => ro.disconnect();
  }, []);

  const isVeryShort = containerSize.height <= 400;
  const isShort = containerSize.height <= 500;
  const isNarrow = containerSize.width <= 540;
  const isTinyWidth = containerSize.width <= 440;

  const stripHeight = isVeryShort ? 32 : isShort ? 40 : STRIP_HEIGHT;
  const rulerHeight = isShort ? 18 : RULER_HEIGHT;
  const maskTrackHeight = isVeryShort ? 20 : isShort ? 22 : 26;
  const rootGap = isShort ? 6 : spacing.md;
  const dockGap = isShort ? 6 : spacing.sm;
  const playOverlaySize = isVeryShort ? 36 : isShort ? 44 : 56;
  const zoomRef = useRef(zoom);
  zoomRef.current = zoom;
  const dragRef = useRef<DragState>(null);
  const regionsRef = useRef<UnitRegion[]>(regions);
  regionsRef.current = regions;
  const masksRef = useRef<UnitMaskRegion[]>(masks);
  masksRef.current = masks;
  const rafRef = useRef<number>(0);

  const units = data?.units ?? [];
  const unitCount = units.length;

  // ── Load ────────────────────────────────────────────────────
  // The preview video is built by the compile that ran at stop, so the
  // editor almost always opens BEFORE it exists: `/units` reports
  // `preparing` for the whole build. That is the normal path, not a
  // failure — but it must be waited out on `/status`, not `/units`.
  //
  // `/units` presigns a URL and is rate limited to 10/min; polling it
  // every 1.5s is 40/min, so the wait itself would 429 after ~15s and the
  // editor would report a rate-limit error instead of a video. `/status`
  // is the cheap endpoint built for polling (60/min) and already carries
  // `editable`, so wait on that and fetch `/units` only at the edges.
  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const fail = (reason: UnitsResponse["editableReason"]) =>
      setLoadError(
        reason === "published"
          ? "This timelapse has already been published, so it can't be edited."
          : reason === "failed"
            ? "This timelapse couldn't be compiled, so there's nothing to edit."
            : reason === "recompiles_exhausted"
              ? "This timelapse has reached its edit limit."
              : "This timelapse isn't available for editing.",
      );

    const loadUnits = async () => {
      const res = await client.getUnits();
      if (cancelled) return;
      if (res.editable && res.originalVideoUrl) {
        setPreparingUnits(null);
        setData(res);
        setRegions(cutsToRegions(res.cuts, res.units));
        setMasks(masksToUnitMasks(res.masks ?? (res as any).blurs ?? [], res.units));
        return;
      }
      if (res.editableReason === "preparing" || res.editableReason === "no_original") {
        // Keep the unit count for the progress estimate, then hand the
        // waiting over to /status.
        setPreparingUnits(res.expectedUnits ?? 0);
        timer = setTimeout(waitForReady, 2000);
        return;
      }
      fail(res.editableReason);
    };

    const waitForReady = async () => {
      if (cancelled) return;
      try {
        const status = await client.getStatus();
        if (cancelled) return;
        if (typeof status.progress === "number") setRealProgress(status.progress);
        if (status.editable) {
          await loadUnits();
          return;
        }
        if (status.status === "complete") {
          fail("published");
          return;
        }
        if (status.status === "failed") {
          fail("failed");
          return;
        }
      } catch (err) {
        // Transient: keep waiting rather than dropping the user out of an
        // edit because one poll failed.
        console.warn("[editor] status poll failed:", err);
      }
      timer = setTimeout(waitForReady, 2000);
    };

    void (async () => {
      try {
        await loadUnits();
      } catch (err) {
        if (!cancelled) setLoadError(err instanceof Error ? err.message : String(err));
      }
    })();

    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, [client]);

  // ── Build progress ──────────────────────────────────────────
  // Real worker progress wins when the /status poll reports it. Until then
  // (and for cut-apply/old-worker compiles that never report it) this is a
  // time estimate scaled by how much footage there is to compile. Either
  // source eases toward — and stops short of — 100%, and only the real
  // thing completing ends the wait; a ring that sat at 100% while the user
  // waited would be worse than none.
  useEffect(() => {
    if (realProgress === null) return;
    realAnchorRef.current = { value: realProgress, atMs: Date.now() };
    setBuildProgress((prev) => Math.max(prev, realProgress));
  }, [realProgress]);
  useEffect(() => {
    if (preparingUnits === null) {
      prepareStartRef.current = null;
      return;
    }
    if (prepareStartRef.current === null) prepareStartRef.current = Date.now();
    const startedAt = prepareStartRef.current;
    const estimateMs = compileEstimateMs(preparingUnits);
    const tick = () => {
      // BOTH sources run, and the ring takes whichever is further along.
      // Neither may switch the other off, and that is the whole trick:
      //
      //  - The estimate is a continuous function of time, so something is
      //    always moving at every 200ms tick — the ring can never sit still.
      //    Letting real progress silence it is what made this feel like a
      //    hang: updates dropped to one per 2s poll, in >=1% steps.
      //  - Real worker progress pulls the ring UP whenever the compile is
      //    further along than the guess, so the number stays tied to truth
      //    instead of drifting off on a curve.
      //  - Easing out of the last real anchor keeps the gaps between polls
      //    smooth, bounded by what one poll is expected to deliver.
      //
      // Monotonic via `prev`, and capped short of 100% — only the status flip
      // ends the wait, so a full ring while the user is still waiting would
      // be a lie.
      const now = Date.now();
      const anchor = realAnchorRef.current;
      const estimated = estimateBuildProgress(now - startedAt, estimateMs);
      const fromReal = anchor
        ? interpolateBuildProgress(anchor.value, now - anchor.atMs, estimateMs)
        : 0;
      setBuildProgress((prev) =>
        Math.min(PROGRESS_CAP, Math.max(prev, estimated, fromReal)),
      );
    };
    tick();
    const id = setInterval(tick, 200);
    return () => clearInterval(id);
  }, [preparingUnits]);

  // ── Edit lease ──────────────────────────────────────────────
  // This editor being open IS the signal that editing is in progress, so
  // it renews the lease while mounted. No countdown, no deadline to race:
  // the session waits as long as the window is up, and publishes on its
  // own shortly after it isn't. Stops once the session is no longer held.
  const leaseHeld = useEditLease(client, !saving && !saveSuccess);
  useEffect(() => {
    if (leaseHeld || saving || saveSuccess) return;
    setLoadError(
      "This timelapse was already published, so it can no longer be edited.",
    );
  }, [leaseHeld, saving, saveSuccess]);

  const prevEditSig = useRef("");
  useEffect(() => {
    const sig = JSON.stringify({ regions, masks });
    if (prevEditSig.current && prevEditSig.current !== sig) {
      setSaveSuccess(false);
    }
    prevEditSig.current = sig;
  }, [regions, masks]);

  // ── Playhead tracking (rAF for a smooth 60fps playhead) ─────
  useEffect(() => {
    const tick = () => {
      const v = videoRef.current;
      if (v) setTime(v.currentTime);
      rafRef.current = requestAnimationFrame(tick);
    };
    rafRef.current = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(rafRef.current);
  }, []);

  // ── Playback skips cut regions (scrubbing passes through) ──
  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    const onTimeUpdate = () => {
      if (v.paused) return;
      const region = regionAtTime(v.currentTime, regionsRef.current);
      if (!region) return;
      if (region.endUnit >= unitCount) {
        v.pause();
        v.currentTime = region.startUnit;
      } else {
        v.currentTime = region.endUnit;
      }
    };
    v.addEventListener("timeupdate", onTimeUpdate);
    return () => v.removeEventListener("timeupdate", onTimeUpdate);
  }, [unitCount, data?.originalVideoUrl]);

  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    const updateAspect = () => {
      if (v.videoWidth && v.videoHeight) {
        setVideoAspect(v.videoWidth / v.videoHeight);
      }
    };
    v.addEventListener("loadedmetadata", updateAspect);
    if (v.videoWidth && v.videoHeight) {
      updateAspect();
    }
    return () => v.removeEventListener("loadedmetadata", updateAspect);
  }, [data?.originalVideoUrl]);

  // ── Filmstrip frame source ──────────────────────────────────
  //
  // Getting pixels out of the preview has two independent problems, and
  // the openers below cover both:
  //
  //  1. Reading a <video> through a canvas needs a taint-free source. A
  //     presigned GET that carries CORS headers can be loaded with
  //     crossOrigin="anonymous" directly; a bucket that doesn't send them
  //     fails that load outright (which is exactly what a missing CORS
  //     config looks like), so the bytes are pulled through the app's own
  //     fetch — on desktop that is Tauri's HTTP plugin, not subject to
  //     browser CORS at all — and handed over as a same-origin blob.
  //  2. WebKitGTK can't be read through a canvas AT ALL while accelerated
  //     compositing is on: the frames come back empty, or as whatever was
  //     in that graphics memory. So a third opener decodes the same bytes
  //     with WebCodecs instead, and on that engine it goes first. See
  //     hooks/filmstripFrames.ts.
  //
  // Whichever opener runs, its tiles are checked before they are shown and
  // the source is dropped if they can't be real frames. Only if all three
  // fail does the timeline degrade to a plain track.
  /** The downloaded preview, shared between openers and kept across track
   *  resizes so a window drag can't re-download it. Held for as long as
   *  the editor is open, because the decoder needs random access to the
   *  samples: ~29KB per recorded minute at the preview tier, so single
   *  digits of MB for a normal session. Only fetched if an opener that
   *  needs the bytes actually runs. */
  const previewBytesRef = useRef<Promise<ArrayBuffer | null> | null>(null);
  useEffect(() => {
    previewBytesRef.current = null;
  }, [data?.originalVideoUrl]);

  useEffect(() => {
    const videoSrc = data?.originalVideoUrl;
    if (!videoSrc) {
      if (probeVideoRef.current) {
        probeVideoRef.current.removeAttribute("src");
        probeVideoRef.current.load();
        probeVideoRef.current = null;
      }
      return;
    }
    const v = document.createElement("video");
    if (!videoSrc.startsWith("blob:")) v.crossOrigin = "anonymous";
    v.muted = true;
    v.playsInline = true;
    v.preload = "auto";
    v.src = videoSrc;
    probeVideoRef.current = v;
    return () => {
      v.removeAttribute("src");
      v.load();
      probeVideoRef.current = null;
    };
  }, [data?.originalVideoUrl]);

  // Track width drives the filmstrip: tiles are whole frames at the
  // video's own aspect ratio, so how many fit is a function of the track,
  // not of how many minutes were recorded.
  const [stripWidth, setStripWidth] = useState(0);
  useEffect(() => {
    const el = timelineRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(([entry]) =>
      setStripWidth(entry.contentRect.width),
    );
    ro.observe(el);
    setStripWidth(el.getBoundingClientRect().width);
    return () => ro.disconnect();
  }, [data, zoom]);

  /**
   * Filmstrip: whole, uncropped frames tiled across the track — the
   * Premiere / CapCut / iOS scrubber look.
   *
   * The math: a tile is the full frame at track height, so
   *   tileW = STRIP_HEIGHT × (videoW / videoH)   — 56 × 16/9 ≈ 100px
   *   tiles = ceil(trackW / tileW)               — ~9 across a 900px track
   * Tile i covers x ∈ [i·tileW, (i+1)·tileW), so it samples the frame at
   * its own midpoint: t = clamp(((i + 0.5)·tileW) / trackW) × duration.
   * The last tile is clipped by the track's overflow, exactly as a real
   * filmstrip is. Deliberately NOT one tile per minute: at 48 minutes
   * that squeezed each frame into 19px and cropped it to a smear.
   */
  const [tileWidth, setTileWidth] = useState(Math.round(STRIP_HEIGHT * (16 / 9)));
  useEffect(() => {
    const src = data?.originalVideoUrl;
    if (!src || unitCount === 0 || stripWidth <= 0) return;
    let cancelled = false;
    let source: FilmstripFrames | null = null;
    let blobUrl: string | null = null;

    // Debounce: a live window drag fires dozens of resizes, and each
    // regeneration is a series of decoder seeks.
    const timer = setTimeout(() => {
      const bytes = () => {
        if (!previewBytesRef.current) {
          previewBytesRef.current = fetch(src)
            .then((r) => {
              if (!r.ok) throw new Error(`HTTP ${r.status}`);
              return r.arrayBuffer();
            })
            .catch((err) => {
              console.error("[editor] preview download failed:", err);
              return null;
            });
        }
        return previewBytesRef.current;
      };

      const viaDecoder = async () => {
        const buf = await bytes();
        return buf ? openDecoderFrames(buf) : null;
      };
      const viaCorsVideo = () => openVideoFrames(src, { crossOrigin: true });
      const viaBlobVideo = async () => {
        const buf = await bytes();
        if (!buf || cancelled) return null;
        blobUrl = URL.createObjectURL(new Blob([buf], { type: "video/mp4" }));
        return openVideoFrames(blobUrl);
      };
      const openers = prefersDecoderFrames()
        ? [viaDecoder, viaCorsVideo, viaBlobVideo]
        : [viaCorsVideo, viaBlobVideo, viaDecoder];

      /** Tiles for one source, or null if the source should be abandoned. */
      const render = async (frames: FilmstripFrames): Promise<string[] | null> => {
        const tileW = Math.max(24, Math.round(STRIP_HEIGHT * frames.aspect));
        setTileWidth(tileW);
        const tiles = Math.min(
          FILMSTRIP_MAX_TILES,
          Math.max(1, Math.ceil(stripWidth / tileW)),
        );
        const duration =
          Number.isFinite(frames.durationSec) && frames.durationSec > 0
            ? frames.durationSec
            : unitCount;

        const dpr = pixelRatio();
        const canvas = document.createElement("canvas");
        canvas.width = Math.round(tileW * dpr);
        canvas.height = Math.round(STRIP_HEIGHT * dpr);
        // Every tile is read back for the fault check, which is exactly
        // the access pattern this hint exists for.
        const ctx = canvas.getContext("2d", { willReadFrequently: true });
        if (!ctx) return null;
        ctx.imageSmoothingQuality = "high";

        const thumbs: string[] = [];
        let faults = 0;
        for (let i = 0; i < tiles; i++) {
          if (cancelled) return null;
          const frac = Math.min(1, ((i + 0.5) * tileW) / stripWidth);
          const t = Math.max(0, Math.min(duration - 0.05, frac * duration));
          const outcome = await frames.draw(t, ctx, canvas.width, canvas.height);
          if (outcome !== "ok") {
            // One bad tile could be the frame's own fault — a recording
            // that really was one flat colour for a minute. Two is the
            // source, so hand over to the next opener. The progressive
            // publish below starts at the third tile, so a source that
            // fails this way never puts anything on screen.
            if (++faults >= MAX_TILE_FAULTS) {
              console.warn(
                `[editor] ${frames.kind} frames unusable (${outcome}); trying the next source`,
              );
              return null;
            }
            continue;
          }
          thumbs.push(canvas.toDataURL("image/jpeg", 0.82));
          if (i % 3 === 2) setFilmstrip([...thumbs]);
        }
        return thumbs.length ? thumbs : null;
      };

      void (async () => {
        for (const open of openers) {
          if (cancelled) return;
          source = await open().catch((err) => {
            console.warn("[editor] filmstrip source failed to open:", err);
            return null;
          });
          if (!source) continue;
          // Opening is async, so the effect can have been torn down while
          // it ran — the cleanup below saw a null `source` and had nothing
          // to close.
          if (cancelled) {
            source.close();
            return;
          }
          const thumbs = await render(source);
          source.close();
          source = null;
          if (cancelled) return;
          if (thumbs) {
            setFilmstrip(thumbs);
            return;
          }
        }
        if (!cancelled) {
          console.error(
            "[editor] no usable frame source; the timeline has no thumbnails",
          );
          setFilmstrip([]);
        }
      })();
    }, 220);

    return () => {
      cancelled = true;
      clearTimeout(timer);
      source?.close();
      if (blobUrl) URL.revokeObjectURL(blobUrl);
    };
  }, [data?.originalVideoUrl, unitCount, stripWidth]);

  // ── Pointer plumbing ────────────────────────────────────────
  const unitFromEvent = useCallback(
    (e: { clientX: number }): number => {
      const el = timelineRef.current;
      if (!el || unitCount === 0) return 0;
      const rect = el.getBoundingClientRect();
      const frac = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
      return frac * unitCount;
    },
    [unitCount],
  );

  const seekTo = useCallback(
    (t: number, smooth: boolean = false) => {
      const v = videoRef.current;
      if (!v) return;
      const target = Math.max(0, Math.min(unitCount - 0.05, t));
      v.currentTime = target;
      setTime(target);
      if (smooth) {
        if (smoothSeekTimerRef.current) clearTimeout(smoothSeekTimerRef.current);
        setIsSmoothSeek(true);
        smoothSeekTimerRef.current = setTimeout(() => {
          setIsSmoothSeek(false);
        }, 320);
      }
      else {
        setIsSmoothSeek(false);
      }
    },
    [unitCount],
  );

  const applyZoom = useCallback(
    (newZoom: number, anchorClientX?: number) => {
      const clamped = Math.max(1, Math.min(16, Math.round(newZoom * 100) / 100));
      const prevZoom = zoomRef.current;
      if (clamped === prevZoom) return;
      zoomRef.current = clamped;

      const scrollEl = timelineScrollRef.current;
      if (!scrollEl) {
        setZoom(clamped);
        return;
      }

      const rect = scrollEl.getBoundingClientRect();
      let anchorXInViewport = rect.width / 2;
      if (anchorClientX !== undefined) {
        anchorXInViewport = Math.max(0, Math.min(rect.width, anchorClientX - rect.left));
      }
      else {
        const totalUnits = Math.max(1, unitCount);
        const curPlayheadFrac = Math.min(time, totalUnits) / totalUnits;
        const curPlayheadPx = curPlayheadFrac * scrollEl.scrollWidth;
        const playheadInView = curPlayheadPx - scrollEl.scrollLeft;
        if (playheadInView >= 0 && playheadInView <= rect.width) {
          anchorXInViewport = playheadInView;
        }
      }

      const trackFrac =
        (scrollEl.scrollLeft + anchorXInViewport) / Math.max(1, scrollEl.scrollWidth);

      zoomAnchorRef.current = { trackFrac, anchorXInViewport };
      setZoom(clamped);
    },
    [unitCount, time],
  );

  useLayoutEffect(() => {
    const anchor = zoomAnchorRef.current;
    if (!anchor || !timelineScrollRef.current) return;
    const scrollEl = timelineScrollRef.current;
    const newScrollLeft = anchor.trackFrac * scrollEl.scrollWidth - anchor.anchorXInViewport;
    scrollEl.scrollLeft = Math.max(0, newScrollLeft);
    zoomAnchorRef.current = null;
  }, [zoom]);

  const zoomIn = useCallback(() => {
    applyZoom(zoomRef.current * 1.5);
  }, [applyZoom]);

  const zoomOut = useCallback(() => {
    applyZoom(zoomRef.current / 1.5);
  }, [applyZoom]);

  const resetZoom = useCallback(() => {
    applyZoom(1);
  }, [applyZoom]);

  useEffect(() => {
    const target = timelineAreaRef.current || timelineScrollRef.current;
    if (!target) return;

    const onWheel = (e: WheelEvent) => {
      if (dragRef.current) return;
      const scrollEl = timelineScrollRef.current;
      if (!scrollEl) return;

      // 1. Shift + scroll: shifts the timeline horizontally left/right
      if (e.shiftKey) {
        e.preventDefault();
        const shiftDelta = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
        scrollEl.scrollLeft += shiftDelta;
        return;
      }

      // 2. Trackpad pinch in Chrome / Ctrl+scroll / Cmd+scroll: zoom centered on cursor
      if (e.ctrlKey || e.metaKey) {
        e.preventDefault();
        const factor = Math.exp(-e.deltaY * 0.01);
        applyZoom(zoomRef.current * factor, e.clientX);
        return;
      }

      // 3. Trackpad horizontal swipe: pan horizontally
      if (Math.abs(e.deltaX) > Math.abs(e.deltaY) && Math.abs(e.deltaX) > 1) {
        scrollEl.scrollLeft += e.deltaX;
        return;
      }

      // 4. Mouse vertical scroll wheel: increases or decreases the size
      if (Math.abs(e.deltaY) > 0) {
        e.preventDefault();
        if (Math.abs(e.deltaY) >= 40 || e.deltaMode !== 0) {
          const factor = e.deltaY < 0 ? 1.25 : (1 / 1.25);
          applyZoom(zoomRef.current * factor, e.clientX);
        }
        else {
          const factor = Math.exp(-e.deltaY * 0.005);
          applyZoom(zoomRef.current * factor, e.clientX);
        }
      }
    };

    // Safari / WebKit trackpad pinch gesture support
    let gestureStartZoom = 1;

    const onGestureStart = (e: Event) => {
      if (dragRef.current) return;
      e.preventDefault();
      gestureStartZoom = zoomRef.current;
    };

    const onGestureChange = (e: Event) => {
      if (dragRef.current) return;
      e.preventDefault();
      const ge = e as UIEvent & { scale?: number; clientX?: number };
      if (typeof ge.scale === "number" && ge.scale > 0) {
        const targetZoom = gestureStartZoom * ge.scale;
        const clientX = typeof ge.clientX === "number" ? ge.clientX : undefined;
        applyZoom(targetZoom, clientX);
      }
    };

    const onGestureEnd = (e: Event) => {
      e.preventDefault();
    };

    target.addEventListener("wheel", onWheel, { passive: false });
    target.addEventListener("gesturestart", onGestureStart, { passive: false });
    target.addEventListener("gesturechange", onGestureChange, { passive: false });
    target.addEventListener("gestureend", onGestureEnd, { passive: false });

    return () => {
      target.removeEventListener("wheel", onWheel);
      target.removeEventListener("gesturestart", onGestureStart);
      target.removeEventListener("gesturechange", onGestureChange);
      target.removeEventListener("gestureend", onGestureEnd);
    };
  }, [applyZoom]);

  useEffect(() => {
    if (!playing || zoom <= 1) return;
    const scrollEl = timelineScrollRef.current;
    if (!scrollEl || unitCount <= 0) return;

    const playheadPx = (time / unitCount) * scrollEl.scrollWidth;
    const scrollLeft = scrollEl.scrollLeft;
    const clientWidth = scrollEl.clientWidth;

    const margin = clientWidth * 0.15;
    if (playheadPx > scrollLeft + clientWidth - margin) {
      scrollEl.scrollLeft = playheadPx - clientWidth + margin;
    }
    else if (playheadPx < scrollLeft + margin) {
      scrollEl.scrollLeft = Math.max(0, playheadPx - margin);
    }
  }, [playing, time, zoom, unitCount]);

  const [scrollProgress, setScrollProgress] = useState(0);
  const [isMinimapDragging, setIsMinimapDragging] = useState(false);
  const [isMinimapThumbHovered, setIsMinimapThumbHovered] = useState(false);

  useEffect(() => {
    const el = timelineScrollRef.current;
    if (!el) return;
    const onScroll = () => {
      const maxScroll = el.scrollWidth - el.clientWidth;
      if (maxScroll > 0) {
        setScrollProgress(el.scrollLeft / maxScroll);
      }
      else {
        setScrollProgress(0);
      }
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    onScroll();
    return () => el.removeEventListener("scroll", onScroll);
  }, [zoom, stripWidth]);

  const onMinimapPointerDown = useCallback((e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    const track = e.currentTarget;
    const rect = track.getBoundingClientRect();
    const scrollEl = timelineScrollRef.current;
    if (!scrollEl) return;
    const maxScroll = scrollEl.scrollWidth - scrollEl.clientWidth;
    if (maxScroll <= 0) return;

    const currentZoom = zoomRef.current;
    const thumbRatio = 1 / currentZoom;
    const thumbW = rect.width * thumbRatio;
    const usableW = rect.width - thumbW;
    if (usableW <= 0) return;

    setIsMinimapDragging(true);

    const updateScroll = (clientX: number) => {
      const xInTrack = Math.max(0, Math.min(usableW, clientX - rect.left - thumbW / 2));
      const frac = xInTrack / usableW;
      scrollEl.scrollLeft = frac * maxScroll;
      setScrollProgress(frac);
    };

    updateScroll(e.clientX);

    let rafId: number | null = null;
    const onPointerMove = (moveEv: PointerEvent) => {
      if (rafId !== null) cancelAnimationFrame(rafId);
      rafId = requestAnimationFrame(() => {
        updateScroll(moveEv.clientX);
      });
    };
    const onPointerUp = () => {
      setIsMinimapDragging(false);
      if (rafId !== null) cancelAnimationFrame(rafId);
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", onPointerUp);
    };
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp);
  }, []);

  const beginDrag = useCallback((e: React.PointerEvent, state: DragState) => {
    dragRef.current = state;
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
    e.preventDefault();
    e.stopPropagation();
  }, []);

  const onTimelinePointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (saving) return;
      if (mode === "mask") {
        beginDrag(e, { kind: "scrub" });
        seekTo(unitFromEvent(e));
        return;
      }
      beginDrag(e, { kind: "maybe", downUnitF: unitFromEvent(e) });
    },
    [beginDrag, mode, saving, seekTo, unitFromEvent],
  );

  const onRulerPointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (saving) return;
      beginDrag(e, { kind: "scrub" });
      seekTo(unitFromEvent(e));
      setSelectedMaskId(null);
    },
    [beginDrag, saving, seekTo, unitFromEvent],
  );

  const onPointerMove = useCallback(
    (e: React.PointerEvent) => {
      const drag = dragRef.current;
      if (!drag) return;
      const unitF = unitFromEvent(e);

      if (drag.kind === "scrub") {
        seekTo(unitF);
        return;
      }

      if (drag.kind === "maybe") {
        if (mode !== "cut") return;
        // Click-vs-drag disambiguation: past a third of a unit of travel,
        // the gesture becomes a new cut region growing from the press point.
        if (Math.abs(unitF - drag.downUnitF) < 0.34) return;
        const a = Math.floor(Math.min(unitF, drag.downUnitF));
        const b = Math.ceil(Math.max(unitF, drag.downUnitF));
        setRegions((prev) => {
          const next = [...prev, { startUnit: a, endUnit: Math.max(b, a + 1) }];
          setSelected(next.length - 1);
          return next;
        });
        dragRef.current = {
          kind: "region",
          index: regionsRef.current.length,
          mode: unitF >= drag.downUnitF ? "end" : "start",
          grabOffset: 0,
          anchorUnit: Math.floor(drag.downUnitF),
        };
        return;
      }

      if (drag.kind === "region") {
        if (mode !== "cut") return;
        setRegions((prev) => {
          const next = prev.map((r) => ({ ...r }));
          const r = next[drag.index];
          if (!r) return prev;
          if (drag.mode === "move") {
            const width = r.endUnit - r.startUnit;
            let start = Math.round(unitF - drag.grabOffset);
            start = Math.max(0, Math.min(unitCount - width, start));
            r.startUnit = start;
            r.endUnit = start + width;
          } else if (drag.mode === "start") {
            r.startUnit = Math.max(0, Math.min(r.endUnit - 1, Math.round(unitF)));
            seekTo(r.startUnit + 0.02);
          } else {
            const anchor = drag.anchorUnit;
            const rounded = Math.round(unitF);
            if (rounded <= anchor) {
              r.startUnit = Math.max(0, rounded);
              r.endUnit = anchor + 1;
              seekTo(r.startUnit + 0.02);
            } else {
              r.endUnit = Math.min(unitCount, Math.max(r.startUnit + 1, rounded));
              seekTo(Math.min(unitCount - 0.05, r.endUnit + 0.02));
            }
          }
          return next;
        });
        setSelected(drag.index);
        return;
      }

      if (drag.kind === "mask") {
        const id = drag.id;
        const m = masksRef.current.find((item) => item.id === id);
        if (!m) return;

        if (drag.mode === "move") {
          const width = drag.initialWidth;
          let start = Math.round(unitF - drag.grabOffset);
          start = Math.max(0, Math.min(unitCount - width, start));
          const end = start + width;
          setMasks((prev) =>
            prev.map((item) =>
              item.id === id ? { ...item, startUnit: start, endUnit: end } : item,
            ),
          );
          seekTo(start + 0.01);
        } else if (drag.mode === "start") {
          const newStart = Math.max(0, Math.min(m.endUnit - 1, Math.round(unitF)));
          setMasks((prev) =>
            prev.map((item) =>
              item.id === id ? { ...item, startUnit: newStart } : item,
            ),
          );
          seekTo(newStart + 0.01);
        } else if (drag.mode === "end") {
          const newEnd = Math.min(unitCount, Math.max(m.startUnit + 1, Math.round(unitF)));
          setMasks((prev) =>
            prev.map((item) =>
              item.id === id ? { ...item, endUnit: newEnd } : item,
            ),
          );
          seekTo(Math.min(unitCount - 0.05, newEnd - 0.01));
        }
        setSelectedMaskId(id);
        return;
      }
    },
    [seekTo, unitCount, unitFromEvent, mode, data?.shots],
  );

  const onPointerUp = useCallback(() => {
    const drag = dragRef.current;
    dragRef.current = null;
    if (!drag) return;
    if (drag.kind === "maybe") {
      // A plain click on open track: seek there and drop any selection.
      seekTo(drag.downUnitF);
      setSelected(null);
      setSelectedMaskId(null);
      return;
    }
    if (drag.kind === "mask") {
      setSelectedMaskId(drag.id);
      return;
    }
    if (drag.kind === "region") {
      if (mode !== "cut") return;
      // Keep the region selected after the gesture. Clearing it here meant
      // a selection could never outlive the click that made it, so "Remove
      // cut" was unreachable. Normalizing can merge regions and shift
      // indices, so re-find the one the gesture ended on rather than
      // trusting the old index.
      const dragged = regionsRef.current[drag.index];
      const next = normalizeRegions(regionsRef.current);
      setRegions(next);
      const idx = dragged
        ? next.findIndex(
            (r) => dragged.startUnit >= r.startUnit && dragged.startUnit < r.endUnit,
          )
        : -1;
      setSelected(idx >= 0 ? idx : null);
    }
  }, [seekTo, mode]);

  const onRegionPointerDown = useCallback(
    (e: React.PointerEvent, index: number, dragMode: "move" | "start" | "end") => {
      if (saving || mode !== "cut") return;
      const r = regionsRef.current[index];
      if (!r) return;
      setSelected(index);
      setSelectedMaskId(null);
      beginDrag(e, {
        kind: "region",
        index,
        mode: dragMode,
        grabOffset: unitFromEvent(e) - r.startUnit,
        anchorUnit: r.startUnit,
      });
    },
    [beginDrag, mode, saving, unitFromEvent],
  );

  const togglePlay = useCallback(() => {
    const v = videoRef.current;
    if (!v) return;
    if (v.paused) {
      const region = regionAtTime(v.currentTime, regionsRef.current);
      if (region && region.endUnit < unitCount) v.currentTime = region.endUnit;
      void v.play();
    } else {
      v.pause();
    }
  }, [unitCount]);

  const cutHere = useCallback(() => {
    if (mode !== "cut") return;
    const v = videoRef.current;
    if (!v || unitCount === 0) return;
    const at = unitAtTime(v.currentTime, unitCount);
    setRegions((prev) => {
      const next = normalizeRegions([...prev, { startUnit: at, endUnit: at + 1 }]);
      setSelected(next.findIndex((r) => at >= r.startUnit && at < r.endUnit));
      return next;
    });
  }, [mode, unitCount]);

  const onStagePointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (saving || mode !== "mask") return;
      const curTime = videoRef.current?.currentTime ?? 0;
      if (!canAddMaskAtTime(curTime, masksRef.current)) {
        setMaskLimitNotice("Maximum 3 overlapping masks allowed");
        setTimeout(() => setMaskLimitNotice(null), 2500);
        return;
      }
      const rect = overlayRef.current?.getBoundingClientRect();
      if (!rect || rect.width === 0 || rect.height === 0) return;
      const x = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
      const y = Math.max(0, Math.min(1, (e.clientY - rect.top) / rect.height));

      setSelectedMaskId(null);
      setSelected(null);
      setDrawingMask({ startX: x, startY: y, currentX: x, currentY: y });
      try {
        (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
      }
      catch {
        // Fallback for Safari pointer capture
      }
      e.preventDefault();
      e.stopPropagation();
    },
    [mode, saving],
  );

  const onMaskBoxPointerDown = useCallback(
    (
      e: React.PointerEvent,
      id: string,
      handle: "move" | "nw" | "ne" | "sw" | "se",
    ) => {
      if (saving || mode !== "mask") return;
      const mask = masksRef.current.find((b) => b.id === id);
      if (!mask) return;
      setSelectedMaskId(id);
      setSelected(null);

      const rect = overlayRef.current?.getBoundingClientRect();
      if (!rect || rect.width === 0 || rect.height === 0) return;
      const x = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
      const y = Math.max(0, Math.min(1, (e.clientY - rect.top) / rect.height));

      setMaskDrag({
        id,
        handle,
        startX: x,
        startY: y,
        initial: { ...mask },
      });
      try {
        (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
      }
      catch {
        // Fallback for Safari pointer capture
      }
      e.preventDefault();
      e.stopPropagation();
    },
    [mode, saving],
  );

  const onStagePointerMove = useCallback(
    (e: React.PointerEvent) => {
      const rect = overlayRef.current?.getBoundingClientRect();
      if (!rect || rect.width === 0 || rect.height === 0) return;
      const x = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
      const y = Math.max(0, Math.min(1, (e.clientY - rect.top) / rect.height));

      if (drawingMask) {
        setDrawingMask((prev) => (prev ? { ...prev, currentX: x, currentY: y } : null));
        return;
      }

      if (maskDrag) {
        const { handle, startX, startY, initial, id } = maskDrag;
        const dx = x - startX;
        const dy = y - startY;

        setMasks((prev) =>
          prev.map((b) => {
            if (b.id !== id) return b;
            const updated = { ...b };
            if (handle === "move") {
              const newX = Math.max(0, Math.min(1 - initial.width, initial.x + dx));
              const newY = Math.max(0, Math.min(1 - initial.height, initial.y + dy));
              updated.x = newX;
              updated.y = newY;
            } else if (handle === "se") {
              updated.width = Math.max(0.02, Math.min(1 - initial.x, initial.width + dx));
              updated.height = Math.max(0.02, Math.min(1 - initial.y, initial.height + dy));
            } else if (handle === "nw") {
              const newX = Math.max(0, Math.min(initial.x + initial.width - 0.02, initial.x + dx));
              const newY = Math.max(0, Math.min(initial.y + initial.height - 0.02, initial.y + dy));
              updated.width = initial.width - (newX - initial.x);
              updated.height = initial.height - (newY - initial.y);
              updated.x = newX;
              updated.y = newY;
            } else if (handle === "ne") {
              const newY = Math.max(0, Math.min(initial.y + initial.height - 0.02, initial.y + dy));
              updated.height = initial.height - (newY - initial.y);
              updated.y = newY;
              updated.width = Math.max(0.02, Math.min(1 - initial.x, initial.width + dx));
            } else if (handle === "sw") {
              const newX = Math.max(0, Math.min(initial.x + initial.width - 0.02, initial.x + dx));
              updated.width = initial.width - (newX - initial.x);
              updated.x = newX;
              updated.height = Math.max(0.02, Math.min(1 - initial.y, initial.height + dy));
            }
            return updated;
          }),
        );
      }
    },
    [drawingMask, maskDrag],
  );

  const seekProbeVideo = useCallback((video: HTMLVideoElement, time: number): Promise<void> => {
    return new Promise((resolve) => {
      if (Math.abs(video.currentTime - time) < 0.02) {
        resolve();
        return;
      }
      let settled = false;
      const done = () => {
        if (settled) return;
        settled = true;
        video.removeEventListener("seeked", onSeeked);
        video.removeEventListener("error", onError);
        resolve();
      };
      const onSeeked = () => done();
      const onError = () => done();
      video.addEventListener("seeked", onSeeked, { once: true });
      video.addEventListener("error", onError, { once: true });
      setTimeout(done, 250);
      try {
        video.currentTime = time;
      }
      catch {
        done();
      }
    });
  }, []);

  const scanRegionShots = useCallback(
    async (mask: UnitMaskRegion, anchorTime?: number) => {
      setExpandingMaskId(mask.id);
      const scanToken = ++activeScanTokenRef.current;
      try {
        const probeVideo = probeVideoRef.current;
        const liveVideo = videoRef.current;
        if (!probeVideo || !liveVideo) return;

        const vw = liveVideo.videoWidth || 1280;
        const vh = liveVideo.videoHeight || 720;
        const sampleW = 48;
        const sampleH = 48;
        const sampleCanvas = document.createElement("canvas");
        sampleCanvas.width = sampleW;
        sampleCanvas.height = sampleH;
        const sCtx = sampleCanvas.getContext("2d", { willReadFrequently: true });
        if (!sCtx) return;

        const cropSx = Math.max(0, Math.floor(mask.x * vw));
        const cropSy = Math.max(0, Math.floor(mask.y * vh));
        const cropSw = Math.max(16, Math.floor(mask.width * vw));
        const cropSh = Math.max(16, Math.floor(mask.height * vh));

        const totalDur = probeVideo.duration || unitCount;
        if (totalDur <= 0) return;

        const baseShots = data?.shots && data.shots.length > 0 ? data.shots : [];
        const candidateTimes: { t: number; boundary: number }[] = [];

        if (baseShots.length > 1) {
          for (const s of baseShots) {
            candidateTimes.push({
              t: (s.startSec + s.endSec) / 2,
              boundary: s.startSec,
            });
          }
        }
        else {
          const step = 0.2;
          for (let t = 0.1; t < totalDur; t += step) {
            candidateTimes.push({
              t: Math.round(t * 100) / 100,
              boundary: Math.round(t * 100) / 100,
            });
          }
        }

        const detectedCuts: number[] = [];
        let prevImgData: ImageData["data"] | null = null;

        for (let i = 0; i < candidateTimes.length; i++) {
          if (scanToken !== activeScanTokenRef.current) return;
          const { t, boundary } = candidateTimes[i];

          await seekProbeVideo(probeVideo, Math.max(0, Math.min(totalDur - 0.05, t)));
          if (scanToken !== activeScanTokenRef.current) return;

          let curData: ImageData["data"] | null = null;
          try {
            sCtx.drawImage(probeVideo, cropSx, cropSy, cropSw, cropSh, 0, 0, sampleW, sampleH);
            curData = sCtx.getImageData(0, 0, sampleW, sampleH).data;
          }
          catch {
            break;
          }

          if (prevImgData && curData) {
            let diffSum = 0;
            for (let p = 0; p < curData.length; p += 4) {
              const dr = Math.abs(prevImgData[p] - curData[p]);
              const dg = Math.abs(prevImgData[p + 1] - curData[p + 1]);
              const db = Math.abs(prevImgData[p + 2] - curData[p + 2]);
              diffSum += (dr + dg + db) / (3 * 255);
            }
            const meanDiff = diffSum / (curData.length / 4);
            if (meanDiff > 0.035 && boundary > 0.05) {
              detectedCuts.push(boundary);
              prevImgData = curData;
            }
          }
          else if (curData) {
            prevImgData = curData;
          }

          if (i % 2 === 0) {
            await new Promise((r) => setTimeout(r, 16));
          }
        }

        if (scanToken !== activeScanTokenRef.current) return;

        const sortedCuts = Array.from(new Set(detectedCuts)).sort((a, b) => a - b);
        const boundaries = [0, ...sortedCuts.filter((c) => c > 0.05 && c < totalDur - 0.05), totalDur];
        const newShots: VideoShot[] = [];
        for (let i = 0; i < boundaries.length - 1; i++) {
          const s = Math.round(boundaries[i] * 10_000) / 10_000;
          const e = Math.round(boundaries[i + 1] * 10_000) / 10_000;
          const dur = Math.round((e - s) * 10_000) / 10_000;
          if (dur > 0.01) {
            newShots.push({
              id: `region-shot-${i}`,
              unitIndex: Math.floor(s),
              frameIndex: i,
              startSec: s,
              endSec: e,
              duration: dur,
            });
          }
        }

        if (newShots.length > 0) {
          setDynamicShots(newShots);
          const targetTime = anchorTime ?? videoRef.current?.currentTime ?? mask.startUnit;
          const matchingShot = newShots.find((s) => targetTime >= s.startSec - 0.001 && targetTime < s.endSec + 0.001)
            ?? newShots.find((s) => s.endSec >= targetTime)
            ?? newShots[0];
          if (matchingShot) {
            setMasks((prev) =>
              prev.map((b) =>
                b.id === mask.id
                  ? { ...b, startUnit: matchingShot.startSec, endUnit: matchingShot.endSec }
                  : b,
              ),
            );
            seekTo(computeSafeCursorTime(matchingShot.startSec, matchingShot.endSec, targetTime), true);
          }
        }
      }
      catch (err) {
        console.warn("[editor] scanRegionShots failed:", err);
      }
      finally {
        if (scanToken === activeScanTokenRef.current) {
          setTimeout(() => setExpandingMaskId(null), 250);
        }
      }
    },
    [data?.shots, unitCount, seekProbeVideo, seekTo],
  );

  const recalculateMaskSpan = useCallback(
    async (
      mask: UnitMaskRegion,
      curTime: number,
      direction: "forward" | "backward" | "both",
    ) => {
      setExpandingMaskId(mask.id);
      const scanToken = ++activeScanTokenRef.current;
      try {
        const probeVideo = probeVideoRef.current;
        const liveVideo = videoRef.current;
        if (!probeVideo || !liveVideo) return;

        const vw = liveVideo.videoWidth || 1280;
        const vh = liveVideo.videoHeight || 720;
        const sampleW = 48;
        const sampleH = 48;
        const sampleCanvas = document.createElement("canvas");
        sampleCanvas.width = sampleW;
        sampleCanvas.height = sampleH;
        const sCtx = sampleCanvas.getContext("2d", { willReadFrequently: true });
        if (!sCtx) return;

        const cropSx = Math.max(0, Math.floor(mask.x * vw));
        const cropSy = Math.max(0, Math.floor(mask.y * vh));
        const cropSw = Math.max(16, Math.floor(mask.width * vw));
        const cropSh = Math.max(16, Math.floor(mask.height * vh));

        const shots = activeShots.length > 0 ? activeShots : [];
        if (shots.length === 0) return;

        let curIdx = shots.findIndex((s) => curTime >= s.startSec - 0.001 && curTime < s.endSec + 0.001);
        if (curIdx === -1) {
          curIdx = shots.findIndex((s) => s.endSec >= curTime);
          if (curIdx === -1) curIdx = 0;
        }

        const baseTime = (shots[curIdx].startSec + shots[curIdx].endSec) / 2;
        await seekProbeVideo(probeVideo, Math.max(0, Math.min(probeVideo.duration - 0.05, baseTime)));
        if (scanToken !== activeScanTokenRef.current) return;

        let baseImgData: ImageData["data"] | null = null;
        try {
          sCtx.drawImage(probeVideo, cropSx, cropSy, cropSw, cropSh, 0, 0, sampleW, sampleH);
          baseImgData = sCtx.getImageData(0, 0, sampleW, sampleH).data;
        }
        catch {
          return;
        }
        if (!baseImgData) return;

        let curStart = mask.startUnit;
        let curEnd = mask.endUnit;

        const checkMatch = async (t: number, lastData: ImageData["data"]) => {
          await seekProbeVideo(probeVideo, Math.max(0, Math.min(probeVideo.duration - 0.05, t)));
          sCtx.drawImage(probeVideo, cropSx, cropSy, cropSw, cropSh, 0, 0, sampleW, sampleH);
          const probeData = sCtx.getImageData(0, 0, sampleW, sampleH).data;
          let diffSum = 0;
          for (let p = 0; p < probeData.length; p += 4) {
            const dr = Math.abs(baseImgData![p] - probeData[p]);
            const dg = Math.abs(baseImgData![p + 1] - probeData[p + 1]);
            const db = Math.abs(baseImgData![p + 2] - probeData[p + 2]);
            diffSum += (dr + dg + db) / (3 * 255);
          }
          const meanDiff = diffSum / (probeData.length / 4);
          return { matches: meanDiff <= 0.04, imgData: probeData };
        };

        if (direction === "forward" || direction === "both") {
          let lastData = baseImgData;
          for (let i = curIdx + 1; i < shots.length; i++) {
            if (scanToken !== activeScanTokenRef.current) return;
            const res = await checkMatch((shots[i].startSec + shots[i].endSec) / 2, lastData);
            if (res.matches) {
              lastData = res.imgData;
              curEnd = shots[i].endSec;
              setMasks((prev) =>
                prev.map((b) => (b.id === mask.id ? { ...b, endUnit: curEnd } : b)),
              );
              await new Promise((r) => setTimeout(r, 16));
            }
            else {
              break;
            }
          }
        }

        if (direction === "backward" || direction === "both") {
          let lastData = baseImgData;
          for (let i = curIdx - 1; i >= 0; i--) {
            if (scanToken !== activeScanTokenRef.current) return;
            const res = await checkMatch((shots[i].startSec + shots[i].endSec) / 2, lastData);
            if (res.matches) {
              lastData = res.imgData;
              curStart = shots[i].startSec;
              setMasks((prev) =>
                prev.map((b) => (b.id === mask.id ? { ...b, startUnit: curStart } : b)),
              );
              await new Promise((r) => setTimeout(r, 16));
            }
            else {
              break;
            }
          }
        }
      }
      catch (err) {
        console.warn("[editor] recalculateMaskSpan failed:", err);
      }
      finally {
        if (scanToken === activeScanTokenRef.current) {
          setTimeout(() => setExpandingMaskId(null), 250);
        }
      }
    },
    [activeShots, seekProbeVideo],
  );

  const onStagePointerUp = useCallback(() => {
    if (drawingMask) {
      const w = Math.abs(drawingMask.currentX - drawingMask.startX);
      const h = Math.abs(drawingMask.currentY - drawingMask.startY);
      if (w >= 0.02 && h >= 0.02) {
        const curTime = videoRef.current?.currentTime ?? 0;
        if (!canAddMaskAtTime(curTime, masksRef.current)) {
          setDrawingMask(null);
          setMaskLimitNotice("Maximum 3 overlapping masks allowed");
          setTimeout(() => setMaskLimitNotice(null), 2500);
          return;
        }
        const snappedTime = activeShots.length > 0
          ? snapToNearestShotBoundary(curTime, activeShots, unitCount)
          : curTime;
        const lookupTime = Math.abs(curTime - snappedTime) < 0.05 ? snappedTime + 0.001 : curTime;
        const shot = findShotAtTime(lookupTime, activeShots) ?? findShotAtTime(curTime, activeShots);
        const safeStart = shot
          ? shot.startSec
          : Math.max(0, Math.min(Math.max(0, unitCount - 1), Math.floor(curTime)));
        const safeEnd = shot
          ? shot.endSec
          : Math.max(safeStart + 1, Math.min(unitCount, safeStart + 1));
        const newMask: UnitMaskRegion = {
          id: `mask-${Math.random().toString(36).slice(2, 9)}`,
          startUnit: safeStart,
          endUnit: safeEnd,
          x: Math.min(drawingMask.startX, drawingMask.currentX),
          y: Math.min(drawingMask.startY, drawingMask.currentY),
          width: w,
          height: h,
        };
        setMasks((prev) => [...prev, newMask]);
        setSelectedMaskId(newMask.id);
        seekTo(computeSafeCursorTime(safeStart, safeEnd, curTime), true);
        void scanRegionShots(newMask, curTime);
      }
      setDrawingMask(null);
    }
    if (maskDrag) {
      const { id, initial } = maskDrag;
      const b = masksRef.current.find((item) => item.id === id);
      if (
        b &&
        (Math.abs(b.x - initial.x) > 0.005 ||
          Math.abs(b.y - initial.y) > 0.005 ||
          Math.abs(b.width - initial.width) > 0.005 ||
          Math.abs(b.height - initial.height) > 0.005)
      ) {
        const curTime = videoRef.current?.currentTime ?? b.startUnit;
        const shot = findShotAtTime(curTime, activeShots);
        const safeStart = shot ? shot.startSec : Math.floor(curTime);
        const safeEnd = shot ? shot.endSec : safeStart + 1;
        const adjustedMask: UnitMaskRegion = { ...b, startUnit: safeStart, endUnit: safeEnd };
        setMasks((prev) => prev.map((item) => (item.id === id ? adjustedMask : item)));
        seekTo(computeSafeCursorTime(safeStart, safeEnd, curTime), true);
        void scanRegionShots(adjustedMask, curTime);
      }
      setMaskDrag(null);
    }
  }, [drawingMask, maskDrag, unitCount, activeShots, seekTo, scanRegionShots]);

  const onMaskTrackPointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (saving) return;
      seekTo(unitFromEvent(e));
      setSelectedMaskId(null);
      setSelected(null);
    },
    [saving, seekTo, unitFromEvent],
  );

  const onMaskSpanPointerDown = useCallback(
    (
      e: React.PointerEvent,
      id: string,
      dragType: "move" | "start" | "end",
    ) => {
      if (saving) return;
      if (mode !== "mask") return;
      e.preventDefault();
      e.stopPropagation();

      const b = masksRef.current.find((item) => item.id === id);
      if (!b) return;

      setSelectedMaskId(id);
      setSelected(null);

      const timelineEl = maskTrackRef.current || timelineRef.current;
      if (!timelineEl) return;
      const rect = timelineEl.getBoundingClientRect();
      if (rect.width === 0) return;

      const initialStart = b.startUnit;
      const initialEnd = b.endUnit;
      const initialWidth = Math.max(0.01, initialEnd - initialStart);
      const startClientX = e.clientX;
      const totalUnits = Math.max(1, unitCount);
      let shiftHeld = e.shiftKey;
      let hasDragged = false;

      if (dragType === "end") {
        seekTo(Math.max(b.startUnit + 0.005, b.endUnit - 0.05), true);
      }
      else {
        seekTo(initialStart + 0.005, true);
      }

      const onWindowPointerMove = (ev: PointerEvent) => {
        ev.preventDefault();
        const deltaPx = ev.clientX - startClientX;
        if (Math.abs(deltaPx) > 3) {
          hasDragged = true;
        }
        if (!hasDragged) return;

        shiftHeld = ev.shiftKey || shiftHeld;
        const deltaUnits = (deltaPx / rect.width) * totalUnits;
        const isPrecise = ev.shiftKey || shiftHeld;

        if (dragType === "move") {
          const rawStart = initialStart + deltaUnits;
          let newStart = Math.max(0, Math.min(totalUnits - initialWidth, rawStart));
          if (!isPrecise) {
            const startShot = findShotAtTime(rawStart + 0.001, activeShots);
            newStart = startShot ? startShot.startSec : snapToNearestShotBoundary(rawStart, activeShots, totalUnits);
            newStart = Math.max(0, Math.min(totalUnits - initialWidth, newStart));
          }
          const newEnd = newStart + initialWidth;

          setMasks((prev) =>
            prev.map((item) =>
              item.id === id ? { ...item, startUnit: newStart, endUnit: newEnd } : item,
            ),
          );
          seekTo(newStart + 0.005);
        }
        else if (dragType === "start") {
          const rawStart = initialStart + deltaUnits;
          let newStart = Math.max(0, Math.min(totalUnits - 0.02, rawStart));
          if (!isPrecise) {
            const shot = findShotAtTime(rawStart + 0.001, activeShots);
            newStart = shot ? shot.startSec : snapToNearestShotBoundary(rawStart, activeShots, totalUnits);
          }
          let newEnd = initialEnd;
          if (newStart >= initialEnd) {
            newStart = Math.max(0, initialEnd - 0.02);
          }

          setMasks((prev) =>
            prev.map((item) =>
              item.id === id ? { ...item, startUnit: newStart, endUnit: newEnd } : item,
            ),
          );
          seekTo(newStart + 0.005);
        }
        else if (dragType === "end") {
          const rawEnd = initialEnd + deltaUnits;
          let newEnd = Math.min(totalUnits, Math.max(0.02, rawEnd));
          if (!isPrecise) {
            const shot = findShotAtTime(rawEnd - 0.001, activeShots);
            newEnd = shot ? shot.endSec : snapToNearestShotBoundary(rawEnd, activeShots, totalUnits);
          }
          let newStart = initialStart;
          if (newEnd <= initialStart) {
            newEnd = Math.min(totalUnits, initialStart + 0.02);
          }

          setMasks((prev) =>
            prev.map((item) =>
              item.id === id ? { ...item, startUnit: newStart, endUnit: newEnd } : item,
            ),
          );
          seekTo(Math.max(newStart + 0.005, newEnd - 0.05));
        }
      };

      const onWindowPointerUp = (ev: PointerEvent) => {
        ev.preventDefault();
        window.removeEventListener("pointermove", onWindowPointerMove);
        window.removeEventListener("pointerup", onWindowPointerUp);
        window.removeEventListener("pointercancel", onWindowPointerUp);

        if (!hasDragged) return;

        const isPrecise = ev.shiftKey || shiftHeld;

        setMasks((prev) =>
          prev.map((item) => {
            if (item.id !== id) return item;
            let s: number;
            let e: number;
            if (isPrecise) {
              s = Math.max(0, Math.min(totalUnits - 0.033, Math.round(item.startUnit * 10_000) / 10_000));
              e = Math.max(s + 0.033, Math.min(totalUnits, Math.round(item.endUnit * 10_000) / 10_000));
            }
            else {
              const startShot = findShotAtTime(item.startUnit + 0.001, activeShots);
              const endShot = findShotAtTime(item.endUnit - 0.001, activeShots);
              s = startShot ? startShot.startSec : snapToNearestShotBoundary(item.startUnit, activeShots, totalUnits);
              e = endShot ? endShot.endSec : snapToNearestShotBoundary(item.endUnit, activeShots, totalUnits);
              if (e <= s) {
                const shot = findShotAtTime(s + 0.001, activeShots) ?? findShotAtTime(s, activeShots);
                e = shot ? shot.endSec : Math.min(totalUnits, s + 1);
              }
            }
            if (dragType === "end") {
              seekTo(Math.max(s + 0.005, e - 0.05));
            }
            else {
              seekTo(s + 0.005);
            }
            const updated = { ...item, startUnit: s, endUnit: e };
            if (dragType === "end") {
              void recalculateMaskSpan(updated, Math.max(s, e - 0.05), "forward");
            }
            else if (dragType === "start") {
              void recalculateMaskSpan(updated, s + 0.05, "backward");
            }
            else if (dragType === "move") {
              void recalculateMaskSpan(updated, s, "both");
            }
            return updated;
          }),
        );
      };

      window.addEventListener("pointermove", onWindowPointerMove);
      window.addEventListener("pointerup", onWindowPointerUp);
      window.addEventListener("pointercancel", onWindowPointerUp);
    },
    [saving, seekTo, unitCount, activeShots, mode, recalculateMaskSpan],
  );

  // ── Keyboard ────────────────────────────────────────────────
  // Capture phase + preventDefault so hosting apps' global key handlers
  // (e.g. the desktop router's Backspace-goes-back) never fire underneath
  // an open editor — losing unsaved cuts to a stray Backspace is the worst
  // possible outcome of this surface.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (target && ["INPUT", "TEXTAREA"].includes(target.tagName)) return;
      if ((e.metaKey || e.ctrlKey) && e.key === "0") {
        e.preventDefault();
        resetZoom();
        return;
      }
      if ((e.metaKey || e.ctrlKey) && (e.key === "=" || e.key === "+")) {
        e.preventDefault();
        zoomIn();
        return;
      }
      if ((e.metaKey || e.ctrlKey) && (e.key === "-" || e.key === "_")) {
        e.preventDefault();
        zoomOut();
        return;
      }
      if (e.metaKey || e.ctrlKey) return;
      if (e.key === " " || e.key === "k") {
        e.preventDefault();
        togglePlay();
      } else if ((e.key === "x" || e.key === "c") && mode === "cut") {
        e.preventDefault();
        cutHere();
      } else if (e.key === "+" || e.key === "=") {
        e.preventDefault();
        zoomIn();
      } else if (e.key === "-" || e.key === "_") {
        e.preventDefault();
        zoomOut();
      } else if (e.key === "0") {
        e.preventDefault();
        resetZoom();
      } else if (e.key === "Delete" || e.key === "Backspace") {
        e.preventDefault();
        if (mode === "mask" && selectedMaskId !== null) {
          setMasks((prev) => prev.filter((b) => b.id !== selectedMaskId));
          setSelectedMaskId(null);
        } else if (mode === "cut" && selected !== null) {
          setRegions((prev) => prev.filter((_, i) => i !== selected));
          setSelected(null);
        }
      } else if (e.key === "Escape") {
        e.preventDefault();
        setSelected(null);
        setSelectedMaskId(null);
        setMaskMenuOpen(false);
      } else if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
        e.preventDefault();
        const step = e.shiftKey ? 10 : 1;
        seekTo(
          (videoRef.current?.currentTime ?? 0) +
            (e.key === "ArrowLeft" ? -step : step),
        );
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [selected, selectedMaskId, seekTo, togglePlay, cutHere, zoomIn, zoomOut, resetZoom, mode]);

  useEffect(() => {
    if (!maskMenuOpen) return;
    const onPointerDownOutside = (e: PointerEvent) => {
      if (maskMenuRef.current && !maskMenuRef.current.contains(e.target as Node)) {
        setMaskMenuOpen(false);
      }
    };
    window.addEventListener("pointerdown", onPointerDownOutside);
    return () => {
      window.removeEventListener("pointerdown", onPointerDownOutside);
    };
  }, [maskMenuOpen]);

  // ── Publish ─────────────────────────────────────────────────
  const save = useCallback(async () => {
    if (!data || saving) return;
    setSaving(true);
    setSaveError(null);
    setSaveSuccess(false);
    try {
      const cuts = regionsToCuts(normalizeRegions(regionsRef.current), data.units);
      const masksList = unitMasksToMasks(masksRef.current, data.units);
      await client.setCuts(cuts, masksList);
      const result = await client.applyCuts();
      if (!result.instant && result.status === "compiling") {
        const pollStart = Date.now();
        const maxWaitMs = 60_000;
        while (Date.now() - pollStart < maxWaitMs) {
          await new Promise((resolve) => setTimeout(resolve, 500));
          try {
            const st = await client.getStatus();
            if (st.status === "complete") break;
            if (st.status === "failed") throw new Error("Compilation failed on server");
            if (!st.editable && st.status !== "compiling") break;
          }
          catch (pollErr) {
            if (pollErr instanceof Error && pollErr.message.includes("Compilation failed")) throw pollErr;
          }
        }
      }
      setSaveSuccess(true);
      setSaving(false);
      onApplied?.(result);
    }
    catch (err) {
      setSaveError(err instanceof Error ? err.message : String(err));
      setSaving(false);
    }
  }, [client, data, saving, onApplied]);

  // ── Derived display values ──────────────────────────────────
  const normalized = useMemo(() => normalizeRegions(regions), [regions]);
  // Count what the SERVER will count. The footer used to count region
  // widths in unit space while the server counted timestamp membership on
  // the serialized intervals — so the two could disagree, and the editor
  // would happily offer a Save the server then rejected. Same input, same
  // shared function, no daylight between them.
  const serializedCuts = useMemo(
    () => (data ? regionsToCuts(normalized, data.units) : []),
    [normalized, data],
  );
  const unitTimesMs = useMemo(
    () => units.map((u) => Date.parse(u.capturedAt)),
    [units],
  );
  const removedUnits = useMemo(
    () => countCutUnits(unitTimesMs, serializedCuts),
    [unitTimesMs, serializedCuts],
  );
  const keptUnits = unitCount - removedUnits;
  const totalMaskedSec = useMemo(() => {
    if (masks.length === 0 || unitCount === 0) return 0;
    const intervals: Array<[number, number]> = masks.map((b) => [
      Math.max(0, b.startUnit),
      Math.min(unitCount, b.endUnit),
    ]);
    intervals.sort((a, b) => a[0] - b[0]);
    let mergedUnits = 0;
    let curInterval: [number, number] | null = null;
    for (const [start, end] of intervals) {
      if (!curInterval) {
        curInterval = [start, end];
      }
      else if (start <= curInterval[1]) {
        curInterval[1] = Math.max(curInterval[1], end);
      }
      else {
        mergedUnits += Math.max(0, curInterval[1] - curInterval[0]);
        curInterval = [start, end];
      }
    }
    if (curInterval) {
      mergedUnits += Math.max(0, curInterval[1] - curInterval[0]);
    }
    return mergedUnits * 60;
  }, [masks, unitCount]);

  const [flowMaskedSec, setFlowMaskedSec] = useState(0);
  useEffect(() => {
    const target = Math.max(0, Math.round(totalMaskedSec));
    if (target === 0) {
      setFlowMaskedSec(0);
      return;
    }
    const raf = requestAnimationFrame(() => {
      setFlowMaskedSec(target);
    });
    return () => cancelAnimationFrame(raf);
  }, [totalMaskedSec]);

  const selectedMask = useMemo(
    () => masks.find((b) => b.id === selectedMaskId) ?? null,
    [masks, selectedMaskId],
  );
  const allCut = unitCount > 0 && keptUnits === 0;
  const gaps = useMemo(() => (data ? gapIndices(data.units) : []), [data]);
  const step = useMemo(
    () => rulerStep(unitCount, stripWidth),
    [unitCount, stripWidth],
  );
  const ticks = useMemo(() => rulerTicks(unitCount, step), [unitCount, step]);
  const currentUnit = unitAtTime(time, Math.max(1, unitCount));
  const inCutNow = regionAtTime(time, normalized) !== null;
  const pct = (u: number) => `${(u / Math.max(1, unitCount)) * 100}%`;

  // Keep the host informed of the working cut list, so closing the
  // window can publish exactly what's on screen.
  const onCutsChangeRef = useRef(onCutsChange);
  onCutsChangeRef.current = onCutsChange;
  useEffect(() => {
    if (!data) return;
    const saved = JSON.stringify(data.cuts ?? []);
    onCutsChangeRef.current?.(
      serializedCuts,
      JSON.stringify(serializedCuts) !== saved,
    );
  }, [serializedCuts, data]);

  const onMasksChangeRef = useRef(onMasksChange);
  onMasksChangeRef.current = onMasksChange;
  const onBlursChangeRef = useRef(onBlursChange);
  onBlursChangeRef.current = onBlursChange;
  useEffect(() => {
    if (!data) return;
    const serializedMasks = unitMasksToMasks(masks, data.units);
    const saved = JSON.stringify(data.masks ?? (data as any).blurs ?? []);
    const isDirty = JSON.stringify(serializedMasks) !== saved;
    onMasksChangeRef.current?.(serializedMasks, isDirty);
    onBlursChangeRef.current?.(serializedMasks, isDirty);
  }, [masks, data]);

  // ── Render ──────────────────────────────────────────────────
  if (loadError) {
    return (
      <div style={{ padding: spacing.xl, maxWidth: 520 }}>
        <ErrorDisplay error={loadError} variant="banner" title="Can't edit" />
        {onCancel && (
          <div style={{ marginTop: spacing.md }}>
            <Button variant="secondary" size="sm" onClick={onCancel}>
              Close
            </Button>
          </div>
        )}
      </div>
    );
  }

  if (!data) {
    return (
      <div
        style={{
          height: "100%",
          minHeight: 260,
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          justifyContent: "center",
          gap: spacing.lg,
          textAlign: "center",
          padding: spacing.xl,
        }}
      >
        {preparingUnits !== null ? (
          <ProgressRing progress={buildProgress} showPercent />
        ) : (
          <Spinner size="lg" />
        )}
        <div>
          <div
            style={{
              fontSize: fontSize.xl,
              fontWeight: fontWeight.semibold,
              color: colors.text.primary,
              letterSpacing: "-0.01em",
            }}
          >
            Preparing your timelapse
          </div>
          <div
            style={{
              fontSize: fontSize.md,
              color: colors.text.secondary,
              marginTop: spacing.xs,
              maxWidth: 340,
              lineHeight: 1.5,
            }}
          >
            {preparingUnits !== null && preparingUnits > 0
              ? `Stitching ${preparingUnits} minute${
                  preparingUnits === 1 ? "" : "s"
                } of footage.`
              : "oooooooooooo"}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div
      ref={containerRef}
      style={{
        height: "100%",
        minHeight: 0,
        display: "flex",
        flexDirection: "column",
        gap: rootGap,
      }}
    >
      {/* ── Stage: shrinks into whatever space the dock leaves ── */}
      <div
        style={{
          flex: "1 1 auto",
          minHeight: 0,
          minWidth: 0,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          position: "relative",
          background: colors.editor.well,
          border: `1px solid ${colors.editor.wellBorder}`,
          borderRadius: 12,
          overflow: "hidden",
        }}
      >
        <div
          ref={videoBoxRef}
          style={{
            position: "relative",
            aspectRatio: videoAspect ? `${videoAspect}` : "16 / 9",
            // max-* rather than width:100% is what lets the stage shrink:
            // the video letterboxes into whatever height is left instead of
            // forcing the dock off the bottom of the window.
            maxWidth: "100%",
            maxHeight: "100%",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            isolation: "isolate",
            transform: "translateZ(0)",
          }}
        >
          <video
            ref={videoRef}
            src={data.originalVideoUrl ?? undefined}
            playsInline
            muted
            onClick={mode === "cut" ? togglePlay : undefined}
            onPlay={() => setPlaying(true)}
            onPause={() => setPlaying(false)}
            onLoadedMetadata={(e) => {
              const v = e.currentTarget;
              if (v.videoWidth && v.videoHeight) {
                setVideoAspect(v.videoWidth / v.videoHeight);
              }
            }}
            style={{
              width: "100%",
              height: "100%",
              maxWidth: "100%",
              maxHeight: "100%",
              display: "block",
              objectFit: "contain",
              cursor: mode === "cut" ? "pointer" : "crosshair",
            }}
          />

          {/* Mask overlay layer directly on the video */}
          <div
            ref={overlayRef}
            onPointerDown={mode === "mask" ? onStagePointerDown : undefined}
            onPointerMove={mode === "mask" ? onStagePointerMove : undefined}
            onPointerUp={mode === "mask" ? onStagePointerUp : undefined}
            style={{
              position: "absolute",
              inset: 0,
              pointerEvents: mode === "mask" ? "auto" : "none",
              cursor: mode === "mask" ? "crosshair" : "default",
              userSelect: "none",
              touchAction: "none",
            }}
          >
            {mode === "mask" && masks.length === 0 && !drawingMask && (
              <div
                style={{
                  position: "absolute",
                  top: 12,
                  left: "50%",
                  transform: "translateX(-50%)",
                  padding: "4px 10px",
                  borderRadius: 9999,
                  background: "rgba(0, 0, 0, 0.65)",
                  backdropFilter: "blur(8px)",
                  WebkitBackdropFilter: "blur(8px)",
                  color: "#fff",
                  fontSize: fontSize.xs,
                  fontWeight: fontWeight.medium,
                  pointerEvents: "none",
                  display: "flex",
                  alignItems: "center",
                  gap: 6,
                  boxShadow: "0 2px 8px rgba(0,0,0,0.3)",
                }}
              >
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="M12 5v14M5 12h14" />
                </svg>
                <span>Click and drag on video to mask an area</span>
              </div>
            )}
            {masks.map((b) => {
              const isSelected = selectedMaskId === b.id && mode === "mask";
              const isActive = isMaskActiveAtTime(time, b);
              if (!isActive) return null;
              const trackIdx = trackAllocation.assignments[b.id] ?? 0;
              const preset = TRACK_PRESETS[trackIdx] ?? TRACK_PRESETS[0];

              return (
                <div
                  key={b.id}
                  className="lk-ed-mask-box"
                  onPointerDown={
                    mode === "mask"
                      ? (e) => onMaskBoxPointerDown(e, b.id, "move")
                      : undefined
                  }
                  style={{
                    position: "absolute",
                    left: `${b.x * 100}%`,
                    top: `${b.y * 100}%`,
                    width: `${b.width * 100}%`,
                    height: `${b.height * 100}%`,
                    backdropFilter: "blur(12px)",
                    WebkitBackdropFilter: "blur(12px)",
                    transform: "translate3d(0, 0, 0)",
                    backgroundColor: isSelected ? preset.bgSelected : preset.bgUnselected,
                    borderRadius: 8,
                    border: isSelected
                      ? `2px solid ${preset.border}`
                      : `1.5px solid ${preset.border}`,
                    boxShadow: isSelected
                      ? `inset 0 0 0 1px rgba(255, 255, 255, 0.25), 0 0 16px ${preset.color}66`
                      : "inset 0 0 0 1px rgba(255, 255, 255, 0.12), 0 2px 8px rgba(0,0,0,0.25)",
                    cursor:
                      mode === "mask"
                        ? isSelected
                          ? "move"
                          : "pointer"
                        : "default",
                    pointerEvents: mode === "mask" ? "auto" : "none",
                    boxSizing: "border-box",
                  }}
                >
                  {isSelected && (
                    <>
                      {(["nw", "ne", "sw", "se"] as const).map((handle) => {
                        const style: React.CSSProperties = {
                          position: "absolute",
                          width: 10,
                          height: 10,
                          borderRadius: "50%",
                          background: "#fff",
                          border: `2px solid ${preset.border}`,
                          boxShadow: "0 1px 3px rgba(0,0,0,0.4)",
                          zIndex: 10,
                        };
                        if (handle === "nw") {
                          style.top = -5;
                          style.left = -5;
                          style.cursor = "nwse-resize";
                        } else if (handle === "ne") {
                          style.top = -5;
                          style.right = -5;
                          style.cursor = "nesw-resize";
                        } else if (handle === "sw") {
                          style.bottom = -5;
                          style.left = -5;
                          style.cursor = "nesw-resize";
                        } else {
                          style.bottom = -5;
                          style.right = -5;
                          style.cursor = "nwse-resize";
                        }
                        return (
                          <div
                            key={handle}
                            onPointerDown={(e) =>
                              onMaskBoxPointerDown(e, b.id, handle)
                            }
                            style={style}
                          />
                        );
                      })}

                      <div
                        onPointerDown={(e) => e.stopPropagation()}
                        style={{
                          position: "absolute",
                          top: 6,
                          left: 6,
                          display: "inline-flex",
                          alignItems: "center",
                          gap: 5,
                          padding: "2px 6px 2px 8px",
                          borderRadius: 9999,
                          background: "#2563eb",
                          color: "#ffffff",
                          fontSize: 10,
                          fontWeight: fontWeight.semibold,
                          fontVariantNumeric: "tabular-nums",
                          letterSpacing: "0.01em",
                          boxShadow: "0 1px 4px rgba(0, 0, 0, 0.4)",
                          zIndex: 12,
                          pointerEvents: "auto",
                          userSelect: "none",
                        }}
                      >
                        <span>Mask {trackIdx + 1}</span>
                        <button
                          type="button"
                          onPointerDown={(e) => e.stopPropagation()}
                          onClick={(e) => {
                            e.stopPropagation();
                            setMasks((prev) =>
                              prev.filter((item) => item.id !== b.id),
                            );
                            setSelectedMaskId(null);
                          }}
                          style={{
                            background: "rgba(255, 255, 255, 0.2)",
                            border: "none",
                            color: "#ffffff",
                            cursor: "pointer",
                            width: 13,
                            height: 13,
                            borderRadius: "50%",
                            display: "flex",
                            alignItems: "center",
                            justifyContent: "center",
                            padding: 0,
                            lineHeight: 1,
                            transition: "background 0.1s",
                          }}
                          onMouseEnter={(e) => {
                            e.currentTarget.style.background = "rgba(255, 255, 255, 0.4)";
                          }}
                          onMouseLeave={(e) => {
                            e.currentTarget.style.background = "rgba(255, 255, 255, 0.2)";
                          }}
                          title="Delete mask"
                          aria-label="Delete mask"
                        >
                          <svg width="8" height="8" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                            <line x1="18" y1="6" x2="6" y2="18" />
                            <line x1="6" y1="6" x2="18" y2="18" />
                          </svg>
                        </button>
                      </div>
                    </>
                  )}
                </div>
              );
            })}

            {drawingMask && (
              <div
                style={{
                  position: "absolute",
                  left: `${Math.min(drawingMask.startX, drawingMask.currentX) * 100}%`,
                  top: `${Math.min(drawingMask.startY, drawingMask.currentY) * 100}%`,
                  width: `${Math.abs(drawingMask.currentX - drawingMask.startX) * 100}%`,
                  height: `${Math.abs(drawingMask.currentY - drawingMask.startY) * 100}%`,
                  border: `2px dashed ${TRACK_PRESETS[drawingTrackIdx]?.border ?? colors.accent.base}`,
                  backgroundColor: TRACK_PRESETS[drawingTrackIdx]?.bgSelected ?? "rgba(59, 130, 246, 0.25)",
                  boxShadow: `0 0 16px ${TRACK_PRESETS[drawingTrackIdx]?.color ?? colors.accent.base}44`,
                  backdropFilter: "blur(4px)",
                  WebkitBackdropFilter: "blur(4px)",
                  borderRadius: 6,
                  pointerEvents: "none",
                }}
              />
            )}

            {maskLimitNotice && (
              <div
                style={{
                  position: "absolute",
                  top: 16,
                  left: "50%",
                  transform: "translateX(-50%)",
                  padding: "6px 14px",
                  borderRadius: radii.md,
                  background: "rgba(220, 38, 38, 0.92)",
                  backdropFilter: "blur(8px)",
                  WebkitBackdropFilter: "blur(8px)",
                  color: "#ffffff",
                  fontSize: fontSize.xs,
                  fontWeight: fontWeight.semibold,
                  boxShadow: "0 4px 12px rgba(0, 0, 0, 0.4)",
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                  zIndex: 50,
                  pointerEvents: "none",
                }}
              >
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <circle cx="12" cy="12" r="10" />
                  <line x1="12" y1="8" x2="12" y2="12" />
                  <line x1="12" y1="16" x2="12.01" y2="16" />
                </svg>
                <span>{maskLimitNotice}</span>
              </div>
            )}
          </div>
        </div>

        <AnimatePresence>
          {!playing && mode === "cut" && (
            <motion.div
              initial={{ opacity: 0, scale: 0.88 }}
              animate={{ opacity: 1, scale: 1 }}
              exit={{ opacity: 0, scale: 0.88 }}
              transition={{ duration: 0.16, ease: [0.25, 1, 0.5, 1] }}
              style={{
                position: "absolute",
                inset: 0,
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                pointerEvents: "none",
              }}
            >
              <div
                style={{
                  width: playOverlaySize,
                  height: playOverlaySize,
                  borderRadius: "50%",
                  background: "rgba(0,0,0,0.55)",
                  backdropFilter: "blur(12px)",
                  WebkitBackdropFilter: "blur(12px)",
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                }}
              >
                <svg
                  width={isVeryShort ? 14 : isShort ? 16 : 20}
                  height={isVeryShort ? 14 : isShort ? 16 : 20}
                  viewBox="0 0 24 24"
                  fill="#fff"
                  aria-hidden="true"
                >
                  <path d="M8 5v14l11-7z" />
                </svg>
              </div>
            </motion.div>
          )}
        </AnimatePresence>

        <AnimatePresence>
          {inCutNow && (
            <motion.div
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              exit={{ opacity: 0 }}
              transition={{ duration: 0.12 }}
              style={{
                position: "absolute",
                inset: 0,
                // Matches the stage's radius: an inset ring on a square
                // box inside a rounded, clipped parent gets sliced at the
                // corners and reads as a rendering glitch.
                borderRadius: 12,
                boxShadow: `inset 0 0 0 3px ${colors.editor.cutBorder}`,
                // Tint only, no hatch: the stage is already showing the
                // frame you're judging, and texture over live footage
                // fights it. The hatch belongs on the timeline, where the
                // question is "which stretch", not "what's in it".
                backgroundColor: "rgba(220, 38, 38, 0.10)",
                pointerEvents: "none",
              }}
            >
              <span
                style={{
                  position: "absolute",
                  top: spacing.md,
                  right: spacing.md,
                  background: colors.editor.cutBorder,
                  color: "#fff",
                  fontSize: fontSize.xs,
                  fontWeight: fontWeight.semibold,
                  padding: "3px 8px",
                  borderRadius: radii.sm,
                  letterSpacing: "0.01em",
                }}
              >
                Will be removed
              </span>
            </motion.div>
          )}
        </AnimatePresence>
      </div>

      {/* ── Dock: transport, timeline, actions ───────────────── */}
      <div
        ref={timelineAreaRef}
        style={{ flex: "0 0 auto", display: "flex", flexDirection: "column", gap: dockGap }}
      >
        {/* Transport */}
        <div style={{ display: "flex", alignItems: "center", gap: isNarrow ? 6 : spacing.sm, minHeight: isShort ? 26 : 30 }}>
          <button
            className="lk-ed-iconbtn"
            onClick={togglePlay}
            aria-label={playing ? "Pause" : "Play"}
            style={{ width: isShort ? 26 : 30, height: isShort ? 26 : 30, borderRadius: radii.md }}
          >
            {playing ? (
              <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                <path d="M6 4h4v16H6zM14 4h4v16h-4z" />
              </svg>
            ) : (
              <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                <path d="M8 5v14l11-7z" />
              </svg>
            )}
          </button>

          <div
            style={{
              fontSize: isNarrow ? fontSize.sm : fontSize.md,
              color: colors.text.primary,
              fontVariantNumeric: "tabular-nums",
              letterSpacing: "-0.01em",
              whiteSpace: "nowrap",
            }}
          >
            {elapsedLabel(currentUnit, unitCount)}
            <span style={{ color: colors.text.tertiary }}>
              {isTinyWidth ? " / " : " of "}
              {elapsedLabel(unitCount, unitCount)}
              {units[currentUnit] && (
                <>
                  {isNarrow ? " · " : " · recorded at "}
                  {unitClockLabel(units[currentUnit])}
                </>
              )}
            </span>
          </div>

          {/* Center region: Hold Shift precision indicator */}
          <div
            style={{
              flex: 1,
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              gap: 8,
              minWidth: 0,
              padding: "0 8px",
            }}
          >
            <AnimatePresence>
              {!isNarrow && mode === "mask" && isTimelineHovered && masks.length > 0 && (
                <motion.div
                  initial={{ opacity: 0, y: -4, scale: 0.95 }}
                  animate={{ opacity: 1, y: 0, scale: 1 }}
                  exit={{ opacity: 0, y: -4, scale: 0.95 }}
                  transition={{ duration: 0.15 }}
                  style={{
                    display: "inline-flex",
                    alignItems: "center",
                    gap: 5,
                    padding: isShort ? "1px 6px" : "2px 8px",
                    borderRadius: 9999,
                    background: colors.bg.sunken,
                    border: `1px solid ${colors.border.default}`,
                    color: colors.text.tertiary,
                    fontSize: fontSize.xs,
                    fontWeight: fontWeight.medium,
                    userSelect: "none",
                    whiteSpace: "nowrap",
                    letterSpacing: "-0.01em",
                  }}
                  title="Hold Shift while dragging mask handles for frame precision without snapping."
                >
                  <kbd
                    style={{
                      fontFamily: "inherit",
                      fontSize: 10,
                      fontWeight: fontWeight.semibold,
                      padding: "1px 5px",
                      borderRadius: 3,
                      background: colors.bg.surface,
                      border: `1px solid ${colors.border.hover}`,
                      color: colors.text.secondary,
                      boxShadow: "0 1px 2px rgba(0, 0, 0, 0.08)",
                    }}
                  >
                    ⇧ Shift
                  </kbd>
                  <span>for precise control</span>
                </motion.div>
              )}
            </AnimatePresence>
          </div>

          {/* Mode Switcher */}
          <div
            style={{
              display: "inline-flex",
              alignItems: "center",
              background: colors.bg.sunken,
              border: `1px solid ${colors.border.default}`,
              borderRadius: radii.md,
              padding: isShort ? 1 : 2,
              gap: isShort ? 1 : 2,
            }}
          >
            <button
              type="button"
              onClick={() => {
                setMode("cut");
                setSelectedMaskId(null);
              }}
              style={{
                display: "inline-flex",
                alignItems: "center",
                gap: isNarrow ? 3 : 5,
                padding: isShort ? "2px 6px" : isNarrow ? "2px 7px" : "3px 10px",
                fontSize: fontSize.xs,
                fontWeight: fontWeight.semibold,
                borderRadius: radii.sm,
                border: "none",
                cursor: "pointer",
                background: mode === "cut" ? colors.bg.surface : "transparent",
                color: mode === "cut" ? colors.text.primary : colors.text.secondary,
                boxShadow: mode === "cut" ? "0 1px 2px rgba(0,0,0,0.2)" : "none",
                transition: "all 120ms ease",
              }}
            >
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <circle cx="6" cy="6" r="3" />
                <circle cx="6" cy="18" r="3" />
                <line x1="20" y1="4" x2="8.12" y2="15.88" />
                <line x1="14.47" y1="14.48" x2="20" y2="20" />
                <line x1="8.12" y1="8.12" x2="12" y2="12" />
              </svg>
              Cut
            </button>
            <button
              type="button"
              onClick={() => {
                setMode("mask");
                setSelected(null);
                videoRef.current?.pause();
              }}
              style={{
                display: "inline-flex",
                alignItems: "center",
                gap: isNarrow ? 3 : 5,
                padding: isShort ? "2px 6px" : isNarrow ? "2px 7px" : "3px 10px",
                fontSize: fontSize.xs,
                fontWeight: fontWeight.semibold,
                borderRadius: radii.sm,
                border: "none",
                cursor: "pointer",
                background: mode === "mask" ? colors.bg.surface : "transparent",
                color: mode === "mask" ? colors.text.primary : colors.text.secondary,
                boxShadow: mode === "mask" ? "0 1px 2px rgba(0,0,0,0.2)" : "none",
                transition: "all 120ms ease",
              }}
            >
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <rect x="3" y="3" width="8" height="8" rx="1" fill="currentColor" />
                <rect x="13" y="3" width="8" height="8" rx="1" fill="currentColor" opacity="0.35" />
                <rect x="3" y="13" width="8" height="8" rx="1" fill="currentColor" opacity="0.35" />
                <rect x="13" y="13" width="8" height="8" rx="1" fill="currentColor" />
              </svg>
              Mask
            </button>
          </div>

          {/* Zoom controls */}
          <div
            style={{
              display: "inline-flex",
              alignItems: "center",
              background: colors.bg.sunken,
              border: `1px solid ${colors.border.default}`,
              borderRadius: radii.md,
              padding: isShort ? 1 : 2,
              gap: 1,
            }}
          >
            <button
              type="button"
              onClick={zoomOut}
              disabled={zoom <= 1}
              title="Zoom out (- / Scroll wheel down)"
              style={{
                display: "inline-flex",
                alignItems: "center",
                justifyContent: "center",
                width: isShort ? 20 : 22,
                height: isShort ? 20 : 22,
                borderRadius: radii.sm,
                border: "none",
                background: "transparent",
                color: zoom <= 1 ? colors.text.quaternary : colors.text.secondary,
                cursor: zoom <= 1 ? "not-allowed" : "pointer",
                padding: 0,
                transition: "all 120ms ease",
              }}
              onMouseEnter={(e) => {
                if (zoom > 1) e.currentTarget.style.background = colors.bg.surface;
              }}
              onMouseLeave={(e) => {
                e.currentTarget.style.background = "transparent";
              }}
            >
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <line x1="5" y1="12" x2="19" y2="12" />
              </svg>
            </button>
            <button
              type="button"
              onClick={resetZoom}
              title={zoom > 1 ? "Click to reset zoom to 1x (Cmd/Ctrl+0)" : "Zoom level"}
              style={{
                display: "inline-flex",
                alignItems: "center",
                justifyContent: "center",
                padding: isShort ? "1px 4px" : "2px 6px",
                fontSize: 10,
                fontWeight: fontWeight.semibold,
                fontVariantNumeric: "tabular-nums",
                borderRadius: radii.sm,
                border: "none",
                background: zoom > 1 ? colors.bg.surface : "transparent",
                color: zoom > 1 ? colors.accent.base : colors.text.tertiary,
                cursor: zoom > 1 ? "pointer" : "default",
                transition: "all 120ms ease",
              }}
            >
              {zoom.toFixed(zoom % 1 === 0 ? 0 : 1)}x
            </button>
            <button
              type="button"
              onClick={zoomIn}
              disabled={zoom >= 16}
              title="Zoom in (+ / Scroll wheel up)"
              style={{
                display: "inline-flex",
                alignItems: "center",
                justifyContent: "center",
                width: isShort ? 20 : 22,
                height: isShort ? 20 : 22,
                borderRadius: radii.sm,
                border: "none",
                background: "transparent",
                color: zoom >= 16 ? colors.text.quaternary : colors.text.secondary,
                cursor: zoom >= 16 ? "not-allowed" : "pointer",
                padding: 0,
                transition: "all 120ms ease",
              }}
              onMouseEnter={(e) => {
                if (zoom < 16) e.currentTarget.style.background = colors.bg.surface;
              }}
              onMouseLeave={(e) => {
                e.currentTarget.style.background = "transparent";
              }}
            >
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <line x1="12" y1="5" x2="12" y2="19" />
                <line x1="5" y1="12" x2="19" y2="12" />
              </svg>
            </button>
          </div>
        </div>

        {/* Timeline Track & Overlay Container (Zero flow shift) */}
        <div
          style={{
            position: "relative",
            width: "100%",
          }}
          onPointerEnter={() => setIsTimelineHovered(true)}
          onPointerLeave={() => setIsTimelineHovered(false)}
        >
          {/* Timeline Scroll Container */}
          <div
            ref={timelineScrollRef}
          className="lk-ed-scroll-track"
          style={{
            position: "relative",
            width: "100%",
            overflowX: zoom > 1 ? "auto" : "hidden",
            overflowY: "hidden",
            userSelect: "none",
            touchAction: "pan-x",
            borderRadius: radii.md,
          }}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
        >
          {/* Scaled Timeline Inner Content */}
          <div
            style={{
              position: "relative",
              width: `${zoom * 100}%`,
              minWidth: "100%",
            }}
          >
          {/* Playhead: a slim cap at the foot of the ruler with a stem
              through the strip. Rendered as a sibling of both lanes (not
              inside the strip) so it isn't clipped by its overflow. */}
          {unitCount > 0 && (
            <div
              style={{
                position: "absolute",
                left: pct(Math.min(time, unitCount)),
                top: 0,
                bottom: 0,
                width: 0,
                zIndex: 3,
                pointerEvents: "none",
                transition: isSmoothSeek ? "left 0.28s cubic-bezier(0.16, 1, 0.3, 1)" : "none",
                willChange: isSmoothSeek ? "left" : "auto",
              }}
            >
              <div
                onPointerDown={onRulerPointerDown}
                aria-hidden="true"
                style={{
                  position: "absolute",
                  top: rulerHeight - HEAD_H,
                  left: -HEAD_HIT / 2,
                  width: HEAD_HIT,
                  height: HEAD_HIT,
                  display: "flex",
                  justifyContent: "center",
                  alignItems: "flex-start",
                  cursor: "ew-resize",
                  pointerEvents: "auto",
                }}
              >
                <div
                  className="lk-ed-playhead"
                  style={{
                    width: HEAD_W,
                    height: HEAD_H,
                    borderRadius: HEAD_W / 2,
                    background: colors.text.primary,
                    boxShadow: "0 1px 3px rgba(0,0,0,0.45)",
                  }}
                />
              </div>
              <div
                style={{
                  position: "absolute",
                  top: rulerHeight - 4,
                  bottom: 0,
                  left: -1,
                  width: 2,
                  background: colors.text.primary,
                  boxShadow: "0 0 0 0.5px rgba(0,0,0,0.35)",
                }}
              />
            </div>
          )}

          {/* Ruler lane — owns scrubbing. Labels sit above their tick, at
              a step chosen so they never crowd (see rulerStep). */}
          <div
            onPointerDown={onRulerPointerDown}
            style={{
              position: "relative",
              height: rulerHeight,
              cursor: "ew-resize",
            }}
          >
            {ticks.map(({ unit, major }) => {
              const left = (unit / Math.max(1, unitCount)) * 100;
              return (
                <div key={unit} style={{ position: "absolute", left: `${left}%`, top: 0, bottom: 0 }}>
                  {major && (
                    <span
                      style={{
                        position: "absolute",
                        top: 0,
                        // First and last labels tuck inside the track
                        // instead of hanging off its edges.
                        left: unit === 0 ? 0 : undefined,
                        right: unit >= unitCount ? 0 : undefined,
                        transform:
                          unit === 0 || unit >= unitCount
                            ? undefined
                            : "translateX(-50%)",
                        fontSize: fontSize.xs,
                        color: colors.text.tertiary,
                        fontVariantNumeric: "tabular-nums",
                        whiteSpace: "nowrap",
                        pointerEvents: "none",
                      }}
                    >
                      {elapsedLabel(unit, unitCount)}
                    </span>
                  )}
                  <div
                    style={{
                      position: "absolute",
                      bottom: 0,
                      left: unit === 0 ? 0 : unit >= unitCount ? -1 : -0.5,
                      width: 1,
                      height: major ? (isShort ? 5 : 7) : (isShort ? 3 : 4),
                      background: major
                        ? colors.text.tertiary
                        : colors.text.quaternary,
                      pointerEvents: "none",
                    }}
                  />
                </div>
              );
            })}
          </div>

          {/* Filmstrip — drag creates a cut, click seeks */}
          <div
            ref={timelineRef}
            className="lk-ed-strip"
            tabIndex={0}
            role="group"
            aria-label={
              mode === "mask"
                ? "Timelapse timeline. Drag or click to scrub video frames."
                : "Timelapse timeline. Drag to remove a stretch of time."
            }
            onPointerDown={onTimelinePointerDown}
            style={{
              position: "relative",
              height: stripHeight,
              borderRadius: radii.md,
              overflow: "hidden",
              cursor: mode === "mask" ? "ew-resize" : "crosshair",
              background: colors.editor.track,
              border: `1px solid ${colors.border.default}`,
            }}
          >
            {/* Whole frames at the source aspect ratio, tiled left to
                right. Fixed width (not flex) is the point: stretching
                tiles to fill would distort them, and `cover` would crop
                them. The final tile runs past the edge and is clipped. */}
            <div
              style={{
                position: "absolute",
                inset: 0,
                display: "flex",
                pointerEvents: "none",
              }}
            >
              {filmstrip.map((url, i) => (
                <img
                  key={i}
                  src={url}
                  alt=""
                  draggable={false}
                  style={{
                    width: tileWidth,
                    height: "100%",
                    flex: "0 0 auto",
                    objectFit: "fill",
                    display: "block",
                  }}
                />
              ))}
            </div>

            {/* Pause markers: the recording stopped between these minutes */}
            {gaps.map((i) => (
              <div
                key={`gap-${i}`}
                title="Recording paused here"
                style={{
                  position: "absolute",
                  left: pct(i),
                  top: 0,
                  bottom: 0,
                  width: 2,
                  marginLeft: -1,
                  background:
                    "repeating-linear-gradient(180deg, var(--color-text-quaternary) 0 3px, transparent 3px 6px)",
                  pointerEvents: "none",
                }}
              />
            ))}

            {regions.map((r, i) => {
              const isSelected = selected === i;
              return (
                <div
                  key={i}
                  className="lk-ed-region"
                  onPointerDown={mode === "cut" ? (e) => onRegionPointerDown(e, i, "move") : undefined}
                  style={{
                    position: "absolute",
                    left: pct(r.startUnit),
                    width: pct(r.endUnit - r.startUnit),
                    top: 0,
                    bottom: 0,
                    borderRadius: radii.sm,
                    // backgroundColor (not background) so the hover rule
                    // in editorStyles can swap the tint without dropping
                    // the hatch layered on top of it.
                    backgroundColor: colors.editor.cutFill,
                    backgroundImage: hatch(10),
                    boxShadow: isSelected && mode === "cut"
                      ? `inset 0 0 0 2px ${colors.editor.cutBorder}`
                      : `inset 0 0 0 1px ${colors.editor.cutBorder}`,
                    cursor: mode === "cut" ? "grab" : "default",
                    pointerEvents: mode === "cut" ? "auto" : "none",
                    boxSizing: "border-box",
                  }}
                >
                  {mode === "cut" && [
                    { mode: "start" as const, side: { left: -6 } },
                    { mode: "end" as const, side: { right: -6 } },
                  ].map(({ mode: handleMode, side }) => (
                    <div
                      key={handleMode}
                      className="lk-ed-handle"
                      onPointerDown={(e) => onRegionPointerDown(e, i, handleMode)}
                      style={{
                        position: "absolute",
                        top: 0,
                        bottom: 0,
                        width: 12,
                        ...side,
                        cursor: "ew-resize",
                        display: "flex",
                        alignItems: "center",
                        justifyContent: "center",
                      }}
                    >
                      <div
                        className="lk-ed-grip"
                        style={{
                          width: 3,
                          height: 22,
                          borderRadius: 2,
                          background: colors.editor.cutBorder,
                        }}
                      />
                    </div>
                  ))}
                </div>
              );
            })}

          </div>

          {/* Stacked Mask Tracks */}
          <AnimatePresence initial={false}>
            {masks.length > 0 && (
              <motion.div
                key="timeline-mask-tracks-container"
                ref={maskTrackRef}
                tabIndex={0}
                role="group"
                aria-label="Mask timeline tracks. Drag mask regions to adjust their duration."
                initial={{ opacity: 0, height: 0, marginTop: 0 }}
                animate={{
                  opacity: 1,
                  height: trackAllocation.trackCount * maskTrackHeight + (trackAllocation.trackCount - 1) * (isShort ? 2 : 4),
                  marginTop: isShort ? 2 : 4,
                }}
                exit={{ opacity: 0, height: 0, marginTop: 0 }}
                transition={{ duration: 0.2, ease: [0.16, 1, 0.3, 1] }}
                style={{
                  position: "relative",
                  display: "flex",
                  flexDirection: "column",
                  gap: isShort ? 2 : 4,
                }}
              >
                {Array.from({ length: trackAllocation.trackCount }).map((_, trackIdx) => {
                  const preset = TRACK_PRESETS[trackIdx] ?? TRACK_PRESETS[0];
                  const trackMasks = trackAllocation.tracks[trackIdx] ?? [];
                  return (
                    <div
                      key={`mask-track-${trackIdx}`}
                      data-track-index={trackIdx}
                      onPointerDown={onMaskTrackPointerDown}
                      style={{
                        position: "relative",
                        height: maskTrackHeight,
                        borderRadius: radii.sm,
                        background: colors.editor.track,
                        border: `1px solid ${preset.border}44`,
                        cursor: "pointer",
                        overflow: "hidden",
                      }}
                    >
                      {activeShots?.map((s) => (
                        <div
                          key={s.id}
                          style={{
                            position: "absolute",
                            left: pct(s.startSec),
                            top: 0,
                            bottom: 0,
                            width: 1,
                            backgroundColor: "rgba(255, 255, 255, 0.08)",
                            pointerEvents: "none",
                          }}
                        />
                      ))}

                      {trackMasks.map((b) => {
                        const isSelected = selectedMaskId === b.id && mode === "mask";
                        const isExpanding = expandingMaskId === b.id;
                        const durationSec = Math.max(0.01, b.endUnit - b.startUnit);
                        const totalUnits = Math.max(1, unitCount);
                        const widthRatio = durationSec / totalUnits;
                        const shotsCovered = activeShots?.filter(
                          (s) => s.startSec >= b.startUnit - 0.001 && s.endSec <= b.endUnit + 0.001,
                        ).length;
                        const shotCount =
                          shotsCovered && shotsCovered > 0
                            ? shotsCovered
                            : Math.max(1, Math.round(durationSec));
                        return (
                          <div
                            key={b.id}
                            className="lk-ed-mask-span"
                            onPointerDown={mode === "mask" ? (e) => onMaskSpanPointerDown(e, b.id, "move") : undefined}
                            title={
                              mode === "mask"
                                ? `Mask: ${durationSec.toFixed(1)}s (${shotCount} shot${shotCount === 1 ? "" : "s"}). Hold Shift while dragging for precise control.`
                                : `Mask: ${durationSec.toFixed(1)}s (${shotCount} shot${shotCount === 1 ? "" : "s"}). Switch to Mask tab to edit.`
                            }
                            style={{
                              position: "absolute",
                              left: pct(b.startUnit),
                              width: pct(durationSec),
                              minWidth: 8,
                              top: 2,
                              bottom: 2,
                              borderRadius: radii.sm,
                              backgroundColor: isExpanding
                                ? preset.bgSelected
                                : isSelected
                                  ? preset.bgSelected
                                  : mode === "mask"
                                    ? preset.bgUnselected
                                    : `${preset.color}33`,
                              border: `1px solid ${
                                isExpanding
                                  ? preset.border
                                  : isSelected
                                    ? preset.border
                                    : mode === "mask"
                                      ? preset.border
                                      : `${preset.border}55`
                              }`,
                              boxShadow: isExpanding
                                ? `0 0 12px ${preset.color}, 0 0 0 1px ${preset.border}`
                                : isSelected
                                  ? `0 0 8px ${preset.color}99, 0 0 0 1px ${preset.border}`
                                  : undefined,
                              cursor: mode === "mask" ? "grab" : "default",
                              pointerEvents: mode === "mask" ? "auto" : "none",
                              boxSizing: "border-box",
                              transition: isExpanding
                                ? "left 0.22s cubic-bezier(0.16, 1, 0.3, 1), width 0.22s cubic-bezier(0.16, 1, 0.3, 1), background-color 0.15s, border-color 0.15s, box-shadow 0.15s"
                                : "background-color 0.15s, border-color 0.15s",
                            }}
                          >
                            <span
                              style={{
                                position: "absolute",
                                inset: 0,
                                display: "flex",
                                alignItems: "center",
                                justifyContent: "center",
                                fontSize: 10,
                                fontWeight: fontWeight.bold,
                                fontVariantNumeric: "tabular-nums",
                                letterSpacing: "-0.01em",
                                color: colors.text.primary,
                                pointerEvents: "none",
                                whiteSpace: "nowrap",
                                overflow: "hidden",
                                textOverflow: "ellipsis",
                                padding: "0 6px",
                                zIndex: 1,
                              }}
                            >
                              {isExpanding
                                ? "Scanning…"
                                : widthRatio >= 0.03
                                  ? "Mask"
                                  : ""}
                            </span>

                            {mode === "mask" && [
                              { mode: "start" as const, pos: { left: 0 } },
                              { mode: "end" as const, pos: { left: "100%" } },
                            ].map(({ mode: handleMode, pos }) => (
                              <div
                                key={handleMode}
                                className="lk-ed-mask-handle"
                                onPointerDown={(e) => onMaskSpanPointerDown(e, b.id, handleMode)}
                                title="Drag handle to extend mask across shots (Hold Shift for frame precision)"
                                style={{
                                  position: "absolute",
                                  top: -3,
                                  bottom: -3,
                                  width: 12,
                                  ...pos,
                                  transform: "translateX(-50%)",
                                  cursor: "ew-resize",
                                  display: "flex",
                                  alignItems: "center",
                                  justifyContent: "center",
                                  zIndex: 10,
                                  touchAction: "none",
                                }}
                              >
                                <div
                                  style={{
                                    width: 3,
                                    height: "100%",
                                    maxHeight: isShort ? 14 : 18,
                                    borderRadius: 1.5,
                                    background: isSelected ? "#ffffff" : preset.handleColor,
                                    boxShadow: "0 0 2px rgba(0,0,0,0.7), 0 1px 3px rgba(0,0,0,0.5)",
                                  }}
                                />
                              </div>
                            ))}
                          </div>
                        );
                      })}
                    </div>
                  );
                })}
              </motion.div>
            )}
          </AnimatePresence>
        </div>
      </div>

          {/* VS Code Style Minimap / Horizontal Overview Map */}
          <AnimatePresence initial={false}>
            {zoom > 1 && (
              <motion.div
                key="timeline-minimap"
                onPointerDown={onMinimapPointerDown}
                role="scrollbar"
                aria-label="Timeline zoom overview"
                aria-valuenow={Math.round(scrollProgress * 100)}
                title="Timeline zoom overview. Click or drag to pan."
                initial={{ opacity: 0, y: 3 }}
                animate={{
                  opacity: isTimelineHovered || isMinimapDragging ? 1 : 0.4,
                  y: 0,
                }}
                exit={{ opacity: 0, y: 3 }}
                transition={{ duration: 0.15 }}
                style={{
                  position: "absolute",
                  bottom: 2,
                  left: 6,
                  right: 6,
                  height: isTimelineHovered || isMinimapDragging ? (isShort ? 8 : 10) : (isShort ? 5 : 6),
                  zIndex: 15,
                  borderRadius: 3,
                  background: isTimelineHovered || isMinimapDragging
                    ? "color-mix(in srgb, var(--color-bg-sunken) 50%, transparent)"
                    : "color-mix(in srgb, var(--color-bg-sunken) 20%, transparent)",
                  border: "1px solid color-mix(in srgb, var(--color-border-default) 40%, transparent)",
                  cursor: "pointer",
                  userSelect: "none",
                  touchAction: "none",
                  overflow: "hidden",
                  transition: "height 0.15s cubic-bezier(0.16, 1, 0.3, 1), opacity 0.15s ease, background 0.15s ease",
                }}
              >
                {/* Cut regions overview */}
                {unitCount > 0 &&
                  normalized.map((r, i) => {
                    const leftPct = (r.startUnit / unitCount) * 100;
                    const widthPct = Math.max(0.5, ((r.endUnit - r.startUnit) / unitCount) * 100);
                    return (
                      <div
                        key={`map-cut-${i}`}
                        style={{
                          position: "absolute",
                          left: `${leftPct}%`,
                          width: `${widthPct}%`,
                          top: 1,
                          bottom: 1,
                          borderRadius: 1,
                          background: colors.editor.cutBorder,
                          opacity: 0.65,
                          pointerEvents: "none",
                          zIndex: 1,
                        }}
                      />
                    );
                  })}

                {/* Mask regions overview */}
                {unitCount > 0 &&
                  masks.map((b) => {
                    const leftPct = (b.startUnit / unitCount) * 100;
                    const widthPct = Math.max(0.5, ((b.endUnit - b.startUnit) / unitCount) * 100);
                    const isSelected = selectedMaskId === b.id && mode === "mask";
                    return (
                      <div
                        key={`map-mask-${b.id}`}
                        style={{
                          position: "absolute",
                          left: `${leftPct}%`,
                          width: `${widthPct}%`,
                          top: 1,
                          bottom: 1,
                          borderRadius: 1,
                          background: colors.accent.base,
                          opacity: isSelected ? 0.95 : 0.7,
                          boxShadow: isSelected ? `0 0 4px ${colors.accent.base}` : undefined,
                          pointerEvents: "none",
                          zIndex: 1,
                        }}
                      />
                    );
                  })}

                {/* Playhead marker */}
                {unitCount > 0 && (
                  <div
                    style={{
                      position: "absolute",
                      left: `${Math.max(0, Math.min(1, time / unitCount)) * 100}%`,
                      top: 0,
                      bottom: 0,
                      width: 1.5,
                      marginLeft: -0.75,
                      background: colors.text.primary,
                      opacity: 0.85,
                      borderRadius: 1,
                      pointerEvents: "none",
                      zIndex: 2,
                      transition: isSmoothSeek ? "left 0.28s cubic-bezier(0.16, 1, 0.3, 1)" : "none",
                      willChange: isSmoothSeek ? "left" : "auto",
                    }}
                  />
                )}

                {/* VS Code Style Translucent Viewport Slider */}
                <div
                  style={{
                    position: "absolute",
                    left: `${scrollProgress * (1 - 1 / zoom) * 100}%`,
                    width: `${(1 / zoom) * 100}%`,
                    minWidth: 16,
                    top: 0,
                    bottom: 0,
                    borderRadius: 2,
                    background: isMinimapDragging
                      ? "color-mix(in srgb, var(--color-text-primary) 30%, transparent)"
                      : isMinimapThumbHovered
                        ? "color-mix(in srgb, var(--color-text-primary) 22%, transparent)"
                        : "color-mix(in srgb, var(--color-text-primary) 14%, transparent)",
                    border: `1px solid ${
                      isMinimapDragging
                        ? colors.accent.base
                        : isMinimapThumbHovered
                          ? "color-mix(in srgb, var(--color-text-primary) 45%, transparent)"
                          : "color-mix(in srgb, var(--color-text-primary) 25%, transparent)"
                    }`,
                    cursor: isMinimapDragging ? "grabbing" : "grab",
                    zIndex: 3,
                    transition: "background 0.12s, border-color 0.12s",
                    boxSizing: "border-box",
                  }}
                  onMouseEnter={() => setIsMinimapThumbHovered(true)}
                  onMouseLeave={() => setIsMinimapThumbHovered(false)}
                />
              </motion.div>
            )}
          </AnimatePresence>
        </div>

        {/* Actions */}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: isNarrow ? 6 : spacing.md,
            flexWrap: "wrap",
            marginTop: isShort ? 0 : spacing.xs,
          }}
        >
          <div
            style={{
              minWidth: 0,
              fontSize: isNarrow ? fontSize.sm : fontSize.lg,
              color: colors.text.primary,
              fontWeight: fontWeight.semibold,
              letterSpacing: "-0.01em",
              display: "inline-flex",
              alignItems: "center",
              gap: isNarrow ? 6 : 8,
              flexWrap: "wrap",
            }}
          >
            <span>
              <MinutesFlow minutes={keptUnits} /> kept
            </span>
            {removedUnits > 0 && (
              <>
                <span style={{ color: colors.text.quaternary, userSelect: "none" }} aria-hidden="true">
                  ·
                </span>
                <span style={{ color: colors.editor.cutBorder, fontWeight: fontWeight.medium }}>
                  <MinutesFlow minutes={removedUnits} color={colors.editor.cutBorder} /> removed
                </span>
              </>
            )}
            {totalMaskedSec > 0 && (
              <>
                <span style={{ color: colors.text.quaternary, userSelect: "none" }} aria-hidden="true">
                  ·
                </span>
                <span
                  style={{
                    color: colors.accent.base,
                    fontWeight: fontWeight.medium,
                    display: "inline-flex",
                    alignItems: "center",
                    gap: 4,
                  }}
                >
                  {totalMaskedSec < 60 ? (
                    <span style={{ fontVariantNumeric: "tabular-nums" }}>
                      <NumberFlow value={flowMaskedSec} suffix="s" />
                    </span>
                  ) : (
                    <MinutesFlow
                      minutes={Math.max(1, Math.round(flowMaskedSec / 60))}
                      color={colors.accent.base}
                    />
                  )}
                  <span>masked</span>
                </span>
              </>
            )}
          </div>

          <div style={{ flex: 1, minWidth: spacing.md }} />

          {mode === "mask" && masks.length > 0 && (
            <div
              ref={maskMenuRef}
              style={{
                position: "relative",
                display: "inline-flex",
                alignItems: "center",
                borderRadius: radii.md,
                border: `1px solid ${colors.border.hover}`,
                background: "transparent",
              }}
            >
              <button
                type="button"
                disabled={selectedMaskId === null}
                onClick={() => {
                  if (selectedMaskId === null) return;
                  setMasks((prev) => prev.filter((b) => b.id !== selectedMaskId));
                  setSelectedMaskId(null);
                }}
                style={{
                  display: "inline-flex",
                  alignItems: "center",
                  justifyContent: "center",
                  gap: 5,
                  padding: isNarrow ? "4px 8px" : "6px 12px",
                  fontSize: 12,
                  fontWeight: fontWeight.semibold,
                  color: selectedMaskId !== null ? colors.text.secondary : colors.text.quaternary,
                  background: "transparent",
                  border: "none",
                  borderTopLeftRadius: radii.md - 1,
                  borderBottomLeftRadius: radii.md - 1,
                  cursor: selectedMaskId !== null ? "pointer" : "not-allowed",
                  opacity: selectedMaskId !== null ? 1 : 0.5,
                  transition: "background 0.15s, color 0.15s",
                }}
                onMouseEnter={(e) => {
                  if (selectedMaskId !== null) e.currentTarget.style.background = colors.bg.surface;
                }}
                onMouseLeave={(e) => {
                  e.currentTarget.style.background = "transparent";
                }}
              >
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <polyline points="3 6 5 6 21 6" />
                  <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
                </svg>
                <span>Remove</span>
              </button>
              <div
                style={{
                  width: 1,
                  height: 16,
                  background: colors.border.hover,
                }}
              />
              <button
                type="button"
                aria-label="More mask options"
                aria-expanded={maskMenuOpen}
                onClick={() => setMaskMenuOpen((prev) => !prev)}
                style={{
                  display: "inline-flex",
                  alignItems: "center",
                  justifyContent: "center",
                  padding: "6px 8px",
                  fontSize: 12,
                  fontWeight: fontWeight.semibold,
                  color: colors.text.secondary,
                  background: maskMenuOpen ? colors.bg.surface : "transparent",
                  border: "none",
                  borderTopRightRadius: radii.md - 1,
                  borderBottomRightRadius: radii.md - 1,
                  cursor: "pointer",
                  transition: "background 0.15s, color 0.15s",
                }}
                onMouseEnter={(e) => {
                  if (!maskMenuOpen) e.currentTarget.style.background = colors.bg.surface;
                }}
                onMouseLeave={(e) => {
                  if (!maskMenuOpen) e.currentTarget.style.background = "transparent";
                }}
              >
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <polyline points="6 9 12 15 18 9" />
                </svg>
              </button>

              <AnimatePresence>
                {maskMenuOpen && (
                  <motion.div
                    initial={{ opacity: 0, y: 4, scale: 0.96 }}
                    animate={{ opacity: 1, y: 0, scale: 1 }}
                    exit={{ opacity: 0, y: 4, scale: 0.96 }}
                    transition={{ duration: 0.12 }}
                    style={{
                      position: "absolute",
                      bottom: "calc(100% + 4px)",
                      left: 0,
                      right: 0,
                      background: colors.bg.panel,
                      border: `1px solid ${colors.border.hover}`,
                      borderRadius: radii.md,
                      boxShadow: "0 4px 16px rgba(0, 0, 0, 0.4)",
                      padding: 4,
                      zIndex: 50,
                    }}
                  >
                    <button
                      type="button"
                      onClick={() => {
                        setMasks([]);
                        setSelectedMaskId(null);
                        setMaskMenuOpen(false);
                      }}
                      style={{
                        width: "100%",
                        display: "flex",
                        alignItems: "center",
                        justifyContent: "center",
                        gap: 6,
                        padding: "6px 8px",
                        fontSize: 12,
                        fontWeight: fontWeight.medium,
                        color: colors.text.primary,
                        background: "transparent",
                        border: "none",
                        borderRadius: radii.sm,
                        cursor: "pointer",
                        transition: "background 0.1s",
                        whiteSpace: "nowrap",
                        boxSizing: "border-box",
                      }}
                      onMouseEnter={(e) => {
                        e.currentTarget.style.background = colors.bg.surface;
                      }}
                      onMouseLeave={(e) => {
                        e.currentTarget.style.background = "transparent";
                      }}
                    >
                      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                        <line x1="18" y1="6" x2="6" y2="18" />
                        <line x1="6" y1="6" x2="18" y2="18" />
                      </svg>
                      Clear all
                    </button>
                  </motion.div>
                )}
              </AnimatePresence>
            </div>
          )}

          {mode === "cut" && (
            <>
              {selected !== null && (
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => {
                    setRegions((prev) => prev.filter((_, i) => i !== selected));
                    setSelected(null);
                  }}
                >
                  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" style={{ marginRight: 5 }}>
                    <polyline points="3 6 5 6 21 6" />
                    <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
                  </svg>
                  Remove
                </Button>
              )}
              {normalized.length > 0 && (
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => {
                    setRegions([]);
                    setSelected(null);
                  }}
                >
                  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" style={{ marginRight: 5 }}>
                    <line x1="18" y1="6" x2="6" y2="18" />
                    <line x1="6" y1="6" x2="18" y2="18" />
                  </svg>
                  Clear all
                </Button>
              )}
            </>
          )}
          <Button
            variant={saveSuccess ? "secondary" : "primary"}
            size="sm"
            onClick={save}
            loading={saving}
            disabled={allCut || saving}
            title={allCut ? "You can't remove the entire timelapse" : undefined}
            style={
              saveSuccess
                ? {
                    borderColor: colors.accent.base,
                    color: colors.accent.base,
                    fontWeight: fontWeight.semibold,
                  }
                : undefined
            }
          >
            {saving ? "Saving…" : saveSuccess ? "Saved ✓" : "Save"}
          </Button>
        </div>

        {saveError && (
          <ErrorDisplay error={saveError} variant="banner" title="Couldn't save your edits" />
        )}
      </div>
    </div>
  );
}
