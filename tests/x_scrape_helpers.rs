//! Unit-style tests for the X/Twitter scraping helpers, exercised strictly
//! through the crate's public surface (the `uninews::x` module is
//! `#[doc(hidden)] pub` precisely so these tests can live here instead of
//! inline in `src/x.rs`).
//!
//! `XTweet` / `XArticleMeta` expose no public fields by design; tests build
//! them from JSON via `serde`, which doubles as coverage of the
//! `Deserialize` implementations used against the live X API payloads.

use serde_json::json;
use uninews::html::parse_scraped_post_from_html;
use uninews::x::{
    extract_tweet_id, is_x_article_url, is_x_url, parse_x_web_article_post,
    x_article_body_unavailable, x_article_plain_text, x_linked_article_url, x_post_is_link_only,
    XArticleMeta, XTweet,
};

/// Build an `XTweet` carrying a single URL entity, mirroring the relevant
/// slice of an X API v2 tweet payload.
fn tweet_with_url_entity(text: &str, entity: serde_json::Value) -> XTweet {
    serde_json::from_value(json!({
        "id": "1",
        "text": text,
        "entities": { "urls": [entity] },
    }))
    .expect("test tweet JSON must deserialize")
}

// ── is_x_url ──────────────────────────────────────────────────────────────

#[test]
fn is_x_url_accepts_x_com() {
    assert!(is_x_url("https://x.com/user/status/123"));
}

#[test]
fn is_x_url_accepts_twitter_com() {
    assert!(is_x_url("https://twitter.com/user/status/123"));
}

#[test]
fn is_x_url_rejects_non_x_urls() {
    assert!(!is_x_url("https://example.com/article"));
    assert!(!is_x_url("https://bbc.com/news/world"));
    assert!(!is_x_url("http://x.com/user/status/123")); // http, not https
    assert!(!is_x_url("https://notx.com/user/status/123"));
    assert!(!is_x_url("https://x.com")); // missing trailing slash and path
    assert!(!is_x_url("https://twitter.com")); // missing trailing slash and path
}

// ── extract_tweet_id ──────────────────────────────────────────────────────

#[test]
fn extract_tweet_id_from_x_com_status() {
    assert_eq!(
        extract_tweet_id("https://x.com/user/status/1234567890"),
        Some("1234567890".to_string())
    );
}

#[test]
fn extract_tweet_id_from_twitter_com_status() {
    assert_eq!(
        extract_tweet_id("https://twitter.com/user/status/9876543210"),
        Some("9876543210".to_string())
    );
}

#[test]
fn extract_tweet_id_ignores_query_params() {
    assert_eq!(
        extract_tweet_id("https://x.com/user/status/111222333?s=20&t=abc"),
        Some("111222333".to_string())
    );
}

#[test]
fn extract_tweet_id_ignores_fragment() {
    assert_eq!(
        extract_tweet_id("https://x.com/user/status/555666777#anchor"),
        Some("555666777".to_string())
    );
}

#[test]
fn extract_tweet_id_returns_none_without_status_path() {
    assert_eq!(extract_tweet_id("https://x.com/user"), None);
    assert_eq!(extract_tweet_id("https://example.com/article"), None);
}

#[test]
fn extract_tweet_id_returns_none_without_digits() {
    // /status/ present but no digits after it
    assert_eq!(extract_tweet_id("https://x.com/user/status/"), None);
}

// ── x_linked_article_url / x_post_is_link_only ────────────────────────────

#[test]
fn x_linked_article_url_prefers_unwound_url() {
    let tweet = tweet_with_url_entity(
        "https://t.co/abc",
        json!({
            "url": "https://t.co/abc",
            "expanded_url": "https://x.com/DiarioBitcoin/status/123",
            "unwound_url": "https://www.diariobitcoin.com/test-article",
        }),
    );

    assert_eq!(
        x_linked_article_url(&tweet),
        Some("https://www.diariobitcoin.com/test-article".to_string())
    );
}

#[test]
fn x_linked_article_url_ignores_status_links() {
    let tweet = tweet_with_url_entity(
        "https://t.co/abc",
        json!({
            "url": "https://t.co/abc",
            "expanded_url": "https://x.com/DiarioBitcoin/status/123",
            "unwound_url": null,
        }),
    );

    assert_eq!(x_linked_article_url(&tweet), None);
}

