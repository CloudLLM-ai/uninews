//! LLM backend selection, session budgeting, and Markdown conversion.
//!
//! This module owns everything related to the CloudLLM-powered
//! HTML → Markdown conversion step:
//!
//! - provider / model resolution from `UNINEWS_LLM_CLIENT` and
//!   `UNINEWS_LLM_MODEL`,
//! - the context-window budget (`UNINEWS_LLM_CONTEXT_WINDOW`,
//!   [`DEFAULT_LLM_CONTEXT_WINDOW`]),
//! - the near-lossless Markdown-conversion prompts,
//! - [`convert_content_to_markdown`] itself.

use std::env;
use std::sync::Arc;

use cloudllm::client_wrapper::{ClientWrapper, Role};
use cloudllm::clients::claude::ClaudeClient;
use cloudllm::clients::gemini::GeminiClient;
use cloudllm::clients::grok::{GrokClient, Model as GrokModel};
use cloudllm::clients::openai::{Model as OpenAIModel, OpenAIClient};
use cloudllm::clients::openrouter::OpenRouterClient;
use cloudllm::LLMSession;

use crate::Post;

/// Default LLM client when `UNINEWS_LLM_CLIENT` is unset.
const DEFAULT_LLM_CLIENT: &str = "openai";

/// Per-client default model slug, used when `UNINEWS_LLM_MODEL` is unset.
///
/// Source of truth for the table in `README.md` → "LLM Providers" →
/// "Supported providers".
fn default_llm_model_for(client_name: &str) -> &'static str {
    match client_name {
        // GPT-5.6 Sol is the flagship OpenAI chat model (cloudllm Model::GPT56Sol).
        "openai" => "gpt-5.6-sol",
        // OpenRouter slug for the same family (alias gpt-5.6 also works upstream).
        "openrouter" => "openai/gpt-5.6-sol",
        // Grok 4.5 is xAI's frontier chat model (cloudllm Model::Grok45).
        "grok" => "grok-4.5",
        "gemini" => "gemini-3.5-flash",
        "claude" => "claude-opus-4.7-fast",
        // Fall back to OpenAI's default for any future/unknown client name.
        _ => "gpt-5.6-sol",
    }
}

