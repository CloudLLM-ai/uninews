//! # Uninews - Universal News Scraper
//!
//! A powerful Rust library for scraping news articles from various websites and converting them to Markdown format using AI.
//!
//! ## Features
//!
//! - **Intelligent HTML Parsing**: Extracts article content from complex HTML structures
//! - **Smart Content Cleaning**: Automatically removes ads, scripts, navigation, and other noise
//! - **AI-Powered Formatting**: Converts raw HTML to near-lossless Markdown using pluggable LLM providers
//! - **Metadata Extraction**: Captures title, author, publication date, and featured images
//! - **Multilingual Support**: Translates content to any language during processing
//! - **Progress Events**: Optional single-listener event stream ([`events`]) for
//!   live scraping feedback in agents, harnesses, and UIs
//! - **archive.org Fallback**: Automatically retries bot-protected or unreachable
//!   pages via the latest Wayback Machine snapshot ([`archive`])
//! - **Async/Await**: Built with Tokio for efficient async operations
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use uninews::universal_scrape;
//!
//! #[tokio::main]
//! async fn main() {
//!     // Make sure the env var for your chosen UNINEWS_LLM_CLIENT is set
//!     // (OPEN_AI_SECRET by default, OPENROUTER_API_KEY for openrouter, etc.)
//!     // Pass None to use the default 256K context window, or Some(n) to pin
//!     // the budget for this call. Override globally via UNINEWS_LLM_CONTEXT_WINDOW.
//!     let post = universal_scrape(
//!         "https://example.com/article",
//!         "english",
//!         None,
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
//! 3. Removes unwanted elements (scripts, styles, ads, navigation, etc.)
//! 4. Cleans empty nodes and whitespace
//! 5. Converts remaining HTML to Markdown using AI while preserving article wording and structure
//! 6. Optionally translates to the requested language
//!
//! ## Module Map
//!
//! - `llm` — LLM provider selection, context-window budgeting, and the
//!   HTML → Markdown conversion (re-exported at the crate root).
//! - `web` — plain-HTTP scraping pipeline for non-X URLs.
//! - `x` — X.com / Twitter tweets, threads, and articles.
//! - `html` — HTML cleaning and metadata extraction.
//! - `browser` — headless-Chrome rendering fallback.
//! - [`archive`] — archive.org Wayback Machine fallback for protected or
//!   unreachable pages.
//! - [`events`] — typed progress events with a single-listener emitter.
//! - `http` — shared, timeout-hardened `reqwest` clients.
//! - `util` — small shared helpers.
//!
//! ## Security Notes
//!
//! - **SSRF**: [`universal_scrape`] fetches arbitrary caller-supplied URLs.
//!   If you expose it behind a service, validate/allow-list URLs yourself —
//!   uninews intentionally does not restrict schemes or hosts.
//! - **Secrets**: API keys are only read from environment variables and are
//!   never written to logs, stderr, or [`Post`] fields.
//! - **Trusted env vars**: `UNINEWS_CHROME_BINARY` names an executable that
//!   gets spawned; only set it to a binary you trust. Target URLs are passed
//!   to Chrome as plain process arguments (no shell), so they cannot inject
//!   commands.
//! - **Availability**: all HTTP requests run with connect/read timeouts (see
//!   the `http` module) so a hung server cannot block a scrape forever.
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

pub mod archive;
mod browser;
pub mod events;
#[doc(hidden)]
pub mod html;
mod http;
pub mod llm;
mod util;
mod web;
#[doc(hidden)]
pub mod x;

use serde::Serialize;