#[test]
fn x_post_is_link_only_when_only_a_url_remains() {
    let tweet = tweet_with_url_entity(
        "https://t.co/abc",
        json!({
            "url": "https://t.co/abc",
            "expanded_url": "https://www.diariobitcoin.com/test-article",
            "unwound_url": null,
        }),
    );

    assert!(x_post_is_link_only(&tweet));
}

#[test]
fn x_post_is_not_link_only_when_text_remains() {
    let tweet = tweet_with_url_entity(
        "Analisis completo https://t.co/abc",
        json!({
            "url": "https://t.co/abc",
            "expanded_url": "https://www.diariobitcoin.com/test-article",
            "unwound_url": null,
        }),
    );

    assert!(!x_post_is_link_only(&tweet));
}

// ── x_article_plain_text ──────────────────────────────────────────────────

#[test]
fn x_article_plain_text_prefers_plain_text() {
    let article: XArticleMeta = serde_json::from_value(json!({
        "title": "Bitcoin bajo presión",
        "plain_text": "  Cuerpo completo del articulo  ",
        "preview_text": "Preview",
    }))
    .expect("test article JSON must deserialize");

    assert_eq!(
        x_article_plain_text(&article),
        Some("Cuerpo completo del articulo".to_string())
    );
}

#[test]
fn x_article_plain_text_falls_back_to_preview_text() {
    let article: XArticleMeta = serde_json::from_value(json!({
        "title": "Bitcoin bajo presión",
        "plain_text": null,
        "preview_text": "  Preview del articulo  ",
    }))
    .expect("test article JSON must deserialize");

    assert_eq!(
        x_article_plain_text(&article),
        Some("Preview del articulo".to_string())
    );
}

// ── is_x_article_url / x_article_body_unavailable ─────────────────────────

#[test]
fn is_x_article_url_matches_i_article_paths() {
    assert!(is_x_article_url(
        "https://x.com/i/article/2034262647731101696"
    ));
    assert!(!is_x_article_url(
        "https://x.com/DiarioBitcoin/status/2034263054754726116"
    ));
}

#[test]
fn x_article_body_unavailable_detects_guest_page() {
    let body = "<html><body><h1>This page is not supported.</h1><p>Please visit the author's profile on the latest version of X to view this content.</p></body></html>";
    assert!(x_article_body_unavailable(body));
}

// ── parse_scraped_post_from_html (X guest-wall guard) ─────────────────────

#[test]
fn parse_scraped_post_from_html_blocks_guest_x_article_page() {
    let post = parse_scraped_post_from_html(
        "https://x.com/i/article/2034262647731101696",
        "<html><body><h1>This page is not supported.</h1></body></html>",
        Some("Expected X Article Title"),
    );

    assert_eq!(post.title, "Expected X Article Title");
    assert!(post
        .error
        .contains("X article body is not available in the guest HTML response"));
}

// ── parse_x_web_article_post ──────────────────────────────────────────────

#[test]
fn parse_x_web_article_post_prefers_graphql_article_payload() {
    let body = r#"{
      "data": {
        "tweetResult": {
          "result": {
            "article": {
              "article_results": {
                "result": {
                  "title": "Bitcoin bajo presión",
                  "plain_text": "Primer parrafo.\n\nSegundo parrafo.",
                  "cover_media": {
                    "media_info": {
                      "original_img_url": "https://pbs.twimg.com/media/example.jpg"
                    }
                  }
                }
              }
            }
          }
        }
      }
    }"#;

    let post = parse_x_web_article_post(
        body,
        Some("Fallback title"),
        Some("2026-03-18T13:38:01.000Z".to_string()),
        Some("@DiarioBitcoin (Diario฿itcoin)".to_string()),
    )
    .expect("GraphQL payload should parse");

    assert_eq!(post.title, "Bitcoin bajo presión");
    assert_eq!(post.content, "Primer parrafo.\n\nSegundo parrafo.");
    assert_eq!(
        post.featured_image_url,
        "https://pbs.twimg.com/media/example.jpg"
    );
    assert_eq!(
        post.publication_date,
        Some("2026-03-18T13:38:01.000Z".to_string())
    );
    assert_eq!(
        post.author,
        Some("@DiarioBitcoin (Diario฿itcoin)".to_string())
    );
}
