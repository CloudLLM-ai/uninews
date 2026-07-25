//! Hermetic tests for the Playwright bot-protection path.
//!
//! Pure helpers are always exercised. The end-to-end orchestration test
//! serves a Cloudflare-style first response then a real article on the
//! second request (reqwest gets wall; Playwright re-fetch gets content).
//! It requires Node + Chromium; when browsers are missing it still asserts
//! that Playwright was attempted and the scrape failed cleanly (no panic).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use uninews::{
    is_falsy_env_flag, looks_like_browser_not_installed_message, parse_playwright_timeout_ms,
    set_event_listener, universal_scrape, ScrapeEvent, DEFAULT_PLAYWRIGHT_TIMEOUT_MS,
};

// ── pure helpers ──────────────────────────────────────────────────────────

/// Verifies falsy env-flag tokens match the public contract used by
/// `playwright_enabled` / archive toggles.
#[test]
fn falsy_env_flag_recognizes_documented_tokens() {
    for value in ["0", "false", "FALSE", "no", "off", " Off ", "\tNo\n"] {
        assert!(is_falsy_env_flag(value), "{value:?} should be falsy");
    }
    for value in ["1", "true", "yes", "", "maybe", "enable"] {
        assert!(!is_falsy_env_flag(value), "{value:?} should not be falsy");
    }
}

/// Verifies timeout parsing: positive ms win; invalid/zero/missing → default.
#[test]
fn playwright_timeout_parser_defaults_on_invalid_or_zero() {
    assert_eq!(
        parse_playwright_timeout_ms(None),
        DEFAULT_PLAYWRIGHT_TIMEOUT_MS
    );
    assert_eq!(parse_playwright_timeout_ms(Some("60000")), 60_000);
    assert_eq!(parse_playwright_timeout_ms(Some(" 12000 ")), 12_000);
    assert_eq!(
        parse_playwright_timeout_ms(Some("0")),
        DEFAULT_PLAYWRIGHT_TIMEOUT_MS
    );
    assert_eq!(
        parse_playwright_timeout_ms(Some("nope")),
        DEFAULT_PLAYWRIGHT_TIMEOUT_MS
    );
    assert_eq!(
        parse_playwright_timeout_ms(Some("")),
        DEFAULT_PLAYWRIGHT_TIMEOUT_MS
    );
}

/// Verifies the auto-install retry trigger classifies missing-browser
/// messages without launching Playwright.
#[test]
fn browser_not_installed_message_classifier() {
    assert!(looks_like_browser_not_installed_message(
        "Browser 'chromium' is not installed."
    ));
    assert!(looks_like_browser_not_installed_message(
        "Playwright Chromium is not installed (BrowserNotInstalled)"
    ));
    assert!(looks_like_browser_not_installed_message(
        "error: browser_not_installed"
    ));
    assert!(!looks_like_browser_not_installed_message(
        "Timeout waiting for selector article"
    ));
    assert!(!looks_like_browser_not_installed_message(
        "Failed to connect to Playwright server"
    ));
}

// ── hermetic orchestration ────────────────────────────────────────────────

/// Serve CF-challenge on the first accept, real article HTML on later ones.
///
/// That models: plain HTTP hits the wall; Playwright's re-navigation sees
/// the article (no JS challenge required — we only need a second response).
fn spawn_cf_then_article_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_thread = Arc::clone(&hits);

    std::thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let n = hits_thread.fetch_add(1, Ordering::SeqCst);

            let (status_line, body) = if n == 0 {
                (
                    "HTTP/1.1 403 Forbidden",
                    "<html><head><title>Just a moment...</title></head>\
                     <body><div class=\"cf-browser-verification\"></div></body></html>",
                )
            } else {
                (
                    "HTTP/1.1 200 OK",
                    "<!DOCTYPE html><html><head>\
                     <title>Hermetic Test Article</title>\
                     <meta property=\"og:title\" content=\"Hermetic Test Article\"/>\
                     <meta name=\"author\" content=\"Uninews Tester\"/>\
                     </head><body><article>\
                     <h1>Hermetic Test Article</h1>\
                     <p>This is a sufficiently long body paragraph for the content \
                     extractor to accept the page as a real news article rather than \
                     an empty shell or teaser card. Playwright should recover this \
                     text after the first-request Cloudflare wall.</p>\
                     <p>Second paragraph keeps the cleaned content well above the \
                     minimum meaningful-content threshold used by the HTML cleaner.</p>\
                     </article></body></html>",
                )
            };

            let response = format!(
                "{status_line}\r\nserver: cloudflare\r\ncf-ray: test-uninews-pw\r\n\
                 content-type: text/html; charset=utf-8\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    format!("http://127.0.0.1:{port}/story")
}

