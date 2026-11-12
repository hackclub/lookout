use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::Core;
#[cfg(target_os = "linux")]
use crate::CoreEvent;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StreamInfo {
    pub node_id: u32,
    /// `monitor`, `window` or `virtual`, when the portal says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_type: Option<String>,
    /// A monitor stream's place in the compositor's logical layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<(i32, i32)>,
    /// The stream's logical size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<(i32, i32)>,
}

/// Emitted to the webview when the compositor tears a screencast session down
/// under us — the user hitting "Stop sharing" in the system indicator. From
/// that moment every capture returns nothing, so the UI must stop claiming to
/// record rather than tick away over a dead stream.
pub use lookout_recorder::ScreencastRevoked;

/// A live XDG screencast session.
///
/// The `ashpd::Session` itself stays in the task that created it (see
/// `portal_session_task`) — it borrows the portal proxy, so the two can only
/// live together, and closing a session is an async D-Bus call that a `Drop`
/// impl can't make. Dropping the handle does NOT stop the cast: the portal
/// keeps streaming until `Close()` lands, which is why every session Lookout
/// ever opened used to run until the process exited. This struct is the
/// remote control — drop it and the owning task closes the session for real.
#[cfg(target_os = "linux")]
pub struct ScreencastSession {
    id: u64,
    /// PipeWire nodes this session owns.
    node_ids: Vec<u32>,
    /// What the portal said about each of them.
    streams: Vec<StreamInfo>,
    /// Our end of the PipeWire socket. Owned, so it closes exactly once.
    fd: std::os::fd::OwnedFd,
    /// A second remote on the same session, for the pointer watcher
    /// (pw_cursor.rs): one PipeWire socket carries one client, and the
    /// frame grabs speak on `fd`. `None` without the metadata cursor mode.
    cursor_fd: Option<std::os::fd::OwnedFd>,
    close_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

#[cfg(target_os = "linux")]
impl ScreencastSession {
    fn raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.fd.as_raw_fd()
    }
}

#[cfg(target_os = "linux")]
impl Drop for ScreencastSession {
    fn drop(&mut self) {
        if let Some(tx) = self.close_tx.take() {
            // An error here means the task already exited, which only happens
            // once the session is closed — nothing left to ask for.
            let _ = tx.send(());
        }
        // `fd` closes with the OwnedFd.
    }
}

