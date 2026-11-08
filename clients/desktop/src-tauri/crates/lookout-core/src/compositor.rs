//! Asking a Wayland compositor where the pointer is and what has the focus.
//! Wayland has no protocol for that, so each compositor is asked its own
//! way:
//!
//! - GNOME Shell: the Lookout extension the app already installs exports
//!   `com.hackclub.Lookout.Activity.Sample` on the shell's bus name.
//! - KDE Plasma (KWin): a one-shot KWin script, loaded over D-Bus, reads
//!   the workspace and calls back into this process.
//! - Hyprland: its IPC socket (`activewindow`, `cursorpos`, `monitors`).
//! - Sway: its i3-compatible IPC socket (tree and outputs; Sway has no way
//!   to report the pointer — that comes from the screencast instead).
//!
//! Everything is in the compositor's logical layout coordinates, the same
//! space the screencast portal reports monitor streams in.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

use crate::activity::Rect;

/// What a compositor told us.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CompositorState {
    pub method: &'static str,
    /// Monitors, in the compositor's own order.
    pub monitors: Vec<Rect>,
    pub pointer: Option<(f64, f64)>,
    /// `None` when nothing has the focus.
    pub focus: Option<Focus>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Focus {
    pub app: Option<String>,
    pub rect: Option<Rect>,
}

/// Budget for any one compositor round trip.
const TIMEOUT: Duration = Duration::from_millis(500);

/// Ask whichever compositor this session runs. `None` when it can't be
/// asked (an unknown compositor, the GNOME extension not loaded yet).
pub(crate) fn query() -> Option<CompositorState> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let is = |name: &str| desktop.split(':').any(|d| d.eq_ignore_ascii_case(name));
    let result = if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        hyprland()
    } else if std::env::var_os("SWAYSOCK").is_some() {
        sway()
    } else if is("GNOME") {
        gnome()
    } else if is("KDE") {
        kwin()
    } else {
        return None;
    };
    match result {
        Ok(state) => Some(state),
        Err(e) => {
            log_once(&e);
            None
        }
    }
}

/// One line per distinct failure, so a missing extension doesn't log six
/// times a minute.
fn log_once(e: &str) {
    static LAST: Mutex<Option<String>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|p| p.into_inner());
    if last.as_deref() != Some(e) {
        lookout_session::log_line!("[activity] compositor query failed: {e}");
        *last = Some(e.to_string());
    }
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn rect_xywh(v: &Value, x: &str, y: &str, w: &str, h: &str) -> Option<Rect> {
    Some(Rect { x: num(&v[x])?, y: num(&v[y])?, w: num(&v[w])?, h: num(&v[h])? })
}

fn non_empty(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

// ── GNOME Shell and KWin: the same JSON shape ───────────────────────────

/// `{ pointer: [x, y] | null, monitors: [{x, y, w, h}], focus: {app, x, y, w, h} | null }`,
/// what both the GNOME extension and the KWin script report.
pub(crate) fn parse_report(method: &'static str, json: &str) -> Result<CompositorState, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("{method}: bad report: {e}"))?;
    let pointer = v["pointer"]
        .as_array()
        .and_then(|p| Some((num(p.first()?)?, num(p.get(1)?)?)));
    let monitors = v["monitors"]
        .as_array()
        .map(|m| m.iter().filter_map(|m| rect_xywh(m, "x", "y", "w", "h")).collect())
        .unwrap_or_default();
    let focus = v["focus"].is_object().then(|| Focus {
        app: non_empty(&v["focus"]["app"]),
        rect: rect_xywh(&v["focus"], "x", "y", "w", "h"),
    });
    Ok(CompositorState { method, monitors, pointer, focus })
}

fn session_bus() -> Result<zbus::blocking::Connection, String> {
    static CONN: Mutex<Option<zbus::blocking::Connection>> = Mutex::new(None);
    let mut guard = CONN.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(c) = guard.as_ref() {
        return Ok(c.clone());
    }
    let conn = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.method_timeout(TIMEOUT).build())
        .map_err(|e| format!("session bus: {e}"))?;
    if let Err(e) = conn.object_server().at(KWIN_REPORT_PATH, KwinReports) {
        lookout_session::log_line!("[activity] could not serve KWin reports: {e}");
    }
    *guard = Some(conn.clone());
    Ok(conn)
}

