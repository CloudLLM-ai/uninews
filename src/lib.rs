//! # Uninews - Universal News Scraper
//!
//! A powerful Rust library for scraping news articles from various websites and converting them to Markdown format using AI.
//!
//! ## Features
//!
//! - **Intelligent HTML Parsing**: Extracts article content from complex HTML structures
//! - **Smart Content Cleaning**: Automatically removes ads, scripts, navigation, and other noise
//! - **AI-Powered Formatting**: Converts raw HTML to beautifully formatted Markdown using OpenAI's GPT models
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
//! 5. Converts remaining HTML to Markdown using AI
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
use std::sync::Arc;
// CloudLLM imports.
use cloudllm::client_wrapper::Role;
use cloudllm::clients::openai::{Model, OpenAIClient};
use cloudllm::LLMSession;

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

/// Fetches a tweet or X thread via the Twitter/X API v2 and returns a [`Post`].
///
/// # Authentication
///
/// Requires the `X_BEARER_TOKEN` environment variable to be set with an
/// OAuth 2.0 app-only Bearer Token from the [X Developer Portal](https://developer.twitter.com/).
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
    // ── 1. Resolve the Bearer Token ──────────────────────────────────────────
    let bearer_token = match env::var("X_BEARER_TOKEN") {
        Ok(t) if !t.trim().is_empty() => t,
        _ => {
            return Post {
                title: String::new(),
                content: String::new(),
                featured_image_url: String::new(),
                publication_date: None,
                author: None,
                error:
                    "Please set the X_BEARER_TOKEN environment variable to access X.com content."
                        .into(),
            };
        }
    };

    // ── 2. Extract the tweet ID from the URL ─────────────────────────────────
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

    // ── 3. Build the HTTP client ──────────────────────────────────────────────
    let client = Client::builder()
        .user_agent("Mozilla/5.0 (compatible; uninews/1.0)")
        .build()
        .unwrap_or_default();

    let auth_header = format!("Bearer {}", bearer_token);

    // ── 4. Fetch the root tweet ───────────────────────────────────────────────
    let root_tweet_url = format!(
        "https://api.twitter.com/2/tweets/{}?tweet.fields=created_at,author_id,conversation_id,text&expansions=author_id&user.fields=name,username,profile_image_url",
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

    let root_data: XTweetResponse = match root_resp.json().await {
        Ok(d) => d,
        Err(e) => {
            return Post {
                title: String::new(),
                content: String::new(),
                featured_image_url: String::new(),
                publication_date: None,
                author: None,
                error: format!("Failed to parse X API response: {}", e),
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
                error: "X API returned no tweet data.".into(),
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
        "https://api.twitter.com/2/tweets/search/recent?query=conversation_id%3A{}&tweet.fields=created_at,author_id,text&max_results=100",
        conversation_id
    );
    if let Ok(search_resp) = client
        .get(&search_url)
        .header("Authorization", &auth_header)
        .send()
        .await
    {
        if let Ok(search_data) = search_resp.json::<XSearchResponse>().await {
            if let Some(tweets) = search_data.data {
                for t in tweets {
                    // Only include tweets from the same author (i.e. the thread,
                    // not replies from other users). Guard against an empty
                    // author_id (which would match any tweet lacking the field).
                    let same_author =
                        !author_id.is_empty() && t.author_id.as_deref() == Some(author_id.as_str());
                    if same_author && t.id != root_tweet.id {
                        thread_tweets.push((t.created_at.unwrap_or_default(), t.text));
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
/// 2. Initializes OpenAI client (uses GPT-5-Nano by default)
/// 3. Creates an LLMSession with a system prompt instructing Markdown formatting
/// 4. Sends the scraped Post as JSON to the LLM
/// 5. Updates the Post's `content` field with formatted Markdown
/// 6. Optionally translates to the requested language
///
/// # Arguments
///
/// - `post`: The scraped Post with raw HTML content
/// - `language`: Target language for output (e.g., "spanish", "french", "japanese")
/// - `openai_model`: Optional specific GPT model to use (defaults to GPT-5-Nano)
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

    // Instantiate the OpenAI client. gpt-4o is default, fastest, and cheapest.
    let model = openai_model.unwrap_or(Model::GPT4o);
    let client = Arc::new(OpenAIClient::new_with_model_enum(&secret_key, model));

    // Normalize language: if empty, default to "english".
    let lang = if language.trim().is_empty() {
        "english"
    } else {
        language
    };

    // Define a system prompt that instructs the LLM on its role.
    let system_prompt = format!(
        "You are an expert markdown formatter and translator. Given a JSON object representing a news post, \
         extract and output only the text content in Markdown format in {}. Remove all HTML tags and extra markup. \
         Do not include any JSON keys or metadata—only the formatted content. If {} is not supported, default to english.",
        lang, lang
    );

    // Create a new LLMSession.
    let mut session = LLMSession::new(client, system_prompt, 128000);

    // Serialize the entire Post to JSON.
    let post_json = serde_json::to_string(&post)
        .map_err(|e| format!("Failed to serialize Post to JSON: {}", e))?;
    let user_prompt = format!(
        "Convert the following Post JSON into Markdown formatted text in {} language, nothing else:\n\n{}",
        lang, post_json
    );

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
/// - `openai_model`: Optional OpenAI model to use; defaults to GPT-5-Nano
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
///         Some(Model::GPT5Nano) // Explicitly specify model
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

    let client = Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
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

    let document = Html::parse_document(&body_text);

    let skip_tags: HashSet<&str> = [
        "script", "style", "noscript", "iframe", "header", "footer", "nav", "aside", "form",
        "input", "button", "svg", "picture", "source",
    ]
    .iter()
    .cloned()
    .collect();

    // Extract title
    let title_selector = Selector::parse("title").unwrap();
    let title = document
        .select(&title_selector)
        .next()
        .map(|elem| elem.text().collect::<Vec<_>>().join(" ").trim().to_string())
        .unwrap_or_default();

    // Extract content
    let content = extract_clean_content(&document, &skip_tags);

    // Extract featured image
    let meta_selector = Selector::parse(r#"meta[property="og:image"]"#).unwrap();
    let featured_image_url = document
        .select(&meta_selector)
        .next()
        .and_then(|meta| meta.value().attr("content"))
        .unwrap_or("")
        .to_string();

    // Extract publication date
    let date_selector = Selector::parse(r#"meta[property="article:published_time"]"#).unwrap();
    let publication_date = document
        .select(&date_selector)
        .next()
        .and_then(|meta| meta.value().attr("content"))
        .map(String::from);

    // Extract author
    let author_selector = Selector::parse(r#"meta[name="author"]"#).unwrap();
    let author = document
        .select(&author_selector)
        .next()
        .and_then(|meta| meta.value().attr("content"))
        .map(String::from);

    if content.trim().is_empty() {
        return Post {
            title: "".into(),
            content: "".into(),
            featured_image_url: "".into(),
            publication_date: None,
            author: None,
            error: "Could not extract meaningful content from the page.".into(),
        };
    }

    let scraped_post = Post {
        title,
        content,
        featured_image_url,
        publication_date,
        author,
        error: "".into(),
    };

    match convert_content_to_markdown(scraped_post.clone(), language, openai_model).await {
        Ok(markdown_post) => markdown_post,
        Err(err) => Post {
            error: err,
            ..scraped_post
        },
    }
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
}
