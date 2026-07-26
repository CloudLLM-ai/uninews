---
schema_version: 1
name: code-review
description: Use when reviewing Rust code in this repository — deep passes over modules, pre-merge checks, or audit of a feature. Produces severity-rated, evidence-based findings with concrete fixes. Pair with the rust-backend-engineer skill, which defines the standards this review enforces.
tags: [rust, code-review, audit, security, performance, reliability, documentation]
triggers: [code review, review this crate, audit, deep review, pre-merge review, security review]
---

# code-review

Disciplined review of the uninews crate (or a diff within it). The
**rust-backend-engineer** skill defines the standards; this skill defines
**how to audit against them** and how to report.

**Core principle: evidence before assertions.** Every finding cites
`file:line` and quotes the offending code. A finding you cannot point at
is a hypothesis, not a finding — verify it or drop it.

## Severity taxonomy

- **Critical** — correctness bugs, panics on untrusted input, security
  holes (SSRF, secret leakage, injection), data loss, public-API breakage.
  Fix immediately.
- **Important** — reliability gaps (missing timeouts, swallowed errors,
  fallback-chain misbehavior), missing tests for risky logic, performance
  problems on hot paths, missing docs on public API.
  Fix before the work is considered done.
- **Minor** — style/idiom deviations, allocation micro-optimizations off
  hot paths, naming, stale comments.
  Note them; batch-fix only with approval.

## Review lenses (run all five)

1. **Style / idioms** — immutability by default; stringly-typed logic that
   should be enums; functions > ~30 lines; iterator-versus-allocation
   patterns; needless `clone()`/`String`.
2. **Documentation** — every public item documented; the *why* not just the
   *what*; `# Errors` / `# Panics` where relevant; doc-examples on
   non-trivial public functions; README/changelog in sync with behavior.
3. **Security** — secrets only from env, never logged or serialized; ALL
   I/O time-bounded; no panics on network/user input; SSRF surface of
   caller-supplied URLs documented; spawned processes use
   `std::process::Command` arg arrays, never shell strings; listener
   callbacks panic-isolated (`catch_unwind`, no lock held across the call).
   Proven hunt-list from the 0.46.0 audit:
   - **Error paths that embed raw response bodies** (secret-leak class —
     the parse-failure path is exactly when the body still carries a live
     token).
   - **"Success" paths that can succeed on empty/drained input**
     (data-loss class — e.g. a trimmer draining the only message and the
     caller reporting success). Pre-flight checks must exist.
   - **Every wait bounded — including library-internal RPCs and cleanup
     paths** (`close()` calls, driver handshakes, hardcoded library
     defaults that ignore the configured budget).
   - **Auto-download/auto-execute paths** (runtime binary installs):
     disclosed, opt-out provided, time-bounded.
4. **Performance (hot paths)** — per-scrape work: HTTP fetch, HTML
   cleaning, LLM conversion. Shared `reqwest::Client`s; selectors/regexes
   parsed once (`OnceLock`/const), not per call; zero-copy where cheap;
   no intermediate `Vec` collect chains that can stay iterators;
   **no process spawns per request** (cache drivers/browsers — keyed
   per-runtime when the library binds objects to their tokio runtime);
   compression enabled on HTTP clients (gzip/brotli).
5. **Reliability** — the fallback chain (plain HTTP → Playwright →
   archive.org) triggers and orders correctly, **and covers "200 but
   unusable" pages (thin content / JS shells), not just walls**; a
   fallback must never return something worse than the original result;
   every failure mode emits the matching `ScrapeEvent`; env-var parsing
   degrades to defaults; errors propagate as `Result` or documented error
   fields, never panics across the public API; **no default-run test hits
   the live network** (e2e tests that must, are `#[ignore]`'d).

## Method

1. **Scope first.** Whole crate, one module, or a diff
   (`git diff BASE..HEAD`)? Say which, and review only that.
2. **Read the actual code.** Do not review from memory or from the
   changelog. Quote what is on disk.
3. **Verify before reporting.** Suspected bug? Trace the call path and
   prove it reachable. Suspected perf issue? Confirm it is on a hot path
   (per-scrape), not startup. "This looks wrong" is not evidence.
4. **Check tests exist** for each Important+ finding — if the risky code
   is untested, that is part of the finding.
5. **Propose one concrete fix per finding** — the smallest change that
   resolves it, consistent with existing patterns. No speculative
   rewrites; no second mechanisms.
6. **Validate conditions with a live smoke when the fix introduces
   thresholds or triggers.** Hermetic fixtures encode the reviewer's
   assumptions; the real world violates them (a "thin page" returning
   534 KB with 138 chars of extraction was missed by strict
   error/size-only conditions until one live run exposed it). One live
   smoke before finalizing any trigger/threshold.

## Report format

Chat (or review comment) — no report files unless asked:

```
## Review: <scope> (<date>)
### Critical
1. `src/web.rs:142` — <claim>. Evidence: <quoted code / trace>.
   Fix: <smallest concrete change>.
### Important
...
### Minor
...
### Verified good (brief) — what was checked and found solid.
```

End with an explicit **Assessment**: ship / fix Critical+Important first /
needs re-review after fixes.

## Push-back protocol

When the author disagrees with a finding:
- Re-verify with stronger evidence (a failing test, a trace, a doc link).
- If the code is provably correct as written, withdraw the finding — a
  withdrawn finding is a win for accuracy, not a loss.
- If evidence still supports it, say so once, with the evidence, and let
  the author decide. The reviewer advises; the author owns the code.

## Red flags (never do these)

- Never report from a skim — every finding has `file:line` evidence.
- Never pad the report with theoretical issues to look thorough.
- Never demand a refactor where a two-line fix resolves the finding.
- Never soften a Critical to keep the peace — mislabeling severity is
  how silent-drop bugs ship (see: Telegram `callback_data` 64-byte
  rejections swallowed as `None`, diariobitcoin 2026-07-25).
