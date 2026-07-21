//! Scrape a URL while printing live progress events to stderr.
//!
//! Demonstrates the single-listener [`ScrapeEvent`] stream: register one
//! closure with [`set_event_listener`] and every pipeline step — fetch,
//! extraction, bot-protection detection, archive.org fallback, LLM
//! conversion — is reported as it happens.
//!
//! Usage:
//!
//! ```bash
//! cargo run --example scrape_with_events -- "https://example.com/article"
//! ```
//!
//! Requires the API key env var for the configured LLM provider
//! (`OPEN_AI_SECRET` by default).

use std::sync::Arc;

use uninews::{set_event_listener, universal_scrape, ScrapeEvent};

#[tokio::main]
async fn main() {
    let url = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: scrape_with_events <url> [language]");
        std::process::exit(2);
    });
    let language = std::env::args().nth(2).unwrap_or_else(|| "english".into());

    // Register the (single) process-wide listener. Events are printed to
    // stderr so stdout stays reserved for the scraped article.
    set_event_listener(Some(Arc::new(|event: &ScrapeEvent| {
        eprintln!("[event] {:?}", event);
    })));

    let post = universal_scrape(&url, &language, None).await;

    if post.error.is_empty() {
        println!("{}\n\n{}", post.title, post.content);
    } else {
        eprintln!("scrape failed: {}", post.error);
        std::process::exit(1);
    }
}
