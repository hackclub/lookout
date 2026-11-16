//! `lookout://` deep links — `handleDeepLinkUrls` (App.tsx:745-830) and
//! `extractToken` / `parsePairCallback` (utils.ts, programLink.ts).

use url::Url;

pub fn is_valid_token(t: &str) -> bool {
    t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit())
}

fn normalize(raw: &str) -> Option<Url> {
    Url::parse(&raw.replacen("lookout://", "https://lookout.local/", 1)).ok()
}

/// `extractToken`: `?token=…`, `/session/<token>`, or a bare `/<token>`.
pub fn extract_token(raw: &str) -> Option<String> {
    let url = normalize(raw)?;
    if let Some((_, t)) = url.query_pairs().find(|(k, _)| k == "token") {
        if is_valid_token(&t) {
            return Some(t.to_string());
        }
    }
    let segs: Vec<&str> = url.path_segments().map(|s| s.filter(|p| !p.is_empty()).collect()).unwrap_or_default();
    let candidate = if segs.len() >= 2 {
        Some(segs[1])
    } else if segs.len() == 1 && segs[0] != "session" {
        Some(segs[0])
    } else {
        None
    };
    candidate.filter(|c| is_valid_token(c)).map(String::from)
}

/// `parsePairCallback`: `lookout://pair?code=…&state=…`.
pub fn parse_pair_callback(raw: &str) -> Option<(String, String)> {
    let url = normalize(raw)?;
    let path = url.path();
    if !(path == "/pair" || path == "/pair/" || path.starts_with("/pair/")) {
        return None;
    }
    let code = url.query_pairs().find(|(k, _)| k == "code")?.1.to_string();
    let state = url.query_pairs().find(|(k, _)| k == "state")?.1.to_string();
    if code.is_empty() || state.is_empty() || code.len() > 512 || state.len() > 512 {
        return None;
    }
    Some((code, state))
}

/// Which Settings page a `lookout://settings[/<page>]` link opens (the
/// System Settings pane's buttons use these). Navigation only: a link from
/// any web page can reach this, so links never change a setting themselves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsLink {
    Menu,
    FilteredApps,
    LinkedPrograms,
    Advanced,
    /// Settings' "About Lookout".
    About,
    /// Settings' "Show onboarding" (`lookout://onboarding`).
    Onboarding,
}

pub fn parse_settings(raw: &str) -> Option<SettingsLink> {
    let url = normalize(raw)?;
    let segs: Vec<&str> = url.path_segments().map(|s| s.filter(|p| !p.is_empty()).collect()).unwrap_or_default();
    match segs.as_slice() {
        ["settings"] => Some(SettingsLink::Menu),
        ["settings", "filtered-apps"] => Some(SettingsLink::FilteredApps),
        ["settings", "linked-programs"] => Some(SettingsLink::LinkedPrograms),
        ["settings", "advanced"] => Some(SettingsLink::Advanced),
        ["settings", "about"] => Some(SettingsLink::About),
        ["onboarding"] => Some(SettingsLink::Onboarding),
        _ => None,
    }
}

/// URLs handed to the process on its command line.
pub fn from_args() -> Vec<String> {
    std::env::args().skip(1).filter(|a| a.starts_with("lookout://")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let t = "a".repeat(64);
        assert_eq!(extract_token(&format!("lookout://session/?token={t}")), Some(t.clone()));
        assert_eq!(extract_token(&format!("lookout://session/{t}")), Some(t.clone()));
        assert_eq!(extract_token(&format!("lookout://{t}")), Some(t.clone()));
        assert_eq!(extract_token("lookout://session/"), None);
        assert!(parse_pair_callback("lookout://pair?code=x&state=y").is_some());
        assert!(parse_pair_callback("lookout://session?code=x&state=y").is_none());
    }

    #[test]
    fn settings() {
        assert_eq!(parse_settings("lookout://settings"), Some(SettingsLink::Menu));
        assert_eq!(parse_settings("lookout://settings/"), Some(SettingsLink::Menu));
        assert_eq!(parse_settings("lookout://settings/filtered-apps"), Some(SettingsLink::FilteredApps));
        assert_eq!(parse_settings("lookout://settings/linked-programs"), Some(SettingsLink::LinkedPrograms));
        assert_eq!(parse_settings("lookout://settings/advanced"), Some(SettingsLink::Advanced));
        assert_eq!(parse_settings("lookout://settings/about"), Some(SettingsLink::About));
        assert_eq!(parse_settings("lookout://onboarding"), Some(SettingsLink::Onboarding));
        assert_eq!(parse_settings("lookout://settings/nope"), None);
        assert_eq!(parse_settings(&format!("lookout://session/{}", "a".repeat(64))), None);
        // A token link is never mistaken for a settings one, and vice versa.
        assert_eq!(extract_token("lookout://settings/advanced"), None);
    }
}
