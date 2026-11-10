//! `lookout-update-tool` — the release side of Lookout Native's update v2.
//!
//! ```text
//! manifest   --version V --artifacts DIR --base-url URL --out DIR
//!            [--channel stable|beta] [--notes-file F] [--rollout 0..1]
//!            [--blocked a,b] [--min-supported V] [--previous DIR]... [--max-patches 3] [--sign]
//! sign       FILE                      writes FILE.minisig (key from the environment, below)
//! verify     FILE [--pubkey KEY]       checks FILE.minisig (default: the release key)
//! patch      create OLD NEW OUT | apply OLD PATCH OUT
//! keygen     --dir DIR                 throwaway unencrypted key (tests / local harness only)
//! fake-release --dir DIR [--from 0.4.0] [--to 0.4.1] [--beta 0.5.0-beta.1] [--base-url URL]
//! ```
//!
//! Native artifacts are found by name: `Lookout-native_<version>_<platform><ext>` with
//! `darwin-aarch64` / `darwin-x86_64` → `.app.tar.gz`, `windows-x86_64` / `windows-aarch64` → `-setup.exe`,
//! `linux-x86_64` → `.AppImage`. Patches are `zstd --patch-from` deltas between *payloads*
//! (the tar inside an `.app.tar.gz`; other files as they are) of the newest `--max-patches`
//! earlier versions found in the `--previous` dirs.
//!
//! Signing key: `LOOKOUT_UPDATE_SIGNING_KEY`, else `TAURI_SIGNING_PRIVATE_KEY` (the minisign
//! secret key text, or base64 of it — the Tauri form), with the password in
//! `LOOKOUT_UPDATE_SIGNING_KEY_PASSWORD` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
//! `LOOKOUT_UPDATE_SIGNING_KEY_FILE` points at a key file instead.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use base64::Engine;
use lookout_update::manifest::{Artifact, Channel, Format, Manifest, Patch, SCHEMA};
use lookout_update::{hash, patch, sig, version};

const PREFIX: &str = "Lookout-native_";
const PLATFORMS: &[(&str, &str, Format)] = &[
    ("darwin-aarch64", ".app.tar.gz", Format::AppTarGz),
    ("darwin-x86_64", ".app.tar.gz", Format::AppTarGz),
    ("windows-x86_64", "-setup.exe", Format::Nsis),
    ("windows-aarch64", "-setup.exe", Format::Nsis),
    ("linux-x86_64", ".AppImage", Format::Appimage),
];

type R<T> = Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("manifest") => cmd_manifest(&args[1..]),
        Some("sign") => args.get(1).ok_or("sign FILE".to_string()).and_then(|f| sign_file(Path::new(f), &signing_key()?)),
        Some("verify") => cmd_verify(&args[1..]),
        Some("patch") => cmd_patch(&args[1..]),
        Some("keygen") => flag(&args[1..], "--dir").ok_or("keygen --dir DIR".into()).and_then(|d| keygen(Path::new(&d)).map(|_| ())),
        Some("fake-release") => cmd_fake_release(&args[1..]),
        _ => Err(include_str!("main.rs").lines().take_while(|l| l.starts_with("//!")).map(|l| l.trim_start_matches("//!")).collect::<Vec<_>>().join("\n")),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

// ── args ────────────────────────────────────────────────────────────────

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn flags(args: &[String], name: &str) -> Vec<String> {
    args.windows(2).filter(|w| w[0] == name).map(|w| w[1].clone()).collect()
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn need(args: &[String], name: &str) -> R<String> {
    flag(args, name).ok_or_else(|| format!("missing {name}"))
}

// ── artifacts ───────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Found {
    version: String,
    platform: &'static str,
    format: Format,
    path: PathBuf,
}

