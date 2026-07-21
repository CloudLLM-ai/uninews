//! HTML content extraction and cleaning.
//!
//! This module implements the content-extraction pipeline: it locates the
//! main article body inside a parsed HTML document, strips unwanted elements
//! (scripts, ads, navigation, …), and pulls metadata (`<title>`, Open Graph
//! tags) out of the page.

use std::sync::OnceLock;

use scraper::{ElementRef, Html, Selector};

use crate::x::{is_x_article_url, x_article_body_unavailable};
use crate::Post;

/// Tag names that are stripped from the extracted content entirely
/// (scripts, ads, navigation, form controls, media wrappers).
///
/// A plain slice is used instead of a `HashSet`: at 14 entries a linear scan
/// is faster than hashing and costs zero allocations.
const SKIP_TAGS: &[&str] = &[
    "script", "style", "noscript", "iframe", "header", "footer", "nav", "aside", "form", "input",
    "button", "svg", "picture", "source",
];

/// Parse a hard-coded CSS selector exactly once and cache it process-wide.
///
/// All selectors used by this module are compile-time constants, so parsing
/// cannot fail; the `expect` documents that invariant rather than handling a
/// runtime error.
fn cached_selector(slot: &'static OnceLock<Selector>, css: &str) -> &'static Selector {
    slot.get_or_init(|| Selector::parse(css).expect("hard-coded CSS selector must be valid"))
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
/// - `skip_tags`: Tag names to completely remove
///
/// # Returns
///
/// Cleaned HTML as a string, or empty string if no content remains
#[must_use]
fn clean_element(element: ElementRef, skip_tags: &[&str]) -> String {
    let tag_name = element.value().name();
    if skip_tags.contains(&tag_name) {
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
/// 1. **Priority**: Clean every `<article>` element and keep the longest
///    result — news pages frequently contain several `<article>` elements
///    (the main story plus teaser/related-story cards), and the main story
///    is almost always the largest.
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
/// - `skip_tags`: Tag names to remove
///
/// # Returns
///
/// Cleaned HTML content string, or empty string if document is malformed
#[must_use]
fn extract_clean_content(document: &Html, skip_tags: &[&str]) -> String {
    static ARTICLE_SELECTOR: OnceLock<Selector> = OnceLock::new();
    static BODY_SELECTOR: OnceLock<Selector> = OnceLock::new();

    // Clean every <article> and keep the longest: pages often contain
    // several (main story + teaser cards), and the main story is the
    // largest. Picking the first match would sometimes return a teaser.
    let best_article = document
        .select(cached_selector(&ARTICLE_SELECTOR, "article"))
        .map(|article| clean_element(article, skip_tags))
        .filter(|cleaned| !cleaned.trim().is_empty())
        .max_by_key(|cleaned| cleaned.len());
    if let Some(content) = best_article {
        return content;
    }

    // Fallback: use the <body>
    if let Some(body) = document
        .select(cached_selector(&BODY_SELECTOR, "body"))
        .next()
    {
        return clean_element(body, skip_tags);
    }
    String::new()
}

/// Parse a raw HTML body into a [`Post`], extracting the title, cleaned
/// content, featured image, publication date, and author.
///
/// `title_override` wins over the `<title>` tag when provided (used by the
/// X pipeline, where the tweet's article title is more accurate than the
/// guest-page `<title>`).
///
/// X article guest pages that withhold the article body are detected up
/// front and reported as an error instead of returning the "this page is
/// not supported" boilerplate as content.
pub fn parse_scraped_post_from_html(
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

    static TITLE_SELECTOR: OnceLock<Selector> = OnceLock::new();
    static OG_IMAGE_SELECTOR: OnceLock<Selector> = OnceLock::new();
    static PUBLISHED_TIME_SELECTOR: OnceLock<Selector> = OnceLock::new();
    static AUTHOR_SELECTOR: OnceLock<Selector> = OnceLock::new();

    let extracted_title = document
        .select(cached_selector(&TITLE_SELECTOR, "title"))
        .next()
        .map(|elem| elem.text().collect::<Vec<_>>().join(" ").trim().to_string())
        .unwrap_or_default();
    let title = title_override
        .filter(|title| !title.trim().is_empty())
        .map(|title| title.trim().to_string())
        .unwrap_or(extracted_title);

    let content = extract_clean_content(&document, SKIP_TAGS);

    let featured_image_url = document
        .select(cached_selector(
            &OG_IMAGE_SELECTOR,
            r#"meta[property="og:image"]"#,
        ))
        .next()
        .and_then(|meta| meta.value().attr("content"))
        .unwrap_or("")
        .to_string();

    let publication_date = document
        .select(cached_selector(
            &PUBLISHED_TIME_SELECTOR,
            r#"meta[property="article:published_time"]"#,
        ))
        .next()
        .and_then(|meta| meta.value().attr("content"))
        .map(String::from);

    let author = document
        .select(cached_selector(&AUTHOR_SELECTOR, r#"meta[name="author"]"#))
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
