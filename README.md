# Uninews
![image](https://github.com/user-attachments/assets/43b59fce-3f0c-4fc8-8ae0-4e97eada5a5b)

Uninews is a universal news smart scraper written in Rust.

It downloads a news article from a given URL, cleans the HTML content, and leverages [CloudLLM](https://github.com/CloudLLM-ai/cloudllm) to convert the content into Markdown format with minimal loss.

The LLM provider is pluggable via `UNINEWS_LLM_CLIENT` and `UNINEWS_LLM_MODEL` environment variables — see the [LLM Providers](#llm-providers) section. Out of the box Uninews talks to OpenAI, but you can route Markdown conversion through OpenRouter, xAI Grok, Google Gemini, or Anthropic Claude without changing your code.

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
- **Markdown Conversion:** Uses the [CloudLLM](https://github.com/CloudLLM-ai/cloudllm/tree/main) Rust API to convert the cleaned HTML content into near-lossless Markdown. The LLM provider is pluggable via env vars (see [LLM Providers](#llm-providers)).
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

OpenAI (default):
```bash
export OPEN_AI_SECRET=sk-xxxxxxxxxxxxxxxxxxxxxxxxxx
uninews <some post url>
```

OpenRouter with any `vendor/model` slug (e.g. Qwen 3.7 Max):
```bash
export UNINEWS_LLM_CLIENT=openrouter
export UNINEWS_LLM_MODEL=qwen/qwen3.7-max
export OPENROUTER_API_KEY=sk-or-xxxxxxxxxxxxxxxxxxxxxxxxxx
uninews <some post url>
```

Or, on a single line without exporting:
```bash
OPEN_AI_SECRET=sk-xxx uninews [-l <some language name>] <some post url>
```

## LLM Providers

Uninews selects the LLM provider used to convert HTML to Markdown based on
two environment variables:

| Variable | Default | Description |
|---|---|---|
| `UNINEWS_LLM_CLIENT` | `openai` | One of `openai`, `openrouter`, `grok`, `gemini`, `claude`. |
| `UNINEWS_LLM_MODEL`  | per-client | Free-form model slug. If unset, each client falls back to the default listed in the table below (e.g. `gpt-5.5` for `openai`, `openai/gpt-5.5` for `openrouter`). For OpenRouter you usually want a `vendor/model` slug (e.g. `qwen/qwen3.7-max`). |

Each provider reads its API key from a dedicated env var. Only the one matching
the active `UNINEWS_LLM_CLIENT` is consulted. When `UNINEWS_LLM_MODEL` is unset,
each client falls back to its built-in default (rightmost column), so you only
need to override `UNINEWS_LLM_MODEL` when you want a different model.

| `UNINEWS_LLM_CLIENT` | API key env var | Default model when `UNINEWS_LLM_MODEL` is unset |
|---|---|---|
| `openai`     | `OPEN_AI_SECRET`     | `gpt-5.5` |
| `openrouter` | `OPENROUTER_API_KEY` | `openai/gpt-5.5` (a `vendor/model` slug) |
| `grok`       | `XAI_API_KEY`        | `grok-4.3` |
| `gemini`     | `GEMINI_API_KEY`     | `gemini-3.5-flash` |
| `claude`     | `CLAUDE_API_KEY`     | `claude-opus-4.7-fast` |

### Examples

**OpenAI (default):**
```bash
export OPEN_AI_SECRET=sk-xxx
uninews https://example.com/article
```

**OpenRouter with Qwen 3.7 Max:**
```bash
export UNINEWS_LLM_CLIENT=openrouter
export UNINEWS_LLM_MODEL=qwen/qwen3.7-max
export OPENROUTER_API_KEY=sk-or-xxx
uninews https://example.com/article
```

**Anthropic Claude:**
```bash
export UNINEWS_LLM_CLIENT=claude
export UNINEWS_LLM_MODEL=claude-sonnet-4-6
export CLAUDE_API_KEY=sk-ant-xxx
uninews https://example.com/article
```

If `UNINEWS_LLM_CLIENT` is set to an unsupported value, or the matching API
key env var is missing, Uninews returns a clear error in `Post::error`.

### Introspecting the active LLM (library use)

If you embed Uninews in another Rust app and want to surface the active
provider/model in a chat notification or log line, the active client is
exposed via the upstream `cloudllm::LLMClientInfo` trait (re-exported as
`uninews::LLMClientInfo`):

```rust
use uninews::{active_llm_client, active_provider_label, LLMClientInfo};

if let Ok(client) = active_llm_client() {
    println!(
        "uninews routed through {} ({})",
        client.llm_provider_name().unwrap_or("unknown"),
        client.llm_model_name().unwrap_or("unknown"),
    );
}

// Or, for a one-line label that's safe to drop into any chat message:
println!("Extrayendo con uninews usando {}...", active_provider_label());
// → "Extrayendo con uninews usando OpenRouter (qwen/qwen3.7-max)..."
```

`active_provider_label()` always reflects whatever `UNINEWS_LLM_CLIENT` /
`UNINEWS_LLM_MODEL` are set to at call time, so consumers can replace their
hardcoded "GPT-5.5" / "Claude" / "Qwen" strings with a single call.

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
8. Pass the assembled content through the AI formatter, preserving the scraped article wording and structure as closely as possible.

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

Uninews reads the LLM provider from `UNINEWS_LLM_CLIENT` and `UNINEWS_LLM_MODEL`. If you want to override them in code (instead of via `std::env::set_var`), do it before calling `universal_scrape`. For example, to force OpenRouter with a Qwen model from inside your app:

```rust
use uninews::{universal_scrape, Post};

// Route Markdown conversion through OpenRouter
std::env::set_var("UNINEWS_LLM_CLIENT", "openrouter");
std::env::set_var("UNINEWS_LLM_MODEL", "qwen/qwen3.7-max");
std::env::set_var("OPENROUTER_API_KEY", "sk-or-...");

// Scrape the URL and convert its content to Markdown in the requested language.
let post = universal_scrape(&url, "english").await;
if !post.error.is_empty() {
    eprintln!("Error during scraping: {}", post.error);
    return;
}

// Print the title and Markdown-formatted content.
println!("{}\n\n{}", post.title, post.content);
```

If you only need OpenAI, just set `OPEN_AI_SECRET` once (e.g. before starting
your process) and call `universal_scrape(url, "english")` — Uninews will pick
it up.

Licensed under the MIT License.

Copyright (c) 2026 Ángel León
