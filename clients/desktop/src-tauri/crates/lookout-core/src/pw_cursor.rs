//! The pointer, as the screencast stream itself reports it: on Wayland the
//! only way a client learns where the cursor is, and it works on every
//! compositor whose portal offers the "metadata" cursor mode (GNOME, KDE).
//!
//! The portal session asks for that mode (screencast.rs), which keeps the
//! cursor out of the pixels and puts its position in each buffer's
//! `SPA_META_Cursor`, relative to the stream's own frame. A watcher per
//! captured node holds a light second link to the stream — capped at two
//! frames a second, buffers handed straight back unread — and keeps the
//! newest position. The frame grabs (pipewire.rs) are untouched.

use std::collections::HashMap;
use std::os::fd::{BorrowedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What a watcher last saw.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct StreamCursor {
    /// The pointer in stream pixels, origin top-left. `None` when the
    /// compositor says it isn't over the stream (a window stream with the
    /// pointer elsewhere), or before any buffer arrived.
    pub position: Option<(i32, i32)>,
    /// The stream's frame size, from the negotiated format.
    pub size: Option<(u32, u32)>,
    /// Whether any buffer carried cursor metadata at all — false means the
    /// portal didn't grant the metadata mode, so `position` means nothing.
    pub has_metadata: bool,
}

struct Watcher {
    fd: RawFd,
    state: Arc<Mutex<StreamCursor>>,
    alive: Arc<AtomicBool>,
    quit: ::pipewire::channel::Sender<()>,
    started: Instant,
}

static WATCHERS: Mutex<Option<HashMap<u32, Watcher>>> = Mutex::new(None);

/// How long a fresh watcher gets to see its first buffer before the
/// sampler stops waiting on it.
const WARMUP: Duration = Duration::from_millis(400);

/// The pointer on `node` (reached through the portal's PipeWire `fd`),
/// starting a watcher the first time. Watchers for nodes not in `keep`
/// are stopped — a capture run only ever samples its own streams.
pub(crate) fn cursor(node: u32, fd: RawFd, keep: &[u32]) -> Option<StreamCursor> {
    let mut guard = WATCHERS.lock().unwrap_or_else(|e| e.into_inner());
    let watchers = guard.get_or_insert_with(HashMap::new);
    watchers.retain(|id, w| {
        let wanted = keep.contains(id) && w.alive.load(Ordering::Acquire);
        if !wanted {
            let _ = w.quit.send(());
        }
        wanted
    });
    if watchers.get(&node).is_some_and(|w| w.fd != fd) {
        if let Some(old) = watchers.remove(&node) {
            let _ = old.quit.send(());
        }
    }
    if !watchers.contains_key(&node) {
        let w = spawn(node, fd)?;
        watchers.insert(node, w);
    }
    let w = watchers.get(&node)?;
    let state = Arc::clone(&w.state);
    let fresh = w.started.elapsed() < WARMUP;
    drop(guard);
    if fresh {
        // A just-started watcher has nothing yet; give it one buffer.
        let deadline = Instant::now() + WARMUP;
        while Instant::now() < deadline {
            if state.lock().unwrap_or_else(|e| e.into_inner()).size.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let s = *state.lock().unwrap_or_else(|e| e.into_inner());
    s.size.is_some().then_some(s)
}

fn spawn(node: u32, fd: RawFd) -> Option<Watcher> {
    // Our own copy of the socket, so closing the session's can't pull it
    // out from under the loop. It must be a remote of its own (see
    // `cursor_fd` in screencast.rs): one PipeWire socket is one client,
    // and the frame grabs speak on theirs.
    let owned = unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned().ok()?;
    let state = Arc::new(Mutex::new(StreamCursor::default()));
    let alive = Arc::new(AtomicBool::new(true));
    let (quit, quit_rx) = ::pipewire::channel::channel::<()>();
    let (thread_state, thread_alive) = (Arc::clone(&state), Arc::clone(&alive));
    let spawned = std::thread::Builder::new()
        .name(format!("lookout-pw-cursor-{node}"))
        .spawn(move || {
            if let Err(e) = run(node, owned, thread_state, quit_rx) {
                lookout_session::log_line!("[activity] cursor watcher for node {node} failed: {e}");
            }
            thread_alive.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        return None;
    }
    Some(Watcher { fd, state, alive, quit, started: Instant::now() })
}

/// `CURSOR_META_SIZE` from the PipeWire docs: the cursor, its bitmap
/// header, and a w×h RGBA bitmap.
fn cursor_meta_size(w: i32, h: i32) -> i32 {
    (std::mem::size_of::<::pipewire::spa::sys::spa_meta_cursor>()
        + std::mem::size_of::<::pipewire::spa::sys::spa_meta_bitmap>()) as i32
        + w * h * 4
}

fn serialize(object: ::pipewire::spa::pod::Object) -> Result<Vec<u8>, String> {
    use ::pipewire::spa::pod::{serialize::PodSerializer, Value};
    PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(object))
        .map(|(c, _)| c.into_inner())
        .map_err(|e| format!("pod serialize: {e:?}"))
}

fn run(
    node: u32,
    fd: std::os::fd::OwnedFd,
    state: Arc<Mutex<StreamCursor>>,
    quit_rx: ::pipewire::channel::Receiver<()>,
) -> Result<(), String> {
    use ::pipewire::context::ContextRc;
    use ::pipewire::keys::{MEDIA_CATEGORY, MEDIA_ROLE, MEDIA_TYPE};
    use ::pipewire::main_loop::MainLoopRc;
    use ::pipewire::properties::properties;
    use ::pipewire::spa;
    use ::pipewire::spa::buffer::meta::MetaCursor;
    use ::pipewire::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use ::pipewire::spa::param::video::{VideoFormat, VideoInfoRaw};
    use ::pipewire::spa::param::{format_utils, ParamType};
    use ::pipewire::spa::pod::{self, ChoiceValue, Pod, Property, PropertyFlags, Value};
    use ::pipewire::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Direction, Fraction, Id, Rectangle, SpaTypes};
    use ::pipewire::stream::{StreamFlags, StreamRc, StreamState};

    ::pipewire::init();
    let main_loop = MainLoopRc::new(None).map_err(|e| format!("main loop: {e}"))?;
    let context = ContextRc::new(&main_loop, None).map_err(|e| format!("context: {e}"))?;
    let core = context.connect_fd_rc(fd, None).map_err(|e| format!("connect: {e}"))?;
    let stream = StreamRc::new(
        core,
        "Lookout pointer",
        properties! {
            *MEDIA_TYPE => "Video",
            *MEDIA_CATEGORY => "Capture",
            *MEDIA_ROLE => "Screen",
        },
    )
    .map_err(|e| format!("stream: {e}"))?;

    // Asked for once the format is fixed, as the PipeWire docs prescribe.
    let meta = serialize(pod::Object {
        type_: SpaTypes::ObjectParamMeta.as_raw(),
        id: ParamType::Meta.as_raw(),
        properties: vec![
            Property {
                key: spa::sys::SPA_PARAM_META_type,
                flags: PropertyFlags::empty(),
                value: Value::Id(Id(spa::sys::SPA_META_Cursor)),
            },
            Property {
                key: spa::sys::SPA_PARAM_META_size,
                flags: PropertyFlags::empty(),
                value: Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: cursor_meta_size(64, 64),
                        min: cursor_meta_size(1, 1),
                        max: cursor_meta_size(512, 512),
                    },
                ))),
            },
        ],
    })?;

    let quitter = main_loop.clone();
    let on_state = Arc::clone(&state);
    let on_process = Arc::clone(&state);
    let _listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(move |_, _, _, new| {
            if matches!(new, StreamState::Error(_) | StreamState::Unconnected) {
                // The node is gone (session closed) — the sampler starts a
                // fresh watcher if the node ever comes back.
                quitter.quit();
            }
        })
        .param_changed(move |stream, _, id, param| {
            let Some(param) = param else { return };
            if id != ParamType::Format.as_raw() {
                return;
            }
            let Ok((MediaType::Video, MediaSubtype::Raw)) = format_utils::parse_format(param) else {
                return;
            };
            let mut info = VideoInfoRaw::default();
            if info.parse(param).is_ok() {
                let size = info.size();
                on_state.lock().unwrap_or_else(|e| e.into_inner()).size = Some((size.width, size.height));
            }
            if let Some(pod) = Pod::from_bytes(&meta) {
                let _ = stream.update_params(&mut [pod]);
            }
        })
        .process(move |stream, _| {
            // Dropping the buffer queues it straight back; only the meta
            // is read.
            if let Some(buffer) = stream.dequeue_buffer() {
                if let Some(cursor) = buffer.find_meta::<MetaCursor>() {
                    let mut s = on_process.lock().unwrap_or_else(|e| e.into_inner());
                    s.has_metadata = true;
                    s.position = cursor.is_valid().then(|| {
                        let p = cursor.position();
                        (p.x, p.y)
                    });
                }
            }
        })
        .register()
        .map_err(|e| format!("listener: {e}"))?;

    let format = serialize(pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA,
            VideoFormat::xBGR,
            VideoFormat::xRGB,
            VideoFormat::RGB,
        ),
        pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            Rectangle { width: 1920, height: 1080 },
            Rectangle { width: 1, height: 1 },
            Rectangle { width: 16384, height: 16384 }
        ),
        // Two frames a second is plenty for a pointer sampled every few
        // seconds, and keeps the compositor from copying frames for us at
        // the display's rate.
        pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: 2, denom: 1 },
            Fraction { num: 0, denom: 1 },
            Fraction { num: 2, denom: 1 }
        ),
    ))?;
    let format_pod = Pod::from_bytes(&format).ok_or("format pod")?;
    stream
        .connect(
            Direction::Input,
            Some(node),
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut [format_pod],
        )
        .map_err(|e| format!("stream connect: {e}"))?;

    let quit_loop = main_loop.clone();
    let _quit = quit_rx.attach(main_loop.loop_(), move |()| quit_loop.quit());
    main_loop.run();
    Ok(())
}

