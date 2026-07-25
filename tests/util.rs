//! Tests for `summarize_body` (untrusted-response summarization), exercised
//! through the `#[doc(hidden)]` public re-export per the project convention
//! (tests live in `tests/`, never in `src/`).

use uninews::summarize_body;

/// Empty and whitespace-only bodies summarize to an empty string.
#[test]
fn empty_body_stays_empty() {
    assert_eq!(summarize_body("", 10), "");
    assert_eq!(summarize_body("   \n\t ", 10), "");
}

/// A body short enough is trimmed, never truncated.
#[test]
fn short_body_is_trimmed_not_truncated() {
    assert_eq!(summarize_body("hello", 10), "hello");
    assert_eq!(summarize_body("  hello world  ", 100), "hello world");
}

/// A body exactly at the limit is kept whole (no ellipsis).
#[test]
fn exact_fit_body_is_not_truncated() {
    assert_eq!(summarize_body("hello", 5), "hello");
}

/// A long ASCII body is truncated with an ellipsis.
#[test]
fn truncates_long_ascii_body_with_ellipsis() {
    assert_eq!(summarize_body("hello world", 5), "hello...");
}

/// Truncation must back off to a char boundary — slicing mid-UTF-8-sequence
/// would panic; these cases pin the boundary-safe behavior.
#[test]
fn multi_byte_char_at_the_boundary_does_not_panic() {
    // 'é' is 2 bytes in UTF-8; max_len = 2 would split the first one.
    assert_eq!(summarize_body("aééé", 2), "a...");

    // 'ñ' is 2 bytes; 10 of them are 20 bytes, max_len = 5 lands
    // mid-char and must back off to byte 4 (two 'ñ's).
    let body = "ñ".repeat(10);
    assert_eq!(summarize_body(&body, 5), "ññ...");

    // A multi-byte char exactly at the boundary is kept whole.
    assert_eq!(summarize_body("ééé", 4), "éé...");
}
