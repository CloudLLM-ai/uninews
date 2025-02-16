use clap::Parser;
use uninews::universal_scrape;

/// Command line arguments for uninews.
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// The URL of the news article to scrape.
    url: String,

    /// Optional output language (default: english)
    #[arg(short, long, default_value = "english")]
    language: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Scrape the URL and convert its content to Markdown in the requested language.
    let post = universal_scrape(&args.url, &args.language).await;
    if !post.error.is_empty() {
        eprintln!("Error during scraping: {}", post.error);
        return;
    }

    // Print the title and Markdown-formatted (and translated) content.
    println!("{}\n\n{}", post.title, post.content);
}
