---
name: rust-backend-engineer
description: Implementation and review guidance for backend Rust code — writing new modules, implementing API endpoints, designing data structures, refactoring existing code, or reviewing backend Rust for correctness, performance, and idiomatic patterns. Use proactively whenever backend Rust code is being written or modified, including for enums, error handling, tests, and Axum/sqlx/tokio/serde/tower services in this workspace.
---

# Rust Backend Engineer

## Overview

Senior Rust backend engineering: think in types first, write code that leverages the compiler as the primary correctness tool, and treat every function as a contract. Use immutable data structures and strong types by default; reserve `mut` and `String` for where they're earned.

This skill consolidates and supersedes the previous Claude and Codex mirrors in `.claude/agents/rust-backend-engineer.md` and `rust/.codex/skills/rust-backend-engineer/`.

## Core Philosophy

**Immutability is the default. Mutability is the last resort.**

- Prefer immutable data structures and functional patterns (map, filter, fold, iterators) over mutable state.
- When mutation is unavoidable, contain it to the smallest possible scope and document why.
- Use `let` bindings, not `let mut`, unless you can justify the mutation.
- Prefer returning new values over modifying existing ones.
- Improves reasoning, concurrency safety, and correctness.

**Strongly-typed domains eliminate entire classes of bugs.**

- Never use raw `String` constants as function parameters, control values, or discriminators.
- Model every domain concept with enums that encode valid states at compile time.
- Design enums to be ergonomically convertible to/from strings when interoperability is required (APIs, serialization, CLI input) using `FromStr`, `Display`, `serde::Serialize`/`Deserialize`, and `strum` where appropriate.
- Eliminates "stringly-typed" logic, prevents invalid inputs at compile time, enables exhaustiveness checks, makes interfaces self-documenting.

**Simplicity and composability over cleverness.**

- Write short, focused functions that do one thing well (typically under 30 lines).
- Compose small functions into larger behaviors rather than writing monoliths.
- Prefer explicit over implicit — avoid excessive trait magic or macro complexity unless it genuinely reduces code and improves clarity.
- If a function needs a comment to explain what it does, break it into smaller, well-named parts.

## Documentation Standards

Every public struct, enum, trait, and function MUST have documentation:

```rust
/// Represents the lifecycle state of an order in the fulfillment pipeline.
///
/// Each variant encodes a valid state transition target. Invalid transitions
/// are prevented at compile time by the type system.
///
/// # Examples
///
/// ```rust
/// use crate::models::OrderStatus;
///
/// let status = OrderStatus::Pending;
/// assert_eq!(status.to_string(), "pending");
///
/// let parsed: OrderStatus = "shipped".parse().unwrap();
/// assert_eq!(parsed, OrderStatus::Shipped);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    /// Order has been created but not yet confirmed.
    Pending,
    /// Order has been confirmed and is being prepared.
    Confirmed,
    /// Order has been handed to the carrier.
    Shipped,
    /// Order has been delivered to the customer.
    Delivered,
    /// Order was cancelled before shipment.
    Cancelled,
}
```

- Document the **why**, not just the **what**.
- Include `# Examples` sections with runnable Rust doc-tests for any non-trivial function.
- Document error conditions, panics (if any — prefer `Result`), and edge cases.
- Use `# Errors`, `# Panics`, `# Safety` sections per Rust convention.

## Enum Design

1. **Always derive**: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize` at minimum.
2. Add `Copy` when variants carry no heap data.
3. Implement `Display` for human-readable output.
4. Implement `FromStr` for parsing from strings with proper error types.
5. Use `#[serde(rename_all = "snake_case")]` for consistent serialization.
6. Consider `strum::EnumString`, `strum::Display` for boilerplate reduction.
7. Create a dedicated error type for parse failures — never use `String` as an error type.

```rust
impl std::fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Confirmed => write!(f, "confirmed"),
            Self::Shipped => write!(f, "shipped"),
            Self::Delivered => write!(f, "delivered"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::str::FromStr for OrderStatus {
    type Err = ParseOrderStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "confirmed" => Ok(Self::Confirmed),
            "shipped" => Ok(Self::Shipped),
            "delivered" => Ok(Self::Delivered),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(ParseOrderStatusError(other.to_owned())),
        }
    }
}
```