/// Native artifacts in `dir` (not recursive), by the naming convention.
fn find_artifacts(dir: &Path) -> R<Vec<Found>> {
    let mut out = Vec::new();
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(rest) = name.strip_prefix(PREFIX) else { continue };
        let Some((ver, tail)) = rest.split_once('_') else { continue };
        if ver.contains("-to-") {
            continue; // a patch
        }
        for (platform, ext, format) in PLATFORMS {
            if tail == format!("{platform}{ext}") {
                out.push(Found { version: ver.trim_start_matches('v').to_string(), platform, format: *format, path: e.path() });
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// The bytes patches are computed over.
fn payload(f: &Found) -> R<Vec<u8>> {
    let raw = std::fs::read(&f.path).map_err(|e| format!("{}: {e}", f.path.display()))?;
    if f.format.payload_is_file() {
        return Ok(raw);
    }
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(&raw[..]).read_to_end(&mut out).map_err(|e| format!("{}: {e}", f.path.display()))?;
    Ok(out)
}

fn join_url(base: &str, file: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), file)
}

struct ManifestOpts {
    version: String,
    channel: Channel,
    artifacts: PathBuf,
    base_url: String,
    out: PathBuf,
    notes: String,
    rollout: f64,
    blocked: Vec<String>,
    min_supported: Option<String>,
    previous: Vec<PathBuf>,
    max_patches: usize,
}

fn build_manifest(o: &ManifestOpts) -> R<Manifest> {
    std::fs::create_dir_all(&o.out).map_err(|e| e.to_string())?;
    let current: Vec<Found> = find_artifacts(&o.artifacts)?.into_iter().filter(|f| version::eq(&f.version, &o.version)).collect();
    if current.is_empty() {
        return Err(format!("no {PREFIX}{}_* artifacts in {}", o.version, o.artifacts.display()));
    }
    let mut older = Vec::new();
    for d in &o.previous {
        older.extend(find_artifacts(d)?.into_iter().filter(|f| version::cmp(&f.version, &o.version).is_lt()));
    }
    let mut platforms = BTreeMap::new();
    for f in &current {
        let file = std::fs::read(&f.path).map_err(|e| e.to_string())?;
        let new_payload = payload(f)?;
        let name = f.path.file_name().unwrap().to_string_lossy().to_string();
        // Newest `max_patches` earlier versions of this platform.
        let mut bases: Vec<&Found> = older.iter().filter(|b| b.platform == f.platform).collect();
        bases.sort_by(|a, b| version::cmp(&b.version, &a.version));
        bases.dedup_by(|a, b| version::eq(&a.version, &b.version));
        bases.truncate(o.max_patches);
        let mut patches = Vec::new();
        for b in bases {
            let base_payload = payload(b)?;
            let p = patch::create(&base_payload, &new_payload, 19).map_err(|e| format!("patch {} → {}: {e}", b.version, o.version))?;
            // Prove it applies before publishing it.
            let back = patch::apply(&base_payload, &p, new_payload.len() as u64).map_err(|e| e.to_string())?;
            if back != new_payload {
                return Err(format!("patch {} → {} doesn't round-trip", b.version, o.version));
            }
            if p.len() >= file.len() {
                eprintln!("  skip patch {} → {} ({}): not smaller than the full file", b.version, o.version, f.platform);
                continue;
            }
            let pname = format!("{PREFIX}{}-to-{}_{}.patch.zst", b.version, o.version, f.platform);
            std::fs::write(o.out.join(&pname), &p).map_err(|e| e.to_string())?;
            eprintln!("  patch {} → {} ({}): {} bytes (full {} bytes)", b.version, o.version, f.platform, p.len(), file.len());
            patches.push(Patch {
                from_version: b.version.clone(),
                from_sha256: hash::sha256_hex(&base_payload),
                url: join_url(&o.base_url, &pname),
                size: p.len() as u64,
                sha256: hash::sha256_hex(&p),
            });
        }
        platforms.insert(
            format!("native-{}", f.platform),
            Artifact {
                url: join_url(&o.base_url, &name),
                size: file.len() as u64,
                sha256: hash::sha256_hex(&file),
                format: f.format,
                payload_size: new_payload.len() as u64,
                payload_sha256: hash::sha256_hex(&new_payload),
                patches,
            },
        );
    }
    let m = Manifest {
        schema: SCHEMA,
        version: o.version.clone(),
        pub_date: now_rfc3339(),
        notes: o.notes.clone(),
        channel: o.channel,
        min_supported: o.min_supported.clone(),
        rollout: o.rollout.clamp(0.0, 1.0),
        blocked: o.blocked.clone(),
        platforms,
    };
    let raw = serde_json::to_vec_pretty(&m).map_err(|e| e.to_string())?;
    std::fs::write(o.out.join("update.json"), raw).map_err(|e| e.to_string())?;
    Ok(m)
}

fn cmd_manifest(a: &[String]) -> R<()> {
    let channel = match flag(a, "--channel").as_deref().unwrap_or("stable") {
        "stable" => Channel::Stable,
        "beta" => Channel::Beta,
        c => return Err(format!("unknown channel {c}")),
    };
    let notes = match flag(a, "--notes-file") {
        Some(f) => std::fs::read_to_string(&f).map_err(|e| format!("{f}: {e}"))?.trim().to_string(),
        None => String::new(),
    };
    let o = ManifestOpts {
        version: need(a, "--version")?.trim_start_matches('v').to_string(),
        channel,
        artifacts: need(a, "--artifacts")?.into(),
        base_url: need(a, "--base-url")?,
        out: need(a, "--out")?.into(),
        notes,
        rollout: flag(a, "--rollout").map(|r| r.parse::<f64>().map_err(|e| e.to_string())).transpose()?.unwrap_or(1.0),
        blocked: flag(a, "--blocked").map(|b| b.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default(),
        min_supported: flag(a, "--min-supported").filter(|s| !s.is_empty()),
        previous: flags(a, "--previous").into_iter().map(PathBuf::from).collect(),
        max_patches: flag(a, "--max-patches").and_then(|n| n.parse().ok()).unwrap_or(3),
    };
    let m = build_manifest(&o)?;
    eprintln!("wrote {} (v{}, {}, {} platform(s))", o.out.join("update.json").display(), m.version, m.channel.as_str(), m.platforms.len());
    if has(a, "--sign") {
        sign_file(&o.out.join("update.json"), &signing_key()?)?;
    }
    Ok(())
}

fn now_rfc3339() -> String {
    // Civil-from-days, so the tool needs no date crate.
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (d, rem) = ((secs / 86400) as i64, secs % 86400);
    let z = d + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
}

// ── signing ─────────────────────────────────────────────────────────────

fn env(names: &[&str]) -> Option<String> {
    names.iter().find_map(|n| std::env::var(n).ok().filter(|v| !v.is_empty()))
}

/// The minisign secret key from the environment (text, base64 text, or a file).
fn signing_key() -> R<minisign::SecretKey> {
    let text = match env(&["LOOKOUT_UPDATE_SIGNING_KEY_FILE"]) {
        Some(p) => std::fs::read_to_string(&p).map_err(|e| format!("{p}: {e}"))?,
        None => env(&["LOOKOUT_UPDATE_SIGNING_KEY", "TAURI_SIGNING_PRIVATE_KEY"])
            .ok_or("no signing key: set LOOKOUT_UPDATE_SIGNING_KEY or TAURI_SIGNING_PRIVATE_KEY")?,
    };
    let password = env(&["LOOKOUT_UPDATE_SIGNING_KEY_PASSWORD", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"]);
    parse_secret_key(&text, password)
}

fn parse_secret_key(text: &str, password: Option<String>) -> R<minisign::SecretKey> {
    let text = text.trim();
    let text = if text.starts_with("untrusted comment:") {
        text.to_string()
    } else {
        let raw = base64::engine::general_purpose::STANDARD.decode(text).map_err(|e| format!("signing key: {e}"))?;
        String::from_utf8(raw).map_err(|e| format!("signing key: {e}"))?
    };
    let bx = minisign::SecretKeyBox::from_string(&text).map_err(|e| format!("signing key: {e}"))?;
    let sk = bx.clone().into_unencrypted_secret_key().ok();
    match sk {
        Some(sk) => Ok(sk),
        // Never prompt (CI has no terminal): an absent password is an empty one.
        None => bx.into_secret_key(Some(password.unwrap_or_default())).map_err(|e| format!("signing key: {e}")),
    }
}

fn sign_file(path: &Path, sk: &minisign::SecretKey) -> R<()> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let trusted = format!("timestamp:{}\tfile:{name}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    let sb = minisign::sign(None, sk, std::io::Cursor::new(&data), Some(&trusted), Some("signature from lookout-update-tool"))
        .map_err(|e| format!("sign: {e}"))?;
    let out = PathBuf::from(format!("{}.minisig", path.display()));
    std::fs::write(&out, sb.to_string()).map_err(|e| e.to_string())?;
    eprintln!("signed {} → {}", path.display(), out.display());
    Ok(())
}

fn cmd_verify(a: &[String]) -> R<()> {
    let file = a.first().ok_or("verify FILE [--pubkey KEY]")?;
    let key = sig::decode_pubkey(&flag(a, "--pubkey").unwrap_or_else(|| sig::RELEASE_PUBKEY.to_string()))?;
    let data = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
    let s = std::fs::read_to_string(format!("{file}.minisig")).map_err(|e| format!("{file}.minisig: {e}"))?;
    sig::verify(&data, &s, &key)?;
    if file.ends_with(".json") {
        let m = Manifest::verify_and_parse(&data, &s, &key).map_err(|e| e.to_string())?;
        eprintln!("ok: v{} ({}), {} platform(s)", m.version, m.channel.as_str(), m.platforms.len());
    } else {
        eprintln!("ok");
    }
    Ok(())
}

fn cmd_patch(a: &[String]) -> R<()> {
    let read = |p: &String| std::fs::read(p).map_err(|e| format!("{p}: {e}"));
    match (a.first().map(String::as_str), a.get(1), a.get(2), a.get(3)) {
        (Some("create"), Some(old), Some(new), Some(out)) => {
            let p = patch::create(&read(old)?, &read(new)?, 19).map_err(|e| e.to_string())?;
            std::fs::write(out, p).map_err(|e| e.to_string())
        }
        (Some("apply"), Some(old), Some(p), Some(out)) => {
            let new = patch::apply(&read(old)?, &read(p)?, 4 << 30).map_err(|e| e.to_string())?;
            std::fs::write(out, new).map_err(|e| e.to_string())
        }
        _ => Err("patch create OLD NEW OUT | patch apply OLD PATCH OUT".into()),
    }
}

/// Writes `test.key` (unencrypted), `test.pub` and `test.pub.b64` (the
/// Tauri form). Returns the base64 public key.
fn keygen(dir: &Path) -> R<String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let kp = minisign::KeyPair::generate_unencrypted_keypair().map_err(|e| e.to_string())?;
    let sk = kp.sk.to_box(Some("lookout throwaway test key — never for releases")).map_err(|e| e.to_string())?.to_string();
    let pk = kp.pk.to_box().map_err(|e| e.to_string())?.to_string();
    let b64 = base64::engine::general_purpose::STANDARD.encode(pk.as_bytes());
    std::fs::write(dir.join("test.key"), &sk).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("test.pub"), &pk).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("test.pub.b64"), &b64).map_err(|e| e.to_string())?;
    Ok(b64)
}

// ── fake releases for the local harness ─────────────────────────────────

/// A fake "AppImage": a shell script that prints its version, padded with
/// deterministic bytes so patches have something to reference.
fn fake_appimage(v: &str) -> Vec<u8> {
    let mut out = format!("#!/bin/sh\necho \"Lookout fake AppImage v{v}\"\nexit 0\n").into_bytes();
    let mut x: u64 = 0x2545F4914F6CDD1D;
    out.extend((0..2_000_000).map(|_| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        b'a' + (x % 26) as u8
    }));
    // Each version changes a few spots.
    for (i, b) in v.bytes().enumerate() {
        let at = 100_000 + i * 150_001;
        if at < out.len() {
            out[at] = b;
        }
    }
    out.extend_from_slice(format!("\n# v{v}\n").as_bytes());
    out
}

