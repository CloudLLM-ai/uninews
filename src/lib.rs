//! # Uninews - Universal News Scraper
//!
//! A powerful Rust library for scraping news articles from various websites and converting them to Markdown format using AI.
//!
//! ## Features
//!
//! - **Intelligent HTML Parsing**: Extracts article content from complex HTML structures
//! - **Smart Content Cleaning**: Automatically removes ads, scripts, navigation, and other noise
//! - **AI-Powered Formatting**: Converts raw HTML to near-lossless Markdown using OpenAI's GPT models
//! - **Metadata Extraction**: Captures title, author, publication date, and featured images
//! - **Multilingual Support**: Translates content to any language during processing
//! - **Async/Await**: Built with Tokio for efficient async operations
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use uninews::universal_scrape;
//!
//! #[tokio::main]
//! async fn main() {
//!     // Make sure OPEN_AI_SECRET environment variable is set
//!     let post = universal_scrape(
//!         "https://example.com/article",
//!         "english",
//!         None
//!     ).await;
//!
//!     if post.error.is_empty() {
//!         println!("Title: {}", post.title);
//!         println!("Author: {:?}", post.author);
//!         println!("Published: {:?}", post.publication_date);
//!         println!("\n{}", post.content); // Already formatted in Markdown
//!     } else {
//!         eprintln!("Error: {}", post.error);
//!     }
//! }
//! ```
//!
//! ## Requirements
//!
//! - Set the `OPEN_AI_SECRET` environment variable with your OpenAI API key
//! - The website must provide proper HTML structure and meta tags for best results
//!
//! ## Supported Metadata
//!
//! The scraper automatically extracts:
//! - **Title**: From `<title>` tag or `og:title` meta tag
//! - **Featured Image**: From `og:image` meta property
//! - **Publication Date**: From `article:published_time` meta property
//! - **Author**: From `author` meta tag
//!
//! ## Content Extraction Strategy
//!
//! The library uses a multi-step approach:
//! 1. Downloads HTML content from the provided URL
//! 2. Attempts to locate main content in `<article>` tags (priority) or `<body>` fallback
//! 3. Removes 17 types of unwanted elements (scripts, styles, ads, navigation, etc.)
//! 4. Cleans empty nodes and whitespace
//! 5. Converts remaining HTML to Markdown using AI while preserving article wording and structure
//! 6. Optionally translates to the requested language
//!
//! ## Error Handling
//!
//! Errors are non-fatal and returned in the [`Post::error`] field. Always check this field:
//!
//! ```rust,no_run
//! # use uninews::universal_scrape;
//! # #[tokio::main]
//! # async fn main() {
//! let post = universal_scrape("https://invalid-url-example", "english", None).await;
//!
//! if !post.error.is_empty() {
//!     match post.error.as_str() {
//!         e if e.contains("Failed to fetch") => println!("Network error"),
//!         e if e.contains("Could not extract meaningful content") => println!("Page structure not supported"),
//!         e if e.contains("LLM Error") => println!("AI processing error"),
//!         e => println!("Unknown error: {}", e),
//!     }
//! }
//! # }
//! ```

use reqwest::Client;
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::env;
use std::error::Error as StdError;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
// CloudLLM imports.
use cloudllm::client_wrapper::Role;
use cloudllm::clients::openai::{Model, OpenAIClient};
use cloudllm::LLMSession;

const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36";
const X_WEB_BEARER_TOKEN: &str =
    "AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs%3D1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA";
const X_WEB_TWEET_RESULT_BY_REST_ID_QUERY_ID: &str = "zy39CwTyYhU-_0LP7dljjg";

/// Represents a scraped news post with all extracted metadata.
///
/// This structure contains the complete article data extracted from a website,
/// including the Markdown-formatted content and associated metadata.
///
/// # Fields
///
/// - **title**: The article's title extracted from the `<title>` tag or meta tags
/// - **content**: The article body, automatically converted to Markdown format
/// - **featured_image_url**: URL to the main article image from Open Graph meta tag
/// - **publication_date**: ISO 8601 formatted publication date if available
/// - **author**: Article author extracted from meta tags
/// - **error**: Empty string on success, contains error message if scraping failed
///
/// # Examples
///
/// ```rust
/// # use uninews::Post;
/// // Create a post after scraping
/// let post = Post {
///     title: "Breaking News".to_string(),
///     content: "# Article Content\n\nThis is the main article...".to_string(),
///     featured_image_url: "https://example.com/image.jpg".to_string(),
///     publication_date: Some("2024-01-15T10:30:00Z".to_string()),
///     author: Some("Jane Doe".to_string()),
///     error: String::new(),
/// };
///
/// // Check if scraping was successful
/// assert!(post.error.is_empty());
/// assert_eq!(post.title, "Breaking News");
/// ```
///
/// # Error Handling
///
/// When scraping fails, the `Post` is still returned with an error message in the `error` field:
///
/// ```rust
/// # use uninews::Post;
/// let failed_post = Post {
///     title: String::new(),
///     content: String::new(),
///     featured_image_url: String::new(),
///     publication_date: None,
///     author: None,
///     error: "Failed to fetch URL: connection timeout".to_string(),
/// };
///
/// if !failed_post.error.is_empty() {
///     eprintln!("Scraping failed: {}", failed_post.error);
/// }
/// ```
#[derive(Debug, Serialize, Clone)]
pub struct Post {
    /// The article title
    pub title: String,
    /// The article content in Markdown format
    pub content: String,
    /// URL to the featured/hero image
    pub featured_image_url: String,
    /// Publication date (ISO 8601 format, if available)
    pub publication_date: Option<String>,
    /// Article author (if available)
    pub author: Option<String>,
    /// Error message; empty string if no error
    pub error: String,
}

/// Recursively cleans an element by skipping unwanted tags and empty content.
///
/// This private function is the core of the content extraction pipeline. It removes
/// unwanted HTML elements (like scripts and ads) while preserving meaningful content.
///
/// # Algorithm
///
/// For each element:
/// - If its tag name is in `skip_tags`, it is completely omitted
/// - Child nodes are processed recursively
/// - Only non-empty children (or non-whitespace text) are kept
/// - Elements with no content after cleaning return an empty string
///
/// # Example Processing
///
/// Input HTML:
/// ```html
/// <div>
///   <p>Keep this text</p>
///   <script>alert('remove me')</script>
///   <p></p>
/// </div>
/// ```
///
/// With `skip_tags` containing "script", output would be:
/// ```html
/// <div><p>Keep this text</p></div>
/// ```
///
/// # Parameters
///
/// - `element`: The HTML element to clean
/// - `skip_tags`: Set of tag names to completely remove
///
/// # Returns
///
/// Cleaned HTML as a string, or empty string if no content remains
#[must_use]
fn clean_element(element: ElementRef, skip_tags: &HashSet<&str>) -> String {
    let tag_name = element.value().name();
    if skip_tags.contains(tag_name) {
        return String::new();
    }

    let mut children_cleaned = String::new();

    // Process children: if an element or text node yields content, append it.
    for child in element.children() {
        if let Some(child_elem) = ElementRef::wrap(child) {
            let cleaned = clean_element(child_elem, skip_tags);
            if !cleaned.trim().is_empty() {
                children_cleaned.push_str(&cleaned);
                children_cleaned.push(' ');
            }
        } else if let Some(text) = child.value().as_text() {
            let text_trimmed = text.trim();
            if !text_trimmed.is_empty() {
                children_cleaned.push_str(text_trimmed);
                children_cleaned.push(' ');
            }
        }
    }

    // If nothing meaningful was found, return an empty string.
    if children_cleaned.trim().is_empty() {
        return String::new();
    }

    // Wrap the cleaned children in the current element's tag.
    format!(
        "<{tag}>{content}</{tag}>",
        tag = tag_name,
        content = children_cleaned.trim()
    )
}

