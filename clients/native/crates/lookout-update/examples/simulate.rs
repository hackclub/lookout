//! The client pipeline without the app: fetch `update.json` + `.minisig`
//! from a base URL, verify, decide, download (patch or full, resumable),
//! and verify the payload into a staging dir.
//!
//! ```sh
//! cargo run -p lookout-update --example simulate -- \
//!   <base-url> <pubkey> <installed-version> <base-payload|-> <work-dir> [stable|beta]
//! ```

use lookout_update::manifest::{Channel, Decision, Install, Manifest};
use lookout_update::{download, hash, pipeline, sig};

#[tokio::main]
async fn main() -> Result<(), String> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 5 {
        return Err("simulate <base-url> <pubkey> <installed> <base-payload|-> <work-dir> [stable|beta]".into());
    }
    let (base, key, installed, base_payload, work) = (&a[0], &a[1], &a[2], &a[3], std::path::PathBuf::from(&a[4]));
    let channel = if a.get(5).map(String::as_str) == Some("beta") { Channel::Beta } else { Channel::Stable };
    let http = reqwest::Client::new();
    let base = base.trim_end_matches('/');
    let raw = download::fetch_small(&http, &format!("{base}/update.json"), 4 << 20).await?;
    let s = String::from_utf8(download::fetch_small(&http, &format!("{base}/update.json.minisig"), 16 << 10).await?).map_err(|e| e.to_string())?;
    let m = Manifest::verify_and_parse(&raw, &s, &sig::decode_pubkey(key)?).map_err(|e| e.to_string())?;
    println!("manifest ok: v{} ({}), rollout {}", m.version, m.channel.as_str(), m.rollout);
    let me = Install { version: installed, channel, install_id: "simulate", platform_key: "native-linux-x86_64", rolled_back: &[] };
    let d = lookout_update::decide(&m, &me).map_err(|e| e.to_string())?;
    let Decision::Update { version, artifact, forced } = d else {
        println!("decision: {d:?}");
        return Ok(());
    };
    println!("decision: update to v{version} (forced: {forced}), {} patch(es) offered", artifact.patches.len());
    let bp = (base_payload != "-").then(|| std::path::PathBuf::from(base_payload));
    let mut tries = 0;
    let got = loop {
        tries += 1;
        match pipeline::obtain_payload(&http, &artifact, installed, bp.as_deref(), &work, |_, _| {}).await {
            Ok(g) => break g,
            // A dropped connection keeps the .part; the next try resumes.
            Err(e) if tries < 3 => println!("attempt {tries} failed: {e} — retrying"),
            Err(e) => return Err(e),
        }
    };
    let h = hash::sha256_file(&got.payload).map_err(|e| e.to_string())?;
    println!("payload: {} via {} after {tries} attempt(s), sha256 {h} ({})", got.payload.display(), if got.via_patch { "patch" } else { "full download" }, if hash::same(&h, &artifact.payload_sha256) { "matches" } else { "MISMATCH" });
    Ok(())
}
