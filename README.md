# Uninews

Uninews is a universal news scraper written in Rust. It downloads a news article from a given URL, cleans the HTML content, and then uses CloudLLM (via OpenAI) to convert the content into Markdown format. The final output is a JSON object with the article's title, the Markdown-formatted content, and a featured image URL.

## Features

- **Scraping & Cleaning:** Extracts the main content of a news article by targeting the `<article>` tag (or falling back to `<body>`) and removing unwanted elements.
- **Markdown Conversion:** Uses [CloudLLM](https://github.com/CloudLLM-ai/cloudllm/tree/main) to convert the cleaned HTML content into nicely formatted Markdown.
- **Reusable Library:** The `universal_scrape` function is exposed for easy integration into other Rust projects.

## Installation

You need to have Rust and Cargo installed on your system.

If you do have Rust installed, follow these steps:

1. **Install Uninews:**
   ```bash
   cargo install uninews
   ```  

If you don't have Rust installed, follow these steps:

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
OPEN_AI_SECRET=sk-xxxxxxxxxxxxxxxxxxxxxxxxxx uninews <some post url>
```

```
uninews --help
Usage: uninews <URL>

Arguments:
<URL>  The URL of the news article to scrape

Options:
-h, --help     Print help
-V, --version  Print version
```   
