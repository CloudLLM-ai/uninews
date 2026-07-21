//! Small shared helpers used across uninews modules.
//!
//! Everything in here is crate-private; public API lives in the top-level
//! modules (`llm`, `web`, `x`, `events`, `archive`).

use std::env;

/// Browser-like User-Agent header used for plain HTML fetches so news sites
/// do not serve bot-wall responses to the scraper.
pub(crate) const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36";

/// Return the value of the first environment variable in `keys` that is set
/// to a non-empty (after trimming) value.
pub(crate) fn first_non_empty_env_var(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| match env::var(key) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    })
}

/// Trim `body` and truncate it to at most `max_len` bytes (on a char
/// boundary), appending an ellipsis when truncation occurs.
pub(crate) fn summarize_body(body: &str, max_len: usize) -> String {
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
