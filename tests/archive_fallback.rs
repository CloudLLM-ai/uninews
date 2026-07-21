//! Integration tests for the archive.org fallback module: the enable/disable
//! env-var switch, the bot-protection heuristics, and the Wayback
//! availability-response parser. All exercised without network access.

use std::env;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use reqwest::header::HeaderMap;
use uninews::archive::{
    archive_fallback_enabled, looks_like_bot_protection, parse_availability_response,
    UNINEWS_ARCHIVE_FALLBACK_ENV,
};
use uninews::{set_event_listener, universal_scrape, ScrapeEvent};

/// Serializes tests that mutate `UNINEWS_ARCHIVE_FALLBACK`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// RAII helper: temporarily override an env var, restore on drop.
struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = env::var(key).ok();
        unsafe {
            env::set_var(key, value);
        }
        Self { key, previous }
    }

    fn unset(key: &'static str) -> Self {
        let previous = env::var(key).ok();
        unsafe {
            env::remove_var(key);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            match self.previous.as_deref() {
                Some(previous) => env::set_var(self.key, previous),
                None => env::remove_var(self.key),
            }
        }
    }
}

// ── archive_fallback_enabled ──────────────────────────────────────────────

#[test]
fn archive_fallback_is_enabled_by_default() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = EnvVarGuard::unset(UNINEWS_ARCHIVE_FALLBACK_ENV);
    assert!(archive_fallback_enabled());
}

#[test]
fn archive_fallback_is_disabled_by_falsy_values() {
    let _lock = ENV_LOCK.lock().unwrap();
    for value in ["0", "false", "FALSE", "no", "off", " Off "] {
        let _guard = EnvVarGuard::set(UNINEWS_ARCHIVE_FALLBACK_ENV, value);
        assert!(
            !archive_fallback_enabled(),
            "value {:?} should disable the fallback",
            value
        );
    }
}

#[test]
fn archive_fallback_stays_enabled_for_other_values() {
    let _lock = ENV_LOCK.lock().unwrap();
    for value in ["1", "true", "yes", ""] {
        let _guard = EnvVarGuard::set(UNINEWS_ARCHIVE_FALLBACK_ENV, value);
        assert!(
            archive_fallback_enabled(),
            "value {:?} should keep the fallback enabled",
            value
        );
    }
}

// ── looks_like_bot_protection ─────────────────────────────────────────────

#[test]
fn detects_cloudflare_challenge_page() {
    let body = r#"<!DOCTYPE html><html><head><title>Just a moment...</title></head>
        <body><div class="cf-browser-verification"></div></body></html>"#;
    assert!(looks_like_bot_protection(403, &HeaderMap::new(), body));
}

#[test]
fn detects_javascript_required_interstitial() {
    let body = "<html><body><p>Enable JavaScript and cookies to continue</p></body></html>";
    assert!(looks_like_bot_protection(200, &HeaderMap::new(), body));
}

#[test]
fn detects_cloudflare_403_via_headers_without_body_markers() {
    let mut headers = HeaderMap::new();
    headers.insert("server", "cloudflare".parse().unwrap());
    assert!(looks_like_bot_protection(403, &headers, "<html></html>"));

    let mut headers = HeaderMap::new();
    headers.insert("cf-ray", "8f00baadf00dbabe-SJC".parse().unwrap());
    assert!(looks_like_bot_protection(429, &headers, "<html></html>"));
}

#[test]
fn does_not_flag_normal_article_pages() {
    let body = "<html><head><title>Daily News</title></head><body><article>\
        <p>The mayor announced a new transit plan on Tuesday.</p></article></body></html>";
    assert!(!looks_like_bot_protection(200, &HeaderMap::new(), body));
}

#[test]
fn does_not_flag_plain_403_without_cloudflare_evidence() {
    assert!(!looks_like_bot_protection(
        403,
        &HeaderMap::new(),
        "<html><body>Forbidden</body></html>"
    ));
}

// ── parse_availability_response ───────────────────────────────────────────

#[test]
fn parses_available_snapshot_and_upgrades_to_https() {
    let body = r#"{
        "url": "https://example.com/article",
        "archived_snapshots": {
            "closest": {
                "available": true,
                "url": "http://web.archive.org/web/20240101000000/https://example.com/article",
                "timestamp": "20240101000000",
                "status": "200"
            }
        }
    }"#;

    let snapshot = parse_availability_response(body).expect("snapshot should parse");
    assert_eq!(
        snapshot.url,
        "https://web.archive.org/web/20240101000000/https://example.com/article"
    );
    assert_eq!(snapshot.timestamp, "20240101000000");
}

