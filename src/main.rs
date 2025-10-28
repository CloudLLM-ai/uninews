//! # Uninews CLI - Command-Line News Scraper
//!
//! A command-line utility for scraping news articles and converting them to Markdown format.
//!
//! ## Usage Examples
//!
//! ### Basic Usage (English output)
//! ```bash
//! uninews "https://www.example.com/article"
//! ```
//!
//! ### Translate to Spanish
//! ```bash
//! uninews "https://www.bbc.com/news/article" --language spanish
//! ```
//!
//! ### Output as JSON
//! ```bash
//! uninews "https://www.example.com/article" --json
//! ```
//!
//! ### Combine language and JSON output
//! ```bash
//! uninews "https://www.example.com/article" -l french -j
//! ```
//!
//! ## Features
//!
//! - 🔗 Scrape any news article from its URL
//! - 📝 Automatic conversion to clean Markdown format
//! - 🌍 Support for 100+ languages via AI translation
//! - 📊 JSON output for programmatic use
//! - 🚀 Powered by OpenAI's GPT models
//! - 🛡️ Graceful error handling with user-friendly messages
//!
//! ## Setup
//!
//! 1. Set your OpenAI API key:
//! ```bash
//! export OPEN_AI_SECRET="sk-..."
//! ```
//!
//! 2. Run:
//! ```bash
//! cargo run -- "https://example.com/article"
//! ```
//!
//! ## Output Examples
//!
//! ### Human-Readable Format (Default)
//! ```text
//! Article Title Here
//!
//! # Main Heading
//!
//! This is the clean, formatted article content in Markdown.
//!
//! ## Subheading
//!
//! More content here...
//! ```
//!
//! ### JSON Format
//! ```json
//! {
//!   "title": "Article Title",
//!   "content": "# Main Heading\n\nMarkdown content...",
//!   "featured_image_url": "https://example.com/image.jpg",
//!   "publication_date": "2024-01-15T10:30:00Z",
//!   "author": "Jane Doe",
//!   "error": ""
//! }
//! ```

use clap::Parser;
use uninews::universal_scrape;

/// Command line arguments for the Uninews scraper.
///
/// This struct defines all available CLI options for scraping news articles.
/// It uses the `clap` crate for automatic argument parsing and validation.
#[derive(Parser)]
#[command(
    author = "CloudLLM Contributors",
    version,
    about = "A universal news scraper that converts articles to Markdown",
    long_about = "Uninews is a powerful CLI tool for scraping news articles from any website \
                  and automatically converting them to beautifully formatted Markdown. \
                  It supports translation to 100+ languages using AI-powered processing. \
                  Requires OPEN_AI_SECRET environment variable to be set."
)]
struct Args {
    /// The URL of the news article to scrape
    ///
    /// Must be a complete, valid HTTP(S) URL.
    /// Examples:
    /// - https://www.bbc.com/news/world
    /// - https://news.ycombinator.com/item?id=123
    /// - https://medium.com/publication/article-title
    url: String,

    /// Target language for output (default: english)
    ///
    /// Specifies which language to translate the article to.
    /// The AI will attempt to output in the requested language.
    /// If the language is not recognized, defaults to English.
    ///
    /// Supported languages include (but not limited to):
    /// - english, spanish, french, german, italian
    /// - chinese, japanese, korean
    /// - portuguese, russian, arabic, hebrew
    /// - dutch, swedish, greek, turkish
    /// - And 80+ more languages
    ///
    /// Example: `--language spanish` or `-l français`
    #[arg(short, long, default_value = "english")]
    language: String,

    /// Output the result as JSON instead of formatted text
    ///
    /// When enabled, the scraped article is output as a pretty-printed JSON object.
    /// This is useful for programmatic processing or integration with other tools.
    ///
    /// The JSON includes all extracted metadata:
    /// - title: Article title
    /// - content: Markdown-formatted content
    /// - featured_image_url: URL to the main image
    /// - publication_date: ISO 8601 publication date
    /// - author: Article author
    /// - error: Error message (empty if successful)
    ///
    /// Example: `--json` or `-j`
    #[arg(short = 'j', long = "json", default_value_t = false)]
    json: bool,
}

/// Main entry point for the Uninews CLI application.
///
/// This async function:
/// 1. Parses command-line arguments
/// 2. Calls the library's `universal_scrape` function
/// 3. Handles any errors gracefully
/// 4. Formats and outputs the results based on user preferences
///
/// # Error Handling
///
/// Errors are printed to stderr and the program exits cleanly.
/// No panics or crashes occur even if scraping fails.
///
/// # Examples
///
/// Run with basic arguments:
/// ```bash
/// # Scrape an article in English (default)
/// uninews "https://example.com/article"
///
/// # Scrape and translate to Spanish
/// uninews "https://example.com/article" --language spanish
///
/// # Get JSON output
/// uninews "https://example.com/article" --json
/// ```
#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Scrape the URL and convert its content to Markdown in the requested language.
    let post = universal_scrape(&args.url, &args.language, None).await;

    // Check for errors during scraping
    if !post.error.is_empty() {
        eprintln!("❌ Error during scraping: {}", post.error);
        return;
    }

    // TODO: make the LLM Client an option [--client <openai|grok|gemini|claude>]

    if args.json {
        // Serialize the Post object to JSON and print it.
        match serde_json::to_string_pretty(&post) {
            Ok(json) => println!("{}", json),
            Err(err) => eprintln!("❌ Error serializing to JSON: {}", err),
        }
    } else {
        // Print the title and Markdown-formatted (and translated) content for human consumption.
        println!("{}\n\n{}", post.title, post.content);
    }
}
