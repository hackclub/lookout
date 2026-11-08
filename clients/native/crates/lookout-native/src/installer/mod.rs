//! `lookout.exe` is its own installer on Windows: no NSIS, one signed file.
//!
//! | how it's started | what happens |
//! |---|---|
//! | double-clicked from anywhere but the install folder | the installer window |
//! | `/S [/UPDATE] [/R] [/D=<dir>]` (the updater; the NSIS flags) | a silent install, then start Lookout with `/R` |
//! | `--uninstall [/S]` ("Installed apps" → Uninstall) | the uninstall window, or a silent uninstall with `/S` |
//! | from the install folder | the app |
//!
//! The updater still downloads the release's `…-setup.exe` and runs it with
//! `/S /UPDATE /R`, so update.json, signing and rollback are unchanged.
//!
//! Debug builds on any OS: `LOOKOUT_INSTALLER=demo` (or `demo-uninstall`)
//! opens the window with simulated steps that touch nothing.
//! `LOOKOUT_INSTALLER_DEMO_FAIL=1` makes it fail halfway.

pub mod ops;
mod window;

use std::path::PathBuf;

use ops::Ops;

#[derive(Clone, Debug)]
pub enum Mode {
    /// Run Lookout itself.
    App,
    Install { ops: Ops },
    Uninstall { ops: Ops },
    Silent { relaunch: bool, dir: Option<PathBuf> },
    SilentUninstall,
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a.eq_ignore_ascii_case(flag))
}

pub fn mode() -> Mode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    mode_for(&args)
}

fn mode_for(args: &[String]) -> Mode {
    #[cfg(debug_assertions)]
    match std::env::var("LOOKOUT_INSTALLER").as_deref() {
        Ok("demo") => return Mode::Install { ops: Ops::Demo },
        Ok("demo-uninstall") => return Mode::Uninstall { ops: Ops::Demo },
        Ok("1") if cfg!(windows) => return Mode::Install { ops: Ops::Real },
        _ => {}
    }
    if has_flag(args, "--uninstall") {
        return if has_flag(args, "/S") { Mode::SilentUninstall } else { Mode::Uninstall { ops: Ops::Real } };
    }
    if has_flag(args, "/S") {
        // NSIS: /D= is the rest of the command line and comes last.
        let dir = args.iter().position(|a| a.len() >= 3 && a[..3].eq_ignore_ascii_case("/D=")).map(|i| {
            let mut d = args[i][3..].to_string();
            for rest in &args[i + 1..] {
                d.push(' ');
                d.push_str(rest);
            }
            PathBuf::from(d)
        });
        return Mode::Silent { relaunch: has_flag(args, "/R"), dir };
    }
    if cfg!(all(windows, not(debug_assertions))) && !runs_from_install(args) {
        return Mode::Install { ops: Ops::Real };
    }
    Mode::App
}

/// Whether this exe is the installed copy (or a launch the installer must
/// stay out of: deep links, the crash reporter).
fn runs_from_install(args: &[String]) -> bool {
    if args.iter().any(|a| a.starts_with("lookout://")) || crate::crash::is_reporter_process() {
        return true;
    }
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from)) else { return true };
    let installed = Ops::Real.existing().map(|e| e.dir).unwrap_or_else(|| Ops::Real.default_dir());
    ops::same_dir(&dir, &installed)
}

/// Runs the installer or uninstaller for `mode` (never [`Mode::App`]) and
/// returns the process exit code.
pub fn run(mode: Mode) -> i32 {
    match mode {
        Mode::App => 0,
        Mode::Silent { relaunch, dir } => {
            let ops = Ops::Real;
            let dir = dir.or_else(|| ops.existing().map(|e| e.dir)).unwrap_or_else(|| ops.default_dir());
            log::info!("[installer] silent install into {}", dir.display());
            match ops.install(&dir, &|_| {}) {
                Ok(exe) => {
                    if relaunch {
                        ops.launch(&exe);
                    }
                    0
                }
                Err(e) => {
                    log::error!("[installer] silent install failed: {e}");
                    1
                }
            }
        }
        Mode::SilentUninstall => match Ops::Real.uninstall(&|_| {}) {
            Ok(()) => 0,
            Err(e) => {
                log::error!("[installer] silent uninstall failed: {e}");
                1
            }
        },
        Mode::Install { ops } => {
            window::run(window::Flow::Install, ops);
            0
        }
        Mode::Uninstall { ops } => {
            window::run(window::Flow::Uninstall, ops);
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn updater_flags_mean_a_silent_install() {
        match mode_for(&args(&["/S", "/UPDATE", "/R"])) {
            Mode::Silent { relaunch: true, dir: None } => {}
            m => panic!("{m:?}"),
        }
    }

    #[test]
    fn nsis_dir_takes_the_rest_of_the_line() {
        match mode_for(&args(&["/s", "/D=C:\\Program", "Files\\Lookout"])) {
            Mode::Silent { relaunch: false, dir: Some(d) } => assert_eq!(d, PathBuf::from("C:\\Program Files\\Lookout")),
            m => panic!("{m:?}"),
        }
    }

    #[test]
    fn uninstall_flags() {
        assert!(matches!(mode_for(&args(&["--uninstall", "/S"])), Mode::SilentUninstall));
        assert!(matches!(mode_for(&args(&["--uninstall"])), Mode::Uninstall { .. }));
    }
}
