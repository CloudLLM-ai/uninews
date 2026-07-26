//! Integration tests for the host-provided content fallback hook
//! (`set_content_fallback`), exercised through the public
//! `universal_scrape` entry point.
//!
//! Hermetic: loopback HTTP servers answer the plain fetches, Playwright
//! and archive.org are disabled, and `UNINEWS_LLM_CLIENT` points at a
//! bogus provider so the LLM stage can never make a live call (the LLM
//! error may be stamped onto otherwise-successful posts; tests assert on
//! content, matching the web_pipeline contract).

use std::env;
use std::io::Write;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use uninews::{
    is_youtube_url, set_content_fallback, universal_scrape, ContentFallback,
    UNINEWS_ARCHIVE_FALLBACK_ENV, UNINEWS_PLAYWRIGHT_ENV,
};

/// Serializes every test in this file: the content-fallback hook and the
/// env overrides are process-global, and `cargo test` runs tests in this
/// binary on multiple threads. Locking at the top of each test keeps hook
/// installs from racing one another. `tokio::sync::Mutex` (not std) so the
/// guard may be held across `.await` points without clippy's
/// `await_holding_lock` firing.
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

/// RAII helper: restore the previous content-fallback hook on drop.
struct HookGuard {
    previous: Option<uninews::ContentFallbackHook>,
}

impl HookGuard {
    fn install(hook: uninews::ContentFallbackHook) -> Self {
        let previous = set_content_fallback(Some(hook));
        Self { previous }
    }
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        set_content_fallback(self.previous.take());
    }
}

/// Spawn a loopback HTTP server that answers one request with the given
/// status line and body. Returns the server URL.
fn spawn_one_shot_server(status: &str, body: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback server");
    let addr = listener.local_addr().expect("local addr");
    let status = status.to_string();
    let body = body.to_string();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut request = [0u8; 4096];
            let _ = std::io::Read::read(&mut stream, &mut request);
            let header = format!(
                "HTTP/1.1 {}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                status,
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(body.as_bytes());
        }
    });
    format!("http://{}/", addr)
}

/// A Cloudflare-style challenge interstitial (matches the bot-protection
/// heuristics: 403 + "Attention Required" / "Just a moment" markers).
const CF_WALL_BODY: &str = "<!DOCTYPE html><html><head><title>Attention Required! | Cloudflare</title></head><body><h1>Sorry, you have been blocked</h1></body></html>";

/// A real article page with enough body text to extract — and, crucially,
/// a raw body ABOVE the 16 KiB JS-shell threshold so the plain fetch does
/// not trip the thin-content trigger (which would legitimately consult
/// the hook).
fn article_body(marker: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><title>Test Article</title></head><body><article><h1>Headline</h1><p>{}</p><p>{}</p></article></body></html>",
        marker,
        "real article body text. ".repeat(800)
    )
}

// ── is_youtube_url ────────────────────────────────────────────────────────────

#[test]
fn youtube_url_shapes() {
    assert!(is_youtube_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
    assert!(is_youtube_url("https://youtube.com/watch?v=dQw4w9WgXcQ"));
    assert!(is_youtube_url("https://youtu.be/dQw4w9WgXcQ"));
    assert!(is_youtube_url("https://www.youtube.com/@channel/videos"));
    assert!(!is_youtube_url("https://example.com/watch?v=dQw4w9WgXcQ"));
    assert!(!is_youtube_url("https://notyoutube.com/watch?v=x"));
}

// ── hook registration ─────────────────────────────────────────────────────────

#[tokio::test]
async fn set_content_fallback_returns_previous() {
    let _lock = TEST_LOCK.lock().await;
    let hook_a: uninews::ContentFallbackHook = Arc::new(|url| {
        Box::pin(async move { Err(format!("no fallback for {url}")) })
    });
    let hook_b: uninews::ContentFallbackHook = Arc::new(|url| {
        Box::pin(async move { Err(format!("no fallback for {url}")) })
    });

    let prev = set_content_fallback(Some(hook_a));
    assert!(prev.is_none(), "fresh process should have no hook");

    let prev = set_content_fallback(Some(hook_b));
    assert!(prev.is_some(), "replacing returns the previous hook");

    let prev = set_content_fallback(None);
    assert!(prev.is_some(), "uninstalling returns the previous hook");

    let prev = set_content_fallback(None);
    assert!(prev.is_none(), "hook slot is empty again");
}

// ── YouTube pre-fetch consultation ────────────────────────────────────────────

/// YouTube URL + hook returning Extracted → the transcript becomes the
/// post content without any HTTP fetch being attempted (the loopback
/// server would 403 anyway; the hook must win before the fetch).
#[tokio::test]
async fn youtube_url_uses_hook_extracted_content() {
    let _lock = TEST_LOCK.lock().await;
    let _playwright = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_ENV, "0");
    let _archive = EnvVarGuard::set(UNINEWS_ARCHIVE_FALLBACK_ENV, "0");
    let _llm = EnvVarGuard::set("UNINEWS_LLM_CLIENT", "definitely-not-a-provider");

    // Even though the URL is youtube-shaped, no network is needed: the
    // hook answers before any fetch. A bogus host makes a fetch fatal.
    let url = "https://www.youtube.com/watch?v=loopback00";

    let _hook = HookGuard::install(Arc::new(|_url| {
        Box::pin(async move {
            Ok(ContentFallback::Extracted {
                title: Some("Test Video Title".to_string()),
                content: "TRANSCRIPT-MARKER: hello world transcript".to_string(),
            })
        })
    }));

    let post = universal_scrape(url, "english", None).await;

    assert!(
        post.content.contains("TRANSCRIPT-MARKER"),
        "hook content must flow into the post, content={:?} error={:?}",
        post.content,
        post.error
    );
    assert_eq!(post.title, "Test Video Title");
}

