//! Tests for the LLM Markdown-conversion prompt builders, exercised through
//! the public `uninews::llm` surface (the prompt helpers are
//! `#[doc(hidden)]` — they exist for testability, not for end users).

use uninews::llm::{markdown_system_prompt, markdown_user_prompt, normalized_output_language};

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
