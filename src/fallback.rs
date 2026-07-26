//! Host-provided content fallback hook.
//!
//! Uninews ships a rich built-in fallback chain (plain HTTP → headless
//! browser renders → archive.org), but some environments have their own
//! ways of obtaining content uninews cannot reach on its own — for
//! example a remote rendering service, or a site-specific extractor for
//! payloads that never appear in the page HTML at all (video transcripts
//! being the canonical case).
//!
//! [`set_content_fallback`] lets the **host application** install a single
//! process-wide hook that uninews consults at well-defined points:
//!
//! 1. **YouTube URLs** — consulted *before* any HTTP fetch, because the
//!    article-equivalent payload of a video (its transcript) is not part
//!    of the watch-page HTML. When the hook returns
//!    [`ContentFallback::Extracted`], the content flows through the normal
//!    LLM Markdown step like any extracted article body.
//! 2. **Bot-protection walls and thin-content pages** — consulted *after*
//!    the built-in Playwright render when that render is disabled or did
//!    not yield usable content, and *before* the archive.org fallback.
//!    [`ContentFallback::RenderedDom`] output is re-validated against the
//!    bot-protection heuristics before extraction, so a walled response
//!    from the host is never mistaken for real content.
//!
//! When no hook is installed, behavior is exactly the built-in chain —
//! the hook is purely additive and can never make a scrape worse.
//!
//! # Single hook by design
//!
//! Like the event listener ([`crate::set_event_listener`]), the hook slot
//! is deliberately single: one host, one content strategy. Registering a
//! new hook replaces the old one and returns it, so embedders can restore
//! the previous hook later.
//!
//! # Example
//!
//! ```rust
//! use std::sync::Arc;
//! use uninews::{set_content_fallback, ContentFallback};
//!
//! set_content_fallback(Some(Arc::new(|url: String| {
//!     Box::pin(async move {
//!         if url.contains("youtube.com/") {
//!             Ok(ContentFallback::Extracted {
//!                 title: Some("Example video".to_string()),
//!                 content: "transcript text…".to_string(),
//!             })
//!         } else {
//!             Err("no fallback available for this URL".to_string())
//!         }
//!     })
//! })));
//! ```

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use crate::browser::is_falsy_env_flag;

/// Environment variable that puts the host content fallback **before** the
/// built-in Playwright render for bot-protection walls.
///
/// Unset (the default), walls try the local Playwright render first and the
/// hook second — the right order when the local IP has good reputation.
/// Set to a truthy value (anything but `0`/`false`/`no`/`off`) when the
/// host runs on a datacenter IP whose local render is doomed to fail the
/// challenge anyway: the hook (e.g. a remote residential renderer) is
/// consulted first, saving the wasted local render attempt (~60 s per
/// walled URL).
///
/// Only wall ordering is affected: the thin-content trigger keeps its
/// local-Playwright-first order, because JS-shell pages are not IP-gated
/// and local renders succeed for them.
///
/// # Examples
///
/// ```
/// use uninews::UNINEWS_CONTENT_FALLBACK_FIRST_ENV;
/// assert_eq!(UNINEWS_CONTENT_FALLBACK_FIRST_ENV, "UNINEWS_CONTENT_FALLBACK_FIRST");
/// ```
pub const UNINEWS_CONTENT_FALLBACK_FIRST_ENV: &str = "UNINEWS_CONTENT_FALLBACK_FIRST";

/// Whether the host content fallback should be consulted before the local
/// Playwright render for bot-protection walls.
///
/// Enabled when [`UNINEWS_CONTENT_FALLBACK_FIRST_ENV`] is set to a
/// non-falsy value; disabled when unset or falsy (see
/// [`is_falsy_env_flag`]).
pub fn content_fallback_first() -> bool {
    match std::env::var(UNINEWS_CONTENT_FALLBACK_FIRST_ENV) {
        Ok(value) => !is_falsy_env_flag(&value),
        Err(_) => false,
    }
}

/// Content supplied by the host for a URL uninews could not handle itself.
///
/// Returned by the [`ContentFallbackHook`]; see the module-level docs for
/// the consultation points.
#[derive(Debug, Clone)]
pub enum ContentFallback {
    /// A fully rendered HTML DOM. Uninews re-validates it against the
    /// bot-protection heuristics and then runs the standard HTML
    /// extraction on it.
    RenderedDom(String),
    /// Content the host already extracted (e.g. a video transcript),
    /// skipping HTML extraction entirely. Flows through the normal LLM
    /// Markdown conversion step like any extracted article body.
    Extracted {
        /// Optional title for the resulting post.
        title: Option<String>,
        /// The extracted content (plain text or Markdown).
        content: String,
    },
}

/// Boxed future returned by the content fallback hook.
pub type ContentFallbackFuture =
    Pin<Box<dyn Future<Output = Result<ContentFallback, String>> + Send>>;

/// The content fallback hook signature: given a URL, produce fallback
/// content or an error describing why none is available.
pub type ContentFallbackHook =
    Arc<dyn Fn(String) -> ContentFallbackFuture + Send + Sync + 'static>;

/// Process-wide hook slot. `RwLock` so the read path (every scrape) is
/// cheap and concurrent; the `Arc` is cloned out before invocation so the
/// hook may itself call [`set_content_fallback`] without deadlocking.
static HOOK: RwLock<Option<ContentFallbackHook>> = RwLock::new(None);

/// Register (or replace) the process-wide content fallback hook.
///
/// Pass `None` to uninstall the hook entirely.
///
/// Returns the previously registered hook, if any, so callers can restore
/// it later (handy for libraries embedding uninews).
pub fn set_content_fallback(hook: Option<ContentFallbackHook>) -> Option<ContentFallbackHook> {
    let mut guard = HOOK.write().unwrap_or_else(|err| err.into_inner());
    std::mem::replace(&mut *guard, hook)
}

/// Clone the installed hook out of the slot, if any.
///
/// The `Arc` is cloned under a read lock and the lock released before the
/// hook is invoked, so re-entrant hook behavior cannot deadlock.
pub(crate) fn content_fallback_hook() -> Option<ContentFallbackHook> {
    HOOK.read().unwrap_or_else(|err| err.into_inner()).clone()
}