/// YouTube URL + hook returning an error → the pipeline falls back to the
/// normal fetch chain (which here hits a loopback CF wall and fails, but
/// must at least ATTEMPT the fetch — proving the hook error did not
/// short-circuit the built-in chain).
#[tokio::test]
async fn youtube_url_hook_error_falls_through_to_normal_chain() {
    let _lock = TEST_LOCK.lock().await;
    let _playwright = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_ENV, "0");
    let _archive = EnvVarGuard::set(UNINEWS_ARCHIVE_FALLBACK_ENV, "0");
    let _llm = EnvVarGuard::set("UNINEWS_LLM_CLIENT", "definitely-not-a-provider");

    let server = spawn_one_shot_server("403 Forbidden", CF_WALL_BODY);
    // Rewrite the loopback URL into youtube shape: path-based override is
    // not possible (host must contain youtube.com), so instead verify the
    // fall-through with a youtube-shaped URL whose fetch necessarily
    // fails (unroutable), and assert the failure mentions the wall/fetch
    // path rather than the hook error.
    let _ = server;

    let _hook = HookGuard::install(Arc::new(|_url| {
        Box::pin(async move { Err("hook has nothing for this video".to_string()) })
    }));

    let post = universal_scrape(
        "https://www.youtube.com/watch?v=definitely-bogus-unroutable",
        "english",
        None,
    )
    .await;

    assert!(
        !post.error.is_empty(),
        "bogus youtube URL without hook content must fail"
    );
    assert!(
        !post.error.contains("hook has nothing"),
        "hook error must not mask the pipeline's own error chain, got: {}",
        post.error
    );
}

// ── Wall consultation (after Playwright, before archive.org) ─────────────────

/// Bot wall + Playwright disabled + hook returning a rendered DOM → the
/// hook's DOM is extracted and returned.
#[tokio::test]
async fn walled_page_uses_hook_rendered_dom() {
    let _lock = TEST_LOCK.lock().await;
    let _playwright = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_ENV, "0");
    let _archive = EnvVarGuard::set(UNINEWS_ARCHIVE_FALLBACK_ENV, "0");
    let _llm = EnvVarGuard::set("UNINEWS_LLM_CLIENT", "definitely-not-a-provider");

    let server = spawn_one_shot_server("403 Forbidden", CF_WALL_BODY);

    let _hook = HookGuard::install(Arc::new(|_url| {
        Box::pin(async move { Ok(ContentFallback::RenderedDom(article_body("HOOK-RENDERED-MARKER"))) })
    }));

    let post = universal_scrape(&server, "english", None).await;

    assert!(
        post.content.contains("HOOK-RENDERED-MARKER"),
        "hook-rendered DOM must be extracted, content={:?} error={:?}",
        post.content,
        post.error
    );
}

