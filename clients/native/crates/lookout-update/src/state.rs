//! The updater's own state file (`<updates dir>/state.json`), separate from
//! the app's `native-state.json` because it is read and written before the
//! UI exists (at launch, to apply or roll back) and after it is gone (on
//! quit). Plus the pure launch-time rules, so they are unit-testable.
//!
//! Directory layout:
//! ```text
//! updates/
//!   state.json
//!   download/   <file>.part while downloading, then the verified file
//!   staged/     payload (+ Lookout.app on macOS) of the version to install
//!   base/       payload of the installed version: the base for the next patch
//!   previous/   Windows: copy of the previous install dir; AppImage: previous file
//! ```
//! (macOS keeps the previous bundle beside the app, in `.lookout-previous/`,
//! so restoring it is a rename on the same volume.)

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::manifest::Format;
use crate::version;

/// Launches a new version gets to become healthy before it is rolled back.
pub const MAX_UNHEALTHY_LAUNCHES: u32 = 2;
/// Failed automatic installs of one staged version before we stop trying
/// on our own and ask instead.
pub const MAX_APPLY_FAILURES: u32 = 2;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Random, made once; only ever hashed (rollout bucket).
    pub install_id: String,
    pub beta: bool,
    pub staged: Option<Staged>,
    pub trial: Option<Trial>,
    /// Versions this install rolled back from.
    pub rolled_back: Vec<String>,
    /// Notes to show once the new version is up.
    pub whats_new: Option<WhatsNew>,
}

/// A verified update in `staged/`, ready to swap in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Staged {
    pub version: String,
    pub format: Format,
    pub payload_sha256: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub forced: bool,
    /// Automatic installs (at quit / launch) that failed.
    #[serde(default)]
    pub failures: u32,
    /// The user was told it won't install.
    #[serde(default)]
    pub notified: bool,
}

/// A freshly installed version on probation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trial {
    pub version: String,
    pub from: String,
    /// Launches of `version` so far that never became healthy.
    pub launches: u32,
    /// Where the previous version was kept (bundle / dir / file), if anywhere.
    pub previous: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WhatsNew {
    pub version: String,
    pub notes: String,
}

#[derive(Clone, Debug)]
pub struct Dirs {
    pub root: PathBuf,
}

impl Dirs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn state_file(&self) -> PathBuf {
        self.root.join("state.json")
    }
    pub fn download(&self) -> PathBuf {
        self.root.join("download")
    }
    pub fn staged(&self) -> PathBuf {
        self.root.join("staged")
    }
    pub fn staged_payload(&self) -> PathBuf {
        self.staged().join("payload")
    }
    pub fn base(&self) -> PathBuf {
        self.root.join("base")
    }
    pub fn base_payload(&self, version: &str) -> PathBuf {
        self.base().join(format!("{}.payload", version.trim_start_matches('v')))
    }
    pub fn previous(&self) -> PathBuf {
        self.root.join("previous")
    }
}

impl State {
    pub fn load(dirs: &Dirs) -> State {
        let mut s: State = std::fs::read(dirs.state_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if s.install_id.is_empty() {
            s.install_id = new_install_id();
            let _ = s.save(dirs);
        }
        s
    }

    /// Atomic (temp file + rename).
    pub fn save(&self, dirs: &Dirs) -> std::io::Result<()> {
        std::fs::create_dir_all(&dirs.root)?;
        let raw = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let tmp = dirs.root.join("state.json.tmp");
        std::fs::write(&tmp, raw)?;
        std::fs::rename(&tmp, dirs.state_file())
    }

    /// The version passed its health check: the trial ends.
    pub fn mark_healthy(&mut self, running: &str) -> bool {
        if self.trial.as_ref().is_some_and(|t| version::eq(&t.version, running)) {
            self.trial = None;
            return true;
        }
        false
    }

    /// After restoring `trial.from`: never offer `trial.version` again
    /// (unless forced) and forget anything staged for it.
    pub fn record_rollback(&mut self, trial: &Trial) {
        if !self.rolled_back.iter().any(|v| version::eq(v, &trial.version)) {
            self.rolled_back.push(trial.version.clone());
        }
        self.trial = None;
        self.whats_new = None;
        if self.staged.as_ref().is_some_and(|s| version::cmp(&s.version, &trial.version) != std::cmp::Ordering::Greater) {
            self.staged = None;
        }
    }

    /// Records a successful install of `staged` from `running`.
    pub fn record_install(&mut self, staged: &Staged, running: &str, previous: Option<PathBuf>) {
        self.trial = Some(Trial { version: staged.version.clone(), from: running.to_string(), launches: 0, previous });
        self.whats_new = (!staged.notes.trim().is_empty()).then(|| WhatsNew { version: staged.version.clone(), notes: staged.notes.clone() });
        self.staged = None;
    }
}

fn new_install_id() -> String {
    use sha2::Digest;
    use std::hash::{BuildHasher, Hasher};
    // No RNG dependency: std's RandomState keys are random per process.
    let mut seed = Vec::new();
    for _ in 0..4 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
        h.write_u32(std::process::id());
        seed.extend_from_slice(&h.finish().to_le_bytes());
    }
    hex::encode(&sha2::Sha256::digest(&seed)[..16])
}

