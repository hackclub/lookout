//! Per-platform staging, install, rollback and relaunch.
//!
//! | | staged as | install | previous version kept in | rollback |
//! |---|---|---|---|---|
//! | macOS | `staged/Lookout.app` (unpacked tar) | two renames | `<app dir>/.lookout-previous/Lookout.app` | two renames |
//! | Windows | `staged/payload` (NSIS setup) | copy install dir, run setup `/S` | `updates/previous/app/` | robocopy back after exit |
//! | AppImage | `staged/payload` | copy, rename over `$APPIMAGE` | `updates/previous/Lookout.AppImage` | copy back |
//!
//! `interactive: false` (launch / quit) never raises an admin prompt: an
//! install that would need one waits for the pill's click instead.

#![allow(unused_imports, unused_variables)]

use std::path::{Path, PathBuf};

use lookout_update::Format;
use lookout_update::state::{Dirs, Staged, Trial};

pub fn expected_format() -> Option<Format> {
    if cfg!(target_os = "macos") {
        Some(Format::AppTarGz)
    } else if cfg!(target_os = "windows") {
        Some(Format::Nsis)
    } else if cfg!(target_os = "linux") {
        Some(Format::Appimage)
    } else {
        None
    }
}

/// The running AppImage (Linux), if this process is one.
pub fn appimage_path() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file())
}

/// This install's payload of the running version, the base for a patch.
pub fn patch_base(dirs: &Dirs, installed: &str) -> Option<PathBuf> {
    if let Some(p) = appimage_path() {
        return Some(p);
    }
    let p = dirs.base_payload(installed);
    p.is_file().then_some(p)
}

/// Moves a verified payload into `staged/` in its ready-to-swap form.
pub fn stage(dirs: &Dirs, payload: &Path, format: Format) -> Result<(), String> {
    let staged = dirs.staged();
    let _ = std::fs::remove_dir_all(&staged);
    std::fs::create_dir_all(&staged).map_err(|e| e.to_string())?;
    let dest = dirs.staged_payload();
    std::fs::rename(payload, &dest).or_else(|_| std::fs::copy(payload, &dest).map(|_| ())).map_err(|e| e.to_string())?;
    match format {
        Format::AppTarGz => unpack_app(&dest, &staged.join("Lookout.app")),
        Format::Appimage => {
            set_exec(&dest);
            Ok(())
        }
        Format::Nsis => Ok(()),
    }
}

/// Unpacks a tar of `Lookout.app/…` into `dest` (dropping the first component).
pub fn unpack_app(tar_path: &Path, dest: &Path) -> Result<(), String> {
    let f = std::fs::File::open(tar_path).map_err(|e| e.to_string())?;
    let mut archive = tar::Archive::new(f);
    archive.set_preserve_permissions(true);
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?;
        let rel: PathBuf = path.iter().skip(1).collect();
        if rel.as_os_str().is_empty() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            continue;
        }
        let out = dest.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        entry.unpack(&out).map_err(|e| format!("unpack {}: {e}", rel.display()))?;
    }
    Ok(())
}

/// After an install: the staged payload becomes the base for the next patch.
fn keep_base(dirs: &Dirs, staged: &Staged) {
    if appimage_path().is_some() {
        return; // the AppImage itself is the base
    }
    let base = dirs.base_payload(&staged.version);
    let _ = std::fs::create_dir_all(dirs.base());
    if std::fs::rename(dirs.staged_payload(), &base).is_ok() {
        lookout_update::state::prune(&dirs.base(), |p| p == base);
    }
}

fn set_exec(p: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
    }
}

/// Unix: start `cmd` once this process has exited.
#[cfg(unix)]
fn relaunch_after_exit(program: &str, arg: &Path) -> Result<(), String> {
    let pid = std::process::id().to_string();
    std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("while kill -0 \"$1\" 2>/dev/null; do sleep 0.1; done; exec \"$2\" \"$3\"")
        .arg("sh")
        .arg(pid)
        .arg(program)
        .arg(arg)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("couldn't relaunch: {e}"))
}

// ── install ─────────────────────────────────────────────────────────────

