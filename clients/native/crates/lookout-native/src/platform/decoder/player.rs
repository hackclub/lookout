//! A clocked player over any [`FrameDecoder`](super::FrameDecoder): the
//! non-Linux playback backend. One thread owns the decoder; commands
//! arrive over a channel; frames are presented on the wall clock from the
//! play anchor, and a paused seek "prerolls" the target frame like
//! GStreamer does, so the editor's scrubbing shows the right frame.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::gpui::RenderImage;

use super::{DecodedFrame, FrameDecoder};

pub enum Msg {
    Frame(Arc<RenderImage>, f32),
    Duration(f64),
    Position(f64),
    Eos,
    Error(String),
}

enum Cmd {
    Play,
    Pause,
    Seek(f64),
    Stop,
}

pub struct Player {
    tx: flume::Sender<Cmd>,
}

fn to_image(f: DecodedFrame) -> Option<(Arc<RenderImage>, f32)> {
    let aspect = f.width as f32 / f.height.max(1) as f32;
    let img = image::RgbaImage::from_raw(f.width, f.height, f.bgra)?;
    Some((Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(img)])), aspect))
}

impl Player {
    pub fn open(url: &str) -> Result<(Self, flume::Receiver<Msg>), String> {
        let (tx, cmd_rx) = flume::unbounded::<Cmd>();
        let (msg_tx, rx) = flume::unbounded::<Msg>();
        let url = url.to_string();
        std::thread::Builder::new()
            .name("lookout-video".into())
            .spawn(move || {
                let dec = super::local_path(&url).and_then(|p| super::open(&p, None));
                match dec {
                    Ok(dec) => run(dec, cmd_rx, msg_tx),
                    Err(e) => {
                        let _ = msg_tx.send(Msg::Error(e));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok((Self { tx }, rx))
    }
    pub fn play(&self) {
        let _ = self.tx.send(Cmd::Play);
    }
    pub fn pause(&self) {
        let _ = self.tx.send(Cmd::Pause);
    }
    pub fn seek(&self, t: f64) {
        let _ = self.tx.send(Cmd::Seek(t));
    }
    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }
}

fn run(mut dec: Box<dyn FrameDecoder>, cmds: flume::Receiver<Cmd>, out: flume::Sender<Msg>) {
    let duration = dec.duration();
    let _ = out.send(Msg::Duration(duration));
    let send_frame = |f: DecodedFrame, out: &flume::Sender<Msg>| {
        let pts = f.pts;
        if let Some((img, a)) = to_image(f) {
            let _ = out.send(Msg::Frame(img, a));
        }
        let _ = out.send(Msg::Position(pts));
    };
    // Preroll the first frame.
    match dec.next_frame() {
        Ok(Some(f)) => send_frame(f, &out),
        Ok(None) => {}
        Err(e) => {
            let _ = out.send(Msg::Error(e));
            return;
        }
    }
    let mut position = 0.0f64;
    // While playing: wall-clock anchor for `position`.
    let mut anchor: Option<(Instant, f64)> = None;
    let mut pending: Option<DecodedFrame> = None;
    loop {
        let timeout = if anchor.is_some() { Duration::from_millis(4) } else { Duration::from_millis(250) };
        match cmds.recv_timeout(timeout) {
            Ok(Cmd::Stop) | Err(flume::RecvTimeoutError::Disconnected) => return,
            Ok(Cmd::Play) => {
                if duration > 0.0 && position >= duration - 0.05 {
                    let _ = dec.seek(0.0);
                    position = 0.0;
                    pending = None;
                }
                anchor = Some((Instant::now(), position));
            }
            Ok(Cmd::Pause) => {
                if let Some((t0, p0)) = anchor.take() {
                    position = (p0 + t0.elapsed().as_secs_f64()).min(duration.max(p0));
                }
            }
            Ok(Cmd::Seek(t)) => {
                // Coalesce a burst of scrub seeks into the last one.
                let mut target = t;
                while let Ok(Cmd::Seek(t2)) = cmds.try_recv() {
                    target = t2;
                }
                position = target;
                pending = None;
                if let Err(e) = dec.seek(target) {
                    let _ = out.send(Msg::Error(e));
                    continue;
                }
                if let Ok(Some(f)) = dec.next_frame() {
                    position = f.pts.max(target);
                    send_frame(f, &out);
                }
                if anchor.is_some() {
                    anchor = Some((Instant::now(), position));
                }
            }
            Err(flume::RecvTimeoutError::Timeout) => {}
        }
        let Some((t0, p0)) = anchor else { continue };
        let now = p0 + t0.elapsed().as_secs_f64();
        if pending.is_none() {
            match dec.next_frame() {
                Ok(Some(f)) => pending = Some(f),
                Ok(None) => {
                    anchor = None;
                    position = duration.max(now.min(duration));
                    let _ = out.send(Msg::Position(position));
                    let _ = out.send(Msg::Eos);
                    continue;
                }
                Err(e) => {
                    let _ = out.send(Msg::Error(e));
                    anchor = None;
                    continue;
                }
            }
        }
        if pending.as_ref().map(|f| f.pts <= now).unwrap_or(false) {
            let f = pending.take().unwrap();
            position = f.pts;
            send_frame(f, &out);
        } else {
            let _ = out.send(Msg::Position(now));
        }
    }
}