#[cfg(target_os = "linux")]
static NEXT_SESSION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Own one portal session for its whole life: run the flow, hand the result
/// back, then park until either we're told to close it or the compositor
/// closes it on us.
#[cfg(target_os = "linux")]
async fn portal_session_task(
    core: Arc<Core>,
    id: u64,
    result_tx: tokio::sync::oneshot::Sender<Result<(Vec<StreamInfo>, std::os::fd::RawFd, Option<std::os::fd::RawFd>), String>>,
    close_rx: tokio::sync::oneshot::Receiver<()>,
) {
    use ashpd::desktop::screencast::{
        CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType,
        StartCastOptions,
    };
    use ashpd::desktop::{CreateSessionOptions, PersistMode};
    use futures_util::StreamExt;
    use std::os::fd::IntoRawFd;

    // Connect to the screencast portal
    let proxy = match Screencast::new().await {
        Ok(p) => p,
        Err(e) => {
            let _ = result_tx.send(Err(format!(
                "Failed to connect to screencast portal: {}",
                e
            )));
            return;
        }
    };

    // Create a new session
    let session = match proxy.create_session(CreateSessionOptions::default()).await {
        Ok(s) => s,
        Err(e) => {
            let _ = result_tx.send(Err(format!("Failed to create screencast session: {}", e)));
            return;
        }
    };

    // Past this point the session exists on the portal side, so every bail-out
    // has to close it — a dismissed picker used to leave one behind too.
    let started: Result<(Vec<StreamInfo>, std::os::fd::RawFd, Option<std::os::fd::RawFd>), String> = async {
        // The cursor stays out of the pixels either way. "Metadata" also
        // reports where it is with each buffer — on Wayland the only way
        // to learn the pointer position (pw_cursor.rs) — where the portal
        // offers it.
        let cursor_mode = match proxy.available_cursor_modes().await {
            Ok(modes) if modes.contains(CursorMode::Metadata) => CursorMode::Metadata,
            _ => CursorMode::Hidden,
        };

        // Ask user to select sources (multiple selection enabled)
        proxy
            .select_sources(
                &session,
                SelectSourcesOptions::default()
                    .set_cursor_mode(cursor_mode)
                    .set_sources(SourceType::Monitor | SourceType::Window)
                    .set_multiple(true)
                    .set_persist_mode(PersistMode::DoNot),
            )
            .await
            .map_err(|e| format!("Failed to select sources: {}", e))?;

        // Start the screencast session and get the response containing streams
        let response = proxy
            .start(&session, None, StartCastOptions::default())
            .await
            .map_err(|e| format!("Failed to start screencast: {}", e))?
            .response()
            .map_err(|e| format!("Failed to get screencast response: {}", e))?;

        // Get the pipewire file descriptor
        let fd = proxy
            .open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default())
            .await
            .map_err(|e| format!("Failed to open pipewire remote: {}", e))?;

        let mut streams = Vec::new();
        for stream in response.streams() {
            streams.push(StreamInfo {
                node_id: stream.pipe_wire_node_id(),
                source_type: stream.source_type().map(|t| {
                    match t {
                        SourceType::Monitor => "monitor",
                        SourceType::Window => "window",
                        SourceType::Virtual => "virtual",
                    }
                    .to_string()
                }),
                position: stream.position(),
                size: stream.size(),
            });
        }

        let cursor_fd = if matches!(cursor_mode, CursorMode::Metadata) {
            proxy
                .open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default())
                .await
                .ok()
                .map(IntoRawFd::into_raw_fd)
        } else {
            None
        };

        Ok((streams, fd.into_raw_fd(), cursor_fd))
    }
    .await;

    let (streams, raw_fd, cursor_raw_fd) = match started {
        Ok(v) => v,
        Err(e) => {
            let _ = session.close().await;
            let _ = result_tx.send(Err(e));
            return;
        }
    };

    let node_ids: Vec<u32> = streams.iter().map(|s| s.node_id).collect();
    lookout_session::log_line!("[screencast] session {id} started, nodes {node_ids:?}");

    if result_tx.send(Ok((streams, raw_fd, cursor_raw_fd))).is_err() {
        // The caller walked away (command cancelled). Don't leave a cast
        // running for nobody, and close the fds we just took ownership of.
        use std::os::fd::FromRawFd;
        drop(unsafe { std::os::fd::OwnedFd::from_raw_fd(raw_fd) });
        if let Some(c) = cursor_raw_fd {
            drop(unsafe { std::os::fd::OwnedFd::from_raw_fd(c) });
        }
        let _ = session.close().await;
        return;
    }

    // Subscribe to the portal's `Closed` signal — this is what fires when the
    // user stops the share from the system indicator.
    //
    // `receive_closed` hands back the signal STREAM: awaiting it only sets the
    // subscription up, it does NOT wait for a close. Treating that first
    // `Ok(_)` as the close itself made every session look revoked the instant
    // it started, which killed capture outright.
    let mut closed = match session.receive_closed().await {
        Ok(stream) => Some(Box::pin(stream)),
        Err(e) => {
            // No signal to listen on. The session still works; a revocation
            // just falls to the capture loop's watchdog instead.
            lookout_session::log_line!("[screencast] session {id} closed-signal subscribe failed: {e}");
            None
        }
    };

    // Park until the session ends, from either side.
    tokio::select! {
        _ = close_rx => {
            match session.close().await {
                Ok(()) => lookout_session::log_line!("[screencast] session {id} closed"),
                Err(e) => lookout_session::log_line!("[screencast] closing session {id} failed: {e}"),
            }
        }
        _ = async {
            match closed.as_mut() {
                // One item on this stream means the session is gone. So does
                // the stream ending (None) — the signal connection is only
                // torn down with the session.
                Some(stream) => { stream.next().await; }
                // Never resolves: with no subscription this arm must not win
                // the select, or we'd revoke a perfectly live session.
                None => std::future::pending::<()>().await,
            }
        } => {
            lookout_session::log_line!("[screencast] session {id} was closed by the compositor");
            on_session_revoked(&core, id, node_ids);
        }
    }
}

/// Run the portal flow in its own task and wait for the outcome.
#[cfg(target_os = "linux")]
async fn open_portal_session(
    core: Arc<Core>,
) -> Result<
    (
        u64,
        Vec<StreamInfo>,
        std::os::fd::OwnedFd,
        Option<std::os::fd::OwnedFd>,
        tokio::sync::oneshot::Sender<()>,
    ),
    String,
>
{
    use std::os::fd::FromRawFd;
    use std::sync::atomic::Ordering;

    let id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let (close_tx, close_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(portal_session_task(core, id, result_tx, close_rx));

    match result_rx.await {
        Ok(Ok((streams, raw_fd, cursor_raw_fd))) => {
            // SAFETY: the task handed ownership of these fds over the
            // channel and gave up its own claim to them.
            let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw_fd) };
            let cursor_fd = cursor_raw_fd.map(|c| unsafe { std::os::fd::OwnedFd::from_raw_fd(c) });
            Ok((id, streams, fd, cursor_fd, close_tx))
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err("Screencast portal task ended unexpectedly".to_string()),
    }
}

