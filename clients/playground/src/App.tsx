import { useCallback, useEffect, useRef, useState } from "react";
import {
  LookoutProvider,
  LookoutRecorder,
  SessionDetail,
  TimelapseEditor,
  createLookoutClient,
  setAccentColor,
  colors,
  fontSize,
  fontWeight,
  radii,
  spacing,
  type CutInterval,
  type MaskRegion,
} from "@lookout/react";

/**
 * A harness for the edit feature.
 *
 * The point isn't to look like the product — it's to make the parts that
 * are hard to eyeball checkable: the editor at arbitrary sizes, and the
 * server's own numbers next to what the editor is claiming. Most of the
 * bugs in this feature were disagreements between those two.
 */

type Tab = "record" | "editor" | "detail";

const LS_KEY = "lookout-playground";

interface Settings {
  apiBaseUrl: string;
  token: string;
  /** Brand accent an embedding program would pass to LookoutProvider. */
  accent: string;
}

function loadSettings(): Settings {
  try {
    const raw = localStorage.getItem(LS_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Settings;
      if (parsed.token) return parsed;
    }
  } catch {
    // Fall through to defaults.
  }
  return {
    apiBaseUrl: "https://lookout-stage.dino.icu",
    token: "",
    accent: "#3b82f6",
  };
}

export function App() {
  const [settings, setSettings] = useState<Settings>(loadSettings);
  const [applied, setApplied] = useState<Settings | null>(() => {
    const s = loadSettings();
    return s.token ? s : null;
  });
  const [tab, setTab] = useState<Tab>("editor");
  const [cuts, setCuts] = useState<CutInterval[]>([]);
  const [masks, setMasks] = useState<MaskRegion[]>([]);

  // Mirrors what <LookoutProvider accentColor> does, so the editor and
  // both dialogs can be checked against a brand colour without wiring a
  // provider around every tab.
  useEffect(() => {
    setAccentColor(applied?.accent ?? null);
  }, [applied?.accent]);

  const apply = () => {
    localStorage.setItem(LS_KEY, JSON.stringify(settings));
    setApplied({ ...settings });
  };

  return (
    <div style={{ height: "100%", display: "flex", flexDirection: "column" }}>
      <Header
        settings={settings}
        onChange={setSettings}
        onApply={apply}
        tab={tab}
        onTab={setTab}
        ready={Boolean(applied?.token)}
      />

      {!applied?.token ? (
        <Empty />
      ) : (
        <div style={{ flex: 1, minHeight: 0, display: "flex" }}>
          <div style={{ flex: 1, minWidth: 0, minHeight: 0, overflow: "auto" }}>
            {tab === "record" && (
              <LookoutProvider token={applied.token} apiBaseUrl={applied.apiBaseUrl}>
                <LookoutRecorder />
              </LookoutProvider>
            )}
            {tab === "editor" && (
              <ResizableEditor
                key={applied.token}
                settings={applied}
                onCuts={setCuts}
                onMasks={setMasks}
              />
            )}
            {tab === "detail" && (
              <SessionDetail
                key={applied.token}
                token={applied.token}
                apiBaseUrl={applied.apiBaseUrl}
              />
            )}
          </div>
          <ServerTruth settings={applied} cuts={cuts} masks={masks} />
        </div>
      )}
    </div>
  );
}

