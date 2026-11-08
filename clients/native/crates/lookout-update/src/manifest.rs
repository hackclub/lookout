//! `update.json` — the signed release manifest — and the rule that turns it
//! into a decision for one install.
//!
//! ```json
//! {
//!   "schema": 1,
//!   "version": "0.4.1",
//!   "pub_date": "2026-11-07T12:00:00Z",
//!   "notes": "Faster uploads.",
//!   "channel": "stable",
//!   "min_supported": "0.3.0",
//!   "rollout": 0.25,
//!   "blocked": ["0.4.0"],
//!   "platforms": {
//!     "native-darwin-aarch64": {
//!       "url": "https://…/Lookout-native_0.4.1_darwin-aarch64.app.tar.gz",
//!       "size": 41234567, "sha256": "…",
//!       "format": "app-tar-gz", "payload_size": 98765432, "payload_sha256": "…",
//!       "patches": [{ "from_version": "0.4.0", "from_sha256": "…",
//!                     "url": "…/Lookout-native_0.4.0-to-0.4.1_darwin-aarch64.zst",
//!                     "size": 812345, "sha256": "…" }]
//!     }
//!   }
//! }
//! ```
//!
//! The *payload* is what gets installed and what patches are computed
//! over: the `.tar` inside an `.app.tar.gz` (patching gzip output would
//! save nothing), the installer `.exe` and the `.AppImage` as they are.
//! The manifest's signature covers every hash, so artifacts carry none.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{rollout, sig, version};

/// The newest schema this client understands. A manifest with a higher
/// schema is ignored (the release tool bumps it only for breaking changes).
pub const SCHEMA: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Beta,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Format {
    /// macOS: gzipped tar of `Lookout.app/…`; payload = the tar.
    AppTarGz,
    /// Windows: the NSIS setup `.exe`; payload = the file.
    Nsis,
    /// Linux: a self-contained AppImage; payload = the file.
    Appimage,
}

impl Format {
    /// Whether the downloaded file is the payload itself.
    pub fn payload_is_file(self) -> bool {
        !matches!(self, Format::AppTarGz)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    pub from_version: String,
    /// SHA-256 of the *payload* the patch applies to, so a client checks
    /// its base before spending the download.
    pub from_sha256: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub url: String,
    pub size: u64,
    pub sha256: String,
    pub format: Format,
    /// Payload size/hash; equal to `size`/`sha256` when the file is the payload.
    pub payload_size: u64,
    pub payload_sha256: String,
    #[serde(default)]
    pub patches: Vec<Patch>,
}

impl Artifact {
    pub fn patch_from(&self, version: &str) -> Option<&Patch> {
        self.patches.iter().find(|p| crate::version::eq(&p.from_version, version))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub version: String,
    #[serde(default)]
    pub pub_date: String,
    #[serde(default)]
    pub notes: String,
    pub channel: Channel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_supported: Option<String>,
    #[serde(default = "one")]
    pub rollout: f64,
    #[serde(default)]
    pub blocked: Vec<String>,
    pub platforms: BTreeMap<String, Artifact>,
}

fn one() -> f64 {
    1.0
}

#[derive(Debug, PartialEq)]
pub enum Error {
    Signature(String),
    Parse(String),
    /// A newer schema than this client knows.
    Schema(u32),
    /// The manifest is for another channel (e.g. a beta manifest at the stable URL).
    WrongChannel { expected: Channel, got: Channel },
    /// The manifest is older than this install, which isn't blocked:
    /// a replayed or rolled-back manifest. Refused.
    Downgrade { installed: String, offered: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Signature(e) => write!(f, "manifest signature: {e}"),
            Error::Parse(e) => write!(f, "manifest: {e}"),
            Error::Schema(s) => write!(f, "manifest schema {s} is newer than this app understands ({SCHEMA})"),
            Error::WrongChannel { expected, got } => {
                write!(f, "manifest is for the {} channel, expected {}", got.as_str(), expected.as_str())
            }
            Error::Downgrade { installed, offered } => write!(f, "refusing to downgrade v{installed} to v{offered}"),
        }
    }
}

impl std::error::Error for Error {}

impl Manifest {
    /// Verify `raw` against `signature` (the `.minisig` text) *before*
    /// looking at a single field, then parse it.
    pub fn verify_and_parse(raw: &[u8], signature: &str, key: &sig::PublicKey) -> Result<Manifest, Error> {
        sig::verify(raw, signature, key).map_err(Error::Signature)?;
        let m: Manifest = serde_json::from_slice(raw).map_err(|e| Error::Parse(e.to_string()))?;
        if m.schema > SCHEMA {
            return Err(Error::Schema(m.schema));
        }
        Ok(m)
    }

