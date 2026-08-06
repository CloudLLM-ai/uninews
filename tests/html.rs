//! Integration tests for the HTML content-extraction pipeline, exercised
//! strictly through the crate's public surface (the `uninews::html` module is
//! `#[doc(hidden)] pub` precisely so these tests can live here instead of
//! inline in `src/html.rs`).
//!
//! All tests are hermetic: they parse in-memory HTML strings, no network and
//! no process-wide state involved.

use uninews::html::parse_scraped_post_from_html;
use uninews::Post;

/// Arbitrary non-X URL. Only used so the X guest-wall guard stays out of the
/// way; X URLs have their own pipeline and their own test file.
const URL: &str = "https://example.com/news/story";

/// Parse an in-memory HTML document with no title override.
fn parse(body: &str) -> Post {
    parse_scraped_post_from_html(URL, body, None)
}

// ── C2 regression: markup injection via decoded entities ───────────────────

/// Regression for the markup-injection finding: entity-encoded markup in
/// text nodes must stay inert in the cleaned output. html5ever decodes
/// character references at parse time, so `&lt;script&gt;` arrives as the
/// literal text `<script>`; the cleaner must re-escape text content instead
/// of emitting it as a live `<script>` element.
#[test]
fn entity_encoded_markup_in_text_stays_inert() {
    let post = parse(
        "<html><body><article><p>&lt;script&gt;alert(1)&lt;/script&gt;</p></article></body></html>",
    );

    assert!(post.error.is_empty());
    assert_eq!(
        post.content,
        "<article><p>&lt;script&gt;alert(1)&lt;/script&gt;</p></article>"
    );
    assert!(!post.content.contains("<script"));
}

/// Plain prose containing `&`, `<`, and `>` must be escaped in the cleaned
/// output (with `&` escaped first, so replacements are never double-escaped).
#[test]
fn prose_special_chars_are_escaped() {
    let cases: &[(&str, &str)] = &[
        ("Fish & Chips <3", "Fish &amp; Chips &lt;3"),
        ("a > b & b < a", "a &gt; b &amp; b &lt; a"),
        ("AT&T", "AT&amp;T"),
    ];

    for (input, expected) in cases {
        let body = format!("<html><body><article><p>{input}</p></article></body></html>");
        let post = parse(&body);
        assert!(post.error.is_empty(), "unexpected error for {input:?}");
        assert_eq!(
            post.content,
            format!("<article><p>{expected}</p></article>"),
            "input: {input:?}"
        );
    }
}

// ── C3 regression: stack overflow on deeply nested markup ──────────────────

/// Regression for the stack-overflow finding: a pathologically deep document
/// (50,000 nested elements) must be cleaned iteratively without overflowing
/// the call stack, and still produce sane output. 50k nesting levels is the
/// proof — the old recursive cleaner overflowed far below this depth.
///
/// The nesting uses `<marquee>` rather than `<div>` on purpose: html5ever
/// walks the whole open-elements stack on every `<div>` start tag ("has `p`
/// in button scope", and `<div>` is not a scope boundary), which makes the
/// *parse itself* quadratic (~90s at 50k in debug). `<marquee>` is a
/// button-scope boundary per the HTML5 spec, so the parse stays linear and
/// the test exercises the cleaner — the thing being regression-tested — in
/// well under a second. The nesting-depth proof is identical either way:
/// `clean_element` is tag-agnostic.
#[test]
fn deeply_nested_document_does_not_overflow_stack() {
    const DEPTH: usize = 50_000;

    let mut body = String::with_capacity(DEPTH * 15 + 64);
    body.push_str("<html><body><article>");
    body.push_str(&"<marquee>".repeat(DEPTH));
    body.push_str("deep text");
    body.push_str(&"</marquee>".repeat(DEPTH));
    body.push_str("</article></body></html>");

    let post = parse(&body);

    assert!(post.error.is_empty());
    assert!(post.content.contains("deep text"));
    assert_eq!(post.content.matches("<marquee>").count(), DEPTH);
}

// ── Cleaning semantics ─────────────────────────────────────────────────────

/// An element whose cleaned content is all whitespace contributes nothing:
/// the empty `<div>`/`<span>` wrapper must be elided entirely, not emitted
/// as an empty tag pair.
#[test]
fn empty_subtrees_are_elided() {
    let post = parse(
        "<html><body><article><p>real text</p><div><span>   </span></div></article></body></html>",
    );

    assert!(post.error.is_empty());
    assert_eq!(post.content, "<article><p>real text</p></article>");
}

/// A `<script>` element is dropped entirely — element AND its text children
/// — while surrounding sibling text survives. This pins the current skip-tag
/// semantics: skipped subtrees are not traversed at all.
#[test]
fn script_element_is_dropped_but_surrounding_text_survives() {
    let post = parse(
        "<html><body><article>Before <script>alert(1)</script> After</article></body></html>",
    );

    assert!(post.error.is_empty());
    assert_eq!(post.content, "<article>Before After</article>");
    assert!(!post.content.contains("alert"));
}

// ── Article selection / fallback ───────────────────────────────────────────

/// With several `<article>` elements (main story + teaser cards), the
/// longest cleaned article wins.
#[test]
fn longest_of_several_articles_wins() {
    let long_story = "Main story sentence. ".repeat(30);
    let body = format!(
        "<html><body><article><p>Teaser blurb.</p></article><article><p>{long_story}</p></article></body></html>"
    );

    let post = parse(&body);

    assert!(post.error.is_empty());
    assert!(post.content.contains("Main story sentence."));
    assert!(!post.content.contains("Teaser blurb."));
}

/// An `<article>` nested inside another `<article>` is not cleaned as its
/// own candidate: the outer article wins as a whole and the inner content
/// appears exactly once (no duplicate processing).
#[test]
fn nested_article_is_not_processed_separately() {
    let post = parse(
        "<html><body><article>Outer intro <article>Inner text</article></article></body></html>",
    );

    assert!(post.error.is_empty());
    assert_eq!(
        post.content,
        "<article>Outer intro <article>Inner text</article></article>"
    );
    assert_eq!(post.content.matches("Inner text").count(), 1);
}

/// Without an `<article>`, the extractor falls back to the whole `<body>`.
#[test]
fn body_fallback_when_no_article() {
    let post = parse("<html><body><p>Body text here</p></body></html>");

    assert!(post.error.is_empty());
    assert_eq!(post.content, "<body><p>Body text here</p></body>");
}

/// A document with no extractable content reports an error via
/// `Post::error` instead of panicking or silently returning empty content.
#[test]
fn empty_document_sets_error_field() {
    let post = parse("");

    assert!(post.content.is_empty());
    assert!(
        post.error.contains("Could not extract meaningful content"),
        "unexpected error: {}",
        post.error
    );
}
