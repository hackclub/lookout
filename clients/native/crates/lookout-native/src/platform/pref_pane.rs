//! macOS: Lookout's pane in System Settings.
//!
//! `Lookout.app/Contents/Library/PreferencePanes/Lookout.prefPane` (built by
//! `packaging/macos/prefpane.sh` from `swift/lookout-prefpane`, signed and
//! notarized with the app) is copied to `~/Library/PreferencePanes` at
//! launch, which is all System Settings needs to list it: nobody has to
//! double-click an installer. A newer app replaces an older copy.
//!
//! Someone who removes the pane keeps it removed: a marker in the config dir
//! records that it was installed once, and a missing pane after that is a
//! choice, not something to repair.
//!
//! The pane is Lookout's Settings screen; it changes settings through
//! `lookout --settings-pane` (`settings_pane.rs`).

#[cfg(target_os = "macos")]
pub fn install_in_background() {
    std::thread::Builder::new()
        .name("pref-pane".into())
        .spawn(|| match imp::install() {
            Ok(imp::Outcome::Installed) => log::info!("[pref-pane] installed in ~/Library/PreferencePanes"),
            Ok(imp::Outcome::Updated) => log::info!("[pref-pane] updated"),
            Ok(_) => {}
            Err(e) => log::warn!("[pref-pane] could not install: {e}"),
        })
        .ok();
}

#[cfg(not(target_os = "macos"))]
pub fn install_in_background() {}

#[cfg(any(target_os = "macos", test))]
mod imp {
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};

    pub const NAME: &str = "Lookout.prefPane";
    const MARKER: &str = "pref-pane-installed";

    #[derive(Debug, PartialEq, Eq)]
    pub enum Outcome {
        /// Not running from an app bundle that carries the pane (`cargo run`).
        NoSource,
        /// It was installed before and the user has since removed it.
        RemovedByUser,
        UpToDate,
        Installed,
        Updated,
    }

    #[cfg(target_os = "macos")]
    pub fn install() -> io::Result<Outcome> {
        let exe = std::env::current_exe()?;
        // Contents/MacOS/lookout → Contents/Library/PreferencePanes/Lookout.prefPane
        let Some(contents) = exe.parent().and_then(Path::parent) else { return Ok(Outcome::NoSource) };
        let src = contents.join("Library/PreferencePanes").join(NAME);
        let Some(home) = dirs::home_dir() else { return Ok(Outcome::NoSource) };
        let dest_dir = home.join("Library/PreferencePanes");
        let marker = crate::store::config_dir().join(MARKER);
        install_from(&src, &dest_dir, &marker)
    }

    pub fn install_from(src: &Path, dest_dir: &Path, marker: &Path) -> io::Result<Outcome> {
        if !src.join("Contents/Info.plist").is_file() {
            return Ok(Outcome::NoSource);
        }
        let dest = dest_dir.join(NAME);
        let present = dest.exists();
        if !present && marker.exists() {
            return Ok(Outcome::RemovedByUser);
        }
        if present && same_build(src, &dest) {
            mark(marker)?;
            return Ok(Outcome::UpToDate);
        }
        // Copy beside it, then swap, so System Settings never sees half a bundle.
        fs::create_dir_all(dest_dir)?;
        let tmp = dest_dir.join(format!(".{NAME}.{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        if let Err(e) = copy_tree(src, &tmp) {
            let _ = fs::remove_dir_all(&tmp);
            return Err(e);
        }
        if present {
            fs::remove_dir_all(&dest)?;
        }
        fs::rename(&tmp, &dest)?;
        mark(marker)?;
        Ok(if present { Outcome::Updated } else { Outcome::Installed })
    }

    fn mark(marker: &Path) -> io::Result<()> {
        if marker.exists() {
            return Ok(());
        }
        if let Some(dir) = marker.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(marker, b"")
    }

    /// The same Info.plist (version) and the same signature, so a rebuilt
    /// pane at an unchanged version still replaces the old one.
    fn same_build(a: &Path, b: &Path) -> bool {
        ["Contents/Info.plist", "Contents/_CodeSignature/CodeResources"].iter().all(|f| {
            match (fs::read(a.join(f)), fs::read(b.join(f))) {
                (Ok(x), Ok(y)) => x == y,
                (Err(_), Err(_)) => true,
                _ => false,
            }
        })
    }

    fn copy_tree(src: &Path, dest: &Path) -> io::Result<()> {
        fs::create_dir(dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let from = entry.path();
            let to: PathBuf = dest.join(entry.file_name());
            let ty = entry.file_type()?;
            if ty.is_symlink() {
                #[cfg(unix)]
                std::os::unix::fs::symlink(fs::read_link(&from)?, &to)?;
            } else if ty.is_dir() {
                copy_tree(&from, &to)?;
            } else {
                fs::copy(&from, &to)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::imp::{NAME, Outcome, install_from};
    use std::fs;

    fn pane(dir: &std::path::Path, version: &str) {
        fs::create_dir_all(dir.join("Contents/MacOS")).unwrap();
        fs::write(dir.join("Contents/Info.plist"), version).unwrap();
        fs::write(dir.join("Contents/MacOS/Lookout"), b"bin").unwrap();
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink("MacOS/Lookout", dir.join("Contents/link"));
    }

    #[test]
    fn installs_updates_and_respects_removal() {
        let root = std::env::temp_dir().join(format!("lookout-pref-pane-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (src, dest_dir, marker) = (root.join("app").join(NAME), root.join("PreferencePanes"), root.join("cfg/marker"));

        assert_eq!(install_from(&src, &dest_dir, &marker).unwrap(), Outcome::NoSource);
        pane(&src, "1");
        assert_eq!(install_from(&src, &dest_dir, &marker).unwrap(), Outcome::Installed);
        assert_eq!(fs::read(dest_dir.join(NAME).join("Contents/MacOS/Lookout")).unwrap(), b"bin");
        assert_eq!(install_from(&src, &dest_dir, &marker).unwrap(), Outcome::UpToDate);

        fs::write(src.join("Contents/Info.plist"), "2").unwrap();
        assert_eq!(install_from(&src, &dest_dir, &marker).unwrap(), Outcome::Updated);
        assert_eq!(fs::read_to_string(dest_dir.join(NAME).join("Contents/Info.plist")).unwrap(), "2");
        // No temp copies left behind.
        assert_eq!(fs::read_dir(&dest_dir).unwrap().count(), 1);

        fs::remove_dir_all(dest_dir.join(NAME)).unwrap();
        assert_eq!(install_from(&src, &dest_dir, &marker).unwrap(), Outcome::RemovedByUser);
        assert!(!dest_dir.join(NAME).exists());
        fs::remove_dir_all(&root).unwrap();
    }
}