fn gnome() -> Result<CompositorState, String> {
    let conn = session_bus()?;
    let reply = conn
        .call_method(
            Some("org.gnome.Shell"),
            "/com/hackclub/Lookout/Activity",
            Some("com.hackclub.Lookout.Activity"),
            "Sample",
            &(),
        )
        .map_err(|e| format!("gnome-shell extension: {e}"))?;
    let json: String = reply.body().deserialize().map_err(|e| format!("gnome-shell: {e}"))?;
    parse_report("gnome-shell", &json)
}

// ── KWin ────────────────────────────────────────────────────────────────

const KWIN_REPORT_PATH: &str = "/com/hackclub/Lookout/Activity";
const KWIN_REPORT_IFACE: &str = "com.hackclub.Lookout.Activity";

/// Where KWin scripts' reports land, newest last.
static KWIN_REPORTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct KwinReports;

#[zbus::interface(name = "com.hackclub.Lookout.Activity")]
impl KwinReports {
    fn report(&self, json: String) {
        let mut r = KWIN_REPORTS.lock().unwrap_or_else(|p| p.into_inner());
        r.push(json);
        let len = r.len();
        if len > 4 {
            r.drain(..len - 4);
        }
    }
}

/// The script KWin runs: read the workspace, call back to `service`.
/// Written for both KWin 6 (`activeWindow`, `screens`) and 5
/// (`activeClient`, `numScreens`).
fn kwin_script(service: &str) -> String {
    format!(
        r#"(function () {{
    const w = workspace.activeWindow || workspace.activeClient || null;
    const p = workspace.cursorPos;
    const monitors = [];
    if (workspace.screens) {{
        for (const s of workspace.screens) {{
            const g = s.geometry;
            monitors.push({{x: g.x, y: g.y, w: g.width, h: g.height}});
        }}
    }} else {{
        for (let i = 0; i < workspace.numScreens; i++) {{
            const g = workspace.clientArea(KWin.ScreenArea, i, workspace.currentDesktop);
            monitors.push({{x: g.x, y: g.y, w: g.width, h: g.height}});
        }}
    }}
    let focus = null;
    if (w) {{
        const g = w.frameGeometry || w.geometry;
        const app = String(w.desktopFileName || w.resourceClass || "") || null;
        focus = {{app: app, x: g.x, y: g.y, w: g.width, h: g.height}};
    }}
    callDBus("{service}", "{KWIN_REPORT_PATH}", "{KWIN_REPORT_IFACE}", "Report",
        JSON.stringify({{pointer: p ? [p.x, p.y] : null, monitors: monitors, focus: focus}}));
}})();
"#
    )
}

fn kwin() -> Result<CompositorState, String> {
    const PLUGIN: &str = "lookout-activity";
    let conn = session_bus()?;
    let service = conn.unique_name().ok_or("no unique bus name")?.to_string();
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let path = dir.join(format!("lookout-kwin-activity-{}.js", service.trim_start_matches(':').replace('.', "_")));
    if !path.exists() {
        std::fs::write(&path, kwin_script(&service)).map_err(|e| format!("kwin script: {e}"))?;
    }

    // A leftover from an interrupted sample would make the load refuse.
    let _ = scripting(&conn, "unloadScript", &(PLUGIN,));
    let reply = scripting(&conn, "loadScript", &(path.to_string_lossy().to_string(), PLUGIN))?;
    let id: i32 = reply.body().deserialize().map_err(|e| format!("kwin loadScript: {e}"))?;
    if id < 0 {
        return Err("kwin refused the script".into());
    }

    KWIN_REPORTS.lock().unwrap_or_else(|p| p.into_inner()).clear();
    // KWin 6 puts scripts under /Scripting/ScriptN, KWin 5 under /N.
    let ran = [format!("/Scripting/Script{id}"), format!("/{id}")].iter().any(|object| {
        conn.call_method(Some("org.kde.KWin"), object.as_str(), Some("org.kde.kwin.Script"), "run", &())
            .is_ok()
    });
    let report = if ran {
        let deadline = std::time::Instant::now() + TIMEOUT;
        loop {
            if let Some(r) = KWIN_REPORTS.lock().unwrap_or_else(|p| p.into_inner()).pop() {
                break Some(r);
            }
            if std::time::Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    } else {
        None
    };
    let _ = scripting(&conn, "unloadScript", &(PLUGIN,));
    match report {
        Some(json) => parse_report("kwin", &json),
        None if ran => Err("kwin script didn't report back".into()),
        None => Err("kwin script didn't run".into()),
    }
}

/// A call on KWin's `org.kde.kwin.Scripting`.
fn scripting<B>(conn: &zbus::blocking::Connection, method: &str, body: &B) -> Result<zbus::Message, String>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    conn.call_method(Some("org.kde.KWin"), "/Scripting", Some("org.kde.kwin.Scripting"), method, body)
        .map_err(|e| format!("kwin {method}: {e}"))
}