/// End-to-end: first HTTP response is a Cloudflare wall; Playwright re-fetch
/// should recover the article (or report a clean Playwright failure when
/// browsers are unavailable). Never panics; never invents content without
/// a PlaywrightSucceeded event.
#[tokio::test]
async fn bot_wall_triggers_playwright_and_recovers_or_fails_cleanly() {
    let events: Arc<Mutex<Vec<ScrapeEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    set_event_listener(Some(Arc::new(move |event: &ScrapeEvent| {
        sink.lock().unwrap().push(event.clone());
    })));

    let url = spawn_cf_then_article_server();
    // Disable archive.org so a Playwright miss does not chain into network
    // noise; this test is about the Playwright segment only.
    // SAFETY: process-local env mutation for a single integration test.
    unsafe {
        std::env::set_var("UNINEWS_ARCHIVE_FALLBACK", "0");
        // Keep Playwright on (default); give it enough budget for cold start.
        std::env::set_var("UNINEWS_PLAYWRIGHT", "1");
        std::env::set_var("UNINEWS_PLAYWRIGHT_TIMEOUT_MS", "60000");
    }

    let post = universal_scrape(&url, "english", None).await;

    set_event_listener(None);
    unsafe {
        std::env::remove_var("UNINEWS_ARCHIVE_FALLBACK");
        std::env::remove_var("UNINEWS_PLAYWRIGHT");
        std::env::remove_var("UNINEWS_PLAYWRIGHT_TIMEOUT_MS");
    }

    let recorded = events.lock().unwrap().clone();
    let has = |predicate: fn(&ScrapeEvent) -> bool| recorded.iter().any(predicate);

    assert!(
        has(|e| matches!(e, ScrapeEvent::BotProtectionDetected { .. })),
        "expected BotProtectionDetected, got: {recorded:?}"
    );
    assert!(
        has(|e| matches!(e, ScrapeEvent::PlaywrightFallbackStarted { .. })),
        "expected PlaywrightFallbackStarted, got: {recorded:?}"
    );

    let playwright_ok = has(|e| matches!(e, ScrapeEvent::PlaywrightFallbackSucceeded { .. }));
    let playwright_fail = has(|e| matches!(e, ScrapeEvent::PlaywrightFallbackFailed { .. }));
    assert!(
        playwright_ok || playwright_fail,
        "expected Playwright success or failure event, got: {recorded:?}"
    );

    if playwright_ok {
        // Content must have been extracted from the second (article) response.
        // LLM may still fail without a key; title/body live on the Post either way.
        assert!(
            post.title.to_ascii_lowercase().contains("hermetic")
                || post.content.to_ascii_lowercase().contains("hermetic")
                || post.error.is_empty(),
            "PlaywrightSucceeded should surface article text (or clean LLM success); \
             title={:?} content_len={} error={}",
            post.title,
            post.content.len(),
            post.error
        );
        assert!(
            !has(|e| matches!(e, ScrapeEvent::ArchiveFallbackStarted { .. })),
            "archive.org must not run after Playwright success"
        );
    } else {
        // No browsers / Node: fail cleanly with an error string, never empty success.
        assert!(
            !post.error.is_empty(),
            "Playwright failure must surface an error, got empty success"
        );
    }
}