#[derive(Clone, Debug, PartialEq)]
pub enum LaunchAction {
    Nothing,
    /// The running version never became healthy: restore `trial.from`.
    Rollback(Trial),
    /// A verified newer version is staged: install it before any window opens.
    Apply(Staged),
}

/// Launch-time bookkeeping, before any window. Mutates `state` (the caller
/// saves it) and says what to do.
pub fn on_launch(state: &mut State, running: &str) -> LaunchAction {
    if let Some(t) = state.trial.as_mut() {
        if version::eq(&t.version, running) {
            if t.launches >= MAX_UNHEALTHY_LAUNCHES {
                return LaunchAction::Rollback(t.clone());
            }
            t.launches += 1;
        } else {
            // The install never took (still on `from`), or someone installed
            // another build by hand. Either way there's nothing to judge.
            state.trial = None;
        }
    }
    if let Some(w) = &state.whats_new {
        if version::cmp(&w.version, running) == std::cmp::Ordering::Less {
            state.whats_new = None;
        }
    }
    match &state.staged {
        Some(s) if version::cmp(&s.version, running) != std::cmp::Ordering::Greater && !s.forced => {
            // Already on it (or past it): the staged files are stale.
            state.staged = None;
            LaunchAction::Nothing
        }
        Some(s) if version::eq(&s.version, running) => {
            state.staged = None;
            LaunchAction::Nothing
        }
        Some(s) if s.failures < MAX_APPLY_FAILURES => LaunchAction::Apply(s.clone()),
        _ => LaunchAction::Nothing,
    }
}

/// Removes everything in `dir` except the entries `keep` returns true for.
pub fn prune(dir: &Path, keep: impl Fn(&Path) -> bool) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if keep(&p) {
            continue;
        }
        let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staged(v: &str) -> Staged {
        Staged { version: v.into(), format: Format::Appimage, payload_sha256: "x".into(), notes: "New stuff".into(), forced: false, failures: 0, notified: false }
    }

    #[test]
    fn install_id_persists() {
        let d = tempfile::tempdir().unwrap();
        let dirs = Dirs::new(d.path());
        let a = State::load(&dirs);
        assert_eq!(a.install_id.len(), 32);
        assert_eq!(State::load(&dirs).install_id, a.install_id);
    }

    #[test]
    fn rollback_after_two_unhealthy_launches() {
        let mut s = State::default();
        s.record_install(&staged("0.5.0"), "0.4.0", Some("/prev".into()));
        assert_eq!(s.whats_new.as_ref().unwrap().version, "0.5.0");
        assert_eq!(on_launch(&mut s, "0.5.0"), LaunchAction::Nothing); // 1st launch
        assert_eq!(on_launch(&mut s, "0.5.0"), LaunchAction::Nothing); // 2nd launch
        let LaunchAction::Rollback(t) = on_launch(&mut s, "0.5.0") else { panic!() }; // 3rd: restore
        assert_eq!(t.from, "0.4.0");
        s.record_rollback(&t);
        assert_eq!(s.rolled_back, vec!["0.5.0".to_string()]);
        assert!(s.trial.is_none() && s.whats_new.is_none());
        // Back on 0.4.0: nothing more to do.
        assert_eq!(on_launch(&mut s, "0.4.0"), LaunchAction::Nothing);
    }

    #[test]
    fn healthy_ends_trial() {
        let mut s = State::default();
        s.record_install(&staged("0.5.0"), "0.4.0", None);
        on_launch(&mut s, "0.5.0");
        assert!(s.mark_healthy("0.5.0"));
        for _ in 0..5 {
            assert_eq!(on_launch(&mut s, "0.5.0"), LaunchAction::Nothing);
        }
    }

    #[test]
    fn install_that_never_took_clears_trial() {
        let mut s = State::default();
        s.record_install(&staged("0.5.0"), "0.4.0", None);
        assert_eq!(on_launch(&mut s, "0.4.0"), LaunchAction::Nothing);
        assert!(s.trial.is_none());
    }

    #[test]
    fn staged_applies_until_it_fails_twice() {
        let mut s = State { staged: Some(staged("0.5.0")), ..Default::default() };
        assert!(matches!(on_launch(&mut s, "0.4.0"), LaunchAction::Apply(_)));
        s.staged.as_mut().unwrap().failures = MAX_APPLY_FAILURES;
        assert_eq!(on_launch(&mut s, "0.4.0"), LaunchAction::Nothing);
        assert!(s.staged.is_some(), "kept for the pill's click");
    }

    #[test]
    fn stale_staged_dropped() {
        let mut s = State { staged: Some(staged("0.4.0")), ..Default::default() };
        assert_eq!(on_launch(&mut s, "0.4.0"), LaunchAction::Nothing);
        assert!(s.staged.is_none());
        // A forced (blocked → older) staged version still applies.
        let mut st = staged("0.3.9");
        st.forced = true;
        let mut s = State { staged: Some(st), ..Default::default() };
        assert!(matches!(on_launch(&mut s, "0.4.0"), LaunchAction::Apply(_)));
    }
}
