use clap::Parser;
use uninews::universal_scrape;

/// Command line arguments for uninews.
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// The URL of the news article to scrape.
    url: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Scrape the URL and convert its content to Markdown.
    let post = universal_scrape(&args.url).await;
    if !post.error.is_empty() {
        eprintln!("Error during scraping: {}", post.error);
        return;
    }

    // Print the title and Markdown-formatted content.
    println!("{}\n\n{}", post.title, post.content);
}