#[test]
fn returns_none_when_no_snapshot_exists() {
    let body = r#"{"url": "https://example.com/nope", "archived_snapshots": {}}"#;
    assert_eq!(parse_availability_response(body), None);
}

#[test]
fn returns_none_for_unavailable_or_non_200_snapshots() {
    let unavailable = r#"{"archived_snapshots": {"closest": {
        "available": false,
        "url": "https://web.archive.org/web/20240101000000/https://example.com/a",
        "timestamp": "20240101000000",
        "status": "200"
    }}}"#;
    assert_eq!(parse_availability_response(unavailable), None);

    let redirected = r#"{"archived_snapshots": {"closest": {
        "available": true,
        "url": "https://web.archive.org/web/20240101000000/https://example.com/a",
        "timestamp": "20240101000000",
        "status": "301"
    }}}"#;
    assert_eq!(parse_availability_response(redirected), None);
}

#[test]
fn returns_none_for_malformed_json() {
    assert_eq!(parse_availability_response("not json"), None);
    assert_eq!(parse_availability_response("{}"), None);
}

// ── end-to-end: protected page → archive fallback orchestration ───────────

/// Serve a single Cloudflare-style 403 challenge page on a loopback port
/// and return the URL to request.
fn spawn_cloudflare_challenge_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().unwrap().port();

    std::thread::spawn(move || {
        // The pipeline may issue more than one request (initial fetch only,
        // but be tolerant); serve until the listener stops accepting.
        for stream in listener.incoming().take(4) {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let body = "<html><head><title>Just a moment...</title></head>\
                        <body><div class=\"cf-browser-verification\"></div></body></html>";
            let response = format!(
                "HTTP/1.1 403 Forbidden\r\nserver: cloudflare\r\ncf-ray: test-uninews\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    format!("http://127.0.0.1:{}/protected-article", port)
}

/// End-to-end: a Cloudflare-walled page must trigger the archive.org
/// fallback chain and emit the matching events. The archive lookup itself
/// hits the real archive.org (a 127.0.0.1 URL has no snapshots), so this
/// test tolerates both the "not found" and the "lookup failed" outcomes —
/// what it pins down is detection + orchestration + event emission.
#[tokio::test]
async fn cloudflare_challenge_triggers_archive_fallback_events() {
    // NOTE: this test is the only event-listener user in this test binary,
    // so no guard is needed around the process-wide listener slot (and a
    // std::Mutex guard must not be held across the await below anyway).
    let events: Arc<Mutex<Vec<ScrapeEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    set_event_listener(Some(Arc::new(move |event: &ScrapeEvent| {
        sink.lock().unwrap().push(event.clone());
    })));

    let url = spawn_cloudflare_challenge_server();
    let post = universal_scrape(&url, "english", None).await;

    set_event_listener(None);

    assert!(
        !post.error.is_empty(),
        "protected page with no usable snapshot must still surface an error"
    );
    assert!(
        post.error.contains("archive.org"),
        "error should report the archive.org fallback outcome, got: {}",
        post.error
    );

    let recorded = events.lock().unwrap();
    let has = |predicate: fn(&ScrapeEvent) -> bool| recorded.iter().any(predicate);

    assert!(
        has(|e| matches!(e, ScrapeEvent::BotProtectionDetected { .. })),
        "expected BotProtectionDetected, got: {:?}",
        *recorded
    );
    assert!(
        has(|e| matches!(e, ScrapeEvent::ArchiveFallbackStarted { .. })),
        "expected ArchiveFallbackStarted, got: {:?}",
        *recorded
    );
    assert!(
        has(|e| matches!(e, ScrapeEvent::ArchiveSnapshotNotFound { .. }))
            || has(|e| matches!(e, ScrapeEvent::ArchiveSnapshotFound { .. })),
        "expected an archive snapshot outcome event, got: {:?}",
        *recorded
    );
    assert!(
        has(|e| matches!(e, ScrapeEvent::ScrapeFailed { .. })),
        "expected ScrapeFailed, got: {:?}",
        *recorded
    );
}