/// Extracts and cleans main content from an HTML document.
///
/// This function implements the content extraction strategy used by the scraper.
/// It prioritizes the `<article>` tag (standard for news sites) but falls back
/// to `<body>` if no article is found.
///
/// # Strategy
///
/// 1. **Priority**: Try to find and clean an `<article>` element
/// 2. **Fallback**: If no article found, use the entire `<body>` element
/// 3. **Cleaning**: Apply the same tag filtering and whitespace removal as `clean_element`
///
/// # Why This Matters
///
/// Most news websites wrap their main article in semantic HTML5 `<article>` tags,
/// making this the most reliable extraction target. The fallback to `<body>` ensures
/// compatibility with less-structured websites.
///
/// # Parameters
///
/// - `document`: Parsed HTML document from scraper
/// - `skip_tags`: Set of unwanted tag names to remove
///
/// # Returns
///
/// Cleaned HTML content string, or empty string if document is malformed
#[must_use]
fn extract_clean_content(document: &Html, skip_tags: &HashSet<&str>) -> String {
    if let Ok(article_sel) = Selector::parse("article") {
        if let Some(article) = document.select(&article_sel).next() {
            let cleaned = clean_element(article, skip_tags);
            if !cleaned.trim().is_empty() {
                return cleaned;
            }
        }
    }

    // Fallback: use the <body>
    if let Ok(body_sel) = Selector::parse("body") {
        if let Some(body) = document.select(&body_sel).next() {
            return clean_element(body, skip_tags);
        }
    }
    String::new()
}

// ─────────────────────────────────────────────────────────────────────────────
// X.com / Twitter support
// ─────────────────────────────────────────────────────────────────────────────

/// A single tweet returned by the Twitter/X API v2.
#[derive(Deserialize, Debug)]
struct XTweet {
    id: String,
    text: String,
    created_at: Option<String>,
    author_id: Option<String>,
    conversation_id: Option<String>,
    article: Option<XArticleMeta>,
    entities: Option<XEntities>,
}

#[derive(Deserialize, Debug)]
struct XArticleMeta {
    title: Option<String>,
    plain_text: Option<String>,
    preview_text: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
struct XUrlEntity {
    url: Option<String>,
    expanded_url: Option<String>,
    unwound_url: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
struct XEntities {
    urls: Option<Vec<XUrlEntity>>,
}

/// Author information from the Twitter/X API v2 `includes.users` array.
#[derive(Deserialize, Debug)]
struct XUser {
    name: String,
    username: String,
    profile_image_url: Option<String>,
}

/// The `includes` block that accompanies expanded API responses.
#[derive(Deserialize, Debug)]
struct XIncludes {
    users: Option<Vec<XUser>>,
}

/// Top-level response for a single-tweet lookup (`GET /2/tweets/:id`).
#[derive(Deserialize, Debug)]
struct XTweetResponse {
    data: Option<XTweet>,
    includes: Option<XIncludes>,
    errors: Option<Vec<serde_json::Value>>,
}

/// Top-level response for a recent-search query (`GET /2/tweets/search/recent`).
#[derive(Deserialize, Debug)]
struct XSearchResponse {
    data: Option<Vec<XTweet>>,
}

/// Response returned by the X app-only token exchange endpoint.
#[derive(Deserialize, Debug)]
struct XBearerTokenResponse {
    token_type: String,
    access_token: String,
}

#[derive(Deserialize, Debug)]
struct XGuestActivateResponse {
    guest_token: String,
}

#[derive(Deserialize, Debug)]
struct XWebTweetResultResponse {
    data: Option<XWebTweetResultData>,
    errors: Option<Vec<serde_json::Value>>,
}

#[derive(Deserialize, Debug)]
struct XWebTweetResultData {
    #[serde(rename = "tweetResult")]
    tweet_result: Option<XWebTweetResultEnvelope>,
}

#[derive(Deserialize, Debug)]
struct XWebTweetResultEnvelope {
    result: Option<XWebTweetResult>,
}

#[derive(Deserialize, Debug)]
struct XWebTweetResult {
    article: Option<XWebArticleEnvelope>,
}

#[derive(Deserialize, Debug)]
struct XWebArticleEnvelope {
    #[serde(rename = "article_results")]
    article_results: Option<XWebArticleResults>,
}

#[derive(Deserialize, Debug)]
struct XWebArticleResults {
    result: Option<XWebArticle>,
}

#[derive(Deserialize, Debug)]
struct XWebArticle {
    title: Option<String>,
    plain_text: Option<String>,
    content_state: Option<XWebArticleContentState>,
    cover_media: Option<XWebArticleCoverMedia>,
}

#[derive(Deserialize, Debug)]
struct XWebArticleContentState {
    blocks: Option<Vec<XWebArticleBlock>>,
}

#[derive(Deserialize, Debug)]
struct XWebArticleBlock {
    text: Option<String>,
}

#[derive(Deserialize, Debug)]
struct XWebArticleCoverMedia {
    media_info: Option<XWebArticleMediaInfo>,
}

#[derive(Deserialize, Debug)]
struct XWebArticleMediaInfo {
    original_img_url: Option<String>,
}

/// Returns `true` when `url` belongs to X.com or Twitter.com.
///
/// # Examples
///
/// ```
/// // These are X/Twitter URLs:
/// //   https://x.com/user/status/1234567890
/// //   https://twitter.com/user/status/1234567890
/// ```
fn is_x_url(url: &str) -> bool {
    url.starts_with("https://x.com/") || url.starts_with("https://twitter.com/")
}

fn is_x_article_url(url: &str) -> bool {
    url.contains("x.com/i/article/") || url.contains("twitter.com/i/article/")
}

/// Extracts the numeric tweet ID from an X.com or Twitter.com status URL.
///
/// Supports trailing query-strings and fragments:
/// - `https://x.com/user/status/1234567890` → `Some("1234567890")`
/// - `https://twitter.com/user/status/1234567890?s=20` → `Some("1234567890")`
///
/// Returns `None` if no numeric ID can be found after `/status/`.
fn extract_tweet_id(url: &str) -> Option<String> {
    // Strip query-string and fragment before searching for the ID.
    let clean = url.split('?').next().unwrap_or(url);
    let clean = clean.split('#').next().unwrap_or(clean);

    const STATUS: &str = "/status/";
    if let Some(pos) = clean.find(STATUS) {
        let after = &clean[pos + STATUS.len()..];
        let id: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !id.is_empty() {
            return Some(id);
        }
    }
    None
}

fn first_non_empty_env_var(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| match env::var(key) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    })
}

fn x_api_error_message(body: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;

    if let Some(message) = parsed.get("error").and_then(|value| value.as_str()) {
        return Some(message.to_string());
    }

    if let Some(message) = parsed.get("detail").and_then(|value| value.as_str()) {
        return Some(message.to_string());
    }

    parsed
        .get("errors")
        .and_then(|value| value.as_array())
        .and_then(|errors| errors.first())
        .and_then(|error| {
            error
                .get("detail")
                .or_else(|| error.get("message"))
                .and_then(|value| value.as_str())
        })
        .map(ToString::to_string)
}

