//! The session a recording talks about: where the server is and what it
//! advertised.

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    pub token: String,
    pub api_base_url: String,
}

/// Clip capability the server advertises for a session (on the session
/// GET). Fetched once at capture-loop start unless the caller already has
/// it; any failure means clips off, i.e. legacy one-JPEG-per-minute
/// behavior.
#[derive(Clone, Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionClipCapabilities {
    #[serde(default)]
    pub clips_enabled: bool,
    #[serde(default)]
    pub frame_interval_ms: Option<u64>,
}

