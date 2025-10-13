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

    /// Output the result as JSON instead of human-readable text
    #[arg(short = 'j', long = "json", default_value_t = false)]
    json: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Scrape the URL and convert its content to Markdown in the requested language.
    let post = universal_scrape(&args.url, &args.language, None).await;
    if !post.error.is_empty() {
        eprintln!("Error during scraping: {}", post.error);
        return;
    }

    // TODO: make the LLM Client an option [--client <openai|grok|gemini|claude>]
    if args.json {
        // Serialize the Post object to JSON and print it.
        match serde_json::to_string_pretty(&post) {
            Ok(json) => println!("{}", json),
            Err(err) => eprintln!("Error serializing to JSON: {}", err),
        }
    } else {
        // Print the title and Markdown-formatted (and translated) content for human consumption.
        println!("{}\n\n{}", post.title, post.content);
    }
}
