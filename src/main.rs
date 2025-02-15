use clap::Parser;
use serde_json::to_string_pretty;
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

    // Call the universal scrape function from the library.
    let post = universal_scrape(&args.url).await;

    // Output the scraped data as pretty-printed JSON.
    let json = to_string_pretty(&post).unwrap();
    println!("{}", json);
}
