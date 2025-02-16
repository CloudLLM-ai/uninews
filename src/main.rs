use std::env;

use clap::Parser;

use uninews::{universal_scrape, Post};

// CloudLLM imports.
use cloudllm::client_wrapper::Role;
use cloudllm::clients::openai::OpenAIClient;
use cloudllm::LLMSession;

/// Command line arguments for uninews.
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// The URL of the news article to scrape.
    url: String,
}

/// Uses CloudLLM to convert a Post (in JSON) to Markdown formatted text.
///
/// The prompt instructs the LLM to extract only the text content of the post,
/// removing all HTML and metadata, and output it using Markdown formatting.
async fn convert_post_to_markdown(post: &Post) -> Result<String, String> {
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

    // Create the user prompt including the Post JSON.
    let post_json = serde_json::to_string(post)
        .map_err(|e| format!("Failed to serialize Post to JSON: {}", e))?;
    let user_prompt = format!(
        "Convert the following Post JSON into Markdown formatted text, nothing else:\n\n{}",
        post_json
    );

    // Send the prompt to the LLM.
    match session.send_message(Role::User, user_prompt).await {
        Ok(response) => Ok(response.content),
        Err(err) => Err(format!("LLM Error: {}", err)),
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // First, scrape the URL.
    let post = universal_scrape(&args.url).await;
    if !post.error.is_empty() {
        eprintln!("Error during scraping: {}", post.error);
        return;
    }

    // Optionally, output the raw JSON (for debugging).
    // println!(
    //     "Scraped Post (JSON):\n{}\n",
    //     to_string_pretty(&post).unwrap()
    // );

    // Convert the Post JSON to Markdown using CloudLLM.
    match convert_post_to_markdown(&post).await {
        Ok(markdown) => {
            println!("{}\n\n{}", post.title, markdown);
        }
        Err(err) => {
            eprintln!("Error converting to Markdown: {}", err);
        }
    }
}