function Header({
  settings,
  onChange,
  onApply,
  tab,
  onTab,
  ready,
}: {
  settings: Settings;
  onChange: (s: Settings) => void;
  onApply: () => void;
  tab: Tab;
  onTab: (t: Tab) => void;
  ready: boolean;
}) {
  const input: React.CSSProperties = {
    background: colors.bg.sunken,
    border: `1px solid ${colors.border.default}`,
    borderRadius: radii.md,
    color: colors.text.primary,
    padding: "6px 10px",
    fontSize: fontSize.md,
    fontFamily: "inherit",
    outline: "none",
  };

  return (
    <div
      style={{
        flex: "0 0 auto",
        display: "flex",
        alignItems: "center",
        gap: spacing.sm,
        padding: spacing.md,
        borderBottom: `1px solid ${colors.border.default}`,
        flexWrap: "wrap",
      }}
    >
      <strong style={{ fontSize: fontSize.md }}>Lookout SDK</strong>
      <input
        style={{ ...input, width: 260 }}
        value={settings.apiBaseUrl}
        placeholder="https://lookout-stage.dino.icu"
        onChange={(e) => onChange({ ...settings, apiBaseUrl: e.target.value })}
      />
      <input
        style={{ ...input, flex: 1, minWidth: 260, fontFamily: "ui-monospace, monospace" }}
        value={settings.token}
        placeholder="64-char session token"
        spellCheck={false}
        onChange={(e) => onChange({ ...settings, token: e.target.value.trim() })}
        onKeyDown={(e) => {
          if (e.key === "Enter") onApply();
        }}
      />
      <label
        title="Accent colour an embedder would pass to LookoutProvider"
        style={{
          display: "inline-flex",
          alignItems: "center",
          gap: 6,
          fontSize: fontSize.sm,
          color: colors.text.secondary,
        }}
      >
        Accent
        <input
          type="color"
          value={settings.accent}
          onChange={(e) => onChange({ ...settings, accent: e.target.value })}
          style={{
            width: 32,
            height: 28,
            padding: 0,
            border: `1px solid ${colors.border.default}`,
            borderRadius: radii.md,
            background: "transparent",
            cursor: "pointer",
          }}
        />
      </label>
      <button onClick={onApply} style={{ ...input, cursor: "pointer", fontWeight: fontWeight.semibold }}>
        Load
      </button>

      {ready && (
        <div style={{ display: "flex", gap: 4, marginLeft: spacing.sm }}>
          {(["editor", "detail", "record"] as Tab[]).map((t) => (
            <button
              key={t}
              onClick={() => onTab(t)}
              style={{
                ...input,
                cursor: "pointer",
                background: tab === t ? colors.bg.selected : "transparent",
                borderColor: tab === t ? colors.border.selected : colors.border.default,
                textTransform: "capitalize",
              }}
            >
              {t}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function Empty() {
  return (
    <div
      style={{
        flex: 1,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        color: colors.text.secondary,
        fontSize: fontSize.lg,
        textAlign: "center",
        padding: spacing.xxl,
        lineHeight: 1.6,
      }}
    >
      Paste a session token to begin.
      <br />
      <span style={{ fontSize: fontSize.md, color: colors.text.tertiary }}>
        Stop it with an edit hold first, or the editor will report it published.
      </span>
    </div>
  );
}

/**
 * The editor inside a box you can resize to arbitrary dimensions.
 *
 * The clipping bug was only visible at particular window shapes, and a
 * maximised browser window never reproduces it. Presets cover the corners:
 * short (the dock must survive), narrow (the action row must wrap), and
 * the real desktop window's minimum.
 */
function ResizableEditor({
  settings,
  onCuts,
  onMasks,
}: {
  settings: Settings;
  onCuts: (cuts: CutInterval[]) => void;
  onMasks: (masks: MaskRegion[]) => void;
}) {
  const [size, setSize] = useState({ w: 900, h: 620 });
  const presets: Array<[string, number, number]> = [
    ["desktop default", 900, 620],
    ["desktop minimum", 620, 480],
    ["short", 900, 360],
    ["narrow", 480, 620],
    ["tiny", 420, 320],
  ];

  return (
    <div style={{ padding: spacing.md }}>
      <div style={{ display: "flex", gap: 6, marginBottom: spacing.md, flexWrap: "wrap" }}>
        {presets.map(([label, w, h]) => (
          <button
            key={label}
            onClick={() => setSize({ w, h })}
            style={{
              background: size.w === w && size.h === h ? colors.bg.selected : "transparent",
              border: `1px solid ${colors.border.default}`,
              borderRadius: radii.md,
              color: colors.text.secondary,
              padding: "4px 10px",
              fontSize: fontSize.sm,
              fontFamily: "inherit",
              cursor: "pointer",
            }}
          >
            {label} · {w}×{h}
          </button>
        ))}
        <span style={{ fontSize: fontSize.sm, color: colors.text.tertiary, alignSelf: "center" }}>
          or drag the corner
        </span>
      </div>

      <div
        style={{
          width: size.w,
          height: size.h,
          maxWidth: "100%",
          resize: "both",
          overflow: "auto",
          border: `1px dashed ${colors.border.hover}`,
          borderRadius: 12,
          padding: spacing.md,
          boxSizing: "border-box",
        }}
      >
        <TimelapseEditor
          token={settings.token}
          apiBaseUrl={settings.apiBaseUrl}
          onApplied={() => console.log("[playground] published")}
          onCutsChange={(cuts, dirty) => {
            onCuts(cuts);
            console.log("[playground] cuts", { dirty, cuts });
          }}
          onMasksChange={(masks, dirty) => {
            onMasks(masks);
            console.log("[playground] masks", { dirty, masks });
          }}
        />
      </div>
    </div>
  );
}

/**
 * What the server actually thinks, polled live.
 *
 * Every serious bug in this feature was the client and the server
 * disagreeing — over-counted cut units, a stale hold, a status the editor
 * read as terminal. Putting the server's own numbers on screen makes those
 * disagreements visible instead of inferable from a 400.
 */
function ServerTruth({
  settings,
  cuts,
  masks,
}: {
  settings: Settings;
  cuts: CutInterval[];
  masks: MaskRegion[];
}) {
  const [status, setStatus] = useState<Record<string, unknown> | null>(null);
  const [units, setUnits] = useState<Record<string, unknown> | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<Record<string, unknown> | null>(null);
  const clientRef = useRef(
    createLookoutClient({ baseUrl: settings.apiBaseUrl, token: settings.token }),
  );

  useEffect(() => {
    clientRef.current = createLookoutClient({
      baseUrl: settings.apiBaseUrl,
      token: settings.token,
    });
  }, [settings.apiBaseUrl, settings.token]);

  // /status is the endpoint built for polling (60/min). /units presigns a
  // URL and allows only 10/min, so it is fetched on load, when /status
  // reports a change worth re-reading, and on demand — never on a timer.
  const [unitsAt, setUnitsAt] = useState(0);
  const lastUnitsRef = useRef(0);
  const signatureRef = useRef("");

  const loadUnits = useCallback(async () => {
    // Hard floor between reads so no combination of triggers can walk
    // into the limit.
    if (Date.now() - lastUnitsRef.current < 6000) return;
    lastUnitsRef.current = Date.now();
    try {
      const r = await fetch(
        `${settings.apiBaseUrl}/api/sessions/${settings.token}/units`,
      );
      const u = (await r.json()) as Record<string, unknown>;
      const { units: list, originalVideoUrl: _url, ...rest } = u;
      setUnits({ ...rest, unitCount: Array.isArray(list) ? list.length : 0 });
      setUnitsAt(Date.now());
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [settings.apiBaseUrl, settings.token]);

  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      try {
        const r = await fetch(
          `${settings.apiBaseUrl}/api/sessions/${settings.token}/status`,
        );
        const s = (await r.json()) as Record<string, unknown>;
        if (cancelled) return;
        setError(null);
        setStatus(s);
        // Re-read /units only when something that changes it changed.
        const sig = `${s.status}:${s.editable}`;
        if (sig !== signatureRef.current) {
          signatureRef.current = sig;
          void loadUnits();
        }
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      }
    };
    void tick();
    const id = setInterval(tick, 3000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [settings.apiBaseUrl, settings.token, loadUnits]);

  // Dry-run the cut and mask list the editor most recently reported, so the
  // server's own arithmetic sits next to the editor's footer.
  const dryRun = useCallback(async (cuts: CutInterval[], masks: MaskRegion[]) => {
    try {
      setPreview({ ...(await clientRef.current.setCuts(cuts, masks)) });
    } catch (e) {
      setPreview({ error: e instanceof Error ? e.message : String(e) });
    }
  }, []);

  const box: React.CSSProperties = {
    background: colors.bg.sunken,
    border: `1px solid ${colors.border.default}`,
    borderRadius: radii.md,
    padding: spacing.sm,
    fontSize: 11,
    fontFamily: "ui-monospace, SFMono-Regular, monospace",
    whiteSpace: "pre-wrap",
    wordBreak: "break-all",
    color: colors.text.secondary,
  };

  return (
    <div
      style={{
        flex: "0 0 320px",
        borderLeft: `1px solid ${colors.border.default}`,
        padding: spacing.md,
        overflow: "auto",
        display: "flex",
        flexDirection: "column",
        gap: spacing.sm,
      }}
    >
      <div style={{ fontSize: fontSize.md, fontWeight: fontWeight.semibold }}>
        Server truth
      </div>
      <div style={{ fontSize: fontSize.xs, color: colors.text.tertiary, lineHeight: 1.5 }}>
        <code>/status</code> every 3s; <code>/units</code> only on change
        or demand (10/min limit). Compare <code>editable</code> and the
        tracked-time pair against what the editor shows.
      </div>

      {error && <div style={{ ...box, color: colors.text.error }}>{error}</div>}

      <Label>GET /status</Label>
      <div style={box}>{JSON.stringify(status, null, 1)}</div>

      <Label>
        GET /units{" "}
        <button
          onClick={() => void loadUnits()}
          style={{
            background: "transparent",
            border: "none",
            color: colors.text.secondary,
            cursor: "pointer",
            font: "inherit",
            textDecoration: "underline",
            padding: 0,
          }}
        >
          refresh
        </button>
      </Label>
      <div style={{ fontSize: fontSize.xs, color: colors.text.tertiary }}>
        {unitsAt ? `read ${new Date(unitsAt).toLocaleTimeString()}` : "not read yet"}
        {" · 10/min limit, so not polled"}
      </div>
      <div style={box}>{JSON.stringify(units, null, 1)}</div>

      <Label>PUT /cuts</Label>
      <button
        onClick={() => void dryRun(cuts, masks)}
        style={{
          background: "transparent",
          border: `1px solid ${colors.border.hover}`,
          borderRadius: radii.md,
          color: colors.text.primary,
          padding: "6px 10px",
          fontSize: fontSize.sm,
          fontFamily: "inherit",
          cursor: "pointer",
        }}
      >
        Verify {cuts.length} cut{cuts.length === 1 ? "" : "s"}, {masks.length} mask{masks.length === 1 ? "" : "s"} against server
      </button>
      <div style={{ fontSize: fontSize.xs, color: colors.text.tertiary, lineHeight: 1.5 }}>
        Sends the editor's current list and shows what the server counts.
        <strong> unitsCut here must match the editor's "removed"</strong> — a
        mismatch is the class of bug that made Save fail with "would remove
        the entire timelapse". This writes the cut list (it does not
        publish).
      </div>
      {preview && <div style={box}>{JSON.stringify(preview, null, 1)}</div>}
    </div>
  );
}

function Label({ children }: { children: React.ReactNode }) {
  return (
    <div
      style={{
        fontSize: fontSize.xs,
        color: colors.text.tertiary,
        textTransform: "uppercase",
        letterSpacing: "0.04em",
        marginTop: spacing.xs,
      }}
    >
      {children}
    </div>
  );
}
