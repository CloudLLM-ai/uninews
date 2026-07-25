//! Hermetic tests for the thin-content Playwright trigger in `src/web.rs`:
//! a successful (2xx), non-walled, non-X page whose extraction fails, or
//! whose raw body is under 16 KiB (JS shell), gets the same Playwright
//! render attempt the bot-wall path uses. The trigger is governed by the
//! existing `UNINEWS_PLAYWRIGHT` toggle — there is no separate env var.
//!
//! All fixtures are loopback `TcpListener` servers; archive.org is disabled
//! and `UNINEWS_LLM_CLIENT` points at a bogus provider so no stage can make
//! a live network call even if a stage boundary shifts (this dev shell may
//! export real LLM keys). `UNINEWS_PLAYWRIGHT_AUTOINSTALL=0` keeps a
//! missing Chromium from turning a test run into a browser download.
//!
//! Browser presence is tolerated, never required (mirroring
//! tests/playwright_fallback.rs): the assertions pin the
//! `PlaywrightFallbackStarted` event and the "never worse than the plain
//! path" contract, not a successful render.

use std::env;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use uninews::{
    set_event_listener, universal_scrape, ScrapeEvent, UNINEWS_ARCHIVE_FALLBACK_ENV,
    UNINEWS_PLAYWRIGHT_AUTOINSTALL_ENV, UNINEWS_PLAYWRIGHT_ENV, UNINEWS_PLAYWRIGHT_TIMEOUT_MS_ENV,
};

/// Serializes the end-to-end tests below: they share the process-global
/// event listener and env vars, so they must not run concurrently. A
/// `tokio::sync::Mutex` (not `std`) so the guard may be held across
/// `.await` without tripping `clippy::await_holding_lock`.
static E2E_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// RAII helper: temporarily override an env var, restore on drop.
struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = env::var(key).ok();
        // SAFETY: process-local env mutation for a single integration test,
        // serialized with sibling env-mutating tests via E2E_LOCK.
        unsafe {
            env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: see `set`.
        unsafe {
            match self.previous.as_deref() {
                Some(previous) => env::set_var(self.key, previous),
                None => env::remove_var(self.key),
            }
        }
    }
}

/// 16 KiB — mirrors `JS_SHELL_MAX_BYTES` in `src/web.rs` (a private const).
const JS_SHELL_MAX_BYTES: usize = 16 * 1024;

/// Sub-16KiB 200 page whose only payload is a `<script>`: extraction finds
/// no usable text and fails outright (longevity.technology-style, where the
/// plain fetch sees a teaser at best while the body is JS-gated).
const JUNK_BODY: &str = "<!DOCTYPE html><html><head><title>Junk</title></head>\
    <body><div id=\"app\"></div><script>window.__DATA__={};</script></body></html>";

/// Sub-16KiB 200 shell with a scrap of visible text: extraction "succeeds"
/// but the output is thin and linkless (axios.com/technology-style SPA
/// shell; the rendered DOM carries the real content).
const SHELL_BODY: &str = "<!DOCTYPE html><html><head><title>Shell</title></head>\
    <body><div id=\"root\">Loading articles</div><script src=\"/bundle.js\"></script></body></html>";

/// Sub-16KiB 200 page that nonetheless yields a GOOD (tiny) extraction.
/// Triggering here is acceptable by design: the render either confirms the
/// content or fails, and the plain result is kept — the trigger can never
/// make a scrape worse.
const TINY_ARTICLE_BODY: &str = "<!DOCTYPE html><html><head><title>Tiny</title></head>\
    <body><article><h1>Tiny but real</h1>\
    <p>A short but genuine article body the plain extractor handles fine.</p>\
    </article></body></html>";

/// Serve the same small HTML body with a 200 status on every request
/// (plain fetch plus any Playwright re-navigation). Returns the URL.
fn spawn_fixed_server(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().unwrap().port();

    std::thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    format!("http://127.0.0.1:{port}/article")
}

/// Register the process-wide event listener capturing into a shared vec.
fn register_event_sink() -> Arc<Mutex<Vec<ScrapeEvent>>> {
    let events: Arc<Mutex<Vec<ScrapeEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    set_event_listener(Some(Arc::new(move |event: &ScrapeEvent| {
        sink.lock().unwrap().push(event.clone());
    })));
    events
}

/// `UNINEWS_PLAYWRIGHT=0`: a sub-16KiB shell page must NOT trigger any
/// Playwright attempt — the thin-content trigger is governed by the same
/// toggle as the wall trigger. The plain (thin) result comes back as-is.
#[tokio::test]
async fn disabled_playwright_never_triggers_on_thin_shell() {
    let _permit = E2E_LOCK.lock().await;
    let _pw = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_ENV, "0");
    let _archive = EnvVarGuard::set(UNINEWS_ARCHIVE_FALLBACK_ENV, "0");
    let _llm = EnvVarGuard::set("UNINEWS_LLM_CLIENT", "definitely-not-a-provider");

    let events = register_event_sink();
    let url = spawn_fixed_server(SHELL_BODY);
    let post = universal_scrape(&url, "english", None).await;
    set_event_listener(None);

    assert!(
        SHELL_BODY.len() < JS_SHELL_MAX_BYTES,
        "fixture must sit under the shell threshold"
    );
    let recorded = events.lock().unwrap().clone();
    assert!(
        !recorded
            .iter()
            .any(|e| matches!(e, ScrapeEvent::PlaywrightFallbackStarted { .. })),
        "UNINEWS_PLAYWRIGHT=0 must suppress the thin-content trigger, got: {recorded:?}"
    );
    // The plain shell extraction is thin but succeeds; the bogus LLM
    // provider may stamp an error afterwards — the contract here is that
    // the PLAIN content is what came back.
    assert!(
        post.content.contains("Loading articles"),
        "the plain shell result must be returned untouched, content={:?} error={}",
        post.content,
        post.error
    );
}

