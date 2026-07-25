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

/// Appends `text` to `out`, escaping `&`, `<`, and `>` so that text-node
/// content can never re-enter the cleaned output as live markup.
///
/// html5ever decodes character references at parse time, so source text like
/// `&lt;img src=x onerror=alert(1)&gt;` arrives here as the literal string
/// `<img src=x onerror=alert(1)>`. Emitting it raw would inject attacker
/// markup that bypasses the DOM skip-list, so every text node is re-escaped.
/// The `&` arm is listed first by convention: replacements introduce `&`
/// themselves, and escaping ampersands first is what prevents
/// double-escaping in single-pass escapers.
fn push_escaped_text(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

/// One unit of pending work for the iterative [`clean_element`] traversal.
///
/// Pushing explicit work items onto a heap-allocated stack keeps DOM nesting
/// depth off the call stack, so pathologically deep attacker markup cannot
/// overflow it.
enum CleanWork<'a> {
    /// Append the element's open tag, then schedule its children (in
    /// document order) followed by a matching `Exit` marker. Skip-tag
    /// elements are dropped here, children included.
    Enter(ElementRef<'a>),
    /// Append already-trimmed text-node content (escaped) plus a separator
    /// space.
    Text(&'a str),
    /// Finalize an element whose children are fully processed: trim the
    /// trailing separator, then either append the close tag or — when
    /// nothing non-whitespace survived inside — truncate the buffer back
    /// past the open tag, eliding the whole subtree.
    Exit {
        /// Tag name, reused for the close tag.
        tag: &'a str,
        /// Buffer length recorded before the open tag was appended.
        start: usize,
        /// Buffer length right after the open tag (start of child content).
        content_start: usize,
    },
}

/// Cleans an element by skipping unwanted tags and empty content.
///
/// This private function is the core of the content extraction pipeline. It removes
/// unwanted HTML elements (like scripts and ads) while preserving meaningful content.
///
/// # Algorithm
///
/// Iterative post-order traversal over an explicit work stack (see
/// [`CleanWork`]); everything is written into a single output buffer.
///
/// For each element:
/// - If its tag name is in `skip_tags`, the element and its entire subtree
///   are omitted (skipped subtrees are not traversed at all)
/// - Child nodes are processed in document order
/// - Only non-empty children (or non-whitespace text) are kept; text-node
///   content is HTML-escaped (see [`push_escaped_text`])
/// - Elements with no content after cleaning are elided entirely, open tag
///   included, by truncating the buffer back to the length recorded before
///   the open tag was appended
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
    let mut out = String::new();
    let mut stack = vec![CleanWork::Enter(element)];

    while let Some(work) = stack.pop() {
        match work {
            CleanWork::Text(text) => {
                push_escaped_text(&mut out, text);
                out.push(' ');
            }
            CleanWork::Enter(elem) => {
                let tag = elem.value().name();
                if skip_tags.contains(&tag) {
                    continue;
                }
                let start = out.len();
                out.push('<');
                out.push_str(tag);
                out.push('>');
                let content_start = out.len();
                stack.push(CleanWork::Exit {
                    tag,
                    start,
                    content_start,
                });
                // Reverse push order so children pop in document order.
                for child in elem.children().rev() {
                    if let Some(child_elem) = ElementRef::wrap(child) {
                        stack.push(CleanWork::Enter(child_elem));
                    } else if let Some(text) = child.value().as_text() {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            stack.push(CleanWork::Text(trimmed));
                        }
                    }
                }
            }
            CleanWork::Exit {
                tag,
                start,
                content_start,
            } => {
                // Drop the separator space left behind by the last child.
                let trimmed_len = out.trim_end().len();
                out.truncate(trimmed_len);
                if out.len() == content_start {
                    // Nothing non-whitespace survived inside: elide the whole
                    // subtree, open tag included (equivalent to the recursive
                    // version returning an empty string for this element).
                    out.truncate(start);
                } else {
                    out.push_str("</");
                    out.push_str(tag);
                    out.push_str("> ");
                }
            }
        }
    }

    // Strip the separator space after the root element's close tag.
    let trimmed_len = out.trim_end().len();
    out.truncate(trimmed_len);
    out
}

/// Returns `true` when `element` is nested inside another `<article>`.
///
/// Nested articles are never cleaned as standalone candidates: the outer
/// article's cleaned output already contains the inner one's content, so a
/// separate pass would duplicate work and produce a shorter, redundant
/// candidate for the longest-article selection.
fn has_article_ancestor(element: ElementRef) -> bool {
    element
        .ancestors()
        .any(|node| node.value().as_element().is_some_and(|e| e.name() == "article"))
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
/// Cleaned HTML content string, or empty string when the document contains
/// no usable `<article>` or `<body>`, or when cleaning stripped/elided all
/// content. (`Html::parse_document` is error-correcting, so malformed markup
/// does not by itself produce an empty result.)
#[must_use]
fn extract_clean_content(document: &Html, skip_tags: &[&str]) -> String {
    static ARTICLE_SELECTOR: OnceLock<Selector> = OnceLock::new();
    static BODY_SELECTOR: OnceLock<Selector> = OnceLock::new();

    // Clean every <article> and keep the longest: pages often contain
    // several (main story + teaser cards), and the main story is the
    // largest. Picking the first match would sometimes return a teaser.
    // Articles nested inside another <article> are skipped: the outer
    // article's cleaned output already contains their content.
    let best_article = document
        .select(cached_selector(&ARTICLE_SELECTOR, "article"))
        .filter(|article| !has_article_ancestor(*article))
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
        .map(|elem| {
            elem.text()
                .fold(String::new(), |mut title, part| {
                    if !title.is_empty() {
                        title.push(' ');
                    }
                    title.push_str(part);
                    title
                })
                .trim()
                .to_string()
        })
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