/// Read the LLM client name from `UNINEWS_LLM_CLIENT`, defaulting to
/// [`DEFAULT_LLM_CLIENT`].
fn uninews_llm_client_name() -> String {
    env::var("UNINEWS_LLM_CLIENT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_LLM_CLIENT.to_string())
        .to_ascii_lowercase()
}

/// Read the LLM model slug from `UNINEWS_LLM_MODEL`, falling back to the
/// per-client default returned by [`default_llm_model_for`] when unset.
fn uninews_llm_model(client_name: &str) -> String {
    env::var("UNINEWS_LLM_MODEL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_llm_model_for(client_name).to_string())
}

/// Environment variable consulted when no explicit context window is passed
/// to [`convert_content_to_markdown`] / [`crate::universal_scrape`].
pub const UNINEWS_LLM_CONTEXT_WINDOW_ENV: &str = "UNINEWS_LLM_CONTEXT_WINDOW";

/// Default context window (in tokens) used when neither an explicit
/// `context_window_tokens` argument nor the `UNINEWS_LLM_CONTEXT_WINDOW` env
/// var provide a value. Set to 256K to comfortably cover the cleaned body of
/// typical long-form news articles plus the Markdown-conversion system prompt
/// and a reasonable completion budget, while staying below the 128K window of
/// older GPT-4o-class models.
pub const DEFAULT_LLM_CONTEXT_WINDOW: usize = 256_000;

/// Read the LLM context window (in tokens) from `UNINEWS_LLM_CONTEXT_WINDOW`,
/// falling back to [`DEFAULT_LLM_CONTEXT_WINDOW`] when unset, empty, or
/// unparseable. A non-positive value is treated as invalid and falls back to
/// the default.
///
/// Exposed (as `pub` + `#[doc(hidden)]`) so integration tests in
/// `tests/context_window_env.rs` can introspect the env-var-driven budget.
/// Library users should normally call [`llm_context_window`] (the same
/// function) for ergonomics.
#[doc(hidden)]
pub fn uninews_llm_context_window() -> usize {
    match env::var(UNINEWS_LLM_CONTEXT_WINDOW_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        Some(raw) => match raw.parse::<usize>() {
            Ok(value) if value > 0 => value,
            Ok(_) => {
                eprintln!(
                    "{}: ignoring non-positive value '{}'",
                    UNINEWS_LLM_CONTEXT_WINDOW_ENV, raw
                );
                DEFAULT_LLM_CONTEXT_WINDOW
            }
            Err(err) => {
                eprintln!(
                    "{}: failed to parse '{}' as usize ({}); using default {}",
                    UNINEWS_LLM_CONTEXT_WINDOW_ENV, raw, err, DEFAULT_LLM_CONTEXT_WINDOW
                );
                DEFAULT_LLM_CONTEXT_WINDOW
            }
        },
        None => DEFAULT_LLM_CONTEXT_WINDOW,
    }
}

/// Resolve the effective LLM context window in tokens from an explicit
/// override (preferred) or, when `None`, the `UNINEWS_LLM_CONTEXT_WINDOW`
/// env var, falling back to [`DEFAULT_LLM_CONTEXT_WINDOW`].
///
/// Exposed (as `pub` + `#[doc(hidden)]`) for integration tests that need to
/// exercise the per-call-override path; library users should pass the
/// `Option<usize>` directly to [`convert_content_to_markdown`] or
/// [`crate::universal_scrape`].
#[doc(hidden)]
pub fn resolve_llm_context_window(context_window_tokens: Option<usize>) -> usize {
    context_window_tokens.unwrap_or_else(uninews_llm_context_window)
}

/// Build the CloudLLM client selected by `UNINEWS_LLM_CLIENT` / `UNINEWS_LLM_MODEL`.
///
/// Each client reads its API key from a provider-specific environment variable:
/// - `openai`      → `OPEN_AI_SECRET`
/// - `openrouter`  → `OPENROUTER_API_KEY`
/// - `grok`        → `XAI_API_KEY`
/// - `gemini`      → `GEMINI_API_KEY`
/// - `claude`      → `CLAUDE_API_KEY`  (Anthropic Claude)
///
/// If `UNINEWS_LLM_MODEL` is unset, the per-client default from
/// [`default_llm_model_for`] is used (see the README's "LLM Providers" table).
fn build_uninews_llm_client() -> Result<Arc<dyn ClientWrapper>, String> {
    let client_name = uninews_llm_client_name();
    let model = uninews_llm_model(&client_name);

    match client_name.as_str() {
        "openai" => {
            let key = env::var("OPEN_AI_SECRET")
                .map_err(|_| "Please set the OPEN_AI_SECRET environment variable.".to_string())?;
            // Prefer strong-typed enums for the stock defaults; any other slug
            // falls through to the string constructor (escape hatch).
            let client = match model.as_str() {
                "gpt-5.6-sol" => {
                    OpenAIClient::new_with_model_enum(&key, OpenAIModel::GPT56Sol)
                }
                "gpt-5.6" => OpenAIClient::new_with_model_enum(&key, OpenAIModel::GPT56),
                "gpt-5.6-terra" => {
                    OpenAIClient::new_with_model_enum(&key, OpenAIModel::GPT56Terra)
                }
                "gpt-5.6-luna" => {
                    OpenAIClient::new_with_model_enum(&key, OpenAIModel::GPT56Luna)
                }
                other => OpenAIClient::new_with_model_string(&key, other),
            };
            Ok(Arc::new(client))
        }
        "openrouter" => {
            let key = env::var("OPENROUTER_API_KEY").map_err(|_| {
                "Please set the OPENROUTER_API_KEY environment variable.".to_string()
            })?;
            Ok(Arc::new(OpenRouterClient::new_with_model_str(
                &key, &model,
            )))
        }
        "grok" => {
            let key = env::var("XAI_API_KEY")
                .map_err(|_| "Please set the XAI_API_KEY environment variable.".to_string())?;
            let client = match model.as_str() {
                "grok-4.5" => GrokClient::new_with_model_enum(&key, GrokModel::Grok45),
                "grok-4.5-latest" => {
                    GrokClient::new_with_model_enum(&key, GrokModel::Grok45Latest)
                }
                other => GrokClient::new_with_model_str(&key, other),
            };
            Ok(Arc::new(client))
        }
        "gemini" => {
            let key = env::var("GEMINI_API_KEY")
                .map_err(|_| "Please set the GEMINI_API_KEY environment variable.".to_string())?;
            Ok(Arc::new(GeminiClient::new_with_model_string(
                &key, &model,
            )))
        }
        "claude" => {
            let key = env::var("CLAUDE_API_KEY")
                .map_err(|_| "Please set the CLAUDE_API_KEY environment variable.".to_string())?;
            Ok(Arc::new(ClaudeClient::new_with_model_str(&key, &model)))
        }
        other => Err(format!(
            "Unsupported UNINEWS_LLM_CLIENT '{}'. Allowed: openai, openrouter, grok, gemini, claude.",
            other
        )),
    }
}

/// Introspect the active CloudLLM client built by [`build_uninews_llm_client`].
///
/// This re-exports `cloudllm::LLMClientInfo` so callers can uniformly ask any
/// `Arc<dyn ClientWrapper>` for its provider slug, model name, and configured
/// tools without downcasting.
pub use cloudllm::LLMClientInfo;

/// Build the active CloudLLM client and return it for downstream introspection.
///
/// The returned `Arc<dyn ClientWrapper>` blanker-implements
/// [`cloudllm::LLMClientInfo`] (since cloudllm 0.15.7), so callers can
/// immediately call `llm_provider_name()`, `llm_model_name()`, and
/// `llm_tools()` on it.
///
/// # Example
///
/// ```no_run
/// use uninews::active_llm_client;
/// use cloudllm::LLMClientInfo;
/// if let Ok(client) = active_llm_client() {
///     println!(
///         "uninews is using {} ({})",
///         client.llm_provider_name().unwrap_or("unknown"),
///         client.llm_model_name().unwrap_or("unknown"),
///     );
/// }
/// ```
pub fn active_llm_client() -> Result<Arc<dyn ClientWrapper>, String> {
    build_uninews_llm_client()
}

/// Return a one-line, human-readable label for the active LLM
/// (`"OpenAI (gpt-5.6-sol)"`, `"OpenRouter (qwen/qwen3.7-max)"`, …).
///
/// Built from the live `Arc<dyn ClientWrapper>` via the `LLMClientInfo`
/// trait (cloudllm 0.15.7+), so it always reflects whatever
/// `UNINEWS_LLM_CLIENT` / `UNINEWS_LLM_MODEL` are set to at call time.
///
/// # Example
///
/// ```no_run
/// use uninews::active_provider_label;
/// println!("uninews is using {}", active_provider_label());
/// ```
pub fn active_provider_label() -> String {
    match build_uninews_llm_client() {
        Ok(client) => {
            let provider = client.llm_provider_name().unwrap_or("unknown");
            let model = client.llm_model_name().unwrap_or("unknown");
            format!("{} ({})", provider, model)
        }
        Err(_) => {
            // Fall back to the env-derived defaults so chat notifications
            // still render something useful even when the API key is missing.
            let client_name = uninews_llm_client_name();
            let model = uninews_llm_model(&client_name);
            format!("{} ({})", client_name, model)
        }
    }
}

/// Return the LLM context window (in tokens) Uninews will hand to the
/// underlying `LLMSession` when no explicit `context_window_tokens`
/// argument is passed to [`convert_content_to_markdown`] or
/// [`crate::universal_scrape`].
///
/// Precedence:
/// 1. Explicit `context_window_tokens: Some(n)` argument (always wins).
/// 2. `UNINEWS_LLM_CONTEXT_WINDOW` env var (parsed as `usize`).
/// 3. [`DEFAULT_LLM_CONTEXT_WINDOW`] (256,000 tokens).
///
/// Invalid or non-positive env-var values fall back to
/// [`DEFAULT_LLM_CONTEXT_WINDOW`] and a warning is written to stderr.
///
/// # Example
///
/// ```no_run
/// use uninews::llm_context_window;
/// println!("uninews is budgeting {} tokens of context", llm_context_window());
/// ```
pub fn llm_context_window() -> usize {
    uninews_llm_context_window()
}

/// Normalize the requested output language: empty or whitespace-only input
/// falls back to `"english"`.
#[doc(hidden)]
pub fn normalized_output_language(language: &str) -> &str {
    if language.trim().is_empty() {
        "english"
    } else {
        language
    }
}

/// System prompt for the near-lossless HTML → Markdown conversion.
#[doc(hidden)]
pub fn markdown_system_prompt(language: &str) -> String {
    format!(
        "You are an expert markdown formatter and translator for scraped news articles. \
         The provided JSON already contains the extracted article body in the `content` field. \
         Convert that content into clean Markdown in {} while preserving the source text and structure as fully as possible. \
         Do not summarize, paraphrase, compress, or omit substantive details. \
         Preserve paragraph order, list items, quotes, headings, names, dates, numbers, and factual claims. \
         Only remove obvious HTML tags, duplicated boilerplate, or navigation noise that slipped through the scraper. \
         If translation is requested, translate faithfully without shortening the article. \
         Output only the final Markdown body text. If {} is not supported, default to english.",
        language, language
    )
}

/// User prompt wrapping the serialized [`Post`] JSON for the conversion call.
#[doc(hidden)]
pub fn markdown_user_prompt(language: &str, post_json: &str) -> String {
    format!(
        "Convert the following Post JSON into Markdown formatted text in {}. \
         Treat `content` as the canonical article body and keep it nearly verbatim except for Markdown formatting, minimal cleanup, and faithful translation if needed. \
         Do not add commentary and do not return JSON.\n\n{}",
        language, post_json
    )
}

/// Converts raw HTML content to Markdown using the configured LLM provider.
///
/// This function takes scraped HTML content and transforms it into beautifully formatted
/// Markdown. It uses the CloudLLM library to communicate with the provider's API, allowing
/// for intelligent formatting and optional translation.
///
/// # How It Works
///
/// 1. Builds a CloudLLM client based on `UNINEWS_LLM_CLIENT` / `UNINEWS_LLM_MODEL`
/// 2. Creates an LLMSession with a system prompt instructing Markdown formatting
/// 3. Sends the scraped Post as JSON to the LLM
/// 4. Updates the Post's `content` field with formatted Markdown
/// 5. Optionally translates to the requested language
///
/// # Arguments
///
/// - `post`: The scraped Post with raw HTML content
/// - `language`: Target language for output (e.g., "spanish", "french", "japanese")
/// - `context_window_tokens`: Optional explicit LLM context window (in tokens).
///   When `Some(n)`, `n` is used as the `LLMSession` budget for the Markdown
///   conversion. When `None`, Uninews reads `UNINEWS_LLM_CONTEXT_WINDOW`
///   (parsed as `usize`); if that env var is also unset, empty, or unparseable,
///   it falls back to [`DEFAULT_LLM_CONTEXT_WINDOW`] (256,000 tokens).
///
/// # Returns
///
/// - `Ok(Post)`: Updated post with Markdown-formatted content in the target language
/// - `Err(String)`: Error message if API communication fails or environment variables are missing
///
/// # Environment Variables
///
/// - `UNINEWS_LLM_CLIENT` - Provider to use. Defaults to `openai`. Allowed:
///   `openai`, `openrouter`, `grok`, `gemini`, `claude`.
/// - `UNINEWS_LLM_MODEL`  - Model slug forwarded to the provider. If unset,
///   each client falls back to a built-in default (e.g. `gpt-5.6-sol` for `openai`,
///   `openai/gpt-5.6-sol` for `openrouter`). For OpenRouter you usually want a
///   `vendor/model` slug (e.g. `qwen/qwen3.7-max`).
/// - `UNINEWS_LLM_CONTEXT_WINDOW` - Optional LLM context-window budget (in
///   tokens) used when `context_window_tokens` is `None`. Falls back to
///   [`DEFAULT_LLM_CONTEXT_WINDOW`] when unset or unparseable.
/// - One provider-specific API key env var is required:
///   - `openai`     → `OPEN_AI_SECRET`
///   - `openrouter` → `OPENROUTER_API_KEY`
///   - `grok`       → `XAI_API_KEY`
///   - `gemini`     → `GEMINI_API_KEY`
///   - `claude`     → `CLAUDE_API_KEY`
///
/// # Errors
///
/// Returns error if:
/// - The required API key env var for the selected client is not set
/// - `UNINEWS_LLM_CLIENT` is set to an unsupported value
/// - Post serialization to JSON fails
/// - LLM API communication fails
/// - LLM returns an error response
///
/// # Examples
///
/// ```rust,no_run
/// # use uninews::{Post, convert_content_to_markdown};
/// #[tokio::main]
/// async fn main() {
///     let post = Post {
///         title: "Article Title".to_string(),
///         content: "<p>Raw HTML content</p>".to_string(),
///         featured_image_url: "".to_string(),
///         publication_date: None,
///         author: None,
///         error: String::new(),
///     };
///
///     // Convert with the provider selected via UNINEWS_LLM_CLIENT (default: openai / gpt-5.6-sol)
///     // and the default 256K context window.
///     match convert_content_to_markdown(post, "english", None).await {
///         Ok(markdown_post) => println!("{}", markdown_post.content),
///         Err(e) => eprintln!("Conversion failed: {}", e),
///     }
/// }
/// ```
///
/// # Supported Languages
///
/// Supports any language that the configured LLM provider understands, including
/// - English, Spanish, French, German, Italian
/// - Chinese, Japanese, Korean
/// - Portuguese, Russian, Arabic
/// - And many more...
///
/// If the specified language is not recognized, the output defaults to English.
pub async fn convert_content_to_markdown(
    mut post: Post,
    language: &str,
    context_window_tokens: Option<usize>,
) -> Result<Post, String> {
    // Build the CloudLLM client selected by UNINEWS_LLM_CLIENT / UNINEWS_LLM_MODEL.
    let client = build_uninews_llm_client()?;

    // Normalize language: if empty, default to "english".
    let lang = normalized_output_language(language);

    // Resolve the LLM context window: explicit override wins, then the env
    // var, then DEFAULT_LLM_CONTEXT_WINDOW. See `resolve_llm_context_window`.
    let context_window = resolve_llm_context_window(context_window_tokens);

    // Define a system prompt that instructs the LLM on its role.
    let system_prompt = markdown_system_prompt(lang);

    // Create a new LLMSession.
    let mut session = LLMSession::new(client, system_prompt, context_window);

    // Serialize the entire Post to JSON.
    let post_json = serde_json::to_string(&post)
        .map_err(|e| format!("Failed to serialize Post to JSON: {}", e))?;
    let user_prompt = markdown_user_prompt(lang, &post_json);

    // Send the prompt to the LLM.
    match session.send_message(Role::User, user_prompt, None).await {
        Ok(response) => {
            post.content = response.content.to_string();
            Ok(post)
        }
        Err(err) => Err(format!("LLM Error: {}", err)),
    }
}
