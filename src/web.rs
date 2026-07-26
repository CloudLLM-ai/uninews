//! Plain-web (non-X) scraping pipeline.
//!
//! This module fetches a URL over HTTP, parses the HTML into a [`Post`],
//! and drives the fallback chain for difficult pages:
//!
//! 1. Plain HTTP fetch with a browser User-Agent.
//! 2. For X Article guest walls: headless-Chrome rendering
//!    ([`crate::browser`]).
//! 3. For bot-protection walls (Cloudflare & co.): Playwright Chromium
//!    render ([`crate::browser::fetch_rendered_dom_with_playwright`]),
//!    when enabled via `UNINEWS_PLAYWRIGHT` (default on).
//! 4. For thin content on an otherwise healthy page: the SAME Playwright
//!    render, triggered when a successful (2xx), non-walled, non-X
//!    response either fails content extraction, yields implausibly short
//!    content (under `MIN_CONTENT_BYTES`, 512 — JS-gated article bodies,
//!    e.g. longevity.technology returning a title + teaser from a large
//!    JS-driven page), or has a raw body under `JS_SHELL_MAX_BYTES`
//!    (16 KiB — JS application shells such as axios.com/technology, where
//!    the server returns only a script bundle). When the render fails or
//!    yields nothing usable, the original plain-fetch result is kept, so
//!    the trigger can never make a scrape worse. Thin-content pages are
//!    not archive.org-eligible, so this render is their only fallback.
//! 5. For remaining bot-protection walls and hard failures (network errors,
//!    5xx): the archive.org Wayback Machine fallback ([`crate::archive`]).
//! 6. LLM Markdown conversion of the extracted body ([`crate::llm`]).

use std::error::Error as StdError;
use std::fmt::Write as _;

use reqwest::header::HeaderMap;

use crate::archive::{archive_fallback_enabled, latest_snapshot, looks_like_bot_protection};
use crate::browser::{
    fetch_rendered_dom_with_chrome, fetch_rendered_dom_with_playwright, playwright_enabled,
};
use crate::events::{emit_event, ScrapeEvent};
use crate::fallback::{content_fallback_hook, ContentFallback};
use crate::html::parse_scraped_post_from_html;
use crate::http::web_client;
use crate::llm::convert_content_to_markdown;
use crate::util::is_youtube_url;
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
    /// The server answered with a 2xx status. Drives the thin-content
    /// Playwright trigger (which must not fire for 4xx/5xx or for fetches
    /// that never produced a response).
    status_success: bool,
    /// Size of the raw response body in bytes (0 when the fetch failed
    /// before a body was read). Compared against [`JS_SHELL_MAX_BYTES`]
    /// by the thin-content trigger.
    body_bytes: usize,
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

/// Maximum response body size accepted from a server, in bytes (16 MiB).
///
/// Bodies are read in bounded chunks ([`read_body_bounded`]) so a flooding
/// server cannot exhaust host memory before the request timeout fires;
/// oversize bodies fail as a network failure, which keeps them eligible
/// for the archive.org fallback like any other hard fetch failure.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Raw-body size under which a successful (2xx), non-walled page is
/// treated as a JavaScript shell and given one Playwright render attempt
/// (16 KiB).
///
/// A real server-rendered news page is never this small; SPA shells such
/// as axios.com/technology are (~5 KB of markup around a `<script>`
/// bundle, with no article text or links in the raw HTML). The render
/// either recovers the JS-produced content or fails cleanly, in which
/// case the original plain-fetch result is kept — the trigger can never
/// make a scrape worse.
const JS_SHELL_MAX_BYTES: usize = 16 * 1024;

/// Minimum extracted content (in bytes) accepted without a render retry.
/// A real article body is never this short; sub-threshold "successful"
/// extractions (e.g. longevity.technology returning a title + teaser from
/// a 534 KB JS-driven page) get the same Playwright retry as outright
/// extraction failures. The plain result is kept when the render does not
/// yield usable content, so this can only improve the outcome.
const MIN_CONTENT_BYTES: usize = 512;

