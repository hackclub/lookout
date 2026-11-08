//! Staged rollout: an install takes a release once `bucket(install_id) <
//! rollout`. The bucket is the first 8 bytes of SHA-256(install id) as a
//! fraction of 2^64, so it is uniform, stable across launches and releases
//! (raising 0.1 → 0.5 keeps the first 10% in), and reveals nothing about
//! the id.

use sha2::{Digest, Sha256};

pub fn bucket(install_id: &str) -> f64 {
    let d = Sha256::digest(install_id.as_bytes());
    let n = u64::from_be_bytes(d[..8].try_into().unwrap());
    n as f64 / 18_446_744_073_709_551_616.0 // 2^64
}

/// `rollout` is clamped to 0..=1; 1 always includes, 0 never does.
pub fn included(install_id: &str, rollout: f64) -> bool {
    if rollout.is_nan() {
        return false;
    }
    let r = rollout.clamp(0.0, 1.0);
    if r >= 1.0 {
        return true;
    }
    bucket(install_id) < r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_in_range() {
        let b = bucket("abc");
        assert_eq!(b, bucket("abc"));
        assert!((0.0..1.0).contains(&b));
        assert_ne!(b, bucket("abd"));
    }

    #[test]
    fn edges() {
        assert!(included("x", 1.0));
        assert!(included("x", 7.0));
        assert!(!included("x", 0.0));
        assert!(!included("x", -1.0));
        assert!(!included("x", f64::NAN));
    }

    #[test]
    fn roughly_uniform_and_monotonic() {
        let ids: Vec<String> = (0..20_000).map(|i| format!("install-{i}")).collect();
        for r in [0.05, 0.25, 0.5, 0.9] {
            let n = ids.iter().filter(|id| included(id, r)).count() as f64 / ids.len() as f64;
            assert!((n - r).abs() < 0.02, "rollout {r}: got {n}");
        }
        // Raising the fraction never drops anyone who was already in.
        for id in &ids[..2000] {
            if included(id, 0.1) {
                assert!(included(id, 0.3));
            }
        }
    }
}
