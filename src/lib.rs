use reqwest::Client;
use scraper::{ElementRef, Html, Selector};
use serde::Serialize;
use std::collections::HashSet;
use std::env;
use std::sync::Arc;
// CloudLLM imports.
use cloudllm::client_wrapper::Role;
use cloudllm::clients::openai::{Model, OpenAIClient};
use cloudllm::LLMSession;

/// Represents a news post.
#[derive(Debug, Serialize, Clone)]
pub struct Post {
    pub title: String,
    pub content: String,
    pub featured_image_url: String,
    pub publication_date: Option<String>, // Optional, as not all pages may provide it
    pub author: Option<String>,           // Optional for the same reason
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

/// Uses CloudLLM to convert the Post's content JSON into Markdown formatted text in a given language.
///
/// The function sends the entire Post (as JSON) to the LLM, and updates its `content` field
/// with the returned Markdown output. The LLM is instructed to output the text in the specified language,
/// and if the language is not supported, to default to English.
pub async fn convert_content_to_markdown(mut post: Post, language: &str, openai_model: Option<Model>,) -> Result<Post, String> {
    // Get the secret key from the environment.
    let secret_key = env::var("OPEN_AI_SECRET")
        .map_err(|_| "Please set the OPEN_AI_SECRET environment variable.".to_string())?;

    // Instantiate the OpenAI client. gpt-4.1-mini is the default model.
    let model = openai_model.unwrap_or(Model::GPT41Mini);
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
/// Finally, it uses CloudLLM to convert the scraped content into Markdown in the specified language,
/// so that the returned Post already has its `content` field formatted in Markdown.
pub async fn universal_scrape(url: &str, language: &str, openai_model: Option<Model>) -> Post {
    let client = Client::new();
    let response = client.get(url).send().await;

    if let Err(err) = response {
        return Post {
            title: "".into(),
            content: "".into(),
            featured_image_url: "".into(),
            publication_date: None,
            author: None,
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
