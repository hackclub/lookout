//! What a recording captures, as data: screens, windows and PipeWire
//! streams by id, cameras by the host's device id. Hosts capture; this
//! crate only names.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CaptureSource {
    #[serde(rename = "monitor")]
    Monitor { id: u32 },
    #[serde(rename = "window")]
    Window { id: u32 },
    #[serde(rename = "pipewire")]
    PipeWire { id: u32 },
}


/// A source as the picker hands it over: screens and windows by numeric
/// id, cameras by the shell's device id. The TypeScript `CaptureSource`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PickedSource {
    Monitor { id: u32 },
    Window { id: u32 },
    Pipewire { id: u32 },
    Camera { id: String },
}

/// What a recording captures.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RecordingSource {
    /// Any mix of monitors, windows and PipeWire streams.
    Screen { sources: Vec<CaptureSource> },
    /// One camera, captured by the shell.
    #[serde(rename_all = "camelCase")]
    Camera { device_id: String },
}

impl RecordingSource {
    /// A camera records alone; screens and windows combine. Mixing them is
    /// refused rather than quietly dropping one.
    pub fn from_picked(picked: Vec<PickedSource>) -> Result<Self, String> {
        if picked.is_empty() {
            return Err("no capture source selected".into());
        }
        let cameras: Vec<&PickedSource> = picked
            .iter()
            .filter(|p| matches!(p, PickedSource::Camera { .. }))
            .collect();
        if !cameras.is_empty() {
            if picked.len() != 1 {
                return Err("a camera can't be combined with other sources".into());
            }
            if let PickedSource::Camera { id } = &picked[0] {
                return Ok(RecordingSource::Camera {
                    device_id: id.clone(),
                });
            }
        }
        let sources = picked
            .into_iter()
            .map(|p| match p {
                PickedSource::Monitor { id } => CaptureSource::Monitor { id },
                PickedSource::Window { id } => CaptureSource::Window { id },
                PickedSource::Pipewire { id } => CaptureSource::PipeWire { id },
                PickedSource::Camera { .. } => unreachable!("handled above"),
            })
            .collect();
        Ok(RecordingSource::Screen { sources })
    }

    pub fn is_camera(&self) -> bool {
        matches!(self, RecordingSource::Camera { .. })
    }

    pub fn camera_device(&self) -> Option<&str> {
        match self {
            RecordingSource::Camera { device_id } => Some(device_id),
            RecordingSource::Screen { .. } => None,
        }
    }

    /// Whether any of these PipeWire nodes is one we're recording from.
    pub fn uses_pipewire_node(&self, node_ids: &[u32]) -> bool {
        match self {
            RecordingSource::Screen { sources } => sources.iter().any(|s| match s {
                CaptureSource::PipeWire { id } => node_ids.contains(id),
                _ => false,
            }),
            RecordingSource::Camera { .. } => false,
        }
    }
}