fn cmd_fake_release(a: &[String]) -> R<()> {
    let dir = PathBuf::from(need(a, "--dir")?);
    let from = flag(a, "--from").unwrap_or_else(|| "0.4.0".into());
    let to = flag(a, "--to").unwrap_or_else(|| "0.4.1".into());
    let beta = flag(a, "--beta").unwrap_or_else(|| "0.5.0-beta.1".into());
    let base = flag(a, "--base-url").unwrap_or_else(|| "http://127.0.0.1:8787/updates".into());
    let platform = "linux-x86_64";
    let _ = std::fs::remove_dir_all(&dir);
    let pub_b64 = keygen(&dir.join("keys"))?;
    let sk = parse_secret_key(&std::fs::read_to_string(dir.join("keys/test.key")).unwrap(), None)?;

    // Builds each version into its own "release" dir, the way CI has them.
    let mut releases = Vec::new();
    for v in [&from, &to, &beta] {
        let rel = dir.join("releases").join(v);
        std::fs::create_dir_all(&rel).map_err(|e| e.to_string())?;
        std::fs::write(rel.join(format!("{PREFIX}{v}_{platform}.AppImage")), fake_appimage(v)).map_err(|e| e.to_string())?;
        releases.push(rel);
    }
    // The installed copy the client starts from.
    let installed = dir.join("Lookout.AppImage");
    std::fs::write(&installed, fake_appimage(&from)).map_err(|e| e.to_string())?;
    set_exec(&installed);

    for (channel, v, prev) in [(Channel::Stable, &to, vec![releases[0].clone()]), (Channel::Beta, &beta, vec![releases[0].clone(), releases[1].clone()])] {
        let out = dir.join(channel.as_str());
        let rel = dir.join("releases").join(v);
        let m = build_manifest(&ManifestOpts {
            version: v.clone(),
            channel,
            artifacts: rel.clone(),
            base_url: join_url(&base, channel.as_str()),
            out: out.clone(),
            notes: format!("Fake {} release v{v} for testing the updater.", channel.as_str()),
            rollout: 1.0,
            blocked: vec![],
            min_supported: None,
            previous: prev,
            max_patches: 3,
        })?;
        // Full artifacts sit beside the manifest, like a GitHub release.
        for f in find_artifacts(&rel)? {
            std::fs::copy(&f.path, out.join(f.path.file_name().unwrap())).map_err(|e| e.to_string())?;
        }
        sign_file(&out.join("update.json"), &sk)?;
        eprintln!("{}: v{} with {} patch(es)", channel.as_str(), m.version, m.platforms.values().map(|a| a.patches.len()).sum::<usize>());
    }
    println!("LOOKOUT_DEV_UPDATES={}", dir.display());
    println!("LOOKOUT_UPDATE_URL={}", join_url(&base, "stable"));
    println!("LOOKOUT_UPDATE_BETA_URL={}", join_url(&base, "beta"));
    println!("LOOKOUT_UPDATE_PUBKEY={pub_b64}");
    println!("APPIMAGE={}", installed.display());
    println!("LOOKOUT_UPDATE_AS_VERSION={from}");
    Ok(())
}