// ── Hyprland ────────────────────────────────────────────────────────────

fn hyprland_socket() -> Option<PathBuf> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let candidates = runtime
        .map(|r| r.join("hypr").join(&sig).join(".socket.sock"))
        .into_iter()
        .chain(std::iter::once(PathBuf::from("/tmp/hypr").join(&sig).join(".socket.sock")));
    candidates.into_iter().find(|p| p.exists())
}

fn hyprctl(socket: &PathBuf, command: &str) -> Result<Value, String> {
    let mut s = UnixStream::connect(socket).map_err(|e| format!("hyprland socket: {e}"))?;
    s.set_read_timeout(Some(TIMEOUT)).ok();
    s.set_write_timeout(Some(TIMEOUT)).ok();
    s.write_all(command.as_bytes()).map_err(|e| format!("hyprland {command}: {e}"))?;
    let mut out = String::new();
    s.read_to_string(&mut out).map_err(|e| format!("hyprland {command}: {e}"))?;
    serde_json::from_str(&out).map_err(|e| format!("hyprland {command}: {e}"))
}

fn hyprland() -> Result<CompositorState, String> {
    hyprland_at(&hyprland_socket().ok_or("hyprland socket not found")?)
}

fn hyprland_at(socket: &PathBuf) -> Result<CompositorState, String> {
    Ok(parse_hyprland(
        &hyprctl(&socket, "j/activewindow")?,
        &hyprctl(&socket, "j/cursorpos")?,
        &hyprctl(&socket, "j/monitors")?,
    ))
}

pub(crate) fn parse_hyprland(active: &Value, cursor: &Value, monitors: &Value) -> CompositorState {
    let monitors = monitors
        .as_array()
        .map(|ms| {
            ms.iter()
                .filter_map(|m| {
                    // width/height are the mode's pixels; the layout is logical.
                    let scale = num(&m["scale"]).filter(|s| *s > 0.0).unwrap_or(1.0);
                    let (mut w, mut h) = (num(&m["width"])? / scale, num(&m["height"])? / scale);
                    if m["transform"].as_i64().is_some_and(|t| t % 2 == 1) {
                        std::mem::swap(&mut w, &mut h);
                    }
                    Some(Rect { x: num(&m["x"])?, y: num(&m["y"])?, w, h })
                })
                .collect()
        })
        .unwrap_or_default();
    let pointer = (|| Some((num(&cursor["x"])?, num(&cursor["y"])?)))();
    // `{}` when nothing has the focus.
    let focus = active["address"].is_string().then(|| Focus {
        app: non_empty(&active["class"]).or_else(|| non_empty(&active["initialClass"])),
        rect: (|| {
            let at = active["at"].as_array()?;
            let size = active["size"].as_array()?;
            Some(Rect { x: num(&at[0])?, y: num(&at[1])?, w: num(&size[0])?, h: num(&size[1])? })
        })(),
    });
    CompositorState { method: "hyprland", monitors, pointer, focus }
}

// ── Sway ────────────────────────────────────────────────────────────────