fn summarize_body(body: &str, max_len: usize) -> String {
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

fn normalized_output_language(language: &str) -> &str {
    if language.trim().is_empty() {
        "english"
    } else {
        language
    }
}

fn markdown_system_prompt(language: &str) -> String {
    format!(
        "You are an expert markdown formatter and translator for scraped news articles. \
         The provided JSON already contains the extracted article body in the `content` field. \
         Convert that content into clean Markdown in {} while preserving the source text and structure as fully as possible. \
         Do not summarize, paraphrase, compress, or omit substantive details. \
         Preserve paragraph order, list items, quotes, headings, names, dates, numbers, and factual claims. \
         Only remove obvious HTML tags, duplicated boilerplate, or navigation noise that slipped through the scraper. \
         If translation is requested, translate faithfully without shortening the article. \
         Output only the final Markdown body text. If {} is not supported, default to english.",
        language, language
    )
}

fn markdown_user_prompt(language: &str, post_json: &str) -> String {
    format!(
        "Convert the following Post JSON into Markdown formatted text in {}. \
         Treat `content` as the canonical article body and keep it nearly verbatim except for Markdown formatting, minimal cleanup, and faithful translation if needed. \
         Do not add commentary and do not return JSON.\n\n{}",
        language, post_json
    )
}

fn x_debug_enabled() -> bool {
    matches!(
        env::var("UNINEWS_DEBUG_X_JSON").ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

fn x_debug_dump(label: &str, body: &str) {
    if x_debug_enabled() {
        eprintln!("--- {} ---\n{}\n--- end {} ---", label, body, label);
    }
}

fn x_debug_dump_http_response(
    label: &str,
    url: &str,
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: &str,
) {
    if !x_debug_enabled() {
        return;
    }

    eprintln!("--- {} ---", label);
    eprintln!("url: {}", url);
    eprintln!("status: {}", status);
    for (name, value) in headers {
        eprintln!(
            "header {}: {}",
            name.as_str(),
            value.to_str().unwrap_or("<non-utf8>")
        );
    }
    eprintln!();
    eprintln!("{}", body);
    eprintln!("--- end {} ---", label);
}

fn x_url_is_status_link(url: &str) -> bool {
    url.contains("/status/")
}

fn normalize_text_url_token(token: &str) -> Option<String> {
    let trimmed = token.trim_matches(|c: char| {
        matches!(
            c,
            '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | '.' | ';' | ':'
        )
    });

    if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        return Some(trimmed.to_string());
    }

    None
}

fn x_text_urls(tweet: &XTweet) -> Vec<String> {
    let mut urls = Vec::new();

    if let Some(entity_urls) = tweet
        .entities
        .as_ref()
        .and_then(|entities| entities.urls.as_ref())
    {
        for url in entity_urls {
            for candidate in [&url.url, &url.expanded_url, &url.unwound_url]
                .into_iter()
                .flatten()
            {
                let candidate = candidate.trim();
                if !candidate.is_empty() && !urls.iter().any(|url| url == candidate) {
                    urls.push(candidate.to_string());
                }
            }
        }
    }

    for token in tweet.text.split_whitespace() {
        if let Some(candidate) = normalize_text_url_token(token) {
            if !urls.iter().any(|url| url == &candidate) {
                urls.push(candidate);
            }
        }
    }

    urls
}

fn x_linked_article_url(tweet: &XTweet) -> Option<String> {
    x_text_urls(tweet).into_iter().find(|candidate| {
        !candidate.is_empty()
            && !candidate.starts_with("https://t.co/")
            && !candidate.starts_with("http://t.co/")
            && !x_url_is_status_link(candidate)
    })
}

async fn resolve_url_redirect(client: &Client, url: &str) -> Option<String> {
    let response = client.get(url).send().await.ok()?;
    let final_url = response.url().as_str().trim().to_string();

    if final_url.is_empty()
        || final_url.starts_with("https://t.co/")
        || final_url.starts_with("http://t.co/")
        || x_url_is_status_link(&final_url)
    {
        return None;
    }

    Some(final_url)
}

async fn resolve_x_linked_article_url(client: &Client, tweet: &XTweet) -> Option<String> {
    if let Some(article_url) = x_linked_article_url(tweet) {
        return Some(article_url);
    }

    for candidate in x_text_urls(tweet) {
        if let Some(resolved_url) = resolve_url_redirect(client, &candidate).await {
            return Some(resolved_url);
        }
    }

    None
}

fn x_text_without_urls(tweet: &XTweet) -> String {
    let mut text = tweet.text.clone();

    for candidate in x_text_urls(tweet) {
        text = text.replace(&candidate, " ");
    }

    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn x_post_is_link_only(tweet: &XTweet) -> bool {
    x_text_without_urls(tweet).trim().is_empty()
}

fn x_article_plain_text(article: &XArticleMeta) -> Option<String> {
    article
        .plain_text
        .as_deref()
        .or(article.preview_text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

fn x_article_body_unavailable(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("this page is not supported")
        || lower.contains("please visit the author's profile")
        || lower.contains("javascript is not available")
}

fn x_web_article_body(article: &XWebArticle) -> Option<String> {
    if let Some(plain_text) = article.plain_text.as_ref() {
        let plain_text = plain_text.trim();
        if !plain_text.is_empty() {
            return Some(plain_text.to_string());
        }
    }

    let blocks = article.content_state.as_ref()?.blocks.as_ref()?;
    let block_text = blocks
        .iter()
        .filter_map(|block| block.text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n\n");

    if block_text.trim().is_empty() {
        None
    } else {
        Some(block_text)
    }
}

fn parse_x_web_article_post(
    body: &str,
    title_override: Option<&str>,
    publication_date: Option<String>,
    author: Option<String>,
) -> Result<Post, String> {
    let response: XWebTweetResultResponse = serde_json::from_str(body).map_err(|error| {
        format!(
            "Failed to parse X web GraphQL article response: {} ({})",
            error,
            summarize_body(body, 400)
        )
    })?;

    if let Some(errors) = response.errors.as_ref() {
        if !errors.is_empty() {
            let message = errors
                .first()
                .and_then(|error| error.get("message").or_else(|| error.get("detail")))
                .and_then(|value| value.as_str())
                .unwrap_or("Unknown X web GraphQL error");
            return Err(format!("X web GraphQL error: {}", message));
        }
    }

    let article = response
        .data
        .and_then(|data| data.tweet_result)
        .and_then(|tweet_result| tweet_result.result)
        .and_then(|tweet_result| tweet_result.article)
        .and_then(|article| article.article_results)
        .and_then(|results| results.result)
        .ok_or_else(|| "X web GraphQL response did not include an article payload.".to_string())?;

    let content = x_web_article_body(&article)
        .ok_or_else(|| "X web GraphQL response did not include article body text.".to_string())?;

    let title = article
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            title_override
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "X article".to_string());

    let featured_image_url = article
        .cover_media
        .and_then(|media| media.media_info)
        .and_then(|media_info| media_info.original_img_url)
        .unwrap_or_default();

    Ok(Post {
        title,
        content,
        featured_image_url,
        publication_date,
        author,
        error: String::new(),
    })
}

fn chrome_binary() -> String {
    if let Some(binary) = first_non_empty_env_var(&["UNINEWS_CHROME_BINARY"]) {
        return binary;
    }

    for candidate in [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ] {
        if Path::new(candidate).exists() {
            return candidate.to_string();
        }
    }

    "google-chrome".to_string()
}

fn should_skip_chrome_profile_entry(name: &str) -> bool {
    matches!(
        name,
        "SingletonCookie" | "SingletonLock" | "SingletonSocket" | "Crashpad"
    )
}

fn copy_dir_recursively(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;

    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let entry_name = entry.file_name();
        let entry_name = entry_name.to_string_lossy();

        if should_skip_chrome_profile_entry(&entry_name) {
            continue;
        }

        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());

        if file_type.is_dir() {
            copy_dir_recursively(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&source_path, &destination_path)?;
        }
    }

    Ok(())
}

fn clone_chrome_profile(
    source_user_data_dir: &Path,
    profile_name: &str,
) -> Result<(PathBuf, String), String> {
    let profile_source = source_user_data_dir.join(profile_name);
    if !profile_source.is_dir() {
        return Err(format!(
            "Chrome profile directory not found: {}",
            profile_source.display()
        ));
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let temp_root = env::temp_dir().join(format!(
        "uninews-chrome-profile-{}-{}",
        std::process::id(),
        nonce
    ));
    fs::create_dir_all(&temp_root).map_err(|err| {
        format!(
            "Failed to create temporary Chrome profile directory {}: {}",
            temp_root.display(),
            err
        )
    })?;

    for root_file in ["Local State", "First Run"] {
        let source_file = source_user_data_dir.join(root_file);
        if source_file.is_file() {
            let destination_file = temp_root.join(root_file);
            fs::copy(&source_file, &destination_file).map_err(|err| {
                format!(
                    "Failed to copy {} into temporary Chrome profile: {}",
                    source_file.display(),
                    err
                )
            })?;
        }
    }

    let staged_profile = temp_root.join(profile_name);
    copy_dir_recursively(&profile_source, &staged_profile).map_err(|err| {
        format!(
            "Failed to clone Chrome profile {} into {}: {}",
            profile_source.display(),
            staged_profile.display(),
            err
        )
    })?;

    Ok((temp_root, profile_name.to_string()))
}

async fn fetch_rendered_dom_with_chrome(url: &str) -> Result<String, String> {
    let browser_binary = chrome_binary();
    let user_data_dir = first_non_empty_env_var(&["UNINEWS_CHROME_USER_DATA_DIR"]);
    let profile_dir = first_non_empty_env_var(&["UNINEWS_CHROME_PROFILE_DIR"]);
    let url = url.to_string();
    let browser_binary_for_error = browser_binary.clone();
    let url_for_error = url.clone();

    let output = tokio::task::spawn_blocking(move || {
        let staged_profile = if let Some(user_data_dir) = user_data_dir.as_ref() {
            let profile_name = profile_dir.as_deref().unwrap_or("Default");
            Some(clone_chrome_profile(Path::new(user_data_dir), profile_name))
        } else {
            None
        };

        let (effective_user_data_dir, effective_profile_dir, staged_root) = match staged_profile {
            Some(Ok((temp_root, profile_name))) => {
                (Some(temp_root.clone()), Some(profile_name), Some(temp_root))
            }
            Some(Err(err)) => return Err(io::Error::other(err)),
            None => (None, profile_dir, None),
        };

        let mut command = Command::new(&browser_binary);
        command
            .arg("--headless=new")
            .arg("--disable-gpu")
            .arg("--virtual-time-budget=15000")
            .arg("--dump-dom");

        if let Some(user_data_dir) = effective_user_data_dir.as_ref() {
            command.arg(format!("--user-data-dir={}", user_data_dir.display()));
        }

        if let Some(profile_dir) = effective_profile_dir.as_ref() {
            command.arg(format!("--profile-directory={}", profile_dir));
        }

        command.arg(&url);
        let result = command.output();

        if let Some(staged_root) = staged_root {
            let _ = fs::remove_dir_all(staged_root);
        }

        result
    })
    .await
    .map_err(|err| format!("Chrome browser fallback task failed: {}", err))?
    .map_err(|err| format!("Failed to launch Chrome browser fallback: {}", err))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            summarize_body(stdout.as_ref(), 400)
        } else {
            "unknown error".to_string()
        };

        return Err(format!(
            "failed to render {} with {}: {}",
            url_for_error, browser_binary_for_error, detail
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn resolve_x_guest_token(client: &Client) -> Result<String, String> {
    let response = client
        .post("https://api.x.com/1.1/guest/activate.json")
        .header("Authorization", format!("Bearer {}", X_WEB_BEARER_TOKEN))
        .header("x-twitter-active-user", "yes")
        .header("x-twitter-client-language", "en")
        .send()
        .await
        .map_err(|error| format!("Failed to activate X guest token: {}", error))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("Failed to read X guest token response: {}", error))?;
    x_debug_dump("X guest token JSON", &body);

    if !status.is_success() {
        let message = x_api_error_message(&body).unwrap_or_else(|| summarize_body(&body, 400));
        return Err(format!(
            "Failed to activate X guest token (status {}): {}",
            status, message
        ));
    }

    let response: XGuestActivateResponse = serde_json::from_str(&body).map_err(|error| {
        format!(
            "Failed to parse X guest token response: {} ({})",
            error,
            summarize_body(&body, 400)
        )
    })?;

    Ok(response.guest_token)
}

async fn scrape_x_article_from_web_graphql(
    client: &Client,
    tweet_id: &str,
    title_override: Option<&str>,
    publication_date: Option<String>,
    author: Option<String>,
) -> Result<Post, String> {
    let guest_token = resolve_x_guest_token(client).await?;
    let endpoint = format!(
        "https://x.com/i/api/graphql/{}/TweetResultByRestId",
        X_WEB_TWEET_RESULT_BY_REST_ID_QUERY_ID
    );

    let variables = serde_json::json!({
        "tweetId": tweet_id,
        "withCommunity": false,
        "includePromotedContent": false,
        "withVoice": false
    })
    .to_string();
    let features = serde_json::json!({
        "withArticleRichContentState": true,
        "withArticlePlainText": true,
        "withArticleSummaryText": false,
        "withArticleVoiceOver": false
    })
    .to_string();
    let field_toggles = serde_json::json!({
        "withArticleRichContentState": true,
        "withArticlePlainText": true,
        "withArticleSummaryText": false,
        "withArticleVoiceOver": false
    })
    .to_string();

    let endpoint = reqwest::Url::parse_with_params(
        &endpoint,
        &[
            ("variables", variables),
            ("features", features),
            ("fieldToggles", field_toggles),
        ],
    )
    .map_err(|error| format!("Failed to build X web GraphQL URL: {}", error))?;

    let response = client
        .get(endpoint)
        .header("Authorization", format!("Bearer {}", X_WEB_BEARER_TOKEN))
        .header("x-guest-token", guest_token)
        .header("x-twitter-active-user", "yes")
        .header("x-twitter-client-language", "en")
        .send()
        .await
        .map_err(|error| format!("Failed to fetch X article via web GraphQL: {}", error))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("Failed to read X web GraphQL response body: {}", error))?;
    x_debug_dump("X web GraphQL JSON", &body);

    if !status.is_success() {
        let message = x_api_error_message(&body).unwrap_or_else(|| summarize_body(&body, 400));
        return Err(format!(
            "X web GraphQL returned HTTP {}: {}",
            status, message
        ));
    }

    parse_x_web_article_post(&body, title_override, publication_date, author)
}

fn parse_scraped_post_from_html(
    source_url: &str,
    body_text: &str,
    title_override: Option<&str>,
) -> Post {
    if is_x_article_url(source_url) && x_article_body_unavailable(body_text) {
        return Post {
            title: title_override.unwrap_or_default().trim().to_string(),
            content: String::new(),
            featured_image_url: String::new(),
            publication_date: None,
            author: None,
            error: "X article body is not available in the guest HTML response.".to_string(),
        };
    }

    let document = Html::parse_document(body_text);

    let skip_tags: HashSet<&str> = [
        "script", "style", "noscript", "iframe", "header", "footer", "nav", "aside", "form",
        "input", "button", "svg", "picture", "source",
    ]
    .iter()
    .cloned()
    .collect();

    let title_selector = Selector::parse("title").unwrap();
    let extracted_title = document
        .select(&title_selector)
        .next()
        .map(|elem| elem.text().collect::<Vec<_>>().join(" ").trim().to_string())
        .unwrap_or_default();
    let title = title_override
        .filter(|title| !title.trim().is_empty())
        .map(|title| title.trim().to_string())
        .unwrap_or(extracted_title);

    let content = extract_clean_content(&document, &skip_tags);

    let meta_selector = Selector::parse(r#"meta[property="og:image"]"#).unwrap();
    let featured_image_url = document
        .select(&meta_selector)
        .next()
        .and_then(|meta| meta.value().attr("content"))
        .unwrap_or("")
        .to_string();

    let date_selector = Selector::parse(r#"meta[property="article:published_time"]"#).unwrap();
    let publication_date = document
        .select(&date_selector)
        .next()
        .and_then(|meta| meta.value().attr("content"))
        .map(String::from);

    let author_selector = Selector::parse(r#"meta[name="author"]"#).unwrap();
    let author = document
        .select(&author_selector)
        .next()
        .and_then(|meta| meta.value().attr("content"))
        .map(String::from);

    if content.trim().is_empty() {
        return Post {
            title,
            content: String::new(),
            featured_image_url,
            publication_date,
            author,
            error: "Could not extract meaningful content from the page.".into(),
        };
    }

    Post {
        title,
        content,
        featured_image_url,
        publication_date,
        author,
        error: String::new(),
    }
}

async fn resolve_x_bearer_token(client: &Client) -> Result<String, String> {
    let api_key = first_non_empty_env_var(&["X_API_KEY", "DBTC_TWITTER_API_KEY"]);
    let api_secret = first_non_empty_env_var(&["X_API_SECRET", "DBTC_TWITTER_API_SECRET"]);

    let (api_key, api_secret) = match (api_key, api_secret) {
        (Some(api_key), Some(api_secret)) => (api_key, api_secret),
        _ => {
            return Err(
                "Please provide both X_API_KEY and X_API_SECRET (or DBTC_TWITTER_API_KEY and DBTC_TWITTER_API_SECRET).".into(),
            );
        }
    };

    let token_resp = client
        .post("https://api.x.com/oauth2/token")
        .basic_auth(api_key, Some(api_secret))
        .header(
            "Content-Type",
            "application/x-www-form-urlencoded;charset=UTF-8",
        )
        .body("grant_type=client_credentials")
        .send()
        .await
        .map_err(|e| format!("Failed to obtain X bearer token: {}", e))?;

    let status = token_resp.status();
    let body = token_resp
        .text()
        .await
        .map_err(|e| format!("Failed to read X bearer token response: {}", e))?;

    if !status.is_success() {
        let message = x_api_error_message(&body).unwrap_or(body);
        return Err(format!(
            "Failed to obtain X bearer token (status {}): {}",
            status, message
        ));
    }

    let token_data: XBearerTokenResponse = serde_json::from_str(&body)
        .map_err(|e| format!("Failed to parse X bearer token response: {} ({})", e, body))?;

    if !token_data.token_type.eq_ignore_ascii_case("bearer") {
        return Err(format!(
            "X token exchange returned unsupported token type: {}",
            token_data.token_type
        ));
    }

    Ok(token_data.access_token)
}

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

async fn scrape_web_url_with_title_override(
    url: &str,
    language: &str,
    openai_model: Option<Model>,
    title_override: Option<&str>,
) -> Post {
    let scraped_post = scrape_web_url_raw_with_title_override(url, title_override).await;
    if !scraped_post.error.is_empty() {
        return scraped_post;
    }

    match convert_content_to_markdown(scraped_post.clone(), language, openai_model).await {
        Ok(markdown_post) => markdown_post,
        Err(err) => Post {
            error: err,
            ..scraped_post
        },
    }
}

async fn scrape_web_url(url: &str, language: &str, openai_model: Option<Model>) -> Post {
    scrape_web_url_with_title_override(url, language, openai_model, None).await
}

/// Fetches a tweet or X thread via the Twitter/X API v2 and returns a [`Post`].
///
/// # Authentication
///
/// Uses `X_API_KEY` and `X_API_SECRET` to exchange for an OAuth 2.0
/// app-only Bearer Token before calling the X API.
///
/// # Thread handling
///
/// When the fetched tweet is the root of a thread (or part of one), the
/// function also queries the recent-search endpoint to collect all tweets
/// in the same conversation that were posted by the same author, then
/// sorts them chronologically so the thread reads naturally.
///
/// > **Note:** The recent-search endpoint only covers the last 7 days.
/// > Tweets older than 7 days are still returned as a single-tweet post.
///
/// # Errors
///
/// All errors are non-fatal and are returned inside [`Post::error`].
async fn scrape_x_url(url: &str, language: &str, openai_model: Option<Model>) -> Post {
    // ── 1. Extract the tweet ID from the URL ─────────────────────────────────
    let tweet_id = match extract_tweet_id(url) {
        Some(id) => id,
        None => {
            return Post {
                title: String::new(),
                content: String::new(),
                featured_image_url: String::new(),
                publication_date: None,
                author: None,
                error: format!("Could not extract a tweet ID from the URL: {}", url),
            };
        }
    };

    // ── 2. Build the HTTP client ──────────────────────────────────────────────
    let client = Client::builder()
        .user_agent(BROWSER_USER_AGENT)
        .build()
        .unwrap_or_default();

    // ── 3. Resolve the Bearer Token ──────────────────────────────────────────
    let bearer_token = match resolve_x_bearer_token(&client).await {
        Ok(token) => token,
        Err(error) => {
            return Post {
                title: String::new(),
                content: String::new(),
                featured_image_url: String::new(),
                publication_date: None,
                author: None,
                error,
            };
        }
    };

    let auth_header = format!("Bearer {}", bearer_token);

    // ── 4. Fetch the root tweet ───────────────────────────────────────────────
    let root_tweet_url = format!(
        "https://api.x.com/2/tweets/{}?tweet.fields=created_at,author_id,conversation_id,text,entities,article&expansions=author_id&user.fields=name,username,profile_image_url",
        tweet_id
    );
    let root_resp = match client
        .get(&root_tweet_url)
        .header("Authorization", &auth_header)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return Post {
                title: String::new(),
                content: String::new(),
                featured_image_url: String::new(),
                publication_date: None,
                author: None,
                error: format!("Failed to call X API: {}", e),
            };
        }
    };

    let root_status = root_resp.status();
    let root_body = match root_resp.text().await {
        Ok(body) => body,
        Err(e) => {
            return Post {
                title: String::new(),
                content: String::new(),
                featured_image_url: String::new(),
                publication_date: None,
                author: None,
                error: format!("Failed to read X API response body: {}", e),
            };
        }
    };
    x_debug_dump("X root tweet JSON", &root_body);

    if !root_status.is_success() {
        let message =
            x_api_error_message(&root_body).unwrap_or_else(|| summarize_body(&root_body, 400));
        return Post {
            title: String::new(),
            content: String::new(),
            featured_image_url: String::new(),
            publication_date: None,
            author: None,
            error: format!("X API returned HTTP {}: {}", root_status, message),
        };
    }

    let root_data: XTweetResponse = match serde_json::from_str(&root_body) {
        Ok(d) => d,
        Err(e) => {
            return Post {
                title: String::new(),
                content: String::new(),
                featured_image_url: String::new(),
                publication_date: None,
                author: None,
                error: format!(
                    "Failed to parse X API response: {} ({})",
                    e,
                    summarize_body(&root_body, 400)
                ),
            };
        }
    };

    // Surface API-level errors (e.g. tweet not found, bad credentials).
    if let Some(errors) = &root_data.errors {
        if !errors.is_empty() {
            let msg = errors
                .first()
                .and_then(|e| e.get("detail").or_else(|| e.get("message")))
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown X API error");
            return Post {
                title: String::new(),
                content: String::new(),
                featured_image_url: String::new(),
                publication_date: None,
                author: None,
                error: format!("X API error: {}", msg),
            };
        }
    }

    let root_tweet = match root_data.data {
        Some(t) => t,
        None => {
            return Post {
                title: String::new(),
                content: String::new(),
                featured_image_url: String::new(),
                publication_date: None,
                author: None,
                error: format!(
                    "X API returned no tweet data. Response body: {}",
                    summarize_body(&root_body, 400)
                ),
            };
        }
    };

    // Resolve the author's display name and profile image.
    let author_info = root_data
        .includes
        .as_ref()
        .and_then(|inc| inc.users.as_ref())
        .and_then(|users| users.first());

    let author_display = author_info.map(|u| format!("@{} ({})", u.username, u.name));
    let profile_image = author_info
        .and_then(|u| u.profile_image_url.clone())
        .unwrap_or_default();

    let author_id = root_tweet.author_id.clone().unwrap_or_default();
    let conversation_id = root_tweet
        .conversation_id
        .clone()
        .unwrap_or_else(|| root_tweet.id.clone());

    if x_post_is_link_only(&root_tweet) {
        let article_title_override = root_tweet
            .article
            .as_ref()
            .and_then(|article| article.title.as_deref());
        let embedded_article_body = root_tweet.article.as_ref().and_then(x_article_plain_text);

        if let Some(content) = embedded_article_body {
            let scraped_article_post = Post {
                title: article_title_override
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .unwrap_or("X article")
                    .to_string(),
                content,
                featured_image_url: profile_image.clone(),
                publication_date: root_tweet.created_at.clone(),
                author: author_display.clone(),
                error: String::new(),
            };

            return match convert_content_to_markdown(
                scraped_article_post.clone(),
                language,
                openai_model,
            )
            .await
            {
                Ok(markdown_post) => markdown_post,
                Err(err) => Post {
                    error: err,
                    ..scraped_article_post
                },
            };
        }

        if let Some(article_url) = resolve_x_linked_article_url(&client, &root_tweet).await {
            if is_x_article_url(&article_url) {
                match scrape_x_article_from_web_graphql(
                    &client,
                    &root_tweet.id,
                    article_title_override,
                    root_tweet.created_at.clone(),
                    author_display.clone(),
                )
                .await
                {
                    Ok(scraped_article_post) => {
                        return match convert_content_to_markdown(
                            scraped_article_post.clone(),
                            language,
                            openai_model,
                        )
                        .await
                        {
                            Ok(markdown_post) => markdown_post,
                            Err(err) => Post {
                                error: err,
                                ..scraped_article_post
                            },
                        };
                    }
                    Err(graphql_error) => {
                        let article_post = scrape_web_url_with_title_override(
                            &article_url,
                            language,
                            openai_model,
                            article_title_override,
                        )
                        .await;
                        if article_post.error.is_empty() {
                            return article_post;
                        }

                        return Post {
                            title: article_post.title,
                            content: article_post.content,
                            featured_image_url: article_post.featured_image_url,
                            publication_date: article_post.publication_date,
                            author: article_post.author,
                            error: format!(
                                "Failed to scrape linked X article {} via X web GraphQL: {}. HTML fallback failed: {}",
                                article_url, graphql_error, article_post.error
                            ),
                        };
                    }
                }
            }

            let article_post = scrape_web_url_with_title_override(
                &article_url,
                language,
                openai_model,
                article_title_override,
            )
            .await;
            if article_post.error.is_empty() {
                return article_post;
            }

            return Post {
                title: article_post.title,
                content: article_post.content,
                featured_image_url: article_post.featured_image_url,
                publication_date: article_post.publication_date,
                author: article_post.author,
                error: format!(
                    "Failed to scrape linked article {}: {}",
                    article_url, article_post.error
                ),
            };
        }
    }

    // ── 5. Collect the full thread ────────────────────────────────────────────
    // Seed the list with the root tweet.
    let mut thread_tweets: Vec<(String, String)> = vec![(
        root_tweet.created_at.clone().unwrap_or_default(),
        root_tweet.text.clone(),
    )];

    // Try to fetch the rest of the conversation from the recent-search endpoint.
    // This only covers the last 7 days; for older tweets we fall back to the
    // single tweet already captured above.
    let search_url = format!(
        "https://api.x.com/2/tweets/search/recent?query=conversation_id%3A{}&tweet.fields=created_at,author_id,text,entities&max_results=100",
        conversation_id
    );
    if let Ok(search_resp) = client
        .get(&search_url)
        .header("Authorization", &auth_header)
        .send()
        .await
    {
        if let Ok(search_body) = search_resp.text().await {
            x_debug_dump("X recent search JSON", &search_body);
            if let Ok(search_data) = serde_json::from_str::<XSearchResponse>(&search_body) {
                if let Some(tweets) = search_data.data {
                    for t in tweets {
                        // Only include tweets from the same author (i.e. the thread,
                        // not replies from other users). Guard against an empty
                        // author_id (which would match any tweet lacking the field).
                        let same_author = !author_id.is_empty()
                            && t.author_id.as_deref() == Some(author_id.as_str());
                        if same_author && t.id != root_tweet.id {
                            thread_tweets.push((t.created_at.unwrap_or_default(), t.text));
                        }
                    }
                }
            }
        }
    }

    // Sort chronologically so the thread reads oldest → newest.
    thread_tweets.sort_by(|a, b| a.0.cmp(&b.0));

    // ── 6. Assemble the Post ──────────────────────────────────────────────────
    let title = format!(
        "{}: {}",
        author_display.as_deref().unwrap_or("X post"),
        root_tweet.text.chars().take(80).collect::<String>()
    );

    let content = thread_tweets
        .iter()
        .map(|(ts, text)| {
            if ts.is_empty() {
                text.clone()
            } else {
                format!("[{}] {}", ts, text)
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let scraped_post = Post {
        title,
        content,
        featured_image_url: profile_image,
        publication_date: root_tweet.created_at,
        author: author_display,
        error: String::new(),
    };

    // ── 7. AI Markdown conversion & optional translation ──────────────────────
    match convert_content_to_markdown(scraped_post.clone(), language, openai_model).await {
        Ok(markdown_post) => markdown_post,
        Err(err) => Post {
            error: err,
            ..scraped_post
        },
    }
}

/// Converts raw HTML content to Markdown using OpenAI's GPT models.
///
/// This function takes scraped HTML content and transforms it into beautifully formatted
/// Markdown. It uses the CloudLLM library to communicate with OpenAI's API, allowing
/// for intelligent formatting and optional translation.
///
/// # How It Works
///
/// 1. Retrieves OpenAI API key from `OPEN_AI_SECRET` environment variable
/// 2. Initializes OpenAI client (uses GPT-5.4 by default)
/// 3. Creates an LLMSession with a system prompt instructing Markdown formatting
/// 4. Sends the scraped Post as JSON to the LLM
/// 5. Updates the Post's `content` field with formatted Markdown
/// 6. Optionally translates to the requested language
///
/// # Arguments
///
/// - `post`: The scraped Post with raw HTML content
/// - `language`: Target language for output (e.g., "spanish", "french", "japanese")
/// - `openai_model`: Optional specific GPT model to use (defaults to GPT-5.4)
///
/// # Returns
///
/// - `Ok(Post)`: Updated post with Markdown-formatted content in the target language
/// - `Err(String)`: Error message if API communication fails or environment variables are missing
///
/// # Environment Variables
///
/// Requires: `OPEN_AI_SECRET` - Your OpenAI API key
///
/// # Errors
///
/// Returns error if:
/// - `OPEN_AI_SECRET` environment variable is not set
/// - Post serialization to JSON fails
/// - OpenAI API communication fails
/// - LLM returns an error response
///
/// # Examples
///
/// ```rust,no_run
/// # use uninews::{Post, convert_content_to_markdown};
/// # use cloudllm::clients::openai::Model;
/// #[tokio::main]
/// async fn main() {
///     let post = Post {
///         title: "Article Title".to_string(),
///         content: "<p>Raw HTML content</p>".to_string(),
///         featured_image_url: "".to_string(),
///         publication_date: None,
///         author: None,
///         error: String::new(),
///     };
///
///     // Convert with default model
///     match convert_content_to_markdown(post, "english", None).await {
///         Ok(markdown_post) => println!("{}", markdown_post.content),
///         Err(e) => eprintln!("Conversion failed: {}", e),
///     }
/// }
/// ```
///
/// # Supported Languages
///
/// Supports any language that OpenAI's GPT models understand, including
/// - English, Spanish, French, German, Italian
/// - Chinese, Japanese, Korean
/// - Portuguese, Russian, Arabic
/// - And many more...
///
/// If the specified language is not recognized, the output defaults to English.
pub async fn convert_content_to_markdown(
    mut post: Post,
    language: &str,
    openai_model: Option<Model>,
) -> Result<Post, String> {
    // Get the secret key from the environment.
    let secret_key = env::var("OPEN_AI_SECRET")
        .map_err(|_| "Please set the OPEN_AI_SECRET environment variable.".to_string())?;

    // Instantiate the OpenAI client, defaulting to GPT-5.4 unless overridden.
    let model = openai_model.unwrap_or(Model::GPT54);
    let client = Arc::new(OpenAIClient::new_with_model_enum(&secret_key, model));

    // Normalize language: if empty, default to "english".
    let lang = normalized_output_language(language);

    // Define a system prompt that instructs the LLM on its role.
    let system_prompt = markdown_system_prompt(lang);

    // Create a new LLMSession.
    let mut session = LLMSession::new(client, system_prompt, 1000000);

    // Serialize the entire Post to JSON.
    let post_json = serde_json::to_string(&post)
        .map_err(|e| format!("Failed to serialize Post to JSON: {}", e))?;
    let user_prompt = markdown_user_prompt(lang, &post_json);

    // Send the prompt to the LLM.
    match session.send_message(Role::User, user_prompt, None).await {
        Ok(response) => {
            post.content = response.content.to_string();
            Ok(post)
        }
        Err(err) => Err(format!("LLM Error: {}", err)),
    }
}

/// The main API function - scrapes a URL and returns structured article data.
///
/// This is the primary entry point for library users. It handles the complete workflow:
/// fetching HTML, extracting content, cleaning noise, and converting to Markdown.
///
/// # Complete Workflow
///
/// 1. **HTTP Fetch**: Downloads HTML from the provided URL using reqwest
/// 2. **HTML Parse**: Parses HTML document using the scraper library
/// 3. **Title Extraction**: Gets title from `<title>` tag
/// 4. **Content Extraction**: Intelligently extracts main article content
/// 5. **Metadata Extraction**: Retrieves author, publication date, and featured image
/// 6. **Content Cleaning**: Removes 17 categories of unwanted elements
/// 7. **Markdown Conversion**: Uses OpenAI to convert HTML to formatted Markdown
/// 8. **Translation**: Optionally translates to requested language
///
/// # Arguments
///
/// - `url`: The URL of the article to scrape (must be a complete, valid URL)
/// - `language`: Target language for output ("english", "spanish", "french", etc.)
/// - `openai_model`: Optional OpenAI model to use; defaults to GPT-5.4
///
/// # Returns
///
/// Always returns a `Post` struct. On success, it contains the article data with
/// `error` field empty. On failure, check the `error` field for details.
///
/// # Error Handling (Non-Panicking)
///
/// This function is designed to never panic. All errors are gracefully handled:
/// - Network errors → error message in `Post::error`
/// - HTML parsing failures → error message in `Post::error`
/// - Content extraction failures → error message in `Post::error`
/// - LLM API errors → error message in `Post::error`
///
/// # Environment Variables
///
/// Requires: `OPEN_AI_SECRET` - Your OpenAI API key
///
/// # Performance Considerations
///
/// - Network requests are the primary bottleneck
/// - LLM processing typically takes 2-5 seconds per article
/// - HTML parsing is fast (< 100ms for most pages)
/// - Content cleaning is O(n) where n = DOM tree size
///
/// # Removed Elements (Clean Tags)
///
/// The scraper automatically removes these 17 element types:
/// - **Metadata**: `script`, `style`, `noscript`
/// - **Navigation**: `header`, `footer`, `nav`, `aside`
/// - **Ads/Forms**: `form`, `input`, `button`, `iframe`
/// - **Media**: `svg`, `picture`, `source`
///
/// # Examples
///
/// ## Basic Usage
///
/// ```rust,no_run
/// # use uninews::universal_scrape;
/// #[tokio::main]
/// async fn main() {
///     // Scrape with default English output
///     let post = universal_scrape(
///         "https://www.example.com/news/article",
///         "english",
///         None
///     ).await;
///
///     if post.error.is_empty() {
///         println!("✓ Successfully scraped!");
///         println!("Title: {}", post.title);
///         println!("Author: {:?}", post.author);
///         println!("Published: {:?}", post.publication_date);
///         println!("\n{}", post.content);
///     } else {
///         eprintln!("✗ Error: {}", post.error);
///     }
/// }
/// ```
///
/// ## Translated Output
///
/// ```rust,no_run
/// # use uninews::universal_scrape;
/// #[tokio::main]
/// async fn main() {
///     // Scrape and translate to Spanish
///     let post = universal_scrape(
///         "https://www.bbc.com/news/article",
///         "spanish",
///         None
///     ).await;
///
///     if post.error.is_empty() {
///         // Content is now in Spanish
///         println!("Artículo: {}", post.title);
///     }
/// }
/// ```
///
/// ## Custom Model Selection
///
/// ```rust,no_run
/// # use uninews::universal_scrape;
/// # use cloudllm::clients::openai::Model;
/// #[tokio::main]
/// async fn main() {
///     let post = universal_scrape(
///         "https://www.example.com/article",
///         "english",
///         Some(Model::GPT54) // Explicitly specify model
///     ).await;
/// }
/// ```
///
/// # Real-World Example: Building an RSS Reader
///
/// ```rust,no_run
/// # use uninews::universal_scrape;
/// #[tokio::main]
/// async fn main() {
///     let article_urls = vec![
///         "https://example.com/article1",
///         "https://example.com/article2",
///     ];
///
///     for url in article_urls {
///         let post = universal_scrape(url, "english", None).await;
///
///         if post.error.is_empty() {
///             // Successfully processed
///             let data = serde_json::json!({
///                 "title": post.title,
///                 "content": post.content,
///                 "author": post.author,
///                 "published": post.publication_date,
///                 "image": post.featured_image_url,
///             });
///             println!("{}", data);
///         }
///     }
/// }
/// ```
///
/// # Supported Websites
///
/// Works best with sites that follow semantic HTML5 standards:
/// - News publishers (BBC, CNN, Reuters, etc.)
/// - Blog platforms (Medium, Substack, etc.)
/// - Tech sites (Hacker News, Dev.to, etc.)
/// - Most modern CMS-based sites
///
/// May have limited success with:
/// - JavaScript-heavy single-page apps (content loaded dynamically)
/// - Paywalled content
/// - Sites with aggressive anti-scraping measures
pub async fn universal_scrape(url: &str, language: &str, openai_model: Option<Model>) -> Post {
    // Delegate to the X.com handler for X / Twitter URLs.
    if is_x_url(url) {
        return scrape_x_url(url, language, openai_model).await;
    }
    scrape_web_url(url, language, openai_model).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_x_url ──────────────────────────────────────────────────────────────

    #[test]
    fn test_is_x_url_x_com() {
        assert!(is_x_url("https://x.com/user/status/123"));
    }

    #[test]
    fn test_is_x_url_twitter_com() {
        assert!(is_x_url("https://twitter.com/user/status/123"));
    }

    #[test]
    fn test_is_x_url_non_x() {
        assert!(!is_x_url("https://example.com/article"));
        assert!(!is_x_url("https://bbc.com/news/world"));
        assert!(!is_x_url("http://x.com/user/status/123")); // http, not https
        assert!(!is_x_url("https://notx.com/user/status/123"));
        assert!(!is_x_url("https://x.com")); // missing trailing slash and path
        assert!(!is_x_url("https://twitter.com")); // missing trailing slash and path
    }

    // ── extract_tweet_id ──────────────────────────────────────────────────────

    #[test]
    fn test_extract_tweet_id_x_com() {
        assert_eq!(
            extract_tweet_id("https://x.com/user/status/1234567890"),
            Some("1234567890".to_string())
        );
    }

    #[test]
    fn test_extract_tweet_id_twitter_com() {
        assert_eq!(
            extract_tweet_id("https://twitter.com/user/status/9876543210"),
            Some("9876543210".to_string())
        );
    }

    #[test]
    fn test_extract_tweet_id_with_query_params() {
        assert_eq!(
            extract_tweet_id("https://x.com/user/status/111222333?s=20&t=abc"),
            Some("111222333".to_string())
        );
    }

    #[test]
    fn test_extract_tweet_id_with_fragment() {
        assert_eq!(
            extract_tweet_id("https://x.com/user/status/555666777#anchor"),
            Some("555666777".to_string())
        );
    }

    #[test]
    fn test_extract_tweet_id_no_status() {
        assert_eq!(extract_tweet_id("https://x.com/user"), None);
        assert_eq!(extract_tweet_id("https://example.com/article"), None);
    }

    #[test]
    fn test_extract_tweet_id_empty_status() {
        // /status/ present but no digits after it
        assert_eq!(extract_tweet_id("https://x.com/user/status/"), None);
    }

    #[test]
    fn test_x_linked_article_url_prefers_unwound_url() {
        let tweet = XTweet {
            id: "1".to_string(),
            text: "https://t.co/abc".to_string(),
            created_at: None,
            author_id: None,
            conversation_id: None,
            article: None,
            entities: Some(XEntities {
                urls: Some(vec![XUrlEntity {
                    url: Some("https://t.co/abc".to_string()),
                    expanded_url: Some("https://x.com/DiarioBitcoin/status/123".to_string()),
                    unwound_url: Some("https://www.diariobitcoin.com/test-article".to_string()),
                }]),
            }),
        };

        assert_eq!(
            x_linked_article_url(&tweet),
            Some("https://www.diariobitcoin.com/test-article".to_string())
        );
    }

    #[test]
    fn test_x_linked_article_url_ignores_status_links() {
        let tweet = XTweet {
            id: "1".to_string(),
            text: "https://t.co/abc".to_string(),
            created_at: None,
            author_id: None,
            conversation_id: None,
            article: None,
            entities: Some(XEntities {
                urls: Some(vec![XUrlEntity {
                    url: Some("https://t.co/abc".to_string()),
                    expanded_url: Some("https://x.com/DiarioBitcoin/status/123".to_string()),
                    unwound_url: None,
                }]),
            }),
        };

        assert_eq!(x_linked_article_url(&tweet), None);
    }

    #[test]
    fn test_x_post_is_link_only() {
        let tweet = XTweet {
            id: "1".to_string(),
            text: "https://t.co/abc".to_string(),
            created_at: None,
            author_id: None,
            conversation_id: None,
            article: None,
            entities: Some(XEntities {
                urls: Some(vec![XUrlEntity {
                    url: Some("https://t.co/abc".to_string()),
                    expanded_url: Some("https://www.diariobitcoin.com/test-article".to_string()),
                    unwound_url: None,
                }]),
            }),
        };

        assert!(x_post_is_link_only(&tweet));
    }

    #[test]
    fn test_x_post_is_not_link_only_when_text_remains() {
        let tweet = XTweet {
            id: "1".to_string(),
            text: "Analisis completo https://t.co/abc".to_string(),
            created_at: None,
            author_id: None,
            conversation_id: None,
            article: None,
            entities: Some(XEntities {
                urls: Some(vec![XUrlEntity {
                    url: Some("https://t.co/abc".to_string()),
                    expanded_url: Some("https://www.diariobitcoin.com/test-article".to_string()),
                    unwound_url: None,
                }]),
            }),
        };

        assert!(!x_post_is_link_only(&tweet));
    }

    #[test]
    fn test_x_article_plain_text_prefers_plain_text() {
        let article = XArticleMeta {
            title: Some("Bitcoin bajo presión".to_string()),
            plain_text: Some("  Cuerpo completo del articulo  ".to_string()),
            preview_text: Some("Preview".to_string()),
        };

        assert_eq!(
            x_article_plain_text(&article),
            Some("Cuerpo completo del articulo".to_string())
        );
    }

    #[test]
    fn test_x_article_plain_text_falls_back_to_preview_text() {
        let article = XArticleMeta {
            title: Some("Bitcoin bajo presión".to_string()),
            plain_text: None,
            preview_text: Some("  Preview del articulo  ".to_string()),
        };

        assert_eq!(
            x_article_plain_text(&article),
            Some("Preview del articulo".to_string())
        );
    }

    #[test]
    fn test_is_x_article_url() {
        assert!(is_x_article_url(
            "https://x.com/i/article/2034262647731101696"
        ));
        assert!(!is_x_article_url(
            "https://x.com/DiarioBitcoin/status/2034263054754726116"
        ));
    }

    #[test]
    fn test_x_article_body_unavailable_detects_guest_page() {
        let body = "<html><body><h1>This page is not supported.</h1><p>Please visit the author's profile on the latest version of X to view this content.</p></body></html>";
        assert!(x_article_body_unavailable(body));
    }

    #[test]
    fn test_parse_scraped_post_from_html_blocks_guest_x_article_page() {
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

    #[test]
    fn test_parse_x_web_article_post_prefers_graphql_article_payload() {
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
            Some("@DiarioBitcoin (Diario฿itcoin)".to_string()),
        )
        .unwrap();

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
            Some("@DiarioBitcoin (Diario฿itcoin)".to_string())
        );
    }

    #[test]
    fn test_normalized_output_language_defaults_to_english() {
        assert_eq!(normalized_output_language(""), "english");
        assert_eq!(normalized_output_language("   "), "english");
        assert_eq!(normalized_output_language("spanish"), "spanish");
    }

    #[test]
    fn test_markdown_prompts_require_near_lossless_preservation() {
        let system_prompt = markdown_system_prompt("english");
        let user_prompt = markdown_user_prompt("english", r#"{"content":"<p>Hello</p>"}"#);

        assert!(
            system_prompt.contains("preserving the source text and structure as fully as possible")
        );
        assert!(system_prompt
            .contains("Do not summarize, paraphrase, compress, or omit substantive details"));
        assert!(user_prompt.contains("Treat `content` as the canonical article body"));
        assert!(user_prompt.contains("keep it nearly verbatim"));
    }
}
