//! Integration tests for the archive.org fallback *eligibility
//! classification* in `src/web.rs`:
//!
//! `eligible = bot_protected || network_failure || server_error`
//!
//! The bot-protection arm is covered end-to-end in
//! `tests/archive_fallback.rs`; this file covers the other two arms by
//! asserting on the `reason` string of [`ScrapeEvent::ArchiveFallbackStarted`]:
//!
//! 1. **network failure** — a URL on a closed loopback port (connect
//!    refused) must engage the fallback with reason `"network failure"`.
//! 2. **server error** — a loopback server answering 500 must engage the
//!    fallback with reason `"server error (5xx)"`.
//! 3. **kill switch** — with `UNINEWS_ARCHIVE_FALLBACK=0`, a network
//!    failure must NOT emit `ArchiveFallbackStarted` at all.
//!
//! Hermeticity note: the archive availability endpoint is not configurable,
//! so scenarios 1 and 2 cannot avoid ONE real archive.org lookup each
//! (bounded by the 30s request timeout; a 127.0.0.1 URL has no snapshots,
//! so it returns quickly). The assertions therefore only pin the
//! classification (`ArchiveFallbackStarted` + reason), not the lookup
//! outcome. Scenario 3 is fully hermetic.
//!
//! All scenarios run in ONE `#[tokio::test]`: the event listener is
//! process-wide state, and a `std::Mutex` guard must never be held across
//! `.await` (clippy::await_holding_lock). A single listener is registered
//! once and the event buffer is drained between scenarios.

use std::env;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use uninews::{
    set_event_listener, universal_scrape, ScrapeEvent, UNINEWS_ARCHIVE_FALLBACK_ENV,
    UNINEWS_PLAYWRIGHT_ENV,
};

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

/// A URL on a loopback port that is (almost certainly) closed: bind an
/// ephemeral listener to learn a free port, then drop it so connects are
/// refused.
fn closed_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);
    format!("http://{}/unreachable", addr)
}

/// Spawn a loopback HTTP server that answers one request with a 500 and an
/// empty body (which the extractor rejects with "Could not extract
/// meaningful content"), then return its URL.
fn spawn_500_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback server");
    let addr = listener.local_addr().expect("local addr");
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut request = [0u8; 1024];
        let _ = stream.read(&mut request);
        let response =
            "HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/html\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    });
    format!("http://{}/boom", addr)
}

/// Extract the `reason` of the first recorded `ArchiveFallbackStarted`, if any.
fn archive_fallback_reason(events: &[ScrapeEvent]) -> Option<String> {
    events.iter().find_map(|event| match event {
        ScrapeEvent::ArchiveFallbackStarted { reason, .. } => Some(reason.clone()),
        _ => None,
    })
}

#[tokio::test]
async fn archive_eligibility_covers_network_failure_and_server_error_arms() {
    // Playwright off (bot-protection path is covered elsewhere and must not
    // launch a browser here), archive ENABLED for scenarios 1-2, and the
    // LLM stage pinned at a bogus provider so this test can never make a
    // live LLM call even if a stage boundary shifts (this dev shell may
    // export real LLM keys).
    let _pw = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_ENV, "0");
    let _archive = EnvVarGuard::unset(UNINEWS_ARCHIVE_FALLBACK_ENV);
    let _llm = EnvVarGuard::set("UNINEWS_LLM_CLIENT", "definitely-not-a-provider");

    // Only test in this binary -> the process-wide listener slot needs no
    // lock; a std::Mutex guard must not be held across the awaits below.
    let events: Arc<Mutex<Vec<ScrapeEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    set_event_listener(Some(Arc::new(move |event: &ScrapeEvent| {
        sink.lock().unwrap().push(event.clone());
    })));

    // ── Scenario 1: network failure (connect refused) → "network failure".
    let url = closed_port_url();
    let post = universal_scrape(&url, "english", None).await;
    assert!(
        post.error.contains("Failed to fetch URL"),
        "expected a fetch failure, got: {}",
        post.error
    );
    {
        let recorded = events.lock().unwrap();
        assert_eq!(
            archive_fallback_reason(&recorded).as_deref(),
            Some("network failure"),
            "network failure arm must engage the archive fallback, got: {:?}",
            *recorded
        );
    }
    events.lock().unwrap().clear();

    // ── Scenario 2: server error (500) → "server error (5xx)".
    let url = spawn_500_server();
    let post = universal_scrape(&url, "english", None).await;
    assert!(
        !post.error.is_empty(),
        "a 500 with an empty (unextractable) body must surface an error"
    );
    {
        let recorded = events.lock().unwrap();
        assert_eq!(
            archive_fallback_reason(&recorded).as_deref(),
            Some("server error (5xx)"),
            "server error arm must engage the archive fallback, got: {:?}",
            *recorded
        );
    }
    events.lock().unwrap().clear();

    // ── Scenario 3: kill switch — archive disabled → no fallback event.
    {
        let _archive_off = EnvVarGuard::set(UNINEWS_ARCHIVE_FALLBACK_ENV, "0");
        let url = closed_port_url();
        let post = universal_scrape(&url, "english", None).await;
        assert!(
            post.error.contains("Failed to fetch URL"),
            "expected a fetch failure, got: {}",
            post.error
        );
        let recorded = events.lock().unwrap();
        assert!(
            archive_fallback_reason(&recorded).is_none(),
            "UNINEWS_ARCHIVE_FALLBACK=0 must suppress ArchiveFallbackStarted, got: {:?}",
            *recorded
        );
    }

    set_event_listener(None);
}
