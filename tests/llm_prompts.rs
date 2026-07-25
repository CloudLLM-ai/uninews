//! Tests for the LLM Markdown-conversion prompt builders, exercised through
//! the public `uninews::llm` surface (the prompt helpers are
//! `#[doc(hidden)]` — they exist for testability, not for end users).

use std::env;

use uninews::llm::{markdown_system_prompt, markdown_user_prompt, normalized_output_language};
use uninews::{convert_content_to_markdown, Post};

/// RAII helper: temporarily override an env var, restore on drop.
struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = env::var(key).ok();
        unsafe {
            env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            match self.previous.as_deref() {
                Some(previous) => env::set_var(self.key, previous),
                None => env::remove_var(self.key),
            }
        }
    }
}

#[test]
fn normalized_output_language_defaults_to_english() {
    assert_eq!(normalized_output_language(""), "english");
    assert_eq!(normalized_output_language("   "), "english");
    assert_eq!(normalized_output_language("spanish"), "spanish");
}

#[test]
fn markdown_prompts_require_near_lossless_preservation() {
    let system_prompt = markdown_system_prompt("english");
    let user_prompt = markdown_user_prompt("english", r#"{"content":"<p>Hello</p>"}"#);

    assert!(system_prompt.contains("preserving the source text and structure as fully as possible"));
    assert!(system_prompt
        .contains("Do not summarize, paraphrase, compress, or omit substantive details"));
    assert!(user_prompt.contains("Treat `content` as the canonical article body"));
    assert!(user_prompt.contains("keep it nearly verbatim"));
}

/// Regression test for the silent empty-conversion bug: when the Post
/// payload does not fit the context window, cloudllm's message-granularity
/// trim would drain the article itself and the model would "convert" an
/// empty request — succeeding with invented content (silent data loss).
/// The pre-flight check must fail loudly BEFORE any network call.
///
/// Hermetic: the OpenAI client is built with a dummy key (no network at
/// build time), and the 1-token window guarantees the pre-flight fires
/// before `send_message` is ever reached. No env lock is needed: the
/// other tests in this binary never read these env vars.
#[tokio::test]
async fn oversized_payload_fails_loudly_instead_of_silent_empty_conversion() {
    let _client = EnvVarGuard::set("UNINEWS_LLM_CLIENT", "openai");
    let _key = EnvVarGuard::set("OPEN_AI_SECRET", "test-dummy-key");

    let post = Post {
        title: "Test".to_string(),
        content: "word ".repeat(10_000),
        featured_image_url: String::new(),
        publication_date: None,
        author: None,
        error: String::new(),
    };

    let result = convert_content_to_markdown(post, "english", Some(1)).await;
    let error = result.expect_err("a 1-token window must reject any payload");
    assert!(
        error.contains("context window") && error.contains("silently dropped"),
        "error must name the cause and the danger, got: {}",
        error
    );
}
