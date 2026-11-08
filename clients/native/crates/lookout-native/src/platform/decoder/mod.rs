//! Native video decoding for macOS (AVFoundation) and Windows (Media
//! Foundation), behind one small interface: the editor's filmstrip and the
//! player's hover previews sample frames from it. (Playback itself lives in
//! [`crate::player`].)

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// One decoded frame: tightly packed BGRA (row stride = `width * 4`).
pub struct DecodedFrame {
    /// Presentation time in seconds from the start of the video.
    pub pts: f64,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub trait FrameDecoder: Send {
    /// Length of the video in seconds (0 if unknown).
    fn duration(&self) -> f64;
    /// Reposition so the next [`next_frame`](Self::next_frame) returns the
    /// first frame whose pts is >= `t` (accurate seek: decode forward from
    /// the preceding keyframe and drop earlier frames).
    fn seek(&mut self, t: f64) -> Result<(), String>;
    /// The next frame in presentation order, or `None` at end of stream.
    fn next_frame(&mut self) -> Result<Option<DecodedFrame>, String>;
}

/// Open a local video file. `max_height` asks the decoder to scale frames
/// down (filmstrip tiles); `None` keeps the native size.
#[allow(unused_variables)]
pub fn open(path: &std::path::Path, max_height: Option<u32>) -> Result<Box<dyn FrameDecoder>, String> {
    #[cfg(target_os = "macos")]
    {
        return macos::open(path, max_height);
    }
    #[cfg(target_os = "windows")]
    {
        return windows::open(path, max_height);
    }
    #[allow(unreachable_code)]
    Err("no native video decoder on this platform".into())
}

/// Videos arrive as URLs (presigned R2 links, or `file://` in development).
/// Native decoders read local files, so remote ones are downloaded once into
/// the cache dir, keyed by the URL path (presigned query strings rotate).
pub fn local_path(url: &str) -> Result<std::path::PathBuf, String> {
    if let Some(p) = url.strip_prefix("file://") {
        return Ok(std::path::PathBuf::from(p));
    }
    let parsed = url::Url::parse(url).map_err(|e| e.to_string())?;
    use sha2::Digest;
    let key = format!("{:x}", sha2::Sha256::digest(format!("{}{}", parsed.host_str().unwrap_or(""), parsed.path()).as_bytes()));
    let ext = std::path::Path::new(parsed.path()).extension().and_then(|e| e.to_str()).unwrap_or("mp4").to_string();
    let dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join(crate::store::APP_ID).join("videos");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.{ext}", &key[..24]));
    if path.exists() {
        return Ok(path);
    }
    let bytes = reqwest::blocking_get(url)?;
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

mod reqwest {
    /// Download on the calling (background) thread with its own runtime.
    pub fn blocking_get(url: &str) -> Result<Vec<u8>, String> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
        rt.block_on(async {
            let res = ::reqwest::get(url).await.map_err(|e| e.to_string())?;
            if !res.status().is_success() {
                return Err(format!("HTTP {}", res.status()));
            }
            res.bytes().await.map(|b| b.to_vec()).map_err(|e| e.to_string())
        })
    }
}
