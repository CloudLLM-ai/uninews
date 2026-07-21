//! Plain-web (non-X) scraping pipeline.
//!
//! This module fetches a URL over HTTP, parses the HTML into a [`Post`],
//! and drives the fallback chain for difficult pages:
//!
//! 1. Plain HTTP fetch with a browser User-Agent.
//! 2. For X Article guest walls: headless-Chrome rendering
//!    ([`crate::browser`]).
//! 3. LLM Markdown conversion of the extracted body ([`crate::llm`]).

use std::error::Error as StdError;

use reqwest::Client;

use crate::browser::fetch_rendered_dom_with_chrome;
use crate::html::parse_scraped_post_from_html;
use crate::llm::convert_content_to_markdown;
use crate::util::BROWSER_USER_AGENT;
use crate::x::{
    is_x_article_url, x_article_body_unavailable, x_debug_dump, x_debug_dump_http_response,
};
use crate::Post;

/// Fetch `url` and parse the HTML body into a [`Post`], without any LLM
/// conversion.
///
/// On failure the returned post carries the error in [`Post::error`]. For
/// X Article URLs whose guest HTML withholds the body, a headless-Chrome
/// render is attempted before giving up.
async fn scrape_web_url_raw_with_title_override(url: &str, title_override: Option<&str>) -> Post {
    let client = Client::builder()
        .user_agent(BROWSER_USER_AGENT)
        .http1_only()
        .build()
        .unwrap_or_default();
    let response = client.get(url).send().await;

    if let Err(err) = response {
        let mut msg = format!("Failed to fetch URL: {}", err);
        let mut src: Option<&dyn StdError> = err.source();
        while let Some(cause) = src {
            msg.push_str(&format!(" => {}", cause));
            src = cause.source();
        }
        return Post {
            title: "".into(),
            content: "".into(),
            featured_image_url: "".into(),
            publication_date: None,
            author: None,
            error: msg,
        };
    }
    let response = response.unwrap();
    let response_url = response.url().to_string();
    let is_x_article = is_x_article_url(&response_url) || is_x_article_url(url);
    let response_status = response.status();
    let response_headers = response.headers().clone();
    let body_text = match response.text().await {
        Ok(text) => text,
        Err(err) => {
            return Post {
                title: "".into(),
                content: "".into(),
                featured_image_url: "".into(),
                publication_date: None,
                author: None,
                error: format!("Failed to read response body: {}", err),
            }
        }
    };

    if is_x_article {
        x_debug_dump_http_response(
            "X article page response",
            &response_url,
            response_status,
            &response_headers,
            &body_text,
        );
    }

    let scraped_post = parse_scraped_post_from_html(&response_url, &body_text, title_override);
    if scraped_post.error.is_empty() || !is_x_article {
        return scraped_post;
    }

    let rendered_dom = match fetch_rendered_dom_with_chrome(&response_url).await {
        Ok(rendered_dom) => rendered_dom,
        Err(browser_error) => {
            if x_article_body_unavailable(&body_text) {
                return Post {
                    error: format!(
                        "X article body is not available to guest sessions. Set UNINEWS_CHROME_USER_DATA_DIR and optionally UNINEWS_CHROME_PROFILE_DIR to a logged-in Chrome profile. Browser fallback failed: {}",
                        browser_error
                    ),
                    ..scraped_post
                };
            }

            return Post {
                error: format!(
                    "{} Chrome browser fallback failed: {}",
                    scraped_post.error, browser_error
                ),
                ..scraped_post
            };
        }
    };

    x_debug_dump("X article rendered DOM", &rendered_dom);

    let rendered_post = parse_scraped_post_from_html(&response_url, &rendered_dom, title_override);
    if rendered_post.error.is_empty() {
        return rendered_post;
    }

    if x_article_body_unavailable(&rendered_dom) {
        return Post {
            error: "X article body is not available to guest sessions. Set UNINEWS_CHROME_USER_DATA_DIR and optionally UNINEWS_CHROME_PROFILE_DIR to a logged-in Chrome profile.".to_string(),
            ..rendered_post
        };
    }

    Post {
        error: format!(
            "{} Browser-rendered fallback also failed: {}",
            scraped_post.error, rendered_post.error
        ),
        ..rendered_post
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