fn sway_ipc(socket: &str, kind: u32) -> Result<Value, String> {
    let mut s = UnixStream::connect(socket).map_err(|e| format!("sway socket: {e}"))?;
    s.set_read_timeout(Some(TIMEOUT)).ok();
    s.set_write_timeout(Some(TIMEOUT)).ok();
    let mut msg = b"i3-ipc".to_vec();
    msg.extend_from_slice(&0u32.to_ne_bytes());
    msg.extend_from_slice(&kind.to_ne_bytes());
    s.write_all(&msg).map_err(|e| format!("sway ipc: {e}"))?;
    let mut header = [0u8; 14];
    s.read_exact(&mut header).map_err(|e| format!("sway ipc: {e}"))?;
    if &header[..6] != b"i3-ipc" {
        return Err("sway ipc: bad reply".into());
    }
    let len = u32::from_ne_bytes([header[6], header[7], header[8], header[9]]) as usize;
    if len > 64 * 1024 * 1024 {
        return Err("sway ipc: reply too large".into());
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).map_err(|e| format!("sway ipc: {e}"))?;
    serde_json::from_slice(&body).map_err(|e| format!("sway ipc: {e}"))
}

fn sway() -> Result<CompositorState, String> {
    let socket = std::env::var("SWAYSOCK").map_err(|_| "SWAYSOCK unset")?;
    sway_at(&socket)
}

fn sway_at(socket: &str) -> Result<CompositorState, String> {
    const GET_OUTPUTS: u32 = 3;
    const GET_TREE: u32 = 4;
    Ok(parse_sway(&sway_ipc(socket, GET_TREE)?, &sway_ipc(socket, GET_OUTPUTS)?))
}

fn sway_focused(node: &Value) -> Option<&Value> {
    if node["focused"].as_bool() == Some(true) {
        return Some(node);
    }
    ["nodes", "floating_nodes"]
        .iter()
        .filter_map(|k| node[*k].as_array())
        .flatten()
        .find_map(sway_focused)
}

