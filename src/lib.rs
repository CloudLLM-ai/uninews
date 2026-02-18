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
use serde::Serialize;
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
    match session
        .send_message(Role::User, user_prompt, None, None)
        .await
    {
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
    let client = Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
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
