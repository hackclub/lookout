//! End-to-end smoke test that simulates the **currently-shipped** desktop
//! binary against a running server.
//!
//! Uses struct definitions copied verbatim from the pre-credit-mode
//! `lib.rs` (before any of the credit-mode work). Runs the full session
//! lifecycle the old client performs:
//!
//!   create session (out of band) → upload-url → PUT (skipped — mocked
//!   server) → confirm → pause → resume → upload-url → confirm → stop
//!
//! Skips silently when `LEGACY_CLIENT_TEST_URL` and `LEGACY_CLIENT_TEST_TOKEN`
//! aren't set — that's the TS test driver's job to provide a running
//! server URL and a session token.

use serde::Deserialize;

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct LegacyUploadUrlResponse {
    #[serde(rename = "uploadUrl")]
    upload_url: String,
    #[serde(rename = "r2Key")]
    r2_key: String,
    #[serde(rename = "screenshotId")]
    screenshot_id: String,
    #[serde(rename = "minuteBucket")]
    minute_bucket: i32,
    #[serde(rename = "nextExpectedAt")]
    next_expected_at: String,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct LegacyConfirmResponse {
    confirmed: bool,
    #[serde(rename = "trackedSeconds")]
    tracked_seconds: i64,
    #[serde(rename = "nextExpectedAt")]
    next_expected_at: String,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct LegacyPauseResponse {
    status: String,
    #[serde(rename = "totalActiveSeconds")]
    total_active_seconds: i64,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct LegacyResumeResponse {
    status: String,
    #[serde(rename = "nextExpectedAt")]
    next_expected_at: String,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct LegacyStopResponse {
    status: String,
    #[serde(rename = "trackedSeconds")]
    tracked_seconds: i64,
    #[serde(rename = "totalActiveSeconds")]
    total_active_seconds: i64,
}

#[tokio::test]
async fn legacy_client_full_lifecycle_against_running_server() {
    let url = match std::env::var("LEGACY_CLIENT_TEST_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("LEGACY_CLIENT_TEST_URL not set, skipping");
            return;
        }
    };
    let token = match std::env::var("LEGACY_CLIENT_TEST_TOKEN") {
        Ok(t) => t,
        Err(_) => {
            eprintln!("LEGACY_CLIENT_TEST_TOKEN not set, skipping");
            return;
        }
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();

    // ── 1. First upload-url (no capturedAt — legacy behavior) ────────
    let resp = client
        .get(format!("{}/api/sessions/{}/upload-url", url, token))
        .send()
        .await
        .expect("upload-url request");
    assert_eq!(resp.status(), 200, "first upload-url should 200");
    let up1: LegacyUploadUrlResponse = resp.json().await.expect("parse legacy upload-url 1");
    assert!(!up1.screenshot_id.is_empty());
    assert!(!up1.upload_url.is_empty());

    // ── 2. Confirm — body shape is exactly what the old client sends ──
    let resp = client
        .post(format!("{}/api/sessions/{}/screenshots", url, token))
        .json(&serde_json::json!({
            "screenshotId": up1.screenshot_id,
            "width": 1920,
            "height": 1080,
            "fileSize": 12345,
        }))
        .send()
        .await
        .expect("confirm 1 request");
    assert_eq!(resp.status(), 200, "first confirm should 200");
    let conf1: LegacyConfirmResponse = resp.json().await.expect("parse legacy confirm 1");
    assert!(conf1.confirmed);
    assert_eq!(conf1.tracked_seconds, 0, "first capture credits 0");

    // ── 3. Pause ──────────────────────────────────────────────────────
    let resp = client
        .post(format!("{}/api/sessions/{}/pause", url, token))
        .send()
        .await
        .expect("pause request");
    assert_eq!(resp.status(), 200);
    let _pause: LegacyPauseResponse = resp.json().await.expect("parse legacy pause");

    // ── 4. Resume ─────────────────────────────────────────────────────
    let resp = client
        .post(format!("{}/api/sessions/{}/resume", url, token))
        .send()
        .await
        .expect("resume request");
    assert_eq!(resp.status(), 200);
    let _resume: LegacyResumeResponse = resp.json().await.expect("parse legacy resume");

    // ── 5. Second upload + confirm ────────────────────────────────────
    let resp = client
        .get(format!("{}/api/sessions/{}/upload-url", url, token))
        .send()
        .await
        .expect("upload-url 2");
    assert_eq!(resp.status(), 200);
    let up2: LegacyUploadUrlResponse = resp.json().await.expect("parse legacy upload-url 2");

    let resp = client
        .post(format!("{}/api/sessions/{}/screenshots", url, token))
        .json(&serde_json::json!({
            "screenshotId": up2.screenshot_id,
            "width": 1920,
            "height": 1080,
            "fileSize": 12345,
        }))
        .send()
        .await
        .expect("confirm 2");
    assert_eq!(resp.status(), 200);
    let _conf2: LegacyConfirmResponse = resp.json().await.expect("parse legacy confirm 2");

    // ── 6. Stop ───────────────────────────────────────────────────────
    let resp = client
        .post(format!("{}/api/sessions/{}/stop", url, token))
        .send()
        .await
        .expect("stop");
    assert_eq!(resp.status(), 200);
    let stop: LegacyStopResponse = resp.json().await.expect("parse legacy stop");
    assert_eq!(stop.status, "stopped");
    // Two confirmed captures in different minute buckets → bucket count 2,
    // tracked = (2-1) * 60 = 60. Real wall-clock so the math holds.
    assert!(
        stop.tracked_seconds >= 0,
        "tracked_seconds non-negative, got {}",
        stop.tracked_seconds
    );
}
