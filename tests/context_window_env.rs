//! Integration tests for the `UNINEWS_LLM_CONTEXT_WINDOW` env var and the
//! per-call `Option<usize>` override on `convert_content_to_markdown` /
//! `universal_scrape`.
//!
//! These live under `tests/` (rather than as inline `#[cfg(test)] mod tests`
//! inside `src/lib.rs`) so they exercise the crate strictly through its
//! public surface — anything they touch must be exported as `pub`.

use std::env;
use std::sync::Mutex;

use uninews::{
    llm_context_window, resolve_llm_context_window, uninews_llm_context_window,
    DEFAULT_LLM_CONTEXT_WINDOW, UNINEWS_LLM_CONTEXT_WINDOW_ENV,
};

/// Process-wide mutex guarding every read/write of `UNINEWS_LLM_CONTEXT_WINDOW`
/// from these tests. `std::env` is shared across all threads in the test
/// binary, so without this lock a parallel test setting the var to `"0"` can
/// race with a sibling test asserting on the default and produce a flaky
/// failure. Acquire via `let _guard = ENV_LOCK.lock().unwrap();` at the top
/// of any test that mutates or asserts on the env var.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// RAII helper: temporarily override an env var, restore on drop.
struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = env::var(key).ok();
        // `std::env::set_var` is unsafe in edition 2024 / newer Rust; uninews
        // is edition 2021 so the unsafe block is the conventional wrapper.
        unsafe {
            env::set_var(key, value);
        }
        Self { key, previous }
    }

    fn unset(key: &'static str) -> Self {
        let previous = env::var(key).ok();
        unsafe {
            env::remove_var(key);
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
fn default_llm_context_window_constant_is_256k() {
    assert_eq!(DEFAULT_LLM_CONTEXT_WINDOW, 256_000);
}

#[test]
fn env_var_name_constant_matches_expected_string() {
    assert_eq!(UNINEWS_LLM_CONTEXT_WINDOW_ENV, "UNINEWS_LLM_CONTEXT_WINDOW");
}

#[test]
fn uninews_llm_context_window_falls_back_to_default_when_unset() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let _guard = EnvVarGuard::unset(UNINEWS_LLM_CONTEXT_WINDOW_ENV);
    assert_eq!(uninews_llm_context_window(), DEFAULT_LLM_CONTEXT_WINDOW);
    assert_eq!(llm_context_window(), DEFAULT_LLM_CONTEXT_WINDOW);
}

#[test]
fn uninews_llm_context_window_falls_back_to_default_when_empty() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let _guard = EnvVarGuard::set(UNINEWS_LLM_CONTEXT_WINDOW_ENV, "   ");
    assert_eq!(uninews_llm_context_window(), DEFAULT_LLM_CONTEXT_WINDOW);
}

#[test]
fn uninews_llm_context_window_parses_valid_positive_value() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let _guard = EnvVarGuard::set(UNINEWS_LLM_CONTEXT_WINDOW_ENV, "2000000");
    assert_eq!(uninews_llm_context_window(), 2_000_000);
}

#[test]
fn uninews_llm_context_window_trims_whitespace_around_value() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let _guard = EnvVarGuard::set(UNINEWS_LLM_CONTEXT_WINDOW_ENV, "  128000 \n");
    assert_eq!(uninews_llm_context_window(), 128_000);
}

#[test]
fn uninews_llm_context_window_falls_back_to_default_on_unparseable() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let _guard = EnvVarGuard::set(UNINEWS_LLM_CONTEXT_WINDOW_ENV, "two-million");
    assert_eq!(uninews_llm_context_window(), DEFAULT_LLM_CONTEXT_WINDOW);
}

#[test]
fn uninews_llm_context_window_falls_back_to_default_on_zero() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let _guard = EnvVarGuard::set(UNINEWS_LLM_CONTEXT_WINDOW_ENV, "0");
    assert_eq!(uninews_llm_context_window(), DEFAULT_LLM_CONTEXT_WINDOW);
}

#[test]
fn resolve_llm_context_window_explicit_override_wins() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let _guard = EnvVarGuard::set(UNINEWS_LLM_CONTEXT_WINDOW_ENV, "1000");
    assert_eq!(resolve_llm_context_window(Some(2_000_000)), 2_000_000);
}

#[test]
fn resolve_llm_context_window_uses_env_when_none() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let _guard = EnvVarGuard::set(UNINEWS_LLM_CONTEXT_WINDOW_ENV, "500000");
    assert_eq!(resolve_llm_context_window(None), 500_000);
}

#[test]
fn resolve_llm_context_window_uses_default_when_none_and_env_unset() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let _guard = EnvVarGuard::unset(UNINEWS_LLM_CONTEXT_WINDOW_ENV);
    assert_eq!(resolve_llm_context_window(None), DEFAULT_LLM_CONTEXT_WINDOW);
}
