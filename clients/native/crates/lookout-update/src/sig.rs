//! minisign verification. `update.json.minisig` is a plain minisign
//! signature file; the key is the one the Tauri app ships
//! (`plugins.updater.pubkey`: base64 of the minisign `.pub` text).

use base64::Engine;
pub use minisign_verify::PublicKey;

/// The release key, as in the Tauri app's `tauri.conf.json`.
pub const RELEASE_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEM0OTY4MTcxNjk3N0YwQQpSV1FLZjVjV0YyaEpEQnZwVWpHVTBUSHdJcC9kZ09kRTJ6cVFZcFZVM0pqbGtwdHc2MjcwbC96Mwo=";

/// Accepts the Tauri form (base64 of the `.pub` file), the `.pub` text
/// itself, or the bare `RW…` key line.
pub fn decode_pubkey(s: &str) -> Result<PublicKey, String> {
    let s = s.trim();
    if s.contains('\n') || s.starts_with("untrusted comment:") {
        return PublicKey::decode(s).map_err(|e| format!("public key: {e}"));
    }
    if s.starts_with("RW") {
        if let Ok(k) = PublicKey::from_base64(s) {
            return Ok(k);
        }
    }
    let raw = base64::engine::general_purpose::STANDARD.decode(s).map_err(|e| format!("public key: {e}"))?;
    let text = String::from_utf8(raw).map_err(|e| format!("public key: {e}"))?;
    PublicKey::decode(text.trim()).map_err(|e| format!("public key: {e}"))
}

/// Verifies `data` against a minisign signature file's text. Also accepts a
/// base64-wrapped signature (Tauri's `.sig` form) for symmetry with the key.
pub fn verify(data: &[u8], signature: &str, key: &PublicKey) -> Result<(), String> {
    let text = signature.trim();
    let text = if text.starts_with("untrusted comment:") {
        text.to_string()
    } else {
        let raw = base64::engine::general_purpose::STANDARD.decode(text).map_err(|e| format!("signature: {e}"))?;
        String::from_utf8(raw).map_err(|e| format!("signature: {e}"))?
    };
    let sig = minisign_verify::Signature::decode(&text).map_err(|e| format!("signature: {e}"))?;
    // Prehashed (Ed) signatures only: the tool never makes legacy ones.
    key.verify(data, &sig, false).map_err(|e| format!("signature check failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_key_decodes() {
        decode_pubkey(RELEASE_PUBKEY).unwrap();
    }
}
