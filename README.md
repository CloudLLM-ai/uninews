# Uninews
![image](https://github.com/user-attachments/assets/43b59fce-3f0c-4fc8-8ae0-4e97eada5a5b)

Uninews is a universal news smart scraper written in Rust.

It downloads a news article from a given URL, cleans the HTML content, and leverages CloudLLM (via OpenAI) to convert the content into Markdown format.

With its powerful translation capabilities, Uninews can seamlessly translate articles into multiple languages while preserving formatting, making it ideal for multilingual content processing.

The final output (via API) is a JSON object containing the article's title, the Markdown-formatted content (translated if specified), and a featured image URL.

It can be used both as a library and as a command-line tool in Linux, Mac and Windows.

When used as a command-line tool, it outputs the final Markdown with the contents of the news article or blog post in the requested language.

## Usage

```
uninews --help
A universal news scraper for extracting content from various news blogs and news sites.

Usage: uninews [OPTIONS] <URL>

Arguments:
  <URL>  The URL of the news article to scrape

Options:
  -l, --language <LANGUAGE>  Optional output language (default: english) [default: english]
  -j, --json                 Output the result as JSON instead of human-readable text
  -h, --help                 Print help
  -V, --version              Print version
```

## Features

- **Scraping & Cleaning:** Extracts the main content of a news article by targeting the `<article>` tag (or falling back to `<body>`) and removing unwanted elements.
- **Markdown Conversion:** Uses gpt-4o through the [CloudLLM](https://github.com/CloudLLM-ai/cloudllm/tree/main) rust API to convert the cleaned HTML content into nicely formatted Markdown.
- **X.com / Twitter Support:** Reads individual tweets and full X threads via the X API v2, assembling the thread chronologically before converting it to Markdown.
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

## X.com / Twitter Support

To read tweets and X threads, set:

1. `X_API_KEY` as your X App **Consumer Key**
2. `X_API_SECRET` as your X App **Consumer Secret**

`uninews` will exchange them for an app-only bearer token automatically.

You can obtain both values from your X App dashboard under **Keys and tokens**.

```bash
export X_API_KEY=your_x_api_key
export X_API_SECRET=your_x_api_secret
uninews "https://x.com/user/status/1234567890"
```

### Environment variables for X.com

| Variable | Required | Description |
|---|---|---|
| `X_API_KEY` | Yes | X App Consumer Key / API Key from the **Keys and tokens** page. |
| `X_API_SECRET` | Yes | X App Consumer Secret / API Secret from the same **Keys and tokens** page. |
| `UNINEWS_CHROME_USER_DATA_DIR` | No | Chrome user-data directory for the secondary X Article browser fallback, if X withholds the article body from its web GraphQL payload and guest HTML. |
| `UNINEWS_CHROME_PROFILE_DIR` | No | Chrome profile directory name such as `Default` or `Profile 1`, used with `UNINEWS_CHROME_USER_DATA_DIR`. |
| `UNINEWS_CHROME_BINARY` | No | Override the Chrome/Chromium executable used for the secondary X Article browser fallback. |

When a URL starts with `https://x.com/` or `https://twitter.com/`, uninews will:
1. Extract the tweet ID from the URL.
2. Fetch the tweet (and its author info) via the X API v2.
3. If the post is only sharing an external article link, follow the expanded article URL and scrape the linked article directly.
4. If the post is only sharing an X Article link (`x.com/i/article/...`), fetch the article body from X's web GraphQL tweet payload.
5. Only if X still withholds the article body there, fall back to the linked article URL / browser fallback path.
6. Otherwise, attempt to retrieve the full thread from the same author using the
   recent-search endpoint (covers the last 7 days).
7. Sort all thread tweets chronologically (oldest → newest).
8. Pass the assembled content through the AI formatter, just like any other URL.

For `x.com/i/article/...` links, `uninews` now first asks X's web GraphQL endpoint for the article title and body text tied to the linking tweet. If X still hides the article body there, `uninews` will try a local Chrome headless fallback automatically. If X still serves the guest wall, point `UNINEWS_CHROME_USER_DATA_DIR` at a logged-in Chrome user-data directory and optionally set `UNINEWS_CHROME_PROFILE_DIR`.

When those variables are set, `uninews` clones the selected Chrome profile into a temporary directory before launching headless Chrome, so your normal Chrome session can stay open and the live profile lock is not touched.

Example on macOS:

```bash
export UNINEWS_CHROME_USER_DATA_DIR="$HOME/Library/Application Support/Google/Chrome"
export UNINEWS_CHROME_PROFILE_DIR="Default"
uninews "https://x.com/DiarioBitcoin/status/2034263054754726116"
```

If either `X_API_KEY` or `X_API_SECRET` is missing, a clear error message is returned instead of silently failing.

This is **not** OAuth 1.0a user-context authentication. `uninews` uses your Consumer Key and Consumer Secret to obtain an OAuth 2.0 app-only bearer token for read-only X API requests.

**Command line usage**
```
A universal news scraper for extracting content from various news blogs and newsites.

Usage: uninews [OPTIONS] <URL>

Arguments:
  <URL>  The URL of the news article to scrape

Options:
  -l, --language <LANGUAGE>  Optional output language (default: english) [default: english]
  -j, --json                 Output the result as JSON instead of human-readable text
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
let post = universal_scrape(&args.url, &args.language, Some(cloudllm::clients::openai::Model::GPT41Mini)).await;
if !post.error.is_empty() {
    eprintln!("Error during scraping: {}", post.error);
    return;
}

// Print the title and Markdown-formatted content.
println!("{}\n\n{}", post.title, post.content);
```

Licensed under the MIT License.

Copyright (c) 2026 Ángel León
