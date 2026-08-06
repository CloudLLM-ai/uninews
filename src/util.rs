//! Small shared helpers used across uninews modules.
//!
//! Everything in here is crate-private; public API lives in the top-level
//! modules (`llm`, `web`, `x`, `events`, `archive`). The sole exception is
//! [`summarize_body`], which is `pub` + `#[doc(hidden)]` so it can be
//! unit-tested (and potentially re-exported for integration tests) without
//! becoming part of the documented public API.

use std::env;

/// Browser-like User-Agent header used for plain HTML fetches so news sites
/// do not serve bot-wall responses to the scraper.
pub(crate) const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36";

/// Return the value of the first environment variable in `keys` that is set
/// to a non-empty (after trimming) value. The returned value is trimmed, so
/// callers never receive leading/trailing whitespace.
pub(crate) fn first_non_empty_env_var(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| match env::var(key) {
        Ok(value) if !value.trim().is_empty() => Some(value.trim().to_string()),
        _ => None,
    })
}

/// Trim `body` and truncate it to at most `max_len` bytes (on a char
/// boundary), appending an ellipsis when truncation occurs.
///
/// Exposed (as `pub` + `#[doc(hidden)]`) so the truncation rules can be
/// unit-tested; not part of the documented public API.
#[doc(hidden)]
pub fn summarize_body(body: &str, max_len: usize) -> String {
    let trimmed = body.trim();
    if trimmed.len() <= max_len {
        return trimmed.to_string();
    }

    let mut end = max_len;
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &trimmed[..end])
}

/// Returns `true` when `url` points at YouTube — any `youtube.com` (or
/// subdomain) path, or a `youtu.be` short link.
///
/// Used by the web pipeline to give the host content-fallback hook first
/// crack at video URLs (the article-equivalent payload of a video — its
/// transcript — never appears in the watch-page HTML). The check is
/// host-based, not substring-based, so `https://notyoutube.com/x` does
/// NOT match. Deliberately shape-based rather than video-ID-precise:
/// channels and playlists match too, and the hook decides what it can
/// serve.
///
/// # Examples
///
/// ```
/// use uninews::is_youtube_url;
/// assert!(is_youtube_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
/// assert!(is_youtube_url("https://youtu.be/dQw4w9WgXcQ"));
/// assert!(!is_youtube_url("https://notyoutube.com/watch?v=x"));
/// ```
pub fn is_youtube_url(url: &str) -> bool {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    host == "youtube.com"
        || host.ends_with(".youtube.com")
        || host == "youtu.be"
        || host.ends_with(".youtu.be")
}