/// Bot wall + hook returning a STILL-WALLED DOM → re-validation rejects it
/// and the pipeline surfaces the wall error (the hook must not be able to
/// smuggle a challenge page in as content).
#[tokio::test]
async fn walled_page_rejects_still_walled_hook_dom() {
    let _lock = TEST_LOCK.lock().await;
    let _playwright = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_ENV, "0");
    let _archive = EnvVarGuard::set(UNINEWS_ARCHIVE_FALLBACK_ENV, "0");
    let _llm = EnvVarGuard::set("UNINEWS_LLM_CLIENT", "definitely-not-a-provider");

    let server = spawn_one_shot_server("403 Forbidden", CF_WALL_BODY);

    let _hook = HookGuard::install(Arc::new(|_url| {
        Box::pin(async move { Ok(ContentFallback::RenderedDom(CF_WALL_BODY.to_string())) })
    }));

    let post = universal_scrape(&server, "english", None).await;

    assert!(
        !post.error.is_empty(),
        "still-walled hook DOM must not count as success"
    );
    assert!(
        !post.content.contains("Headline"),
        "no extracted article content may come out of a wall DOM, content={:?}",
        post.content
    );
}

/// Bot wall + NO hook installed → behavior is exactly the built-in chain
/// (fails with the wall error; guards against the hook changing the
/// no-hook path at all).
#[tokio::test]
async fn walled_page_without_hook_behaves_like_baseline() {
    let _lock = TEST_LOCK.lock().await;
    let _playwright = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_ENV, "0");
    let _archive = EnvVarGuard::set(UNINEWS_ARCHIVE_FALLBACK_ENV, "0");
    let _llm = EnvVarGuard::set("UNINEWS_LLM_CLIENT", "definitely-not-a-provider");
    let _hook = set_content_fallback(None);

    let server = spawn_one_shot_server("403 Forbidden", CF_WALL_BODY);

    let post = universal_scrape(&server, "english", None).await;

    assert!(!post.error.is_empty(), "wall without any fallback must fail");
    assert!(
        post.error.contains("bot-protection") || post.error.contains("Could not extract"),
        "error should be the built-in wall/extraction chain, got: {}",
        post.error
    );
}

/// Healthy page + hook installed → the hook is NOT consulted for URLs the
/// built-in pipeline handles fine (consult points are YouTube-pre-fetch
/// and post-Playwright walls/thin content only).
#[tokio::test]
async fn healthy_page_does_not_consult_hook() {
    let _lock = TEST_LOCK.lock().await;
    let _playwright = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_ENV, "0");
    let _archive = EnvVarGuard::set(UNINEWS_ARCHIVE_FALLBACK_ENV, "0");
    let _llm = EnvVarGuard::set("UNINEWS_LLM_CLIENT", "definitely-not-a-provider");

    let server = spawn_one_shot_server("200 OK", &article_body("PLAIN-FETCH-MARKER"));

    let consulted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let consulted_in_hook = consulted.clone();
    let _hook = HookGuard::install(Arc::new(move |_url| {
        let consulted = consulted_in_hook.clone();
        Box::pin(async move {
            consulted.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(ContentFallback::Extracted {
                title: None,
                content: "hook should not run".to_string(),
            })
        })
    }));

    let post = universal_scrape(&server, "english", None).await;

    assert!(
        post.content.contains("PLAIN-FETCH-MARKER"),
        "plain fetch must produce the content, content={:?}",
        post.content
    );
    assert!(
        !consulted.load(std::sync::atomic::Ordering::SeqCst),
        "hook must not be consulted for a healthy page"
    );
}

// ── UNINEWS_CONTENT_FALLBACK_FIRST ordering ───────────────────────────────────

/// Collect the events a scrape emits (via the single-slot listener) so
/// ordering assertions are deterministic. Restores the previous listener
/// on drop.
struct EventRecorder {
    previous: Option<uninews::ScrapeEventListener>,
    events: Arc<Mutex<Vec<String>>>,
}

impl EventRecorder {
    fn start() -> Self {
        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let previous = uninews::set_event_listener(Some(Arc::new(
            move |event: &uninews::ScrapeEvent| {
                let json = serde_json::to_value(event).expect("event serializes");
                if let Some(name) = json["event"].as_str() {
                    sink.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(name.to_string());
                }
            },
        )));
        Self { previous, events }
    }