/// With Playwright enabled, the thin-content trigger fires for
///   (a) a 200 page whose body fails extraction outright (junk shell), and
///   (b) a sub-16KiB 200 page whose extraction is thin but "successful",
/// and also for
///   (c) a sub-16KiB page with a GOOD tiny extraction — triggering there
///       is acceptable by design (render confirms or plain result kept).
///
/// All three scenarios share the same env, so they run sequentially in one
/// test under one set of guards (never holding a std::Mutex across
/// `.await`). Whether a real browser is present only changes which
/// terminal Playwright event fires; the assertions hold either way.
#[tokio::test]
async fn thin_content_trigger_fires_and_never_regresses_plain_result() {
    let _permit = E2E_LOCK.lock().await;
    let _pw = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_ENV, "1");
    let _archive = EnvVarGuard::set(UNINEWS_ARCHIVE_FALLBACK_ENV, "0");
    let _noinstall = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_AUTOINSTALL_ENV, "0");
    let _timeout = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_TIMEOUT_MS_ENV, "30000");
    let _llm = EnvVarGuard::set("UNINEWS_LLM_CLIENT", "definitely-not-a-provider");

    let events = register_event_sink();

    // (a) Extraction failure on a healthy 200 page → trigger fires.
    let url = spawn_fixed_server(JUNK_BODY);
    let post = universal_scrape(&url, "english", None).await;
    let recorded = events.lock().unwrap().clone();
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, ScrapeEvent::PlaywrightFallbackStarted { .. })),
        "(a) extraction failure on a 200 page must trigger Playwright, got: {recorded:?}"
    );
    // A script-only page yields nothing even when a real browser renders
    // it, so the attempt must fail cleanly and never invent success.
    assert!(
        !recorded
            .iter()
            .any(|e| matches!(e, ScrapeEvent::PlaywrightFallbackSucceeded { .. })),
        "(a) an unrecoverable junk page must never emit PlaywrightFallbackSucceeded, got: {recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, ScrapeEvent::PlaywrightFallbackFailed { .. })),
        "(a) the failed attempt must surface PlaywrightFallbackFailed, got: {recorded:?}"
    );
    assert!(
        !post.error.is_empty(),
        "(a) the original extraction error must survive the failed render"
    );
    events.lock().unwrap().clear();

    // (b) Sub-16KiB shell with thin-but-successful extraction → trigger
    // fires on body size alone.
    let url = spawn_fixed_server(SHELL_BODY);
    let post = universal_scrape(&url, "english", None).await;
    let recorded = events.lock().unwrap().clone();
    assert!(
        recorded.iter().any(
            |e| matches!(e, ScrapeEvent::FetchSucceeded { body_bytes, .. } if *body_bytes < JS_SHELL_MAX_BYTES)
        ),
        "(b) fixture sanity: the plain fetch must see a sub-threshold body, got: {recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, ScrapeEvent::PlaywrightFallbackStarted { .. })),
        "(b) a sub-16KiB shell must trigger Playwright, got: {recorded:?}"
    );
    // No worse than the plain path: whether the render failed (no browser)
    // and the plain result was kept, or a real browser re-extracted the
    // same shell text, the shell's text survives.
    assert!(
        post.content.contains("Loading articles"),
        "(b) the plain shell content must survive the trigger, content={:?} error={}",
        post.content,
        post.error
    );
    events.lock().unwrap().clear();

    // (c) Sub-16KiB page with GOOD extraction → trigger still fires (the
    // render either confirms the content or fails), and the plain content
    // survives when the render fails.
    let url = spawn_fixed_server(TINY_ARTICLE_BODY);
    let post = universal_scrape(&url, "english", None).await;
    let recorded = events.lock().unwrap().clone();
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, ScrapeEvent::PlaywrightFallbackStarted { .. })),
        "(c) a good-but-tiny page still triggers (body-size condition), got: {recorded:?}"
    );
    assert!(
        post.content.contains("short but genuine article body"),
        "(c) good plain content must survive even when the render fails, content={:?} error={}",
        post.content,
        post.error
    );

    // (d) LARGE body (> 16 KiB) whose extraction "succeeds" with an
    // implausibly short result (< 512 bytes) → trigger fires on
    // MIN_CONTENT_BYTES. This is the longevity.technology shape: a big
    // JS-driven page where plain extraction keeps only a title + teaser.
    let big_body: &'static str = Box::leak(
        format!(
            "<!DOCTYPE html><html><head><title>Teaser</title></head><body>\
             <article><h1>Brain scans hint at new way to slow Alzheimer's</h1>\
             <p>Short teaser only.</p></article>\
             <!-- {} --></body></html>",
            "p".repeat(JS_SHELL_MAX_BYTES)
        )
        .into_boxed_str(),
    );
    let url = spawn_fixed_server(big_body);
    let post = universal_scrape(&url, "english", None).await;
    let recorded = events.lock().unwrap().clone();
    assert!(
        recorded.iter().any(
            |e| matches!(e, ScrapeEvent::FetchSucceeded { body_bytes, .. } if *body_bytes >= JS_SHELL_MAX_BYTES)
        ),
        "(d) fixture sanity: the plain fetch must see a LARGE body, got: {recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, ScrapeEvent::PlaywrightFallbackStarted { .. })),
        "(d) a large page with sub-512-byte content must trigger Playwright, got: {recorded:?}"
    );
    // No worse than the plain path: the teaser survives a failed render.
    assert!(
        post.content.contains("Short teaser only"),
        "(d) the plain teaser content must survive the trigger, content={:?} error={}",
        post.content,
        post.error
    );

    set_event_listener(None);
}