## Error Handling

- Use `thiserror` for library/domain errors, `anyhow` only in binary/test code if needed.
- Define specific error enums per module — never use `Box<dyn Error>` in library code.
- Prefer `Result<T, E>` over panics. Reserve `unwrap()` and `expect()` for cases where you can prove infallibility, and always include a message with `expect()`.
- Use the `?` operator for error propagation — avoid verbose match chains for simple propagation.

## Testing Standards

**Project convention: tests live in the crate's `tests/` directory, never in `src/`.**

- **All tests are integration tests** — one `tests/test_<topic>.rs` file per behavior.
- **NEVER** add `#[cfg(test)] mod tests { ... }` blocks to source files in this workspace. The source modules must stay free of inline test modules and test-only imports.
- If a helper is private and you need to test it from `tests/`, raise its visibility to `pub` (not `pub(crate)` — `pub(crate)` is not reachable from integration tests). This is preferred over adding a `#[cfg(test)]` escape hatch.
- Tests are documented with `///` comments explaining what behavior they verify.
- Use descriptive test names: `test_parse_embed_tweet_response_rejects_success_false` not `test1`.
- Test both happy paths and error cases.
- Test edge cases and boundary conditions.
- For enums: always test `Display`/`FromStr` roundtripping and invalid input rejection.

Reference layout (e.g. `dbtc_rss_notifier/`):

```text
dbtc_rss_notifier/
  src/lib.rs            # production code only — no #[cfg(test)] blocks
  tests/
    test_x_notification.rs
    test_threads_notification.rs
    test_facebook_notification.rs
    test_instagram_notification.rs
    test_linkedin_simple_post.rs
    test_social_notification_logic.rs
    test_analisis_filter.rs
    test_dry_run.rs
    test_interval_setting.rs
    test_embed_tweet_response.rs
    common/             # shared test helpers (load_test_properties, etc.)
```

Each `tests/test_<topic>.rs` imports from the crate like an external user would:

```rust
use dbtc_rss_notifier::{Post, notify_x_feed, prepare_social_feed_content};
```

End-to-end / "live API" tests (X, Telegram, Threads, Facebook, Instagram, LinkedIn) are marked `#[ignore]` and only run with `--ignored` plus a populated `DBTC_PROPERTIES` file. Pure unit tests of internal helpers have no `#[ignore]` and run as part of the default `cargo test` suite.

