//! HTML content extraction and cleaning.
//!
//! This module implements the content-extraction pipeline: it locates the
//! main article body inside a parsed HTML document, strips unwanted elements
//! (scripts, ads, navigation, …), and pulls metadata (`<title>`, Open Graph
//! tags) out of the page.

use std::collections::HashSet;

use scraper::{ElementRef, Html, Selector};

use crate::x::{is_x_article_url, x_article_body_unavailable};
use crate::Post;

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
pub(crate) fn parse_scraped_post_from_html(
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
