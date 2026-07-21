//! Scraping progress events.
//!
//! Uninews emits typed [`ScrapeEvent`]s at every meaningful step of the
//! scraping pipeline — fetch start/success/failure, content extraction,
//! bot-protection detection, archive.org fallback, LLM conversion, and
//! overall completion — so agents, harnesses, and UIs can render live
//! progress instead of staring at a silent `await`.
//!
//! # Registering a listener
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use uninews::{set_event_listener, ScrapeEvent};
//!
//! set_event_listener(Some(Arc::new(|event: &ScrapeEvent| {
//!     eprintln!("uninews: {:?}", event);
//! })));
//! ```
//!
//! # Single listener by design
//!
//! Uninews deliberately supports **one** process-wide listener. Keeping the
//! emitter single-slot avoids unbounded fan-out, ordering questions, and
//! hidden performance costs inside the library.
//!
//! > **Developer note:** if you need multiple consumers (e.g. a progress UI
//! > *and* a metrics sink), register a single closure that multiplexes —
//! > forward each event to your own `Vec` of subscribers, a
//! > `tokio::sync::broadcast` channel, or an actor. That keeps fan-out
//! > policy (and its costs) in your code, where it belongs.
//!
//! # Callback contract
//!
//! - The listener is invoked **synchronously** on the async task performing
//!   the scrape. Keep it fast and non-blocking; hand real work off to a
//!   channel or another task.
//! - A panicking listener is caught and reported to stderr; it never
//!   aborts the scrape.
//! - The listener must be `Send + Sync` because scraping can run on any
//!   Tokio worker thread.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, RwLock};

use serde::Serialize;

/// A snapshot of pipeline progress, emitted by [`emit_event`].
///
/// The enum is `Serialize` with a snake_case `event` tag so listeners can
/// forward events as JSON without any mapping of their own:
///
/// ```rust
/// use uninews::ScrapeEvent;
///
/// let event = ScrapeEvent::FetchSucceeded {
///     url: "https://example.com/a".to_string(),
///     status: 200,
///     body_bytes: 42_000,
/// };
/// let json = serde_json::to_value(&event).unwrap();
/// assert_eq!(json["event"], "fetch_succeeded");
/// assert_eq!(json["status"], 200);
/// ```
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ScrapeEvent {
    /// [`crate::universal_scrape`] has begun processing `url`.
    ScrapeStarted {
        /// The URL being scraped.
        url: String,
    },
    /// An HTTP request is about to be sent.
    FetchStarted {
        /// The request URL (page URL or API endpoint).
        url: String,
    },
    /// An HTTP response was received and its body read successfully.
    FetchSucceeded {
        /// The request URL.
        url: String,
        /// HTTP status code.
        status: u16,
        /// Size of the response body in bytes.
        body_bytes: usize,
    },
    /// An HTTP request failed (DNS, TLS, connect/read timeout, body error).
    FetchFailed {
        /// The request URL.
        url: String,
        /// Human-readable failure description.
        error: String,
    },
    /// Article content was successfully extracted from the HTML.
    ContentExtracted {
        /// The page URL the content was extracted from.
        url: String,
        /// Size of the cleaned content in bytes.
        content_bytes: usize,
    },
    /// No meaningful content could be extracted from the HTML.
    ContentExtractionFailed {
        /// The page URL.
        url: String,
        /// Human-readable failure description.
        error: String,
    },
    /// The response looks like a bot-protection wall (Cloudflare challenge
    /// page, JavaScript-required interstitial, …) rather than real content.
    BotProtectionDetected {
        /// The protected page URL.
        url: String,
    },
    /// The archive.org Wayback Machine fallback has been engaged.
    ArchiveFallbackStarted {
        /// The original URL that could not be scraped directly.
        url: String,
        /// Why the fallback was engaged.
        reason: String,
    },
    /// archive.org has a usable snapshot of the URL.
    ArchiveSnapshotFound {
        /// The original URL.
        url: String,
        /// The Wayback Machine snapshot URL that will be scraped instead.
        snapshot_url: String,
        /// Snapshot timestamp (`yyyyMMddhhmmss`).
        timestamp: String,
    },
    /// archive.org has no usable snapshot of the URL.
    ArchiveSnapshotNotFound {
        /// The original URL.
        url: String,
    },
    /// The extracted content is about to be sent to the LLM for Markdown
    /// conversion.
    LlmConversionStarted {
        /// Human-readable provider label, e.g. `"OpenAI (gpt-5.6-sol)"`.
        provider: String,
        /// Size of the content being converted, in bytes.
        content_bytes: usize,
    },
    /// The LLM Markdown conversion completed successfully.
    LlmConversionSucceeded {
        /// Human-readable provider label.
        provider: String,
        /// Size of the produced Markdown, in bytes.
        markdown_bytes: usize,
    },
    /// The LLM Markdown conversion failed.
    LlmConversionFailed {
        /// Human-readable provider label.
        provider: String,
        /// Human-readable failure description.
        error: String,
    },
    /// The scrape finished successfully; the [`crate::Post`] is ready.
    ScrapeCompleted {
        /// The scraped URL.
        url: String,
    },
    /// The scrape failed; see [`crate::Post::error`] for details.
    ScrapeFailed {
        /// The URL that failed.
        url: String,
        /// Human-readable failure description.
        error: String,
    },
}

/// The listener callback signature.
///
/// Register one with [`set_event_listener`]. See the module-level docs for
/// the single-listener design rationale and the multiplexing recipe.
pub type ScrapeEventListener = Arc<dyn Fn(&ScrapeEvent) + Send + Sync + 'static>;

/// Process-wide listener slot. `RwLock` so the common read path (emit) is
/// cheap and concurrent; the `Arc` is cloned out before invocation so the
/// callback may itself call [`set_event_listener`] without deadlocking.
static LISTENER: RwLock<Option<ScrapeEventListener>> = RwLock::new(None);

/// Register (or replace) the process-wide scrape event listener.
///
/// Pass `None` to stop event delivery entirely.
///
/// Returns the previously registered listener, if any, so callers can
/// restore it later (handy for libraries embedding uninews).
///
/// Only **one** listener is supported; registering a new one replaces the
/// old. If you need several consumers, register a multiplexing closure —
/// see the module-level docs.
pub fn set_event_listener(listener: Option<ScrapeEventListener>) -> Option<ScrapeEventListener> {
    let mut guard = LISTENER.write().unwrap_or_else(|err| err.into_inner());
    std::mem::replace(&mut *guard, listener)
}

/// Emit an event to the registered listener, if any.
///
/// The listener `Arc` is cloned out of the lock before invocation so
/// re-entrant calls (listener replacing itself, listener emitting events)
/// cannot deadlock. A panicking listener is caught and logged to stderr —
/// listener bugs must never abort a scrape.
///
/// Exposed (as `pub` + `#[doc(hidden)]`) so integration tests in
/// `tests/events.rs` can drive the emitter directly; end users only need
/// [`set_event_listener`].
#[doc(hidden)]
pub fn emit_event(event: ScrapeEvent) {
    let listener = LISTENER
        .read()
        .unwrap_or_else(|err| err.into_inner())
        .clone();

    if let Some(listener) = listener {
        if catch_unwind(AssertUnwindSafe(|| listener(&event))).is_err() {
            eprintln!("uninews: scrape event listener panicked; event dropped");
        }
    }
}