/// Installs `staged`. Returns where the previous version was kept.
pub fn install(dirs: &Dirs, staged: &Staged, relaunch: bool, interactive: bool) -> Result<Option<PathBuf>, String> {
    if Some(staged.format) != expected_format() {
        return Err(format!("staged update is a {:?}, not for this platform", staged.format));
    }
    let prev = install_impl(dirs, staged, relaunch, interactive)?;
    keep_base(dirs, staged);
    let _ = std::fs::remove_dir_all(dirs.staged());
    Ok(prev)
}

#[cfg(target_os = "linux")]
fn install_impl(dirs: &Dirs, staged: &Staged, relaunch: bool, _interactive: bool) -> Result<Option<PathBuf>, String> {
    let target = appimage_path().ok_or("not running from an AppImage")?;
    let prev = dirs.previous().join("Lookout.AppImage");
    std::fs::create_dir_all(dirs.previous()).map_err(|e| e.to_string())?;
    std::fs::copy(&target, &prev).map_err(|e| format!("couldn't keep the previous version: {e}"))?;
    replace_file(&dirs.staged_payload(), &target)?;
    if relaunch {
        relaunch_after_exit(&target.to_string_lossy(), Path::new(""))?;
    }
    Ok(Some(prev))
}

/// Copy `src` beside `target`, then rename over it (atomic on one volume;
/// a running AppImage keeps its old inode until it exits).
#[cfg(target_os = "linux")]
fn replace_file(src: &Path, target: &Path) -> Result<(), String> {
    let name = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Lookout.AppImage".into());
    let tmp = target.with_file_name(format!(".{name}.new"));
    let _ = std::fs::remove_file(&tmp);
    std::fs::copy(src, &tmp).map_err(|e| format!("couldn't write the new version beside {}: {e}", target.display()))?;
    set_exec(&tmp);
    std::fs::rename(&tmp, target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("couldn't replace {}: {e}", target.display())
    })
}

#[cfg(target_os = "macos")]
fn bundle_path() -> Result<PathBuf, String> {
    // …/Lookout.app/Contents/MacOS/lookout → …/Lookout.app
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    exe.parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .filter(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("not running from an app bundle ({})", exe.display()))
}

#[cfg(target_os = "macos")]
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".lookout-write-test-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

#[cfg(target_os = "macos")]
fn shq(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', r"'\''"))
}

#[cfg(target_os = "macos")]
fn run_as_admin(cmd: &str) -> Result<(), String> {
    let script = format!("do shell script \"{}\" with administrator privileges", cmd.replace('\\', r"\\").replace('"', "\\\""));
    let ok = std::process::Command::new("/usr/bin/osascript").arg("-e").arg(script).status().map(|s| s.success()).unwrap_or(false);
    if ok { Ok(()) } else { Err("the administrator move was cancelled or failed".into()) }
}

