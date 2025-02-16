use reqwest::Client;
use scraper::{ElementRef, Html, Selector};
use serde::Serialize;
use std::collections::HashSet;
use std::env;

// CloudLLM imports.
use cloudllm::client_wrapper::Role;
use cloudllm::clients::openai::OpenAIClient;
use cloudllm::LLMSession;

/// Represents a news post.
#[derive(Debug, Serialize, Clone)]
pub struct Post {
    pub title: String,
    pub content: String,
    pub featured_image_url: String,
    pub error: String,
}

/// Recursively cleans an element by skipping unwanted tags and empty content.
///
/// For each element:
/// - If its tag is in `skip_tags`, it is omitted entirely.
/// - Child nodes are processed recursively. Only non‑empty children (or non‑whitespace text)
///   are kept.
/// - If an element yields no content after cleaning, it returns an empty string.
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

/// Tries to extract a clean content string from the document.
///
/// First, it attempts to select an `<article>` element (often the container for main content).
/// If found, it cleans that node; otherwise, it falls back to cleaning the `<body>` element.
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

/// Uses CloudLLM to convert the Post's content JSON into Markdown formatted text.
///
/// The function sends the entire Post (as JSON) to the LLM, and updates its `content` field
/// with the returned Markdown output.
pub async fn convert_content_to_markdown(mut post: Post) -> Result<Post, String> {
    // Get the secret key from the environment.
    let secret_key = env::var("OPEN_AI_SECRET")
        .map_err(|_| "Please set the OPEN_AI_SECRET environment variable.".to_string())?;

    // Instantiate the OpenAI client.
    let client = OpenAIClient::new(&secret_key, "gpt-4o");

    // Define a system prompt that instructs the LLM on its role.
    let system_prompt = "You are an expert markdown formatter. Given a JSON object representing a news post, \
                         extract and output only the text content in Markdown format. Remove all HTML tags and extra markup. \
                         Do not include any JSON keys or metadata—only the formatted content."
        .to_string();

    // Create a new LLMSession.
    let mut session = LLMSession::new(client, system_prompt, 128000);

    // Serialize the entire Post to JSON.
    let post_json = serde_json::to_string(&post)
        .map_err(|e| format!("Failed to serialize Post to JSON: {}", e))?;
    let user_prompt = format!(
        "Convert the following Post JSON into Markdown formatted text, nothing else:\n\n{}",
        post_json
    );

    // Send the prompt to the LLM.
    match session.send_message(Role::User, user_prompt).await {
        Ok(response) => {
            post.content = response.content;
            Ok(post)
        }
        Err(err) => Err(format!("LLM Error: {}", err)),
    }
}

/// Scrapes the provided URL and returns a `Post` struct with the extracted data.
///
/// Downloads the HTML, extracts the `<title>`, and then cleans the main content (preferring an
/// `<article>` element if available) by removing unwanted tags and empty nodes.
/// Also attempts to extract a featured image from an Open Graph meta tag.
///
/// Finally, it uses CloudLLM to convert the scraped content into Markdown,
/// so that the returned Post already has its `content` field formatted in Markdown.
pub async fn universal_scrape(url: &str) -> Post {
    let client = Client::new();
    let response = client.get(url).send().await;

    if let Err(err) = response {
        return Post {
            title: "".into(),
            content: "".into(),
            featured_image_url: "".into(),
            error: format!("Failed to fetch URL: {}", err),
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
                error: format!("Failed to read response body: {}", err),
            }
        }
    };

    // Parse the HTML document.
    let document = Html::parse_document(&body_text);

    // Build a set of tags to skip.
    // We skip script, style, navigation, headers, footers, sidebars, forms, etc.
    let skip_tags: HashSet<&str> = [
        "script", "style", "noscript", "iframe", "header", "footer", "nav", "aside", "form",
        "input", "button", "svg", "picture", "source",
    ]
    .iter()
    .cloned()
    .collect();

    // Extract the title from the <title> tag.
    let title_selector = Selector::parse("title").unwrap();
    let title = document
        .select(&title_selector)
        .next()
        .map(|elem| elem.text().collect::<Vec<_>>().join(" ").trim().to_string())
        .unwrap_or_default();

    // Extract and clean the main content.
    let content = extract_clean_content(&document, &skip_tags);

    // Attempt to extract a featured image from the og:image meta tag.
    let meta_selector = Selector::parse(r#"meta[property="og:image"]"#).unwrap();
    let featured_image_url = document
        .select(&meta_selector)
        .next()
        .and_then(|meta| meta.value().attr("content"))
        .unwrap_or("")
        .to_string();

    // If no meaningful content is found, set an error.
    if content.trim().is_empty() {
        return Post {
            title: "".into(),
            content: "".into(),
            featured_image_url: "".into(),
            error: "Could not extract meaningful content from the page.".into(),
        };
    }

    let scraped_post = Post {
        title,
        content,
        featured_image_url,
        error: "".into(),
    };

    // Convert the scraped content to Markdown via CloudLLM.
    match convert_content_to_markdown(scraped_post.clone()).await {
        Ok(markdown_post) => markdown_post,
        Err(err) => Post {
            error: err,
            ..scraped_post
        },
    }
}