pub(crate) fn parse_sway(tree: &Value, outputs: &Value) -> CompositorState {
    let monitors = outputs
        .as_array()
        .map(|os| {
            os.iter()
                .filter(|o| o["active"].as_bool() != Some(false))
                .filter_map(|o| rect_xywh(&o["rect"], "x", "y", "width", "height"))
                .collect()
        })
        .unwrap_or_default();
    // An empty focused workspace is focused too, but isn't a window.
    let focus = sway_focused(tree)
        .filter(|n| matches!(n["type"].as_str(), Some("con") | Some("floating_con")))
        .map(|n| Focus {
            app: non_empty(&n["app_id"]).or_else(|| non_empty(&n["window_properties"]["class"])),
            rect: rect_xywh(&n["rect"], "x", "y", "width", "height"),
        });
    CompositorState { method: "sway", monitors, pointer: None, focus }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_the_gnome_and_kwin_report() {
        let s = parse_report(
            "gnome-shell",
            r#"{"pointer":[100,200],"monitors":[{"x":0,"y":0,"w":1920,"h":1080}],
                "focus":{"app":"org.gnome.Terminal","x":10,"y":20,"w":800,"h":600}}"#,
        )
        .unwrap();
        assert_eq!(s.pointer, Some((100.0, 200.0)));
        assert_eq!(s.monitors, vec![Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 }]);
        let f = s.focus.unwrap();
        assert_eq!(f.app.as_deref(), Some("org.gnome.Terminal"));
        assert_eq!(f.rect, Some(Rect { x: 10.0, y: 20.0, w: 800.0, h: 600.0 }));

        let none = parse_report("kwin", r#"{"pointer":null,"monitors":[],"focus":null}"#).unwrap();
        assert_eq!(none.pointer, None);
        assert_eq!(none.focus, None);
        assert!(parse_report("kwin", "not json").is_err());
    }

    #[test]
    fn reads_hyprland_in_logical_layout_coordinates() {
        let s = parse_hyprland(
            &json!({"address":"0x1","at":[2600,100],"size":[1000,700],"class":"kitty","initialClass":"kitty"}),
            &json!({"x": 2700, "y": 300}),
            &json!([
                {"id":0,"x":0,"y":0,"width":2560,"height":1440,"scale":1.25,"transform":0},
                {"id":1,"x":2048,"y":0,"width":1080,"height":1920,"scale":1.0,"transform":1},
            ]),
        );
        assert_eq!(s.monitors[0], Rect { x: 0.0, y: 0.0, w: 2048.0, h: 1152.0 });
        // Rotated: the mode's 1080x1920 lies on its side.
        assert_eq!(s.monitors[1], Rect { x: 2048.0, y: 0.0, w: 1920.0, h: 1080.0 });
        assert_eq!(s.pointer, Some((2700.0, 300.0)));
        assert_eq!(s.focus.unwrap().app.as_deref(), Some("kitty"));
        assert_eq!(parse_hyprland(&json!({}), &json!({}), &json!([])).focus, None);
    }

    #[test]
    fn reads_the_focused_sway_window_and_outputs() {
        let tree = json!({"type":"root","nodes":[{"type":"output","nodes":[{"type":"workspace",
            "nodes":[{"type":"con","focused":false,"app_id":"foot","rect":{"x":0,"y":0,"width":960,"height":1080}}],
            "floating_nodes":[{"type":"floating_con","focused":true,"app_id":null,
                "window_properties":{"class":"Code"},"rect":{"x":100,"y":100,"width":800,"height":600}}]}]}]});
        let outputs = json!([
            {"name":"eDP-1","active":true,"rect":{"x":0,"y":0,"width":1920,"height":1080}},
            {"name":"HDMI-A-1","active":false,"rect":{"x":0,"y":0,"width":0,"height":0}},
        ]);
        let s = parse_sway(&tree, &outputs);
        assert_eq!(s.monitors.len(), 1);
        assert_eq!(s.pointer, None);
        let f = s.focus.unwrap();
        assert_eq!(f.app.as_deref(), Some("Code"));
        assert_eq!(f.rect, Some(Rect { x: 100.0, y: 100.0, w: 800.0, h: 600.0 }));

        let empty_ws = json!({"type":"root","nodes":[{"type":"workspace","focused":true,"nodes":[]}]});
        assert_eq!(parse_sway(&empty_ws, &outputs).focus, None);
    }

    /// A one-thread Unix socket server answering `replies` in order.
    fn serve(path: &std::path::Path, replies: Vec<Box<dyn Fn(&mut UnixStream) + Send>>) {
        let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            for reply in replies {
                let (mut conn, _) = listener.accept().unwrap();
                reply(&mut conn);
            }
        });
    }

    #[test]
    fn talks_to_hyprland_over_its_socket() {
        let dir = std::env::temp_dir().join(format!("lookout-hypr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".socket.sock");
        let _ = std::fs::remove_file(&path);
        let answer = |expect: &'static str, body: &'static str| -> Box<dyn Fn(&mut UnixStream) + Send> {
            Box::new(move |c: &mut UnixStream| {
                let mut buf = [0u8; 64];
                let n = c.read(&mut buf).unwrap();
                assert_eq!(&buf[..n], expect.as_bytes());
                c.write_all(body.as_bytes()).unwrap();
            })
        };
        serve(&path, vec![
            answer("j/activewindow", r#"{"address":"0x5","at":[10,20],"size":[300,200],"class":"kitty"}"#),
            answer("j/cursorpos", r#"{"x":15,"y":25}"#),
            answer("j/monitors", r#"[{"x":0,"y":0,"width":1920,"height":1080,"scale":1,"transform":0}]"#),
        ]);
        let s = hyprland_at(&path).unwrap();
        assert_eq!(s.pointer, Some((15.0, 25.0)));
        assert_eq!(s.focus.unwrap().app.as_deref(), Some("kitty"));
        assert_eq!(s.monitors.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn talks_to_sway_over_i3_ipc() {
        let path = std::env::temp_dir().join(format!("lookout-sway-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let answer = |kind: u32, body: &'static str| -> Box<dyn Fn(&mut UnixStream) + Send> {
            Box::new(move |c: &mut UnixStream| {
                let mut header = [0u8; 14];
                c.read_exact(&mut header).unwrap();
                assert_eq!(&header[..6], b"i3-ipc");
                assert_eq!(u32::from_ne_bytes(header[10..14].try_into().unwrap()), kind);
                let mut msg = b"i3-ipc".to_vec();
                msg.extend_from_slice(&(body.len() as u32).to_ne_bytes());
                msg.extend_from_slice(&kind.to_ne_bytes());
                msg.extend_from_slice(body.as_bytes());
                c.write_all(&msg).unwrap();
            })
        };
        serve(&path, vec![
            answer(4, r#"{"type":"root","nodes":[{"type":"con","focused":true,"app_id":"foot","rect":{"x":0,"y":0,"width":960,"height":1080}}]}"#),
            answer(3, r#"[{"active":true,"rect":{"x":0,"y":0,"width":1920,"height":1080}}]"#),
        ]);
        let s = sway_at(path.to_str().unwrap()).unwrap();
        assert_eq!(s.focus.unwrap().app.as_deref(), Some("foot"));
        assert_eq!(s.monitors, vec![Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 }]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_kwin_script_calls_back_to_us() {
        let s = kwin_script(":1.42");
        assert!(s.contains(r#"callDBus(":1.42", "/com/hackclub/Lookout/Activity", "com.hackclub.Lookout.Activity", "Report""#));
    }
}

/// The GNOME and KWin paths end to end over a real bus, against stand-ins
/// for the shell extension and KWin's scripting API. Run on a private bus,
/// inside a tokio runtime's blocking pool as the recorder does:
/// `dbus-run-session -- cargo test -p lookout-core live_dbus_stand_ins -- --ignored`
#[cfg(test)]
#[test]
#[ignore]
fn live_dbus_stand_ins() {
    use std::sync::Mutex as StdMutex;

    const REPORT: &str = r#"{"pointer":[50,60],"monitors":[{"x":0,"y":0,"w":1920,"h":1080}],"focus":{"app":"org.gnome.Terminal","x":1,"y":2,"w":3,"h":4}}"#;

    struct Shell;
    #[zbus::interface(name = "com.hackclub.Lookout.Activity")]
    impl Shell {
        fn sample(&self) -> String {
            REPORT.to_string()
        }
    }

    struct Scripting {
        path: std::sync::Arc<StdMutex<Option<String>>>,
    }
    #[zbus::interface(name = "org.kde.kwin.Scripting")]
    impl Scripting {
        #[zbus(name = "loadScript")]
        fn load_script(&self, path: String, _plugin: String) -> i32 {
            *self.path.lock().unwrap() = Some(path);
            7
        }
        #[zbus(name = "unloadScript")]
        fn unload_script(&self, _plugin: String) -> bool {
            true
        }
    }

    struct Script {
        path: std::sync::Arc<StdMutex<Option<String>>>,
    }
    #[zbus::interface(name = "org.kde.kwin.Script")]
    impl Script {
        /// What KWin does with the script: run it, which calls back.
        #[zbus(name = "run")]
        async fn run(&self, #[zbus(connection)] conn: &zbus::Connection) {
            let path = self.path.lock().unwrap().clone().unwrap();
            let js = std::fs::read_to_string(path).unwrap();
            let service = js.split("callDBus(\"").nth(1).unwrap().split('"').next().unwrap().to_string();
            let report = r#"{"pointer":[7,8],"monitors":[],"focus":{"app":"org.kde.konsole","x":0,"y":0,"w":10,"h":10}}"#;
            conn.call_method(
                Some(service.as_str()),
                KWIN_REPORT_PATH,
                Some(KWIN_REPORT_IFACE),
                "Report",
                &(report,),
            )
            .await
            .unwrap();
        }
    }

    assert!(std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some(), "run under dbus-run-session");
    let path = std::sync::Arc::new(StdMutex::new(None));
    let _shell = zbus::blocking::connection::Builder::session()
        .unwrap()
        .name("org.gnome.Shell")
        .unwrap()
        .serve_at("/com/hackclub/Lookout/Activity", Shell)
        .unwrap()
        .build()
        .unwrap();
    let _kwin = zbus::blocking::connection::Builder::session()
        .unwrap()
        .name("org.kde.KWin")
        .unwrap()
        .serve_at("/Scripting", Scripting { path: path.clone() })
        .unwrap()
        .serve_at("/Scripting/Script7", Script { path })
        .unwrap()
        .build()
        .unwrap();

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let (gnome_state, kwin_state) = rt.block_on(async {
        tokio::task::spawn_blocking(|| (gnome(), kwin())).await.unwrap()
    });
    let g = gnome_state.unwrap();
    assert_eq!(g.method, "gnome-shell");
    assert_eq!(g.pointer, Some((50.0, 60.0)));
    assert_eq!(g.focus.unwrap().app.as_deref(), Some("org.gnome.Terminal"));
    let k = kwin_state.unwrap();
    assert_eq!(k.method, "kwin");
    assert_eq!(k.pointer, Some((7.0, 8.0)));
    assert_eq!(k.focus.unwrap().app.as_deref(), Some("org.kde.konsole"));
}
