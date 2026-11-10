//! Single instance — a second launch hands its `lookout://` links to the
//! running app and exits (what `tauri-plugin-single-instance` did). Unix
//! uses a socket in the runtime dir; Windows a per-user named pipe (not a
//! loopback port: Hyper-V and WSL's mirrored networking reserve whole port
//! ranges, and a failed bind made every launch a second app).

/// Either the primary instance (with a receiver of forwarded links), or a
/// secondary that already forwarded its arguments and should exit.
pub enum Instance {
    Primary(flume::Receiver<Vec<String>>),
    Secondary,
}

#[cfg(unix)]
fn path() -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join(format!("{}.native.sock", crate::store::APP_ID))
}

pub fn acquire(urls: &[String]) -> Instance {
    #[cfg(unix)]
    {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::{UnixListener, UnixStream};
        let p = path();
        if let Ok(mut s) = UnixStream::connect(&p) {
            let _ = writeln!(s, "{}", serde_json::to_string(urls).unwrap_or_else(|_| "[]".into()));
            return Instance::Secondary;
        }
        let _ = std::fs::remove_file(&p);
        let (tx, rx) = flume::unbounded();
        match UnixListener::bind(&p) {
            Ok(listener) => {
                std::thread::Builder::new()
                    .name("lookout-single-instance".into())
                    .spawn(move || {
                        for stream in listener.incoming().flatten() {
                            let mut line = String::new();
                            if BufReader::new(stream).read_line(&mut line).is_ok() {
                                let urls: Vec<String> = serde_json::from_str(line.trim()).unwrap_or_default();
                                let _ = tx.send(urls);
                            }
                        }
                    })
                    .ok();
            }
            Err(e) => log::warn!("[single-instance] couldn't bind {}: {e}", p.display()),
        }
        Instance::Primary(rx)
    }
    #[cfg(windows)]
    {
        windows_pipe::acquire(urls)
    }
}

#[cfg(windows)]
mod windows_pipe {
    use std::io::{BufRead, BufReader, ErrorKind, Write};
    use std::mem::ManuallyDrop;
    use std::os::windows::io::FromRawHandle;

    use windows::Win32::Foundation::{ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows::core::HSTRING;

    use super::Instance;

    fn name() -> String {
        let user = std::env::var("USERNAME").unwrap_or_default();
        format!(r"\\.\pipe\{}.native-{user}", crate::store::APP_ID)
    }

    /// Hand `urls` to a running instance. `false` if there's none.
    fn forward(name: &str, urls: &[String]) -> bool {
        for _ in 0..5 {
            match std::fs::OpenOptions::new().write(true).open(name) {
                Ok(mut f) => {
                    let _ = writeln!(f, "{}", serde_json::to_string(urls).unwrap_or_else(|_| "[]".into()));
                    return true;
                }
                // The server is between clients: wait for it to listen again.
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) => unsafe {
                    let _ = WaitNamedPipeW(&HSTRING::from(name), 2000);
                },
                Err(e) if e.kind() == ErrorKind::NotFound => return false,
                Err(e) => {
                    log::warn!("[single-instance] couldn't open {name}: {e}");
                    return false;
                }
            }
        }
        false
    }

    pub fn acquire(urls: &[String]) -> Instance {
        let name = name();
        for _ in 0..3 {
            if forward(&name, urls) {
                return Instance::Secondary;
            }
            // `FIRST_PIPE_INSTANCE` makes the create itself the lock: it
            // fails if another launch made the pipe since we looked.
            let pipe = unsafe {
                CreateNamedPipeW(
                    &HSTRING::from(name.as_str()),
                    PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    0,
                    64 * 1024,
                    0,
                    None,
                )
            };
            if pipe == INVALID_HANDLE_VALUE {
                log::warn!("[single-instance] couldn't create {name}: {}", std::io::Error::last_os_error());
                continue;
            }
            let (tx, rx) = flume::unbounded();
            let raw = pipe.0 as isize;
            std::thread::Builder::new()
                .name("lookout-single-instance".into())
                .spawn(move || serve(HANDLE(raw as _), tx))
                .ok();
            return Instance::Primary(rx);
        }
        log::warn!("[single-instance] giving up; running without link hand-off");
        Instance::Primary(flume::unbounded().1)
    }

    fn serve(pipe: HANDLE, tx: flume::Sender<Vec<String>>) {
        // Borrow the handle as a `File` for reads; it lives as long as the app.
        let file = ManuallyDrop::new(unsafe { std::fs::File::from_raw_handle(pipe.0 as _) });
        loop {
            if let Err(e) = unsafe { ConnectNamedPipe(pipe, None) } {
                if e.code() != ERROR_PIPE_CONNECTED.to_hresult() {
                    log::warn!("[single-instance] ConnectNamedPipe: {e}");
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    continue;
                }
            }
            let mut line = String::new();
            if BufReader::new(&*file).read_line(&mut line).is_ok() {
                let urls: Vec<String> = serde_json::from_str(line.trim()).unwrap_or_default();
                let _ = tx.send(urls);
            }
            let _ = unsafe { DisconnectNamedPipe(pipe) };
        }
    }
}
