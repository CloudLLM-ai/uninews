# Uninews

Uninews is a universal news scraper written in Rust. It downloads a news article from a given URL, cleans the HTML content, and leverages CloudLLM (via OpenAI) to convert the content into Markdown format. With its powerful translation capabilities, Uninews can seamlessly translate articles into multiple languages while preserving formatting, making it ideal for multilingual content processing. The final output (via API) is a JSON object containing the article's title, the Markdown-formatted content (translated if specified), and a featured image URL. When used as a command-line tool, it outputs the final Markdown with the contents of the news article or blog post in the requested language.

## Features

- **Scraping & Cleaning:** Extracts the main content of a news article by targeting the `<article>` tag (or falling back to `<body>`) and removing unwanted elements.
- **Markdown Conversion:** Uses gpt-4o through the [CloudLLM](https://github.com/CloudLLM-ai/cloudllm/tree/main) rust API to convert the cleaned HTML content into nicely formatted Markdown.
- **Reusable Library:** The `universal_scrape` function is exposed for easy integration into other Rust projects.
- **Multilanguage Support:** The `universal_scrape` function accepts an optional language parameter to specify the language of the article to scrape, otherwise it defaults to English.

## Installation

You need to have Rust and Cargo installed on your system.

If you do have Rust installed, follow these steps:

1. **Install Uninews:**
```bash
cargo install uninews
```  

If you don't have Rust installed, follow these steps to install Rust and build from source:

1. **Install Rust:**

On Unix/macOS:
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

2. **Verify Installation**
```bash
rustc --version
cargo --version
```

3. **Clone the Project:**
 ```bash
 git clone https://github.com/gubatron/uninews.git
 cd uninews
```
   
4. **Build & Install the Project:**
```bash
make build
make install
```

5. **Run it in the command line:**
```bash
# make sure to either export the OPEN_AI_SECRET token before running it
export OPEN_AI_SECRET=sk-xxxxxxxxxxxxxxxxxxxxxxxxxx
uninews <some post url>

# or you can set it on the same statement and not export it
OPEN_AI_SECRET=sk-xxxxxxxxxxxxxxxxxxxxxxxxxx uninews [-l <some language name>] <some post url>
```

**Command line usage**
```
A universal news scraper for extracting content from various news blogs and newsites.

Usage: uninews [OPTIONS] <URL>

Arguments:
  <URL>  The URL of the news article to scrape

Options:
  -l, --language <LANGUAGE>  Optional output language (default: english) [default: english]
  -h, --help                 Print help
  -V, --version              Print version
```   

**Integrating it with your rust project**

**uninews** requires the `OPEN_AI_SECRET` environment variable to be set, you can set it in your code before calling the `universal_scrape` function.

If you've loaded your `OPEN_AI_SECRET` from a file or some other means, you can set it like this so uninews won't break:
`std::env::set_var("OPEN_AI_SECRET", my_open_ai_secret);`


```rust
using uninews::{universal_scrape, Post};

// Scrape the URL and convert its content to Markdown in the requested language.
let post = universal_scrape(&args.url, &args.language).await;
if !post.error.is_empty() {
    eprintln!("Error during scraping: {}", post.error);
    return;
}

// Print the title and Markdown-formatted content.
println!("{}\n\n{}", post.title, post.content);
```

Licensed under the MIT License.

Copyright (c) 2025 Ángel León
