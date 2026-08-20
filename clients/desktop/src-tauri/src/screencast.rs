use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct StreamInfo {
    pub node_id: u32,
}

/// Emitted to the webview when the compositor tears a screencast session down
/// under us — the user hitting "Stop sharing" in the system indicator. From
/// that moment every capture returns nothing, so the UI must stop claiming to
/// record rather than tick away over a dead stream.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreencastRevoked {
    /// The PipeWire nodes that went away with the session.
    pub node_ids: Vec<u32>,
}

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
    /// Our end of the PipeWire socket. Owned, so it closes exactly once.
    fd: std::os::fd::OwnedFd,
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
    app: tauri::AppHandle,
    id: u64,
    result_tx: tokio::sync::oneshot::Sender<Result<(Vec<StreamInfo>, std::os::fd::RawFd), String>>,
    close_rx: tokio::sync::oneshot::Receiver<()>,
) {
    use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
    use ashpd::desktop::PersistMode;
    use ashpd::WindowIdentifier;
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
    let session = match proxy.create_session().await {
        Ok(s) => s,
        Err(e) => {
            let _ = result_tx.send(Err(format!("Failed to create screencast session: {}", e)));
            return;
        }
    };

    // Past this point the session exists on the portal side, so every bail-out
    // has to close it — a dismissed picker used to leave one behind too.
    let started: Result<(Vec<StreamInfo>, std::os::fd::RawFd), String> = async {
        // Ask user to select sources (multiple selection enabled)
        proxy
            .select_sources(
                &session,
                CursorMode::Hidden,
                SourceType::Monitor | SourceType::Window,
                true,
                None,
                PersistMode::DoNot,
            )
            .await
            .map_err(|e| format!("Failed to select sources: {}", e))?;

        // Start the screencast session and get the response containing streams
        let response = proxy
            .start(&session, &WindowIdentifier::default())
            .await
            .map_err(|e| format!("Failed to start screencast: {}", e))?
            .response()
            .map_err(|e| format!("Failed to get screencast response: {}", e))?;

        // Get the pipewire file descriptor
        let fd = proxy
            .open_pipe_wire_remote(&session)
            .await
            .map_err(|e| format!("Failed to open pipewire remote: {}", e))?;

        let mut streams = Vec::new();
        for stream in response.streams() {
            streams.push(StreamInfo {
                node_id: stream.pipe_wire_node_id(),
            });
        }

        Ok((streams, fd.into_raw_fd()))
    }
    .await;

    let (streams, raw_fd) = match started {
        Ok(v) => v,
        Err(e) => {
            let _ = session.close().await;
            let _ = result_tx.send(Err(e));
            return;
        }
    };

    let node_ids: Vec<u32> = streams.iter().map(|s| s.node_id).collect();
    eprintln!("[screencast] session {id} started, nodes {node_ids:?}");

    if result_tx.send(Ok((streams, raw_fd))).is_err() {
        // The caller walked away (command cancelled). Don't leave a cast
        // running for nobody, and close the fd we just took ownership of.
        use std::os::fd::FromRawFd;
        drop(unsafe { std::os::fd::OwnedFd::from_raw_fd(raw_fd) });
        let _ = session.close().await;
        return;
    }

    // Park until the session ends, from either side.
    tokio::select! {
        _ = close_rx => {
            match session.close().await {
                Ok(()) => eprintln!("[screencast] session {id} closed"),
                Err(e) => eprintln!("[screencast] closing session {id} failed: {e}"),
            }
        }
        // The portal's `Closed` signal — this is what fires when the user
        // stops the share from the system indicator. NOTE: `receive_closed`
        // resolves once, with the close details; if a future ashpd hands back
        // a Stream instead, this becomes a single `.next().await`.
        res = session.receive_closed() => {
            match res {
                Ok(_) => eprintln!("[screencast] session {id} was closed by the compositor"),
                // The signal stream dying also means the session is gone.
                Err(e) => eprintln!("[screencast] session {id} closed signal ended: {e}"),
            }
            on_session_revoked(&app, id, node_ids);
        }
    }
}

/// Run the portal flow in its own task and wait for the outcome.
#[cfg(target_os = "linux")]
async fn open_portal_session(
    app: tauri::AppHandle,
) -> Result<(u64, Vec<StreamInfo>, std::os::fd::OwnedFd, tokio::sync::oneshot::Sender<()>), String>
{
    use std::os::fd::FromRawFd;
    use std::sync::atomic::Ordering;

    let id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let (close_tx, close_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(portal_session_task(app, id, result_tx, close_rx));

    match result_rx.await {
        Ok(Ok((streams, raw_fd))) => {
            // SAFETY: the task handed ownership of this fd over the channel
            // and gave up its own claim to it.
            let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw_fd) };
            Ok((id, streams, fd, close_tx))
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err("Screencast portal task ended unexpectedly".to_string()),
    }
}

/// Rebuild the node → fd map the capture path reads, from the live session
/// list. Nodes whose session is gone drop out, so a capture can never reach
/// for an fd we've already closed.
#[cfg(target_os = "linux")]
fn rebuild_fd_map(state: &crate::AppState) {
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
fn on_session_revoked(app: &tauri::AppHandle, id: u64, node_ids: Vec<u32>) {
    use tauri::{Emitter, Manager};

    let state = match app.try_state::<crate::AppState>() {
        Some(s) => s,
        None => return,
    };
    if let Ok(mut sessions) = state.screencast_sessions.lock() {
        sessions.retain(|s| s.id != id);
    }
    rebuild_fd_map(&state);
    let _ = app.emit("screencast-revoked", ScreencastRevoked { node_ids });
}

/// Replace all existing screencast sources with a fresh portal session.
#[cfg(target_os = "linux")]
pub async fn request_screencast(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
) -> Result<Vec<StreamInfo>, String> {
    let (id, streams, fd, close_tx) = open_portal_session(app).await?;
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
            fd,
            close_tx: Some(close_tx),
        });
    }
    rebuild_fd_map(&state);

    Ok(streams)
}

/// Add sources from a new portal session to the existing set (does not remove
/// previously selected streams). This lets users incrementally build up their
/// source list even on DEs where the portal dialog doesn't support multi-select.
#[cfg(target_os = "linux")]
pub async fn add_screencast(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
) -> Result<Vec<StreamInfo>, String> {
    let (id, streams, fd, close_tx) = open_portal_session(app).await?;
    let node_ids: Vec<u32> = streams.iter().map(|s| s.node_id).collect();

    // Append — keep existing sessions, add the new one
    if let Ok(mut sessions) = state.screencast_sessions.lock() {
        sessions.push(ScreencastSession {
            id,
            node_ids,
            fd,
            close_tx: Some(close_tx),
        });
    }
    rebuild_fd_map(&state);

    Ok(streams)
}

/// Close every screencast session and forget its nodes.
///
/// Called when a recording session actually ends — deliberately NOT from
/// `stop_capture_loop`, which pause routes through as well: a pause has to
/// keep the cast alive or resuming would have to re-prompt for sources.
#[cfg(target_os = "linux")]
pub fn release_screencast(state: &crate::AppState) {
    if let Ok(mut sessions) = state.screencast_sessions.lock() {
        if !sessions.is_empty() {
            eprintln!("[screencast] releasing {} session(s)", sessions.len());
        }
        sessions.clear();
    }
    if let Ok(mut fds) = state.pipewire_fds.lock() {
        fds.clear();
    }
}
