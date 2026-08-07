//! Integration tests for the render / host-content-fallback ordering on
//! **hard failures** (network errors / 5xx) in `src/web.rs`.
//!
//! Regression for a real outage (`diariobitcoin` chain 2026-08-07): a
//! Miami Herald URL *timed out* (a network failure) and the pipeline jumped
//! straight to archive.org (which then returned HTTP 429 → the whole read
//! failed), without ever trying the host content fallback (the residential
//! broker) or Playwright. A timeout / 5xx on a datacenter IP is commonly
//! Cloudflare & co. dropping the request *before* a bot-protection signal
//! is even visible, so the render + host fallback must be attempted on hard
//! failures too — with the host hook consulted FIRST via
//! `UNINEWS_CONTENT_FALLBACK_FIRST` when the local render is doomed (the
//! AWS case), and archive.org kept as the LAST resort.
//!
//! Two scenarios, all in ONE `#[tokio::test]` (process-wide listener/hook
//! state, and a `std::Mutex` guard must not be held across `.await`).
//!
//! **Scenario 1 — host fallback serves a hard failure.** With the hook-first
//! flag and a hook that returns usable content for every URL, a
//! connect-refused (network failure) URL must be served by the hook
//! (`ContentFallbackStarted` then `ContentFallbackSucceeded`), and must NOT
//! reach the archive fallback at all. Fully hermetic.
//!
//! **Scenario 2 — archive remains after a failed render chain.** With
//! hook-first and a hook that errors, the hook must be consulted BEFORE
//! archive.org (event order `ContentFallbackStarted` <
//! `ArchiveFallbackStarted`), proving the hard-failure path now runs the
//! render/host chain first and still lands on archive.org only as the last
//! fallback. This makes ONE real archive.org availability lookup on a
//! 127.0.0.1 URL (no snapshot, quick), bounded by the request timeout.

use std::env;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use uninews::{
    set_content_fallback, set_event_listener, universal_scrape, ContentFallback, ScrapeEvent,
    UNINEWS_ARCHIVE_FALLBACK_ENV, UNINEWS_CONTENT_FALLBACK_FIRST_ENV, UNINEWS_PLAYWRIGHT_ENV,
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
/// refused. Reused verbatim from `archive_eligibility.rs`.
fn closed_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);
    format!("http://{}/unreachable", addr)
}

/// Emitted-event helpers.
fn has_event(events: &[ScrapeEvent], f: impl Fn(&ScrapeEvent) -> bool) -> bool {
    events.iter().any(f)
}

fn first_index(events: &[ScrapeEvent], f: impl Fn(&ScrapeEvent) -> bool) -> Option<usize> {
    events.iter().position(f)
}

#[tokio::test]
async fn hard_failure_consults_host_fallback_before_archive() {
    // Playwright off (never launch a browser here), archive on, LLM pinned
    // to a bogus provider so no live call fires even if a boundary shifts
    // (this dev shell may export real LLM keys).
    let _pw = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_ENV, "0");
    let _archive = EnvVarGuard::unset(UNINEWS_ARCHIVE_FALLBACK_ENV);
    let _llm = EnvVarGuard::set("UNINEWS_LLM_CLIENT", "definitely-not-a-provider");
    // Hook-first: the AWS/datacenter case where local Playwright is doomed.
    let _hook_first = EnvVarGuard::set(UNINEWS_CONTENT_FALLBACK_FIRST_ENV, "1");

    let events: Arc<Mutex<Vec<ScrapeEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    set_event_listener(Some(Arc::new(move |event: &ScrapeEvent| {
        sink.lock().unwrap().push(event.clone());
    })));

    // ── Scenario 1: host fallback serves a hard failure (no archive).
    set_content_fallback(Some(Arc::new(|_url: String| {
        Box::pin(async move {
            Ok(ContentFallback::Extracted {
                title: Some("Broker content".to_string()),
                content: "The residential broker rendered the article body for this URL."
                    .to_string(),
            })
        })
    })));
    let url = closed_port_url();
    let _post = universal_scrape(&url, "english", None).await;
    {
        let recorded = events.lock().unwrap();
        assert!(
            has_event(&recorded, |e| matches!(
                e,
                ScrapeEvent::ContentFallbackStarted { .. }
            )),
            "network failure must consult the host content fallback, got: {:?}",
            *recorded
        );
        assert!(
            has_event(&recorded, |e| matches!(
                e,
                ScrapeEvent::ContentFallbackSucceeded { .. }
            )),
            "usable host-fallback content must be accepted, got: {:?}",
            *recorded
        );
        assert!(
            !has_event(&recorded, |e| matches!(e, ScrapeEvent::ArchiveFallbackStarted { .. })),
            "archive.org must NOT be reached when the host fallback served the hard failure, got: {:?}",
            *recorded
        );
    }
    events.lock().unwrap().clear();

    // ── Scenario 2: hook errors → archive.org is still the LAST fallback,
    // but the hook (broker) is consulted BEFORE it.
    set_content_fallback(Some(Arc::new(|_url: String| {
        Box::pin(async move { Err("broker could not render".to_string()) })
    })));
    let url = closed_port_url();
    let _post = universal_scrape(&url, "english", None).await;
    {
        let recorded = events.lock().unwrap();
        let hook_idx = first_index(&recorded, |e| {
            matches!(e, ScrapeEvent::ContentFallbackStarted { .. })
        });
        let archive_idx = first_index(&recorded, |e| {
            matches!(e, ScrapeEvent::ArchiveFallbackStarted { .. })
        });
        let (Some(hook_idx), Some(archive_idx)) = (hook_idx, archive_idx) else {
            panic!("expected both ContentFallbackStarted and ArchiveFallbackStarted on a failed render chain, got: {:?}", *recorded);
        };
        assert!(
            hook_idx < archive_idx,
            "host content fallback must run BEFORE archive.org on a hard failure, got: {:?}",
            *recorded
        );
    }

    set_event_listener(None);
    set_content_fallback(None);
}
