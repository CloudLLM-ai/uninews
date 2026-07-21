//! Plain-web (non-X) scraping pipeline.
//!
//! This module fetches a URL over HTTP, parses the HTML into a [`Post`],
//! and drives the fallback chain for difficult pages:
//!
//! 1. Plain HTTP fetch with a browser User-Agent.
//! 2. For X Article guest walls: headless-Chrome rendering
//!    ([`crate::browser`]).
//! 3. For bot-protection walls and hard failures (network errors, 5xx):
//!    the archive.org Wayback Machine fallback ([`crate::archive`]).
//! 4. LLM Markdown conversion of the extracted body ([`crate::llm`]).

use std::error::Error as StdError;
use std::fmt::Write as _;

use crate::archive::{archive_fallback_enabled, latest_snapshot, looks_like_bot_protection};
use crate::browser::fetch_rendered_dom_with_chrome;
use crate::events::{emit_event, ScrapeEvent};
use crate::html::parse_scraped_post_from_html;
use crate::http::web_client;
use crate::llm::convert_content_to_markdown;
use crate::x::{
    is_x_article_url, is_x_url, x_article_body_unavailable, x_debug_dump,
    x_debug_dump_http_response,
};
use crate::Post;

/// Outcome of a single raw fetch + parse attempt, with the failure
/// classification needed to decide whether the archive.org fallback
/// applies.
struct RawFetch {
    /// The parsed post; carries the error (if any) in [`Post::error`].
    post: Post,
    /// No usable response at all (connect error, timeout, body read
    /// failure).
    network_failure: bool,
    /// The server answered with a 5xx status.
    server_error: bool,
    /// The response looks like a bot-protection wall (Cloudflare & co.).
    bot_protected: bool,
}

/// Build a [`Post`] carrying only an error message.
fn error_post(error: String) -> Post {
    Post {
        title: String::new(),
        content: String::new(),
        featured_image_url: String::new(),
        publication_date: None,
        author: None,
        error,
    }
}

/// Fetch `url`, parse the HTML body into a [`Post`], and classify any
/// failure for the archive.org fallback decision.
///
/// For X Article URLs whose guest HTML withholds the body, a headless-Chrome
/// render is attempted before giving up.
async fn fetch_and_parse(url: &str, title_override: Option<&str>) -> RawFetch {
    emit_event(ScrapeEvent::FetchStarted {
        url: url.to_string(),
    });

    let response = match web_client().get(url).send().await {
        Ok(response) => response,
        Err(err) => {
            // Walk the full error source chain so DNS/TLS/proxy causes are
            // visible in the final message.
            let mut msg = format!("Failed to fetch URL: {}", err);
            let mut src: Option<&dyn StdError> = err.source();
            while let Some(cause) = src {
                let _ = write!(msg, " => {}", cause);
                src = cause.source();
            }
            emit_event(ScrapeEvent::FetchFailed {
                url: url.to_string(),
                error: msg.clone(),
            });
            return RawFetch {
                post: error_post(msg),
                network_failure: true,
                server_error: false,
                bot_protected: false,
            };
        }
    };
    let response_url = response.url().to_string();
    let is_x_article = is_x_article_url(&response_url) || is_x_article_url(url);
    let response_status = response.status();
    let response_headers = response.headers().clone();
    let body_text = match response.text().await {
        Ok(text) => text,
        Err(err) => {
            emit_event(ScrapeEvent::FetchFailed {
                url: url.to_string(),
                error: format!("Failed to read response body: {}", err),
            });
            return RawFetch {
                post: error_post(format!("Failed to read response body: {}", err)),
                network_failure: true,
                server_error: false,
                bot_protected: false,
            };
        }
    };

    emit_event(ScrapeEvent::FetchSucceeded {
        url: response_url.clone(),
        status: response_status.as_u16(),
        body_bytes: body_text.len(),
    });

    if is_x_article {
        x_debug_dump_http_response(
            "X article page response",
            &response_url,
            response_status,
            &response_headers,
            &body_text,
        );
    }

    let server_error = response_status.is_server_error();
    let bot_protected =
        looks_like_bot_protection(response_status.as_u16(), &response_headers, &body_text);
    if bot_protected {
        emit_event(ScrapeEvent::BotProtectionDetected {
            url: response_url.clone(),
        });
    }

    let mut scraped_post = parse_scraped_post_from_html(&response_url, &body_text, title_override);
    if scraped_post.error.is_empty() {
        emit_event(ScrapeEvent::ContentExtracted {
            url: response_url.clone(),
            content_bytes: scraped_post.content.len(),
        });
    } else {
        emit_event(ScrapeEvent::ContentExtractionFailed {
            url: response_url.clone(),
            error: scraped_post.error.clone(),
        });
    }

    // A challenge interstitial can still yield *some* extractable text; do
    // not mistake that for real article content.
    if bot_protected && scraped_post.error.is_empty() {
        scraped_post.error =
            "The page appears to be behind a bot-protection wall (e.g. a Cloudflare challenge)."
                .to_string();
    }

    if scraped_post.error.is_empty() || !is_x_article {
        return RawFetch {
            post: scraped_post,
            network_failure: false,
            server_error,
            bot_protected,
        };
    }

    let rendered_dom = match fetch_rendered_dom_with_chrome(&response_url).await {
        Ok(rendered_dom) => rendered_dom,
        Err(browser_error) => {
            if x_article_body_unavailable(&body_text) {
                return RawFetch {
                    post: Post {
                        error: format!(
                            "X article body is not available to guest sessions. Set UNINEWS_CHROME_USER_DATA_DIR and optionally UNINEWS_CHROME_PROFILE_DIR to a logged-in Chrome profile. Browser fallback failed: {}",
                            browser_error
                        ),
                        ..scraped_post
                    },
                    network_failure: false,
                    server_error,
                    bot_protected,
                };
            }

            return RawFetch {
                post: Post {
                    error: format!(
                        "{} Chrome browser fallback failed: {}",
                        scraped_post.error, browser_error
                    ),
                    ..scraped_post
                },
                network_failure: false,
                server_error,
                bot_protected,
            };
        }
    };

    x_debug_dump("X article rendered DOM", &rendered_dom);

    let rendered_post = parse_scraped_post_from_html(&response_url, &rendered_dom, title_override);
    if rendered_post.error.is_empty() {
        return RawFetch {
            post: rendered_post,
            network_failure: false,
            server_error,
            bot_protected,
        };
    }

    if x_article_body_unavailable(&rendered_dom) {
        return RawFetch {
            post: Post {
                error: "X article body is not available to guest sessions. Set UNINEWS_CHROME_USER_DATA_DIR and optionally UNINEWS_CHROME_PROFILE_DIR to a logged-in Chrome profile.".to_string(),
                ..rendered_post
            },
            network_failure: false,
            server_error,
            bot_protected,
        };
    }

    RawFetch {
        post: Post {
            error: format!(
                "{} Browser-rendered fallback also failed: {}",
                scraped_post.error, rendered_post.error
            ),
            ..rendered_post
        },
        network_failure: false,
        server_error,
        bot_protected,
    }
}

