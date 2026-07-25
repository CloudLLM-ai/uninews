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
    playwright_autoinstall_enabled, playwright_overall_budget_ms, set_event_listener,
    universal_scrape, ScrapeEvent, CHROME_DUMP_DOM_DEADLINE_MS, DEFAULT_PLAYWRIGHT_TIMEOUT_MS,
    PLAYWRIGHT_OVERALL_GRACE_MS, UNINEWS_PLAYWRIGHT_AUTOINSTALL_ENV,
    UNINEWS_PLAYWRIGHT_TIMEOUT_MS_ENV,
};

/// Serializes tests that mutate process-wide env vars. Sync tests hold it
/// only for short critical sections (never across `.await`).
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// RAII helper: temporarily override an env var, restore on drop.
struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: process-local env mutation for a single integration test,
        // serialized with sibling env-mutating tests via ENV_LOCK.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }

    fn unset(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: see `set`.
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: see `set`.
        unsafe {
            match self.previous.as_deref() {
                Some(previous) => std::env::set_var(self.key, previous),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

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

/// Verifies `UNINEWS_PLAYWRIGHT_AUTOINSTALL` parsing mirrors
/// `playwright_enabled`: default on; falsy tokens off; junk stays on.
#[test]
fn playwright_autoinstall_enabled_parsing() {
    let _lock = ENV_LOCK.lock().unwrap();
    {
        let _guard = EnvVarGuard::unset(UNINEWS_PLAYWRIGHT_AUTOINSTALL_ENV);
        assert!(
            playwright_autoinstall_enabled(),
            "unset must default to enabled"
        );
    }
    for value in ["0", "false", "FALSE", "no", "off", " Off "] {
        let _guard = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_AUTOINSTALL_ENV, value);
        assert!(
            !playwright_autoinstall_enabled(),
            "{value:?} should disable auto-install"
        );
    }
    for value in ["1", "true", "yes", "", "junk"] {
        let _guard = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_AUTOINSTALL_ENV, value);
        assert!(
            playwright_autoinstall_enabled(),
            "{value:?} should keep auto-install enabled"
        );
    }
}

/// Verifies the overall-timeout wrapper budget is exactly the configured
/// per-step timeout plus the grace, and that the Chrome watchdog deadline
/// is the documented 60 s.
///
/// Reads the env defensively (observe → compute → re-observe) so a
/// concurrent end-to-end test toggling `UNINEWS_PLAYWRIGHT_TIMEOUT_MS`
/// cannot flake the assertion; no real browser is launched.
#[test]
fn playwright_overall_budget_is_timeout_plus_grace() {
    assert_eq!(PLAYWRIGHT_OVERALL_GRACE_MS, 15_000);
    assert_eq!(CHROME_DUMP_DOM_DEADLINE_MS, 60_000);

    for _ in 0..100 {
        let before = std::env::var(UNINEWS_PLAYWRIGHT_TIMEOUT_MS_ENV).ok();
        let observed = playwright_overall_budget_ms();
        let after = std::env::var(UNINEWS_PLAYWRIGHT_TIMEOUT_MS_ENV).ok();
        if before == after {
            assert_eq!(
                observed,
                parse_playwright_timeout_ms(before.as_deref()) + PLAYWRIGHT_OVERALL_GRACE_MS,
                "overall budget must be configured timeout + grace"
            );
            return;
        }
    }
    panic!("{UNINEWS_PLAYWRIGHT_TIMEOUT_MS_ENV} kept changing under the assertion");
}

/// Verifies the budget arithmetic against an explicit env override.
///
/// Retries until the override is observed stable: a racing end-to-end test
/// may briefly toggle the same var between our set and the assertion.
#[test]
fn playwright_overall_budget_honors_env_override() {
    let _lock = ENV_LOCK.lock().unwrap();
    for _ in 0..100 {
        let guard = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_TIMEOUT_MS_ENV, "30000");
        let observed = playwright_overall_budget_ms();
        let stable = std::env::var(UNINEWS_PLAYWRIGHT_TIMEOUT_MS_ENV).as_deref() == Ok("30000");
        drop(guard);
        if stable {
            assert_eq!(observed, 30_000 + PLAYWRIGHT_OVERALL_GRACE_MS);
            return;
        }
    }
    panic!("racing end-to-end test never let the override stick");
}

// ── hermetic orchestration ────────────────────────────────────────────────

/// Serializes the end-to-end tests below: they share the process-global
/// event listener and env vars, so they must not run concurrently. A
/// `tokio::sync::Mutex` (not `std`) so the guard may be held across `.await`
/// without tripping `clippy::await_holding_lock`.
static E2E_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    let _permit = E2E_LOCK.lock().await;
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

/// Serve the Cloudflare challenge on EVERY request: the plain-HTTP hit is
/// walled AND the Playwright re-render still lands on the interstitial.
/// Models a wall that headless Chromium cannot clear (security-critical:
/// the rendered DOM must never be mistaken for article content).
fn spawn_cf_always_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().unwrap().port();

    std::thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);

            let body = "<html><head><title>Just a moment...</title></head>\
                        <body><div class=\"cf-browser-verification\"></div></body></html>";
            let response = format!(
                "HTTP/1.1 403 Forbidden\r\nserver: cloudflare\r\ncf-ray: test-uninews-pw\r\n\
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

/// Security-critical arm: even when Playwright successfully renders the
/// page, a rendered DOM that is STILL a bot-protection wall must produce
/// `PlaywrightFallbackFailed` and never `PlaywrightFallbackSucceeded` or
/// invented content. Holds whether browsers are present (rendered wall
/// rejected) or absent (clean launch failure).
#[tokio::test]
async fn still_protected_rendered_dom_fails_without_success() {
    let _permit = E2E_LOCK.lock().await;
    let events: Arc<Mutex<Vec<ScrapeEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    set_event_listener(Some(Arc::new(move |event: &ScrapeEvent| {
        sink.lock().unwrap().push(event.clone());
    })));

    let url = spawn_cf_always_server();
    // SAFETY: process-local env mutation for a single integration test,
    // serialized with the sibling e2e test via E2E_LOCK.
    unsafe {
        std::env::set_var("UNINEWS_ARCHIVE_FALLBACK", "0");
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
    assert!(
        has(|e| matches!(e, ScrapeEvent::PlaywrightFallbackFailed { .. })),
        "a still-walled rendered DOM must emit PlaywrightFallbackFailed, got: {recorded:?}"
    );
    assert!(
        !has(|e| matches!(e, ScrapeEvent::PlaywrightFallbackSucceeded { .. })),
        "a still-walled rendered DOM must NEVER emit PlaywrightFallbackSucceeded, got: {recorded:?}"
    );
    assert!(
        !post.error.is_empty(),
        "still-protected page must surface an error, never an empty success"
    );
}