    fn names(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl Drop for EventRecorder {
    fn drop(&mut self) {
        uninews::set_event_listener(self.previous.take());
    }
}

/// Flag SET + wall + hook installed: the hook is consulted BEFORE the
/// local Playwright render (no PlaywrightFallbackStarted at all when the
/// hook succeeds).
#[tokio::test]
async fn fallback_first_consults_hook_before_playwright_on_walls() {
    let _lock = TEST_LOCK.lock().await;
    let _flag = EnvVarGuard::set(uninews::UNINEWS_CONTENT_FALLBACK_FIRST_ENV, "1");
    let _playwright = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_ENV, "1");
    let _archive = EnvVarGuard::set(UNINEWS_ARCHIVE_FALLBACK_ENV, "0");
    let _llm = EnvVarGuard::set("UNINEWS_LLM_CLIENT", "definitely-not-a-provider");

    let server = spawn_one_shot_server("403 Forbidden", CF_WALL_BODY);
    let _hook = HookGuard::install(Arc::new(|_url| {
        Box::pin(async move { Ok(ContentFallback::RenderedDom(article_body("HOOK-FIRST-MARKER"))) })
    }));

    let recorder = EventRecorder::start();
    let post = universal_scrape(&server, "english", None).await;
    let events = recorder.names();

    assert!(
        post.content.contains("HOOK-FIRST-MARKER"),
        "hook content must win, content={:?}",
        post.content
    );
    assert!(
        events.contains(&"content_fallback_started".to_string()),
        "hook must be consulted: {events:?}"
    );
    assert!(
        !events.contains(&"playwright_fallback_started".to_string()),
        "local Playwright must be skipped when the hook succeeds first: {events:?}"
    );
}

/// Flag UNSET + wall + hook installed: the built-in order is preserved —
/// Playwright first (and its failure), then the hook.
#[tokio::test]
async fn default_order_keeps_playwright_before_hook_on_walls() {
    let _lock = TEST_LOCK.lock().await;
    let _flag = EnvVarGuard::set(uninews::UNINEWS_CONTENT_FALLBACK_FIRST_ENV, "0");
    let _playwright = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_ENV, "1");
    let _pw_timeout = EnvVarGuard::set(uninews::UNINEWS_PLAYWRIGHT_TIMEOUT_MS_ENV, "5000");
    let _archive = EnvVarGuard::set(UNINEWS_ARCHIVE_FALLBACK_ENV, "0");
    let _llm = EnvVarGuard::set("UNINEWS_LLM_CLIENT", "definitely-not-a-provider");

    let server = spawn_one_shot_server("403 Forbidden", CF_WALL_BODY);
    let _hook = HookGuard::install(Arc::new(|_url| {
        Box::pin(async move { Ok(ContentFallback::RenderedDom(article_body("HOOK-SECOND-MARKER"))) })
    }));

    let recorder = EventRecorder::start();
    let post = universal_scrape(&server, "english", None).await;
    let events = recorder.names();

    assert!(
        post.content.contains("HOOK-SECOND-MARKER"),
        "hook must rescue the scrape after local Playwright fails, content={:?}",
        post.content
    );
    let pw = events.iter().position(|e| e == "playwright_fallback_started");
    let hook = events.iter().position(|e| e == "content_fallback_started");
    assert!(
        pw.is_some() && hook.is_some() && pw < hook,
        "default order must be Playwright-then-hook: {events:?}"
    );
}

/// Flag SET but NO hook installed: pure no-op — the built-in chain runs
/// untouched (wall → Playwright → failure, no fallback events).
#[tokio::test]
async fn fallback_first_without_hook_is_a_no_op() {
    let _lock = TEST_LOCK.lock().await;
    let _flag = EnvVarGuard::set(uninews::UNINEWS_CONTENT_FALLBACK_FIRST_ENV, "1");
    let _playwright = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_ENV, "0");
    let _archive = EnvVarGuard::set(UNINEWS_ARCHIVE_FALLBACK_ENV, "0");
    let _llm = EnvVarGuard::set("UNINEWS_LLM_CLIENT", "definitely-not-a-provider");
    let _hook = set_content_fallback(None);

    let server = spawn_one_shot_server("403 Forbidden", CF_WALL_BODY);

    let recorder = EventRecorder::start();
    let post = universal_scrape(&server, "english", None).await;
    let events = recorder.names();

    assert!(!post.error.is_empty(), "wall with no fallbacks must fail");
    assert!(
        !events.contains(&"content_fallback_started".to_string()),
        "no hook → no fallback events: {events:?}"
    );
}