/// Fetch `url` and parse the HTML body into a [`Post`], without any LLM
/// conversion.
///
/// On failure the returned post carries the error in [`Post::error`]. When
/// the failure is caused by bot protection or is a hard failure (network
/// error, 5xx), the archive.org Wayback Machine fallback is attempted first
/// (unless disabled via `UNINEWS_ARCHIVE_FALLBACK=0`).
async fn scrape_web_url_raw_with_title_override(url: &str, title_override: Option<&str>) -> Post {
    let raw = fetch_and_parse(url, title_override).await;
    if raw.post.error.is_empty() {
        return raw.post;
    }

    // The archive.org fallback covers bot-protection walls and hard
    // failures. X URLs keep their own dedicated fallback chain.
    let eligible = raw.bot_protected || raw.network_failure || raw.server_error;
    if !archive_fallback_enabled() || is_x_url(url) || !eligible {
        return raw.post;
    }

    let reason = if raw.bot_protected {
        "bot protection detected"
    } else if raw.network_failure {
        "network failure"
    } else {
        "server error (5xx)"
    };
    emit_event(ScrapeEvent::ArchiveFallbackStarted {
        url: url.to_string(),
        reason: reason.to_string(),
    });

    match latest_snapshot(url).await {
        Ok(Some(snapshot)) => {
            emit_event(ScrapeEvent::ArchiveSnapshotFound {
                url: url.to_string(),
                snapshot_url: snapshot.url.clone(),
                timestamp: snapshot.timestamp.clone(),
            });

            let archived = fetch_and_parse(&snapshot.url, title_override).await;
            if archived.post.error.is_empty() {
                return archived.post;
            }

            Post {
                error: format!(
                    "{} (archive.org snapshot {} also failed: {})",
                    raw.post.error, snapshot.url, archived.post.error
                ),
                ..raw.post
            }
        }
        Ok(None) => {
            emit_event(ScrapeEvent::ArchiveSnapshotNotFound {
                url: url.to_string(),
            });
            Post {
                error: format!("{} (no archive.org snapshot available)", raw.post.error),
                ..raw.post
            }
        }
        Err(lookup_error) => Post {
            error: format!(
                "{} (archive.org lookup failed: {})",
                raw.post.error, lookup_error
            ),
            ..raw.post
        },
    }
}

/// Fetch, parse, and Markdown-convert a web URL, honoring an optional title
/// override (used when following links out of X posts).
pub(crate) async fn scrape_web_url_with_title_override(
    url: &str,
    language: &str,
    title_override: Option<&str>,
    context_window_tokens: Option<usize>,
) -> Post {
    let scraped_post = scrape_web_url_raw_with_title_override(url, title_override).await;
    if !scraped_post.error.is_empty() {
        return scraped_post;
    }

    match convert_content_to_markdown(scraped_post.clone(), language, context_window_tokens).await {
        Ok(markdown_post) => markdown_post,
        Err(err) => Post {
            error: err,
            ..scraped_post
        },
    }
}

/// Fetch, parse, and Markdown-convert a plain web URL.
pub(crate) async fn scrape_web_url(
    url: &str,
    language: &str,
    context_window_tokens: Option<usize>,
) -> Post {
    scrape_web_url_with_title_override(url, language, None, context_window_tokens).await
}