/// Read a response body in bounded chunks, enforcing [`MAX_BODY_BYTES`].
///
/// Invalid UTF-8 is replaced with U+FFFD (browsers are equally tolerant);
/// the previous `Response::text()` behavior differed only for pages
/// declaring a non-UTF-8 charset, which the LLM conversion downstream
/// handles equally well via replacement characters.
async fn read_body_bounded(mut response: reqwest::Response) -> Result<String, String> {
    let mut body: Vec<u8> = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err(format!(
                        "Response body exceeded the {} MiB limit",
                        MAX_BODY_BYTES / (1024 * 1024)
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(err) => return Err(format!("Failed to read response body: {}", err)),
        }
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
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
                status_success: false,
                body_bytes: 0,
            };
        }
    };
    let response_url = response.url().to_string();
    let is_x_article = is_x_article_url(&response_url) || is_x_article_url(url);
    let response_status = response.status();
    let response_headers = response.headers().clone();
    let body_text = match read_body_bounded(response).await {
        Ok(text) => text,
        Err(err) => {
            emit_event(ScrapeEvent::FetchFailed {
                url: url.to_string(),
                error: err.clone(),
            });
            return RawFetch {
                post: error_post(err),
                network_failure: true,
                server_error: false,
                bot_protected: false,
                status_success: false,
                body_bytes: 0,
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
    let status_success = response_status.is_success();
    let body_bytes = body_text.len();
    let bot_protected =
        looks_like_bot_protection(response_status.as_u16(), &response_headers, &body_text);
    if bot_protected {
        emit_event(ScrapeEvent::BotProtectionDetected {
            url: response_url.clone(),
        });
    }

    let mut scraped_post = parse_scraped_post_from_html(&response_url, &body_text, title_override);

    // A challenge interstitial can still yield *some* extractable text; do
    // not mistake that for real article content. The override must happen
    // BEFORE the extraction event so a walled page that yielded junk text
    // reports ContentExtractionFailed, not a misleading ContentExtracted.
    if bot_protected && scraped_post.error.is_empty() {
        scraped_post.error =
            "The page appears to be behind a bot-protection wall (e.g. a Cloudflare challenge)."
                .to_string();
    }

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

    if scraped_post.error.is_empty() || !is_x_article {
        return RawFetch {
            post: scraped_post,
            network_failure: false,
            server_error,
            bot_protected,
            status_success,
            body_bytes,
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
                    status_success,
                    body_bytes,
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
                status_success,
                body_bytes,
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
            status_success,
            body_bytes,
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
            status_success,
            body_bytes,
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
        status_success,
        body_bytes,
    }
}

/// Try Playwright Chromium for a bot-protected or thin-content page.
/// Returns `Some(post)` when the rendered DOM yields usable article
/// content; `None` when the fallback is skipped, fails, or still looks
/// blocked (caller continues to archive.org or keeps the plain result).
async fn try_playwright_fallback(
    url: &str,
    title_override: Option<&str>,
    prior_error: &str,
) -> Option<Post> {
    if !playwright_enabled() {
        return None;
    }

    emit_event(ScrapeEvent::PlaywrightFallbackStarted {
        url: url.to_string(),
    });

    let html = match fetch_rendered_dom_with_playwright(url).await {
        Ok(html) => html,
        Err(err) => {
            emit_event(ScrapeEvent::PlaywrightFallbackFailed {
                url: url.to_string(),
                error: err,
            });
            return None;
        }
    };

    // Still a challenge interstitial → do not treat as success.
    if looks_like_bot_protection(200, &HeaderMap::new(), &html) {
        let msg = "Playwright rendered DOM still looks like a bot-protection wall".to_string();
        emit_event(ScrapeEvent::PlaywrightFallbackFailed {
            url: url.to_string(),
            error: msg,
        });
        return None;
    }

    let rendered = parse_scraped_post_from_html(url, &html, title_override);
    if rendered.error.is_empty() {
        emit_event(ScrapeEvent::PlaywrightFallbackSucceeded {
            url: url.to_string(),
            body_bytes: html.len(),
        });
        emit_event(ScrapeEvent::ContentExtracted {
            url: url.to_string(),
            content_bytes: rendered.content.len(),
        });
        return Some(rendered);
    }

    emit_event(ScrapeEvent::PlaywrightFallbackFailed {
        url: url.to_string(),
        error: format!(
            "rendered DOM extracted no usable content (prior: {prior_error}; extraction: {})",
            rendered.error
        ),
    });
    None
}

/// Consult the host-provided content fallback hook for `url`.
///
/// Returns `Some(post)` only when the hook produced usable content:
/// [`ContentFallback::Extracted`] with non-empty content (used as-is), or
/// [`ContentFallback::RenderedDom`] that survives bot-wall re-validation
/// *and* yields extractable article content. Returns `None` when no hook
/// is installed or the hook's output was unusable — callers continue
/// their normal fallback chain, so the hook can never make a scrape
/// worse.
async fn try_host_content_fallback(url: &str, title_override: Option<&str>) -> Option<Post> {
    let hook = content_fallback_hook()?;

    emit_event(ScrapeEvent::ContentFallbackStarted {
        url: url.to_string(),
    });

    match hook(url.to_string()).await {
        Ok(ContentFallback::Extracted { title, content }) => {
            if content.trim().is_empty() {
                emit_event(ScrapeEvent::ContentFallbackFailed {
                    url: url.to_string(),
                    error: "host fallback returned empty content".to_string(),
                });
                return None;
            }
            emit_event(ScrapeEvent::ContentFallbackSucceeded {
                url: url.to_string(),
                content_bytes: content.len(),
            });
            emit_event(ScrapeEvent::ContentExtracted {
                url: url.to_string(),
                content_bytes: content.len(),
            });
            Some(Post {
                title: title.unwrap_or_default(),
                content,
                featured_image_url: String::new(),
                publication_date: None,
                author: None,
                error: String::new(),
            })
        }
        Ok(ContentFallback::RenderedDom(html)) => {
            // A walled DOM from the host is not a success — re-validate
            // exactly like the built-in Playwright render path does.
            if looks_like_bot_protection(200, &HeaderMap::new(), &html) {
                emit_event(ScrapeEvent::ContentFallbackFailed {
                    url: url.to_string(),
                    error: "host-rendered DOM still looks like a bot-protection wall"
                        .to_string(),
                });
                return None;
            }
            let rendered = parse_scraped_post_from_html(url, &html, title_override);
            if rendered.error.is_empty() {
                emit_event(ScrapeEvent::ContentFallbackSucceeded {
                    url: url.to_string(),
                    content_bytes: html.len(),
                });
                emit_event(ScrapeEvent::ContentExtracted {
                    url: url.to_string(),
                    content_bytes: rendered.content.len(),
                });
                return Some(rendered);
            }
            emit_event(ScrapeEvent::ContentFallbackFailed {
                url: url.to_string(),
                error: format!(
                    "host-rendered DOM extracted no usable content: {}",
                    rendered.error
                ),
            });
            None
        }
        Err(err) => {
            emit_event(ScrapeEvent::ContentFallbackFailed {
                url: url.to_string(),
                error: err,
            });
            None
        }
    }
}

/// Fetch `url` and parse the HTML body into a [`Post`], without any LLM
/// conversion.
///
/// On failure the returned post carries the error in [`Post::error`].
/// Playwright Chromium is tried first (unless disabled via
/// `UNINEWS_PLAYWRIGHT=0`) for bot-protection walls and for thin-content
/// pages — a successful (2xx), non-walled, non-X response whose extraction
/// failed or whose raw body is under `JS_SHELL_MAX_BYTES`. When the render
/// does not yield usable content the original plain-fetch post is kept
/// untouched. Remaining bot walls and hard failures (network error, 5xx)
/// then go through the archive.org Wayback Machine fallback (unless
/// disabled via `UNINEWS_ARCHIVE_FALLBACK=0`); thin-content pages are not
/// archive-eligible, so Playwright is their only fallback.
async fn scrape_web_url_raw_with_title_override(url: &str, title_override: Option<&str>) -> Post {
    // URLs whose real payload never appears in the page HTML (YouTube
    // videos: the article-equivalent content is the transcript) go
    // straight to the host content fallback when one is installed. When
    // no hook is installed — or it cannot serve the URL — the normal
    // fetch pipeline below runs unchanged.
    if is_youtube_url(url) {
        if let Some(post) = try_host_content_fallback(url, title_override).await {
            return post;
        }
    }

    let raw = fetch_and_parse(url, title_override).await;

    // Thin-content trigger: a healthy, non-walled page whose extraction
    // failed (JS-gated body), whose extracted content is implausibly short
    // for a real article (`MIN_CONTENT_BYTES` — JS-gated body behind a
    // large shell page), or whose raw body is too small to be a real
    // server-rendered article (`JS_SHELL_MAX_BYTES` — JS shell) gets the
    // same Playwright render the wall path uses. Walled pages carry
    // markers, so in practice the two conditions are mutually exclusive —
    // and if both could apply, the wall path below wins by construction
    // (a walled page never satisfies `thin_content`). X URLs keep their
    // own dedicated chain.
    let thin_content = raw.status_success
        && !raw.bot_protected
        && !is_x_url(url)
        && (!raw.post.error.is_empty()
            || raw.post.content.len() < MIN_CONTENT_BYTES
            || raw.body_bytes < JS_SHELL_MAX_BYTES);

    if raw.post.error.is_empty() && !thin_content {
        return raw.post;
    }

    // Bot-protection / thin content → Playwright before archive.org
    // (fresher content, and the only fallback thin-content pages get).
    let mut post_after_playwright = raw.post;
    if raw.bot_protected || thin_content {
        if let Some(rendered) =
            try_playwright_fallback(url, title_override, &post_after_playwright.error).await
        {
            return rendered;
        }
        // Annotate so the final error chain shows Playwright was attempted.
        // A thin-shell page whose plain extraction SUCCEEDED keeps its
        // empty-error result untouched when the render does not help.
        if playwright_enabled() && !post_after_playwright.error.is_empty() {
            post_after_playwright.error = format!(
                "{} (Playwright Chromium fallback did not yield usable content)",
                post_after_playwright.error
            );
        }

        // Host content fallback (when installed): after the built-in
        // render, before archive.org. Covers walls the local render could
        // not pass and thin-content pages when local Playwright is
        // disabled or unavailable. On failure the chain below continues
        // exactly as if the hook were absent.
        if let Some(fallback_post) = try_host_content_fallback(url, title_override).await {
            return fallback_post;
        }
    }

    // The archive.org fallback covers bot-protection walls and hard
    // failures. X URLs keep their own dedicated fallback chain.
    let eligible = raw.bot_protected || raw.network_failure || raw.server_error;
    if !archive_fallback_enabled() || is_x_url(url) || !eligible {
        return post_after_playwright;
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
                    post_after_playwright.error, snapshot.url, archived.post.error
                ),
                ..post_after_playwright
            }
        }
        Ok(None) => {
            emit_event(ScrapeEvent::ArchiveSnapshotNotFound {
                url: url.to_string(),
            });
            Post {
                error: format!(
                    "{} (no archive.org snapshot available)",
                    post_after_playwright.error
                ),
                ..post_after_playwright
            }
        }
        Err(lookup_error) => {
            emit_event(ScrapeEvent::ArchiveLookupFailed {
                url: url.to_string(),
                error: lookup_error.clone(),
            });
            Post {
                error: format!(
                    "{} (archive.org lookup failed: {})",
                    post_after_playwright.error, lookup_error
                ),
                ..post_after_playwright
            }
        }
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