pub use archive::{archive_fallback_enabled, ArchiveSnapshot, UNINEWS_ARCHIVE_FALLBACK_ENV};
pub use events::{set_event_listener, ScrapeEvent, ScrapeEventListener};
pub use llm::{
    active_llm_client, active_provider_label, convert_content_to_markdown, llm_context_window,
    resolve_llm_context_window, uninews_llm_context_window, LLMClientInfo,
    DEFAULT_LLM_CONTEXT_WINDOW, UNINEWS_LLM_CONTEXT_WINDOW_ENV,
};

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
/// 6. **Content Cleaning**: Removes unwanted elements (scripts, ads, navigation, etc.)
/// 7. **Markdown Conversion**: Uses the configured LLM to convert HTML to formatted Markdown
/// 8. **Translation**: Optionally translates to requested language
///
/// # Arguments
///
/// - `url`: The URL of the article to scrape (must be a complete, valid URL)
/// - `language`: Target language for output ("english", "spanish", "french", etc.)
/// - `context_window_tokens`: Optional LLM context window (in tokens) passed
///   through to [`convert_content_to_markdown`]. When `None`, Uninews reads
///   `UNINEWS_LLM_CONTEXT_WINDOW`; if that env var is also unset or unparseable,
///   it falls back to [`DEFAULT_LLM_CONTEXT_WINDOW`] (256,000 tokens).
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
/// See [`convert_content_to_markdown`] for the full list of supported
/// `UNINEWS_LLM_CLIENT` / `UNINEWS_LLM_MODEL` /
/// `UNINEWS_LLM_CONTEXT_WINDOW` values and the provider-specific API key
/// env vars.
///
/// # Performance Considerations
///
/// - Network requests are the primary bottleneck
/// - LLM processing typically takes 2-5 seconds per article
/// - HTML parsing is fast (< 100ms for most pages)
/// - Content cleaning is O(n) where n = DOM tree size
///
/// # Examples
///
/// ## Basic Usage
///
/// ```rust,no_run
/// # use uninews::universal_scrape;
/// #[tokio::main]
/// async fn main() {
///     // Scrape with default English output and the default 256K context window.
///     let post = universal_scrape(
///         "https://www.example.com/news/article",
///         "english",
///         None,
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
///         None,
///     ).await;
///
///     if post.error.is_empty() {
///         // Content is now in Spanish
///         println!("Artículo: {}", post.title);
///     }
/// }
/// ```
///
/// ## Provider Selection
///
/// Set `UNINEWS_LLM_CLIENT` and `UNINEWS_LLM_MODEL` before calling to route the
/// Markdown conversion through a different provider (e.g. OpenRouter).
///
/// ```text
/// # Set these in your shell:
/// export UNINEWS_LLM_CLIENT=openrouter
/// export UNINEWS_LLM_MODEL=qwen/qwen3.7-max
/// export OPENROUTER_API_KEY=sk-or-...
/// ```
///
/// ## Pinning the Context Window
///
/// Pass an explicit `Some(n)` to override `UNINEWS_LLM_CONTEXT_WINDOW` and
/// the [`DEFAULT_LLM_CONTEXT_WINDOW`] fallback for a single call. Useful when
/// the underlying model advertises a larger window than the env var
/// suggests (e.g. switching to Gemini-class 1M+ context for very long
/// articles).
///
/// ```rust,no_run
/// # use uninews::universal_scrape;
/// #[tokio::main]
/// async fn main() {
///     let post = universal_scrape(
///         "https://example.com/very-long-article",
///         "english",
///         Some(2_000_000), // 2M tokens
///     ).await;
///     if post.error.is_empty() {
///         println!("{}", post.content);
///     }
/// }
/// ```
///
/// ## Real-World Example: Building an RSS Reader
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
pub async fn universal_scrape(
    url: &str,
    language: &str,
    context_window_tokens: Option<usize>,
) -> Post {
    events::emit_event(ScrapeEvent::ScrapeStarted {
        url: url.to_string(),
    });

    // Delegate to the X.com handler for X / Twitter URLs.
    let post = if x::is_x_url(url) {
        x::scrape_x_url(url, language, context_window_tokens).await
    } else {
        web::scrape_web_url(url, language, context_window_tokens).await
    };

    if post.error.is_empty() {
        events::emit_event(ScrapeEvent::ScrapeCompleted {
            url: url.to_string(),
        });
    } else {
        events::emit_event(ScrapeEvent::ScrapeFailed {
            url: url.to_string(),
            error: post.error.clone(),
        });
    }

    post
}