/// Refuse a staged bundle that couldn't start, before anything is moved.
/// Rollback only runs when the new version launches, so a bundle that can't
/// launch at all would otherwise leave Lookout broken with nothing to undo
/// it. Checks the executable the bundle declares is there and runnable, and,
/// when the running app is signed, that the new one passes `codesign
/// --verify` and is signed by the same team.
#[cfg(target_os = "macos")]
fn validate_bundle(current: &Path, new_app: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let exe_name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_owned()))
        .ok_or("can't tell which executable is running")?;
    let exe = new_app.join("Contents/MacOS").join(&exe_name);
    let runnable = exe.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if !runnable || !new_app.join("Contents/Info.plist").is_file() {
        return Err("the downloaded update is incomplete".into());
    }
    let team = |app: &Path| -> Option<String> {
        let out = std::process::Command::new("/usr/bin/codesign").arg("-dv").arg(app).output().ok()?;
        // `codesign -dv` reports on stderr.
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .find_map(|l| l.strip_prefix("TeamIdentifier="))
            .map(str::trim)
            .filter(|t| *t != "not set")
            .map(str::to_owned)
    };
    let Some(expected) = team(current) else {
        return Ok(()); // unsigned/ad-hoc dev build: nothing to compare against
    };
    let verified = std::process::Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(new_app)
        .status()
        .is_ok_and(|s| s.success());
    if !verified || team(new_app).as_deref() != Some(expected.as_str()) {
        return Err("the downloaded update isn't signed the same way as this copy of Lookout".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn install_impl(dirs: &Dirs, _staged: &Staged, relaunch: bool, interactive: bool) -> Result<Option<PathBuf>, String> {
    use std::os::unix::fs::MetadataExt;
    let bundle = bundle_path()?;
    let parent = bundle.parent().ok_or("app bundle has no parent folder")?.to_path_buf();
    let mut new_app = dirs.staged().join("Lookout.app");
    if !new_app.is_dir() {
        return Err("staged app is missing".into());
    }
    validate_bundle(&bundle, &new_app)?;
    let prev_dir = parent.join(".lookout-previous");
    let prev = prev_dir.join("Lookout.app");
    if !writable(&parent) {
        if !interactive {
            return Err(format!("{} needs an administrator to update", parent.display()));
        }
        let cmd = format!(
            "rm -rf {pd} && mkdir -p {pd} && /usr/bin/ditto {n} {pd}/.incoming.app && mv -f {b} {p} && {{ mv -f {pd}/.incoming.app {b} || {{ mv -f {p} {b}; exit 1; }}; }}",
            pd = shq(&prev_dir),
            n = shq(&new_app),
            b = shq(&bundle),
            p = shq(&prev)
        );
        run_as_admin(&cmd)?;
    } else {
        // Renames only work within one volume: bring the new app beside the old one first.
        let same_dev = |a: &Path, b: &Path| a.metadata().ok().zip(b.metadata().ok()).is_some_and(|(x, y)| x.dev() == y.dev());
        let _ = std::fs::remove_dir_all(&prev_dir);
        std::fs::create_dir_all(&prev_dir).map_err(|e| e.to_string())?;
        if !same_dev(&new_app, &parent) {
            let incoming = prev_dir.join(".incoming.app");
            let ok = std::process::Command::new("/usr/bin/ditto").arg(&new_app).arg(&incoming).status().map(|s| s.success()).unwrap_or(false);
            if !ok {
                return Err("couldn't copy the new app beside the old one".into());
            }
            new_app = incoming;
        }
        std::fs::rename(&bundle, &prev).map_err(|e| format!("couldn't move the old app aside: {e}"))?;
        if let Err(e) = std::fs::rename(&new_app, &bundle) {
            let _ = std::fs::rename(&prev, &bundle);
            return Err(format!("couldn't move the new app into place: {e}"));
        }
    }
    let _ = std::process::Command::new("touch").arg(&bundle).status();
    if relaunch {
        relaunch_after_exit("/usr/bin/open", &bundle)?;
    }
    Ok(Some(prev))
}

#[cfg(target_os = "windows")]
fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), to)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn install_impl(dirs: &Dirs, _staged: &Staged, relaunch: bool, interactive: bool) -> Result<Option<PathBuf>, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let install_dir = exe.parent().ok_or("no install folder")?.to_path_buf();
    // Keep the current install for rollback.
    let prev = dirs.previous().join("app");
    let _ = std::fs::remove_dir_all(&prev);
    copy_dir(&install_dir, &prev).map_err(|e| format!("couldn't keep the previous version: {e}"))?;
    std::fs::write(dirs.previous().join("install-dir.txt"), install_dir.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;

    let setup = dirs.staged().join("Lookout-setup.exe");
    std::fs::copy(dirs.staged_payload(), &setup).map_err(|e| format!("couldn't write the installer: {e}"))?;
    let mut args = vec!["/S", "/UPDATE"];
    if relaunch {
        args.push("/R");
    }
    // Run it from a temp copy: `staged/` is cleared right after this returns.
    let tmp = std::env::temp_dir().join(format!("lookout-update-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let run = tmp.join("Lookout-setup.exe");
    std::fs::copy(&setup, &run).map_err(|e| e.to_string())?;
    match std::process::Command::new(&run).args(&args).spawn() {
        Ok(_) => Ok(Some(prev)),
        // ERROR_ELEVATION_REQUIRED: only through UAC, and only when asked.
        Err(e) if e.raw_os_error() == Some(740) && interactive => std::process::Command::new("cmd")
            .args(["/C", "start", "\"\""])
            .arg(&run)
            .args(&args)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(|_| Some(prev))
            .map_err(|e| format!("couldn't start the installer: {e}")),
        Err(e) if e.raw_os_error() == Some(740) => Err("the installer needs administrator rights".into()),
        Err(e) => Err(format!("couldn't start the installer: {e}")),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn install_impl(_: &Dirs, _: &Staged, _: bool, _: bool) -> Result<Option<PathBuf>, String> {
    Err("updates aren't supported on this platform".into())
}

// ── rollback ────────────────────────────────────────────────────────────

/// Restores the version `trial` replaced and relaunches it (after this
/// process exits).
pub fn rollback(dirs: &Dirs, trial: &Trial) -> Result<(), String> {
    let prev = trial.previous.clone().ok_or("the previous version wasn't kept")?;
    if !prev.exists() {
        return Err(format!("the previous version is gone ({})", prev.display()));
    }
    rollback_impl(dirs, &prev)
}

#[cfg(target_os = "linux")]
fn rollback_impl(_dirs: &Dirs, prev: &Path) -> Result<(), String> {
    let target = appimage_path().ok_or("not running from an AppImage")?;
    replace_file(prev, &target)?;
    relaunch_after_exit(&target.to_string_lossy(), Path::new(""))
}

#[cfg(target_os = "macos")]
fn rollback_impl(_dirs: &Dirs, prev: &Path) -> Result<(), String> {
    let bundle = bundle_path()?;
    let parent = bundle.parent().ok_or("no parent")?.to_path_buf();
    let failed = parent.join(".lookout-previous").join(".failed.app");
    let _ = std::fs::remove_dir_all(&failed);
    let moved = std::fs::rename(&bundle, &failed).and_then(|_| {
        std::fs::rename(prev, &bundle).inspect_err(|_| {
            let _ = std::fs::rename(&failed, &bundle);
        })
    });
    match moved {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&failed);
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            // Restoring a broken install is worth one password prompt.
            run_as_admin(&format!(
                "mv -f {b} {f} && {{ mv -f {p} {b} || {{ mv -f {f} {b}; exit 1; }}; }} && rm -rf {f}",
                b = shq(&bundle),
                f = shq(&failed),
                p = shq(prev)
            ))?;
        }
        Err(e) => return Err(format!("couldn't restore the previous app: {e}")),
    }
    let _ = std::process::Command::new("touch").arg(&bundle).status();
    relaunch_after_exit("/usr/bin/open", &bundle)
}

#[cfg(target_os = "windows")]
fn rollback_impl(dirs: &Dirs, prev: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let install_dir = std::fs::read_to_string(dirs.previous().join("install-dir.txt"))
        .map(PathBuf::from)
        .or_else(|_| exe.parent().map(Path::to_path_buf).ok_or(()))
        .map_err(|_| "no install folder".to_string())?;
    let pid = std::process::id();
    // Wait for this process to exit, mirror the kept copy back, start it.
    let script = format!(
        "@echo off\r\n:wait\r\ntasklist /FI \"PID eq {pid}\" | find \"{pid}\" >nul && (timeout /t 1 /nobreak >nul & goto wait)\r\nrobocopy \"{src}\" \"{dst}\" /MIR /R:5 /W:1 >nul\r\nstart \"\" \"{exe}\"\r\n",
        src = prev.display(),
        dst = install_dir.display(),
        exe = install_dir.join(exe.file_name().unwrap_or_default()).display()
    );
    let path = std::env::temp_dir().join(format!("lookout-rollback-{pid}.cmd"));
    std::fs::write(&path, script).map_err(|e| e.to_string())?;
    std::process::Command::new("cmd").arg("/C").arg(&path).creation_flags(CREATE_NO_WINDOW).spawn().map(|_| ()).map_err(|e| format!("couldn't start the rollback: {e}"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn rollback_impl(_: &Dirs, _: &Path) -> Result<(), String> {
    Err("unsupported".into())
}