/// End to end against a real PipeWire: publish a small video stream that
/// carries cursor metadata (as GNOME and KDE do for a portal stream) and
/// read it back through a watcher. Needs a running PipeWire with a session
/// manager; point it at one with `LOOKOUT_PW_SOCKET`:
/// `LOOKOUT_PW_SOCKET=$XDG_RUNTIME_DIR/pipewire-0 cargo test -p lookout-core live_cursor_metadata -- --ignored`
#[cfg(test)]
#[test]
#[ignore]
fn live_cursor_metadata() {
    use std::os::fd::IntoRawFd;

    let socket = std::env::var("LOOKOUT_PW_SOCKET").expect("LOOKOUT_PW_SOCKET");
    let (node_tx, node_rx) = std::sync::mpsc::channel::<u32>();
    let producer_socket = socket.clone();
    let at = Arc::new(Mutex::new(Some((80, 180))));
    let producer_at = Arc::clone(&at);
    std::thread::spawn(move || producer(&producer_socket, producer_at, node_tx));
    let node = node_rx.recv_timeout(Duration::from_secs(5)).expect("the producer's node id");

    // Two connections, as the portal hands out: one for the watcher, one
    // for the frame grabs. A PipeWire socket carries one client only.
    let fd = std::os::unix::net::UnixStream::connect(&socket).unwrap().into_raw_fd();
    let grab_fd = std::os::unix::net::UnixStream::connect(&socket).unwrap().into_raw_fd();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = None;
    while Instant::now() < deadline {
        last = cursor(node, fd, &[node]);
        if last.is_some_and(|c| c.position.is_some()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let c = last.expect("the watcher saw the stream");
    assert_eq!(c.size, Some((320, 240)));
    assert!(c.has_metadata, "cursor metadata negotiated");
    assert_eq!(c.position, Some((80, 180)));

    // The frame grab still works with the watcher linked to the same node
    // (needs GStreamer's pipewiresrc).
    if std::env::var_os("LOOKOUT_PW_GRAB").is_some() {
        let frame = crate::pipewire::capture_pipewire_node(node, grab_fd).expect("a frame beside the watcher");
        assert_eq!((frame.width(), frame.height()), (320, 240));
    }

    // The pointer leaves the stream (the compositor zeroes the cursor id).
    *at.lock().unwrap() = None;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && cursor(node, fd, &[node]).unwrap().position.is_some() {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(cursor(node, fd, &[node]).unwrap().position, None);
}

/// A 320x240 video source whose every buffer carries a cursor at `at`
/// (or, when `None`, a cursor marked not over the stream).
#[cfg(test)]
fn producer(socket: &str, at: Arc<Mutex<Option<(i32, i32)>>>, node_tx: std::sync::mpsc::Sender<u32>) {
    use ::pipewire::context::ContextRc;
    use ::pipewire::main_loop::MainLoopRc;
    use ::pipewire::properties::properties;
    use ::pipewire::spa;
    use ::pipewire::spa::buffer::meta::MetaCursor;
    use ::pipewire::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use ::pipewire::spa::param::video::VideoFormat;
    use ::pipewire::spa::param::ParamType;
    use ::pipewire::spa::pod::{self, ChoiceValue, Pod, Property, PropertyFlags, Value};
    use ::pipewire::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Direction, Fraction, Id, Rectangle, SpaTypes};
    use ::pipewire::stream::{StreamFlags, StreamRc, StreamState};
    use std::os::fd::OwnedFd;

    ::pipewire::init();
    let main_loop = MainLoopRc::new(None).unwrap();
    let context = ContextRc::new(&main_loop, None).unwrap();
    let fd: OwnedFd = std::os::unix::net::UnixStream::connect(socket).unwrap().into();
    let core = context.connect_fd_rc(fd, None).unwrap();
    let stream = StreamRc::new(
        core,
        "lookout-test-source",
        properties! {
            *::pipewire::keys::MEDIA_CLASS => "Video/Source",
            *::pipewire::keys::MEDIA_TYPE => "Video",
            *::pipewire::keys::MEDIA_CATEGORY => "Source",
        },
    )
    .unwrap();

    let int = |key: u32, v: i32| Property { key, flags: PropertyFlags::empty(), value: Value::Int(v) };
    let params = serialize(pod::Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![
            Property {
                key: spa::sys::SPA_PARAM_BUFFERS_buffers,
                flags: PropertyFlags::empty(),
                value: Value::Choice(ChoiceValue::Int(Choice(ChoiceFlags::empty(), ChoiceEnum::Range { default: 4, min: 2, max: 8 }))),
            },
            int(spa::sys::SPA_PARAM_BUFFERS_blocks, 1),
            int(spa::sys::SPA_PARAM_BUFFERS_size, 320 * 240 * 4),
            int(spa::sys::SPA_PARAM_BUFFERS_stride, 320 * 4),
        ],
    })
    .unwrap();
    let meta = serialize(pod::Object {
        type_: SpaTypes::ObjectParamMeta.as_raw(),
        id: ParamType::Meta.as_raw(),
        properties: vec![
            Property { key: spa::sys::SPA_PARAM_META_type, flags: PropertyFlags::empty(), value: Value::Id(Id(spa::sys::SPA_META_Cursor)) },
            int(spa::sys::SPA_PARAM_META_size, cursor_meta_size(64, 64)),
        ],
    })
    .unwrap();

    let sent = std::cell::Cell::new(false);
    let _listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(move |s, _, _, new| {
            if matches!(new, StreamState::Paused | StreamState::Streaming) && !sent.replace(true) {
                let _ = node_tx.send(s.node_id());
            }
        })
        .param_changed(move |s, _, id, param| {
            if param.is_some() && id == ParamType::Format.as_raw() {
                let _ = s.update_params(&mut [Pod::from_bytes(&params).unwrap(), Pod::from_bytes(&meta).unwrap()]);
            }
        })
        .register()
        .unwrap();

    let format = serialize(pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        pod::property!(FormatProperties::VideoFormat, Id, VideoFormat::BGRx),
        pod::property!(FormatProperties::VideoSize, Rectangle, Rectangle { width: 320, height: 240 }),
        pod::property!(FormatProperties::VideoFramerate, Fraction, Fraction { num: 0, denom: 1 }),
        pod::property!(
            FormatProperties::VideoMaxFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: 10, denom: 1 },
            Fraction { num: 1, denom: 1 },
            Fraction { num: 30, denom: 1 }
        ),
    ))
    .unwrap();
    stream
        .connect(
            Direction::Output,
            None,
            StreamFlags::DRIVER | StreamFlags::MAP_BUFFERS,
            &mut [Pod::from_bytes(&format).unwrap()],
        )
        .unwrap();

    // Push a frame every 100ms, cursor meta filled in.
    let pusher = stream.clone();
    let timer = main_loop.loop_().add_timer(move |_| {
        if let Some(mut buffer) = pusher.dequeue_buffer() {
            if let Some(meta) = buffer.find_meta::<MetaCursor>() {
                let raw = meta.as_raw() as *const spa::sys::spa_meta_cursor as *mut spa::sys::spa_meta_cursor;
                let at = *at.lock().unwrap();
                unsafe {
                    (*raw).id = u32::from(at.is_some());
                    let (x, y) = at.unwrap_or_default();
                    (*raw).position.x = x;
                    (*raw).position.y = y;
                    (*raw).bitmap_offset = 0;
                }
            }
            if let Some(d) = buffer.datas_mut().first_mut() {
                let chunk = d.chunk_mut();
                *chunk.offset_mut() = 0;
                *chunk.stride_mut() = 320 * 4;
                *chunk.size_mut() = 320 * 240 * 4;
            }
        }
    });
    timer.update_timer(Some(Duration::from_millis(100)), Some(Duration::from_millis(100))).into_result().unwrap();
    main_loop.run();
}