/// Rebuild the node → fd map the capture path reads, from the live session
/// list. Nodes whose session is gone drop out, so a capture can never reach
/// for an fd we've already closed.
#[cfg(target_os = "linux")]
fn rebuild_fd_map(state: &Core) {
    let sessions = match state.screencast_sessions.lock() {
        Ok(s) => s,
        Err(_) => return,
    };
    if let Ok(mut fds) = state.pipewire_fds.lock() {
        fds.clear();
        for session in sessions.iter() {
            for node in &session.node_ids {
                fds.insert(*node, session.raw_fd());
            }
        }
    }
}

/// Drop one session after the compositor closed it, and tell the webview —
/// the capture loop is streaming from a node that no longer exists.
#[cfg(target_os = "linux")]
fn on_session_revoked(core: &Core, id: u64, node_ids: Vec<u32>) {
    if let Ok(mut sessions) = core.screencast_sessions.lock() {
        sessions.retain(|s| s.id != id);
    }
    rebuild_fd_map(core);
    core.emit(CoreEvent::ScreencastRevoked(ScreencastRevoked { node_ids }));
}

impl Core {
    /// Every live screencast stream, with the PipeWire remote the pointer
    /// watcher reaches it on — `None` for a session without the metadata
    /// cursor mode, where there is no pointer to read.
    #[cfg(target_os = "linux")]
    pub(crate) fn screencast_streams(&self) -> Vec<(StreamInfo, Option<std::os::fd::RawFd>)> {
        use std::os::fd::AsRawFd;
        let Ok(sessions) = self.screencast_sessions.lock() else {
            return Vec::new();
        };
        sessions
            .iter()
            .flat_map(|s| {
                let fd = s.cursor_fd.as_ref().map(AsRawFd::as_raw_fd);
                s.streams.iter().map(move |info| (info.clone(), fd))
            })
            .collect()
    }

    /// Replace all existing screencast sources with a fresh portal session.
    /// Linux only; elsewhere it fails with a fixed message.
    pub async fn request_screencast(self: &Arc<Self>) -> Result<Vec<StreamInfo>, String> {
        #[cfg(not(target_os = "linux"))]
        {
            Err("Screencast portal is only supported on Linux".into())
        }
        #[cfg(target_os = "linux")]
        {
            let state = self;
            let (id, streams, fd, cursor_fd, close_tx) = open_portal_session(Arc::clone(self)).await?;
            let node_ids: Vec<u32> = streams.iter().map(|s| s.node_id).collect();

            if let Ok(mut sessions) = state.screencast_sessions.lock() {
                // Replace: dropping the old handles closes their portal sessions and
                // their fds. Closing only the fds (which is all this used to do) left
                // the cast itself running, so every trip through the source picker
                // stacked one more entry in the system's screen-sharing indicator.
                sessions.clear();
                sessions.push(ScreencastSession {
                    id,
                    node_ids,
                    streams: streams.clone(),
                    fd,
                    cursor_fd,
                    close_tx: Some(close_tx),
                });
            }
            rebuild_fd_map(state);

            Ok(streams)
        }
    }

    /// Add sources from a new portal session to the existing set (does not remove
    /// previously selected streams). This lets users incrementally build up their
    /// source list even on DEs where the portal dialog doesn't support multi-select.
    /// Linux only; elsewhere it fails with a fixed message.
    pub async fn add_screencast(self: &Arc<Self>) -> Result<Vec<StreamInfo>, String> {
        #[cfg(not(target_os = "linux"))]
        {
            Err("Screencast portal is only supported on Linux".into())
        }
        #[cfg(target_os = "linux")]
        {
            let state = self;
            let (id, streams, fd, cursor_fd, close_tx) = open_portal_session(Arc::clone(self)).await?;
            let node_ids: Vec<u32> = streams.iter().map(|s| s.node_id).collect();

            // Append — keep existing sessions, add the new one
            if let Ok(mut sessions) = state.screencast_sessions.lock() {
                sessions.push(ScreencastSession {
                    id,
                    node_ids,
                    streams: streams.clone(),
                    fd,
                    cursor_fd,
                    close_tx: Some(close_tx),
                });
            }
            rebuild_fd_map(state);

            Ok(streams)
        }
    }

    /// Close every screencast session and forget its nodes.
    ///
    /// Called when a recording session actually ends — deliberately NOT from
    /// `stop_capture_loop`, which pause routes through as well: a pause has to
    /// keep the cast alive or resuming would have to re-prompt for sources.
    /// A no-op off Linux.
    pub fn release_screencast(&self) {
        #[cfg(target_os = "linux")]
        {
            let state = self;
            if let Ok(mut sessions) = state.screencast_sessions.lock() {
                if !sessions.is_empty() {
                    lookout_session::log_line!("[screencast] releasing {} session(s)", sessions.len());
                }
                sessions.clear();
            }
            if let Ok(mut fds) = state.pipewire_fds.lock() {
                fds.clear();
            }
        }
    }
}
