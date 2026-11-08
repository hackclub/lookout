//! Binary deltas in the `zstd --patch-from` format: a regular zstd frame
//! compressed with the old file as a raw-content *reference prefix*
//! (`ZSTD_CCtx_refPrefix`), a window large enough to reach back across the
//! whole old file, and long-distance matching — what the CLI's
//! `--patch-from` does. So a patch made here applies with
//! `zstd -d --patch-from=old patch -o new` and vice versa (pass
//! `--long=<window_log>` to the CLI when the window exceeds 2^27).

use std::io::{self, Read, Write};

/// zstd's largest window on 64-bit targets.
const WINDOW_LOG_MAX: u32 = 31;

/// The window that lets the patch reference anywhere in `base` + `new`.
pub fn window_log_for(base_len: usize, new_len: usize) -> u32 {
    let span = (base_len.max(new_len) as u64).max(1 << 10);
    (64 - (span - 1).leading_zeros()).clamp(10, WINDOW_LOG_MAX)
}

/// Patch from `base` to `new`. Level 19 is what the release tool uses.
pub fn create(base: &[u8], new: &[u8], level: i32) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(new.len() / 8 + 1024);
    {
        let mut enc = zstd::stream::write::Encoder::with_ref_prefix(&mut out, level, base)?;
        enc.window_log(window_log_for(base.len(), new.len()))?;
        enc.long_distance_matching(true)?;
        enc.include_checksum(true)?;
        enc.include_contentsize(true)?;
        enc.set_pledged_src_size(Some(new.len() as u64))?;
        enc.write_all(new)?;
        enc.finish()?;
    }
    Ok(out)
}

/// Rebuild the new file from `base` and `patch`. `max_len` caps the output
/// (the manifest's payload size) so a hostile patch can't fill memory.
pub fn apply(base: &[u8], patch: &[u8], max_len: u64) -> io::Result<Vec<u8>> {
    let mut dec = zstd::stream::read::Decoder::with_ref_prefix(patch, base)?;
    dec.window_log_max(WINDOW_LOG_MAX)?;
    let mut out = Vec::with_capacity(max_len.min(1 << 30) as usize);
    let n = dec.take(max_len.saturating_add(1)).read_to_end(&mut out)?;
    if n as u64 > max_len {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "patch output is larger than expected"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    #[test]
    fn roundtrip_and_small() {
        // 4 MiB of incompressible bytes with a few edits: the patch must be
        // tiny, which proves it references the base rather than recompressing.
        let base = noise(4 << 20, 1);
        let mut new = base.clone();
        new[1000..1100].copy_from_slice(&noise(100, 2));
        new.splice(2_000_000..2_000_000, noise(5000, 3));
        new.truncate(new.len() - 300_000);
        let p = create(&base, &new, 19).unwrap();
        assert!(p.len() < 20_000, "patch is {} bytes", p.len());
        assert_eq!(apply(&base, &p, new.len() as u64).unwrap(), new);
    }

    #[test]
    fn wrong_base_fails_or_differs() {
        let base = noise(1 << 20, 4);
        let mut new = base.clone();
        new[10] ^= 0xff;
        let p = create(&base, &new, 3).unwrap();
        let other = noise(1 << 20, 5);
        // Either zstd's checksum rejects it or the bytes differ; never `new`.
        match apply(&other, &p, new.len() as u64) {
            Ok(out) => assert_ne!(out, new),
            Err(_) => {}
        }
    }

    #[test]
    fn output_cap() {
        let base = vec![0u8; 1000];
        let new = vec![1u8; 100_000];
        let p = create(&base, &new, 3).unwrap();
        assert!(apply(&base, &p, 50_000).is_err());
    }

    #[test]
    fn window_log() {
        assert_eq!(window_log_for(0, 0), 10);
        assert_eq!(window_log_for(1 << 20, 10), 20);
        assert_eq!(window_log_for((1 << 20) + 1, 10), 21);
    }
}
