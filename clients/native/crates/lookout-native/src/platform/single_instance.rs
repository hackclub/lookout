//! Single instance — a second launch hands its `lookout://` links to the
//! running app and exits (what `tauri-plugin-single-instance` did). Unix
//! uses a socket in the runtime dir; Windows a loopback port.

use std::io::{BufRead, BufReader, Write};

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
        use std::net::{TcpListener, TcpStream};
        const ADDR: &str = "127.0.0.1:47614";
        if let Ok(mut s) = TcpStream::connect(ADDR) {
            let _ = writeln!(s, "{}", serde_json::to_string(urls).unwrap_or_else(|_| "[]".into()));
            return Instance::Secondary;
        }
        let (tx, rx) = flume::unbounded();
        if let Ok(listener) = TcpListener::bind(ADDR) {
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let mut line = String::new();
                    if BufReader::new(stream).read_line(&mut line).is_ok() {
                        let urls: Vec<String> = serde_json::from_str(line.trim()).unwrap_or_default();
                        let _ = tx.send(urls);
                    }
                }
            });
        }
        Instance::Primary(rx)
    }
}
