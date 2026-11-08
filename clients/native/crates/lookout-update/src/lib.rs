//! Update v2 for Lookout Native — everything that doesn't need a window.
//!
//! - [`manifest`]: the signed `update.json` (schema, channel, rollout,
//!   blocked versions, per-platform artifacts and patches) and [`decide`],
//!   the pure "should this install update, and how urgently" rule.
//! - [`sig`]: minisign verification against the release key.
//! - [`patch`]: `zstd --patch-from`-style deltas (a raw-content prefix +
//!   long-distance matching), both directions.
//! - [`download`]: resumable HTTP downloads into a `.part` file.
//! - [`state`]: the updater's on-disk state — install id, channel, staged
//!   download, the post-install health trial and rollback bookkeeping.
//!
//! The app (`lookout-native/src/updater`) wires these to GPUI and the
//! platform installers; `lookout-update-tool` uses the same types to build
//! and sign releases.

pub mod hash;
pub mod manifest;
pub mod patch;
pub mod rollout;
pub mod sig;
pub mod state;
pub mod version;

#[cfg(feature = "net")]
pub mod download;
#[cfg(feature = "net")]
pub mod pipeline;

pub use manifest::{Artifact, Channel, Decision, Format, Manifest, Patch, decide};