Example — `tests/test_order_status.rs` (lives in the crate's `tests/` directory, NOT in `src/`):

```rust
use my_crate::OrderStatus;

/// Verifies that every `OrderStatus` variant survives a
/// `Display` → `FromStr` roundtrip without data loss.
#[test]
fn test_order_status_display_fromstr_roundtrip() {
    let variants = [
        OrderStatus::Pending,
        OrderStatus::Confirmed,
        OrderStatus::Shipped,
        OrderStatus::Delivered,
        OrderStatus::Cancelled,
    ];
    for status in variants {
        let s = status.to_string();
        let parsed: OrderStatus = s.parse().expect("roundtrip should succeed");
        assert_eq!(status, parsed);
    }
}

/// Ensures that invalid strings produce a meaningful parse error
/// rather than silently succeeding.
#[test]
fn test_order_status_rejects_invalid_input() {
    let result = "nonexistent".parse::<OrderStatus>();
    assert!(result.is_err());
}
```

## Performance Optimization

Once code is correct and tested:

1. Identify hot paths — profile or reason about which code runs per-request vs. once at startup.
2. Prefer zero-copy — use `&str` over `String`, `Cow<'_, str>` when ownership is conditional.
3. Avoid unnecessary allocations — use iterators and combinators instead of collecting into intermediate `Vec`s.
4. Use `Arc` over `Clone` for large immutable shared data.
5. Prefer `SmallVec` or `ArrayVec` for small, bounded collections on hot paths.
6. Batch database queries — avoid N+1 patterns; use `sqlx::query!` with `IN` clauses.
7. Cache expensive computations — consider `once_cell::sync::Lazy` or `std::sync::OnceLock`.
8. Benchmark before optimizing — use `criterion` for microbenchmarks, `tracing` for latency measurement.

## Project Notes (Axum + sqlx)

- Axum 0.7: `FromRequestParts` requires `#[async_trait]` on impl blocks.
- `FromRef<AppState>` is needed for extracting `PgPool` from state in custom extractors.
- Database schema uses `CREATE TABLE IF NOT EXISTS` — no migration files.
- For adding columns: use `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`.
- Social networks are looked up by name string, not ID.
- Never hardcode reference table IDs in frontend — always JOIN and use `display_name` from API.

## Workflow

1. Understand the requirement — ask clarifying questions if the domain is ambiguous.
2. Design types first — define structs, enums, and traits before writing logic.
3. Implement with immutability and composability — small functions, no unnecessary mutation.
4. Document everything — doc comments with examples on all public items.
5. Write tests — integration tests in `tests/test_<topic>.rs` (or `tests/<topic>.rs`), covering happy paths, errors, and edge cases. Never add `#[cfg(test)]` blocks to `src/`.
6. Review for hot paths — once correct, optimize allocation patterns and data flow.
7. Self-review — before presenting code, verify: Are all types documented? Are there tests in `tests/` (not `#[cfg(test)]` in `src/`)? Is mutation minimized? Are strings eliminated in favor of enums? Are errors properly typed?
8. **Quality gate (MANDATORY before claiming done or asking to commit/deploy):** from `rust/`, run `make clippy` and it MUST exit 0 with zero warnings. The Makefile runs `cargo fmt` then `cargo clippy -- -D warnings` across the workspace — any `dead_code`, `unused`, style, or correctness lint fails the build. Do not hand off code that fails this gate. If a public getter is only exercised by integration tests, either call it from a production path (e.g. a log line) or the bin target will still fail `dead_code` under `-D warnings`.

## Review Focus

- Check type design before line-by-line logic.
- Check failure modes, boundary cases, and test coverage before micro-optimizations.
- Prefer a small refactor that improves type safety over a large stylistic rewrite.
- Confirm `make clippy` is clean (see Workflow §8) before approving.

## Typical Triggers

- "Add a new Rust endpoint or binary behavior."
- "Refactor this module to be more testable."
- "Review this Rust code for correctness and idiomatic patterns."
- "Introduce a typed enum or error model here."
- "Create a service that processes payment webhooks."
- "We need a new enum to represent order states."
- "Add a new endpoint to handle user registration."

## Hard-Earned Lessons (diariobitcoin workspace, 2026-07)

Distilled from post-mortems of real mistakes (MentisDB diariobitcoin chain
#155+). These override generic best practices when they conflict.

1. **One mechanism. No speculative fallbacks.** Three times the operator
   reverted over-engineered designs: dual source-config mechanisms
   (inline list + file), a CANUTO_LLM_* fallback chain for scout, tiered
   source rotation. Ship exactly one way to do something; a fallback is a
   second mechanism to test, document, and debug. Simplicity beats
   flexibility the user didn't ask for.
2. **Follow the existing naming convention before inventing one.** Props
   keys are `DBTC_<TOOL>_*` (`DBTC_SCOUT_LLM_CLIENT`), not per-app
   prefixes I made up (`CANUTO_SCOUT_LLM_*`). Grep the existing keys first.
3. **Verify claims before asserting them.** I labeled ~30 sources
   "scrape-friendly, no Cloudflare/paywall" from general knowledge; live
   verification killed a third of them (The Defiant, Blockworks, Axios…).
   Rule: feed-200 ≠ article-readable. Verify at the deepest level the
   pipeline depends on (here: the uninews CLI on a real article URL).
4. **Match the tool to the job.** Headline scanning is deterministic
   parsing (RSS/Atom, SSR anchors), not LLM scraping. I initially reused
   the draft command's uninews pattern because it existed — it produced
   thin, linkless output and `dbtc_publish #` garbage. Copying a pattern
   without evaluating fit is how you ship a broken core value prop. If the
   main output is unusable, fix it before presenting, not "in v2".
5. **Simulate the exact external-API contract before shipping UX.**
   Three Slack-ack iterations failed for three different documented
   semantics I skipped (in_channel response display, response_url,
   replace_original needing in_channel). Verifying transport (curl 200) is
   not verifying rendering. Read the platform's interaction docs first;
   test the exact payload shape.
6. **Verify topology before theorizing.** Hours lost debugging a 502 by
   assuming nginx ran on the host — it ran in a container. First commands
   in any prod mystery: `docker ps`, `ss -tlnp`, `ps aux`. A wrong mental
   model is the most expensive bug; cheapest to kill first.
7. **Narrate before touching production.** Even formally-safe runbook
   actions (graceful USR2 reload) alarm the operator when they arrive
   unexplained. State what will run, why it's safe, get the OK. This is in
   AGENTS.md and applies to every agent.
8. **After surprising edit successes, re-read the file.** An edit whose
   oldString "shouldn't have matched" did match — and left a duplicated
   brace that only the compiler caught. Surprise = signal.

## Hard-Earned Lessons II (uninews 0.46.0 deep-review cycle, 2026-07-25)

Distilled from auditing and fixing the uninews crate (5 Critical +
19 Important findings, all shipped in 0.46.0). They apply to every Rust
crate in this workspace — canuto's bots, the proxy crates, and anything
that fetches, shells out, or talks to an LLM.

1. **Timeouts must cover EVERY wait — including library internals and
   cleanup paths.** playwright-rs's `browser.close()` is an RPC with no
   timeout; `wait_for_load_state` used a hardcoded 30s that silently
   ignored the configured budget; Chrome's `--virtual-time-budget` never
   bounds a trickling server. Wrap the whole external operation in
   `tokio::time::timeout` (or a watchdog that kills the child), not just
   the steps that accept a timeout parameter. Same class here: any
   blocking HTTP client (telegram-api-rs) has no timeout story at all.
2. **"Success" on empty input is data loss.** cloudllm trims history at
   message granularity — an oversized article was drained whole and
   "converted" to invented content reported as success. Pre-flight any
   invariant that a downstream trim/queue/buffer can silently violate;
   fail loudly before the call.
3. **Error strings that get persisted or displayed must never embed raw
   response bodies.** The parse-failure path is exactly when the body
   still carries a live secret (an X `access_token` leaked into
   `Post::error` this way — the same class as tokens in logs here).
   Summarize untrusted bodies with char-boundary-safe truncation.
4. **Fallback triggers must cover "200 but unusable", not just walls** —
   extraction failures, thin content, JS shells. A fallback must never
   return something worse than the original result (keep-plain-result
   guarantee); that makes every trigger safe to fire.
5. **Never hold a `std::Mutex` guard across `.await`** — rustc 1.96's
   `clippy::await_holding_lock` errors under `-D warnings`. Tests that
   mutate the same env vars run as sequential scenarios inside ONE
   `#[tokio::test]` under one set of RAII guards.
6. **Pin bogus provider credentials in "hermetic" tests.** Dev shells
   export real API keys; an unguarded test quietly makes live calls (one
   ran 300s). Set the provider env var to a junk value so the network
   stage can never fire.
7. **Hermetic tests alone miss real-world shape — run one live smoke.**
   The 512-byte content threshold in uninews's thin-content trigger came
   from one live run (534 KB page, 138 chars extracted), not from the
   hermetic suite. This generalizes lesson 5 above (simulate the exact
   external-API contract): fixtures encode YOUR assumptions.
8. **HTTP 200 ≠ usable.** `dlnews.com/rss/` returns an HTML shell with a
   200 — check content type/body, not status. For JS-shell sites, verify
   feed existence by rendering with Playwright and inspecting the live
   DOM for `link[rel="alternate"]` ("axios has no section feeds" is
   proven, not assumed). Sharpens lesson 3 above (verify at the deepest
   level the pipeline depends on).
9. **Process spawn on a hot path is a performance bug.** Per-request
   node-driver + Chromium launches cost seconds. Cache heavyweight
   externals process-wide; when a library binds objects to their tokio
   runtime, key the cache per runtime.
10. **Platform caps are part of the contract.** Telegram `callback_data`
    is 1-64 BYTES; every scout story message died on `publish:<url>`
    while plain headers sailed through. Any dynamic content placed into a
    size-capped or parser-validated field gets a test pinning the limit
    with production-shaped payloads, not toy fixtures.

## Hard-Earned Lessons III (residential render relay + wall-order optimization, 2026-07-26)

Distilled from building the proxy render relay (proxy_common/broker/agent +
uninews content-fallback hook + scout relay fallback) and its three production
incidents (MentisDB diariobitcoin chain #196–#208).

1. **Reachability ≠ 2xx.** ureq (and most clients) surface non-2xx as `Err`,
   so `probe.call().is_ok()` answers "did the endpoint return success", not
   "is the endpoint there". The EC2 metadata probe returned false ON EC2 for
   months because IMDSv2 answers tokenless GETs with 401. A link-local
   address answering AT ALL is the signal — match
   `Ok(_) | Err(StatusCode(401|403))`. Any detection/probe helper written
   this way must be re-audited.
2. **Detection helpers fail silently — verify them live on the target
   machine class.** The broken probe above made a whole feature
   (wall-order optimization) silently never engage in prod while its logic
   and hermetic tests were correct. Env overrides (`FORCE_RELAY`/`FORCE_DIRECT`)
   are not a substitute for checking the real path once on the real host.
3. **Evolve wire protocols with `#[serde(default)]` optional fields, and
   design the degradation explicitly.** `render_browser: Option<bool>`
   round-trips both directions across mixed broker/agent/client versions:
   old agents ignore it (plain HTTP — same as before), new agents on old
   payloads see `None`. Write the mixed-version test matrix down, then pin
   it with old-payload/new-payload serde tests.
4. **Expose one public render/scrape API rather than duplicating the logic
   across crates.** The agent nearly grew a second copy of uninews's ~150
   lines of browser launch/wait/challenge-settle code. Making
   `fetch_rendered_dom_with_playwright` pub and depending on it keeps CF
   heuristics in exactly one place; two copies ALWAYS drift (see #116/#119).
   A binary crate depending on a bigger library beats a stale copy.
5. **Process-global slots (env vars, event listeners, fallback hooks) make
   parallel tests race.** Serialize with a file-local
   `tokio::sync::Mutex` (await-safe; std `Mutex` across `.await` trips
   `clippy::await_holding_lock`), and assert ORDER with a recording event
   listener (deterministic) instead of wall-clock timing (flaky).
6. **A lib fix ships nowhere until every binary that links the lib is
   reinstalled.** `dbtc_publish` execs `dbtc_draft`, not `dbtc_canuto` —
   the flag logic was deployed to the service but the CLI kept the old
   behavior. Keep the binary matrix in mind on every shared-lib change:
   `make install` ALL affected binaries, then verify with
   `strings <binary> | grep <new-symbol>`.
7. **Never hardcode secrets in start scripts — source from the props file
   at exec time.** Three different `DBTC_PROXY_API_KEY` values lived in two
   start scripts and production.props; a routine broker restart with the
   stale script 401'd the entire relay. `export KEY=$(grep ... props)` —
   one source of truth, and after any service restart run a cheap
   authenticated probe (400 ≠ 401) before declaring health.
8. **Every fallback layer must re-validate the layer's output before
   accepting it.** The residential relay's rendered DOM goes through the
   same `looks_like_bot_protection` check as the local render — a remote
   render CAN come back walled, and accepting it would smuggle a challenge
   page in as article content. A fallback that can never make things worse
   is a fallback you can reorder freely.
9. **Put host-class knobs in the public crate's env conventions, host-class
   detection in the private host.** uninews learned
   `UNINEWS_CONTENT_FALLBACK_FIRST` (generic ordering preference, off by
   default); dbtc learned to SET it from `is_cloud_environment()`. The
   public crate gained a justifiable feature; zero AWS/relay semantics
   leaked into it. Public crates get capabilities, private crates get
   policies.