    pub fn is_blocked(&self, v: &str) -> bool {
        self.blocked.iter().any(|b| version::eq(b, v))
    }
}

/// What this install knows about itself.
#[derive(Clone, Debug)]
pub struct Install<'a> {
    pub version: &'a str,
    pub channel: Channel,
    pub install_id: &'a str,
    pub platform_key: &'a str,
    /// Versions this install rolled back from; not offered again unless forced.
    pub rolled_back: &'a [String],
}

#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    UpToDate,
    /// Newer, but this install's rollout bucket isn't in yet.
    NotYet { bucket: f64, rollout: f64 },
    /// Newer, but no build for this platform.
    NoBuild,
    /// Newer, but this install rolled back from it.
    Skipped,
    /// Install `artifact`. `forced`: the running version is blocked or
    /// below `min_supported` — install as soon as it is safe, ignoring the
    /// rollout and the wait for quit.
    Update { version: String, artifact: Artifact, forced: bool },
}

pub fn decide(m: &Manifest, me: &Install) -> Result<Decision, Error> {
    if m.schema > SCHEMA {
        return Err(Error::Schema(m.schema));
    }
    if m.channel != me.channel {
        return Err(Error::WrongChannel { expected: me.channel, got: m.channel });
    }
    let installed_blocked = m.is_blocked(me.version);
    let below_min = m.min_supported.as_deref().is_some_and(|min| version::cmp(me.version, min) == Ordering::Less);
    let forced = installed_blocked || below_min;
    match version::cmp(&m.version, me.version) {
        Ordering::Equal => return Ok(Decision::UpToDate),
        // A blocked install may go *back* to the manifest's (good) version.
        Ordering::Less if !installed_blocked => {
            return Err(Error::Downgrade { installed: me.version.to_string(), offered: m.version.clone() });
        }
        _ => {}
    }
    // Never move onto a version the manifest itself blocks.
    if m.is_blocked(&m.version) {
        return Ok(Decision::UpToDate);
    }
    if !forced && me.rolled_back.iter().any(|v| version::eq(v, &m.version)) {
        return Ok(Decision::Skipped);
    }
    if !forced && !rollout::included(me.install_id, m.rollout) {
        return Ok(Decision::NotYet { bucket: rollout::bucket(me.install_id), rollout: m.rollout });
    }
    let Some(artifact) = m.platforms.get(me.platform_key) else {
        return Ok(Decision::NoBuild);
    };
    Ok(Decision::Update { version: m.version.trim_start_matches('v').to_string(), artifact: artifact.clone(), forced })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn artifact() -> Artifact {
        Artifact {
            url: "https://example.com/a.AppImage".into(),
            size: 10,
            sha256: "00".into(),
            format: Format::Appimage,
            payload_size: 10,
            payload_sha256: "00".into(),
            patches: vec![],
        }
    }

    pub fn manifest(v: &str) -> Manifest {
        Manifest {
            schema: 1,
            version: v.into(),
            pub_date: "2026-11-07T00:00:00Z".into(),
            notes: "notes".into(),
            channel: Channel::Stable,
            min_supported: None,
            rollout: 1.0,
            blocked: vec![],
            platforms: [("native-linux-x86_64".to_string(), artifact())].into_iter().collect(),
        }
    }

    fn me(v: &str) -> Install<'_> {
        Install { version: v, channel: Channel::Stable, install_id: "id-1", platform_key: "native-linux-x86_64", rolled_back: &[] }
    }

    fn sign(raw: &[u8]) -> (sig::PublicKey, String) {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk = sig::decode_pubkey(&kp.pk.to_box().unwrap().to_string()).unwrap();
        let sb = minisign::sign(None, &kp.sk, std::io::Cursor::new(raw), Some("t"), None).unwrap();
        (pk, sb.to_string())
    }

    #[test]
    fn verifies_before_parsing() {
        let raw = serde_json::to_vec(&manifest("0.5.0")).unwrap();
        let (pk, s) = sign(&raw);
        assert_eq!(Manifest::verify_and_parse(&raw, &s, &pk).unwrap().version, "0.5.0");

        // One flipped byte anywhere → rejected, even though it still parses.
        let mut tampered = raw.clone();
        let i = tampered.windows(5).position(|w| w == b"0.5.0").unwrap();
        tampered[i + 2] = b'9';
        assert!(matches!(Manifest::verify_and_parse(&tampered, &s, &pk), Err(Error::Signature(_))));

        // Garbage that is validly signed fails as a parse error, not a panic.
        let (pk2, s2) = sign(b"not json");
        assert!(matches!(Manifest::verify_and_parse(b"not json", &s2, &pk2), Err(Error::Parse(_))));

        // Another key's signature.
        let (pk3, _) = sign(b"x");
        assert!(matches!(Manifest::verify_and_parse(&raw, &s, &pk3), Err(Error::Signature(_))));
    }

    #[test]
    fn newer_schema_refused() {
        let mut m = manifest("0.5.0");
        m.schema = 2;
        let raw = serde_json::to_vec(&m).unwrap();
        let (pk, s) = sign(&raw);
        assert_eq!(Manifest::verify_and_parse(&raw, &s, &pk), Err(Error::Schema(2)));
    }

    #[test]
    fn anti_downgrade() {
        let m = manifest("0.4.0");
        assert!(matches!(decide(&m, &me("0.4.1")), Err(Error::Downgrade { .. })));
        assert_eq!(decide(&m, &me("0.4.0")), Ok(Decision::UpToDate));
        // …unless the installed version is blocked: then going back is the fix.
        let mut m = manifest("0.4.0");
        m.blocked = vec!["0.4.1".into()];
        match decide(&m, &me("0.4.1")).unwrap() {
            Decision::Update { version, forced, .. } => {
                assert_eq!(version, "0.4.0");
                assert!(forced);
            }
            d => panic!("{d:?}"),
        }
    }

    #[test]
    fn prerelease_ordering() {
        assert!(matches!(decide(&manifest("0.5.0"), &me("0.5.0-beta.3")).unwrap(), Decision::Update { .. }));
        assert!(matches!(decide(&manifest("0.5.0-beta.3"), &me("0.5.0")), Err(Error::Downgrade { .. })));
    }

    #[test]
    fn channel_must_match() {
        let mut m = manifest("0.5.0");
        m.channel = Channel::Beta;
        assert!(matches!(decide(&m, &me("0.4.0")), Err(Error::WrongChannel { .. })));
    }

    #[test]
    fn rollout_and_forcing() {
        let mut m = manifest("0.5.0");
        m.rollout = 0.0;
        assert!(matches!(decide(&m, &me("0.4.0")).unwrap(), Decision::NotYet { .. }));
        // Blocked installs ignore the rollout…
        m.blocked = vec!["0.4.0".into()];
        assert!(matches!(decide(&m, &me("0.4.0")).unwrap(), Decision::Update { forced: true, .. }));
        // …and so do installs below min_supported.
        m.blocked.clear();
        m.min_supported = Some("0.4.5".into());
        assert!(matches!(decide(&m, &me("0.4.0")).unwrap(), Decision::Update { forced: true, .. }));
        // A normal update isn't forced.
        let m = manifest("0.5.0");
        assert!(matches!(decide(&m, &me("0.4.0")).unwrap(), Decision::Update { forced: false, .. }));
    }

    #[test]
    fn rollout_bucket_threshold() {
        let id = "some-install";
        let b = rollout::bucket(id);
        let mut m = manifest("0.5.0");
        let mut i = me("0.4.0");
        i.install_id = id;
        m.rollout = b + 1e-9;
        assert!(matches!(decide(&m, &i).unwrap(), Decision::Update { .. }));
        m.rollout = b;
        assert!(matches!(decide(&m, &i).unwrap(), Decision::NotYet { .. }));
    }

    #[test]
    fn skips_rolled_back_and_blocked_targets() {
        let m = manifest("0.5.0");
        let rb = vec!["0.5.0".to_string()];
        let mut i = me("0.4.0");
        i.rolled_back = &rb;
        assert_eq!(decide(&m, &i).unwrap(), Decision::Skipped);
        let mut m = manifest("0.5.0");
        m.blocked = vec!["0.5.0".into()];
        assert_eq!(decide(&m, &me("0.4.0")).unwrap(), Decision::UpToDate);
    }

    #[test]
    fn missing_platform() {
        let mut m = manifest("0.5.0");
        m.platforms.clear();
        assert_eq!(decide(&m, &me("0.4.0")).unwrap(), Decision::NoBuild);
    }
}
