//! Version ordering. Release versions are semver (`0.4.1`, `0.5.0-beta.2`);
//! anything that isn't (a stray `v`, two components) is read leniently so a
//! sloppy manifest can't crash the check.

use std::cmp::Ordering;

pub fn parse(v: &str) -> semver::Version {
    let v = v.trim().trim_start_matches('v');
    if let Ok(p) = semver::Version::parse(v) {
        return p;
    }
    let (core, pre) = v.split_once('-').map(|(c, p)| (c, Some(p))).unwrap_or((v, None));
    let mut n = core.split('.').map(|p| p.trim().parse::<u64>().unwrap_or(0));
    let mut out = semver::Version::new(n.next().unwrap_or(0), n.next().unwrap_or(0), n.next().unwrap_or(0));
    if let Some(pre) = pre.and_then(|p| semver::Prerelease::new(p).ok()) {
        out.pre = pre;
    }
    out
}

pub fn cmp(a: &str, b: &str) -> Ordering {
    // Build metadata never orders releases.
    let (mut a, mut b) = (parse(a), parse(b));
    a.build = semver::BuildMetadata::EMPTY;
    b.build = semver::BuildMetadata::EMPTY;
    a.cmp(&b)
}

pub fn eq(a: &str, b: &str) -> bool {
    cmp(a, b) == Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering() {
        assert_eq!(cmp("0.4.1", "0.4.0"), Ordering::Greater);
        assert_eq!(cmp("v0.4.0", "0.4.0"), Ordering::Equal);
        assert_eq!(cmp("0.5.0-beta.1", "0.5.0"), Ordering::Less);
        assert_eq!(cmp("0.5.0-beta.2", "0.5.0-beta.10"), Ordering::Less);
        assert_eq!(cmp("0.10.0", "0.9.9"), Ordering::Greater);
        assert_eq!(cmp("1.2", "1.2.0"), Ordering::Equal);
        assert_eq!(cmp("garbage", "0.0.0"), Ordering::Equal);
    }
}