fn set_exec(p: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
    }
    let _ = p;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_end_to_end() {
        let d = std::env::temp_dir().join(format!("lut-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (old, new, out) = (d.join("old"), d.join("new"), d.join("out"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join(format!("{PREFIX}0.4.0_linux-x86_64.AppImage")), fake_appimage("0.4.0")).unwrap();
        std::fs::write(new.join(format!("{PREFIX}0.4.1_linux-x86_64.AppImage")), fake_appimage("0.4.1")).unwrap();
        // A mac artifact too, to cover the tar.gz payload path.
        let mut tar = tar::Builder::new(Vec::new());
        let body = fake_appimage("0.4.1");
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o755);
        h.set_cksum();
        tar.append_data(&mut h, "Lookout.app/Contents/MacOS/lookout", &body[..]).unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &tar.into_inner().unwrap()).unwrap();
        std::fs::write(new.join(format!("{PREFIX}0.4.1_darwin-aarch64.app.tar.gz")), gz.finish().unwrap()).unwrap();

        let m = build_manifest(&ManifestOpts {
            version: "0.4.1".into(),
            channel: Channel::Stable,
            artifacts: new.clone(),
            base_url: "https://example.com/dl/".into(),
            out: out.clone(),
            notes: "n".into(),
            rollout: 0.5,
            blocked: vec!["0.3.9".into()],
            min_supported: None,
            previous: vec![old.clone()],
            max_patches: 3,
        })
        .unwrap();
        let lin = &m.platforms["native-linux-x86_64"];
        assert_eq!(lin.patches.len(), 1);
        assert!(lin.patches[0].size < 10_000);
        assert_eq!(lin.url, format!("https://example.com/dl/{PREFIX}0.4.1_linux-x86_64.AppImage"));
        let mac = &m.platforms["native-darwin-aarch64"];
        assert_eq!(mac.format, Format::AppTarGz);
        assert!(mac.payload_size > mac.size);

        // Sign with a throwaway key; the client-side check accepts it.
        let b64 = keygen(&d.join("k")).unwrap();
        let sk = parse_secret_key(&std::fs::read_to_string(d.join("k/test.key")).unwrap(), None).unwrap();
        sign_file(&out.join("update.json"), &sk).unwrap();
        let raw = std::fs::read(out.join("update.json")).unwrap();
        let s = std::fs::read_to_string(out.join("update.json.minisig")).unwrap();
        let parsed = Manifest::verify_and_parse(&raw, &s, &sig::decode_pubkey(&b64).unwrap()).unwrap();
        assert_eq!(parsed, m);
        // The patch on disk rebuilds the new payload from the old one.
        let p = std::fs::read(out.join(format!("{PREFIX}0.4.0-to-0.4.1_linux-x86_64.patch.zst"))).unwrap();
        assert_eq!(patch::apply(&fake_appimage("0.4.0"), &p, lin.payload_size).unwrap(), fake_appimage("0.4.1"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn base64_wrapped_encrypted_key() {
        // The Tauri form: base64 of an encrypted minisign secret key.
        let kp = minisign::KeyPair::generate_encrypted_keypair(Some("pw".into())).unwrap();
        let text = kp.sk.to_box(None).unwrap().to_string();
        let b64 = base64::engine::general_purpose::STANDARD.encode(text);
        let sk = parse_secret_key(&b64, Some("pw".into())).unwrap();
        let s = minisign::sign(None, &sk, std::io::Cursor::new(b"x"), None, None).unwrap();
        let pk = sig::decode_pubkey(&kp.pk.to_box().unwrap().to_string()).unwrap();
        sig::verify(b"x", &s.to_string(), &pk).unwrap();
    }
}
