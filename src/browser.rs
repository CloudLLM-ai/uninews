//! Headless browser rendering fallbacks.
//!
//! Uninews has two browser-render paths:
//!
//! 1. **Chrome `--dump-dom`** ([`fetch_rendered_dom_with_chrome`]) — used for
//!    X Articles behind a guest wall. Shells out to a local Chrome/Chromium
//!    binary. Optionally clones a logged-in profile via
//!    `UNINEWS_CHROME_USER_DATA_DIR` / `UNINEWS_CHROME_PROFILE_DIR`.
//! 2. **Playwright Chromium** ([`fetch_rendered_dom_with_playwright`]) — used
//!    for bot-protection walls (Cloudflare & co.) before the archive.org
//!    fallback. Driven by the `playwright-rs` crate (Microsoft Playwright
//!    Node driver). Needs Node.js on `PATH` and a one-time Chromium install
//!    (`npx playwright@<version> install chromium`, or the crate's
//!    `install_browsers` helper which uninews may invoke once on first
//!    "browser not installed" error).
//!
//! Toggle Playwright with `UNINEWS_PLAYWRIGHT=0` (enabled by default).
//! Optional timeout: `UNINEWS_PLAYWRIGHT_TIMEOUT_MS` (default 45000).
//!
//! # Security note
//!
//! `UNINEWS_CHROME_BINARY` is trusted input: it names the executable that
//! gets spawned. Only set it to a browser binary you trust. The target URL
//! is passed as a plain process argument (no shell), so it cannot be used
//! for command injection.
//!
//! ## Playwright auto-install
//!
//! On the first "browser not installed" error, uninews runs
//! `install_browsers(chromium)` once per process. Be aware of three facts:
//!
//! 1. It **downloads and executes hundreds of MB** of browser binaries at
//!    runtime.
//! 2. On Linux, playwright-rs 0.15 silently appends `--with-deps`, which
//!    also installs **system packages** (via the OS package manager).
//! 3. The download has no built-in timeout, so uninews bounds the whole
//!    install at [`PLAYWRIGHT_AUTOINSTALL_TIMEOUT`].
//!
//! Set `UNINEWS_PLAYWRIGHT_AUTOINSTALL=0` (or any falsy flag) to opt out
//! and install manually with `npx playwright@<version> install chromium`.
//! Auto-install is enabled by default to preserve existing behavior.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use playwright_rs::{
    install_browsers, Browser, BrowserContextOptions, GotoOptions, LaunchOptions, Playwright,
    Viewport, WaitForOptions, WaitForState, WaitUntil, PLAYWRIGHT_VERSION,
};
use tokio::sync::OnceCell;

use crate::util::{first_non_empty_env_var, summarize_body, BROWSER_USER_AGENT};

/// Environment variable that toggles the Playwright bot-protection fallback.
///
/// Enabled by default; set to `0`, `false`, `no`, or `off` (any case) to
/// disable. Exposed publicly so operators and tests can reason about it.
///
/// # Examples
///
/// ```
/// use uninews::UNINEWS_PLAYWRIGHT_ENV;
/// assert_eq!(UNINEWS_PLAYWRIGHT_ENV, "UNINEWS_PLAYWRIGHT");
/// ```
pub const UNINEWS_PLAYWRIGHT_ENV: &str = "UNINEWS_PLAYWRIGHT";

/// Optional override for the Playwright navigation / content wait budget
/// in milliseconds (default [`DEFAULT_PLAYWRIGHT_TIMEOUT_MS`]).
///
/// Invalid or non-positive values fall back to the default and log a warning
/// to stderr.
pub const UNINEWS_PLAYWRIGHT_TIMEOUT_MS_ENV: &str = "UNINEWS_PLAYWRIGHT_TIMEOUT_MS";

/// Default Playwright navigation + content-wait budget (45 s).
pub const DEFAULT_PLAYWRIGHT_TIMEOUT_MS: u64 = 45_000;

/// Environment variable that toggles the one-shot Playwright Chromium
/// auto-install (`install_browsers`).
///
/// Enabled by default; set to `0`, `false`, `no`, or `off` (any case) to
/// disable. See the module-level *Security note* for why you might want to.
///
/// # Examples
///
/// ```
/// use uninews::UNINEWS_PLAYWRIGHT_AUTOINSTALL_ENV;
/// assert_eq!(UNINEWS_PLAYWRIGHT_AUTOINSTALL_ENV, "UNINEWS_PLAYWRIGHT_AUTOINSTALL");
/// ```
pub const UNINEWS_PLAYWRIGHT_AUTOINSTALL_ENV: &str = "UNINEWS_PLAYWRIGHT_AUTOINSTALL";

/// Grace added on top of the configured per-step Playwright timeout to form
/// the overall wall-clock budget of one render attempt (15 s). Covers the
/// un-bounded steps: driver spawn + handshake, context/page creation,
/// `page.content()`, and best-effort context cleanup.
///
/// Exposed as `#[doc(hidden)]` so tests can pin the budget arithmetic.
#[doc(hidden)]
pub const PLAYWRIGHT_OVERALL_GRACE_MS: u64 = 15_000;

/// Hard real-time deadline for one headless-Chrome `--dump-dom` run (60 s).
///
/// `--virtual-time-budget` only fast-forwards page timers; a slowloris-style
/// server keeps Chrome's fetch pending in *real* time, so the child process
/// must be killed by a watchdog instead of trusted to exit on its own.
///
/// Exposed as `#[doc(hidden)]` so tests can pin the watchdog deadline.
#[doc(hidden)]
pub const CHROME_DUMP_DOM_DEADLINE_MS: u64 = 60_000;

/// Upper bound for the one-shot `install_browsers(chromium)` download
/// (10 min). playwright-rs gives the download no timeout of its own.
const PLAYWRIGHT_AUTOINSTALL_TIMEOUT: Duration = Duration::from_secs(600);

/// Returns `true` when `value` is a falsy env-flag after trim + lowercasing.
///
/// Recognized falsy tokens: `0`, `false`, `no`, `off`. Used by
/// [`playwright_enabled`] (and mirrored by the archive.org toggle).
///
/// Exposed as `#[doc(hidden)]` so integration tests can pin the contract
/// without depending on process-wide env mutation for every assertion.
#[doc(hidden)]
pub fn is_falsy_env_flag(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

/// Whether the Playwright bot-protection fallback is enabled.
///
/// Enabled by default (env unset). Disabled when [`UNINEWS_PLAYWRIGHT_ENV`]
/// is a falsy flag (see [`is_falsy_env_flag`]).
///
/// # Examples
///
/// ```
/// use uninews::playwright_enabled;
/// // Unset or non-falsy values keep the fallback on.
/// let _ = playwright_enabled();
/// ```
pub fn playwright_enabled() -> bool {
    match env::var(UNINEWS_PLAYWRIGHT_ENV) {
        Ok(value) => !is_falsy_env_flag(&value),
        Err(_) => true,
    }
}

/// Whether the one-shot Playwright Chromium auto-install is enabled.
///
/// Enabled by default (env unset). Disabled when
/// [`UNINEWS_PLAYWRIGHT_AUTOINSTALL_ENV`] is a falsy flag (see
/// [`is_falsy_env_flag`]). Parsed exactly like [`playwright_enabled`].
///
/// # Examples
///
/// ```
/// use uninews::playwright_autoinstall_enabled;
/// // Unset or non-falsy values keep auto-install on.
/// let _ = playwright_autoinstall_enabled();
/// ```
pub fn playwright_autoinstall_enabled() -> bool {
    match env::var(UNINEWS_PLAYWRIGHT_AUTOINSTALL_ENV) {
        Ok(value) => !is_falsy_env_flag(&value),
        Err(_) => true,
    }
}

/// Parse a timeout-ms env value into a positive millisecond count.
///
/// Returns [`DEFAULT_PLAYWRIGHT_TIMEOUT_MS`] when `raw` is missing, empty,
/// zero, or unparseable. `None` means "use the env var / default path".
///
/// Exposed as `#[doc(hidden)]` for hermetic unit tests of the parser.
#[doc(hidden)]
pub fn parse_playwright_timeout_ms(raw: Option<&str>) -> u64 {
    match raw {
        Some(raw) => match raw.trim().parse::<u64>() {
            Ok(ms) if ms > 0 => ms,
            _ => DEFAULT_PLAYWRIGHT_TIMEOUT_MS,
        },
        None => DEFAULT_PLAYWRIGHT_TIMEOUT_MS,
    }
}

/// Playwright navigation / content-wait timeout (from env or default).
fn playwright_timeout() -> Duration {
    let raw = env::var(UNINEWS_PLAYWRIGHT_TIMEOUT_MS_ENV).ok();
    if let Some(raw) = raw.as_deref() {
        if !matches!(raw.trim().parse::<u64>(), Ok(ms) if ms > 0) {
            eprintln!(
                "uninews: invalid {}={:?}; using default {} ms",
                UNINEWS_PLAYWRIGHT_TIMEOUT_MS_ENV, raw, DEFAULT_PLAYWRIGHT_TIMEOUT_MS
            );
        }
    }
    Duration::from_millis(parse_playwright_timeout_ms(raw.as_deref()))
}

/// Overall wall-clock budget for one Playwright render attempt, in
/// milliseconds: the configured per-step timeout plus
/// [`PLAYWRIGHT_OVERALL_GRACE_MS`].
///
/// The per-step `UNINEWS_PLAYWRIGHT_TIMEOUT_MS` only reaches
/// launch/goto/locator timeouts; `Playwright::launch()`, context/page
/// creation, `page.content()`, `wait_for_load_state`, and context cleanup
/// are un-bounded in playwright-rs 0.15. Wrapping the whole attempt in this
/// budget guarantees no scrape can hang forever on a wedged driver.
///
/// Exposed as `#[doc(hidden)]` so tests can pin the budget arithmetic.
#[doc(hidden)]
pub fn playwright_overall_budget_ms() -> u64 {
    playwright_timeout().as_millis() as u64 + PLAYWRIGHT_OVERALL_GRACE_MS
}

/// Process-wide shared Playwright driver + Chromium browser (one per tokio
/// runtime).
///
/// Launching the Node driver and a Chromium instance costs seconds; paying
/// that per scrape puts process spawn on the hot path. We therefore launch
/// once and cache the pair, creating only a fresh browser context + page
/// per scrape (contexts are cheap and keep cookies/storage isolated
/// between scrapes).
///
/// Lifecycle: the shared browser is intentionally NOT closed per scrape —
/// it lives until process exit (the `Playwright` handle is kept alive in
/// the cache because its `Drop` kills the Node driver process). Only the
/// per-scrape context is closed. A failed launch is NOT cached
/// (`get_or_try_init` leaves the cell empty), so the next scrape retries
/// and surfaces the same error. If the cached browser's connection died
/// (crash/OOM kill), the next scrape performs one fresh, uncached launch —
/// see [`render_dom_attempt`].
struct SharedBrowser {
    /// Kept so the Node driver process is not killed when the last
    /// `Playwright` clone drops.
    _playwright: Playwright,
    /// Shared headless Chromium; per-scrape contexts are created from it.
    browser: Browser,
}

/// Per-runtime shared-browser cells.
///
/// playwright-rs 0.15 binds a browser's protocol channels to the tokio
/// runtime that launched it: using it from another runtime silently
/// deadlocks in release (and panics in debug). Each `#[tokio::test]` gets
/// its own runtime, and library users may too, so the cache is keyed by
/// [`tokio::runtime::Id`]. Entries are never evicted; a dead runtime's
/// browser simply lingers until process exit (test-only artifact).
static SHARED_BROWSERS: LazyLock<Mutex<HashMap<tokio::runtime::Id, Arc<OnceCell<SharedBrowser>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Return the shared-browser cell for the *current* tokio runtime, creating
/// it on first use. The std mutex guard never crosses an `.await`.
fn shared_browser_cell() -> Arc<OnceCell<SharedBrowser>> {
    let runtime_id = tokio::runtime::Handle::current().id();
    let mut caches = SHARED_BROWSERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(
        caches
            .entry(runtime_id)
            .or_insert_with(|| Arc::new(OnceCell::const_new())),
    )
}

/// One-shot `install_browsers(chromium)` coordination cell.
///
/// Caches the install outcome process-wide: concurrent scrapes that hit
/// "browser not installed" all await the same install future (instead of
/// returning the original error while the winner's install is still in
/// flight), and later scrapes reuse the cached success or failure.
static PLAYWRIGHT_BROWSER_INSTALL: OnceCell<Result<(), String>> = OnceCell::const_new();

/// Resolve the Chrome/Chromium binary to use for headless rendering.
///
/// Precedence: `UNINEWS_CHROME_BINARY`, then well-known macOS install
/// locations, then `google-chrome` from `$PATH`.
fn chrome_binary() -> String {
    if let Some(binary) = first_non_empty_env_var(&["UNINEWS_CHROME_BINARY"]) {
        return binary;
    }

    for candidate in [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ] {
        if Path::new(candidate).exists() {
            return candidate.to_string();
        }
    }

    "google-chrome".to_string()
}

/// Chrome profile entries that must not be copied into the staged profile
/// clone (singleton locks, crash handler state).
fn should_skip_chrome_profile_entry(name: &str) -> bool {
    matches!(
        name,
        "SingletonCookie" | "SingletonLock" | "SingletonSocket" | "Crashpad"
    )
}

/// Recursively copy `source` into `destination`, skipping volatile Chrome
/// profile entries.
fn copy_dir_recursively(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;

    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let entry_name = entry.file_name();
        let entry_name = entry_name.to_string_lossy();

        if should_skip_chrome_profile_entry(&entry_name) {
            continue;
        }

        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());

        if file_type.is_dir() {
            copy_dir_recursively(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&source_path, &destination_path)?;
        }
    }

    Ok(())
}

/// Clone a Chrome profile into a fresh temporary user-data dir so headless
/// Chrome can run with the user's cookies without touching the live profile.
///
/// Returns the temporary root and the profile directory name to pass via
/// `--profile-directory`.
fn clone_chrome_profile(
    source_user_data_dir: &Path,
    profile_name: &str,
) -> Result<(PathBuf, String), String> {
    let profile_source = source_user_data_dir.join(profile_name);
    if !profile_source.is_dir() {
        return Err(format!(
            "Chrome profile directory not found: {}",
            profile_source.display()
        ));
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let temp_root = env::temp_dir().join(format!(
        "uninews-chrome-profile-{}-{}",
        std::process::id(),
        nonce
    ));
    fs::create_dir_all(&temp_root).map_err(|err| {
        format!(
            "Failed to create temporary Chrome profile directory {}: {}",
            temp_root.display(),
            err
        )
    })?;

    // Stage the profile; on any mid-copy error remove the partial temp dir
    // so a failed clone never leaks files.
    if let Err(err) = stage_chrome_profile(source_user_data_dir, profile_name, &temp_root) {
        let _ = fs::remove_dir_all(&temp_root);
        return Err(err);
    }

    Ok((temp_root, profile_name.to_string()))
}

/// Copy the root hint files and the profile tree into `temp_root`.
fn stage_chrome_profile(
    source_user_data_dir: &Path,
    profile_name: &str,
    temp_root: &Path,
) -> Result<(), String> {
    let profile_source = source_user_data_dir.join(profile_name);

    for root_file in ["Local State", "First Run"] {
        let source_file = source_user_data_dir.join(root_file);
        if source_file.is_file() {
            let destination_file = temp_root.join(root_file);
            fs::copy(&source_file, &destination_file).map_err(|err| {
                format!(
                    "Failed to copy {} into temporary Chrome profile: {}",
                    source_file.display(),
                    err
                )
            })?;
        }
    }

    let staged_profile = temp_root.join(profile_name);
    copy_dir_recursively(&profile_source, &staged_profile).map_err(|err| {
        format!(
            "Failed to clone Chrome profile {} into {}: {}",
            profile_source.display(),
            staged_profile.display(),
            err
        )
    })
}

/// Spawn `command` and wait for it to exit, killing the child when
/// `deadline` of *real* time elapses.
///
/// `--virtual-time-budget` only fast-forwards page timers; a slowloris-style
/// server keeps Chrome's fetch pending in real time and plain
/// `Command::output()` would block forever. Stdout/stderr are drained on
/// reader threads so a chatty child cannot deadlock on a full pipe buffer
/// while we poll `try_wait`.
fn run_command_with_deadline(command: &mut Command, deadline: Duration) -> io::Result<Output> {
    let mut child = command.spawn()?;

    let mut stdout_pipe = child.stdout.take();
    let stdout_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(mut pipe) = stdout_pipe.take() {
            let _ = pipe.read_to_end(&mut buffer);
        }
        buffer
    });
    let mut stderr_pipe = child.stderr.take();
    let stderr_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(mut pipe) = stderr_pipe.take() {
            let _ = pipe.read_to_end(&mut buffer);
        }
        buffer
    });

    let started = Instant::now();
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None if started.elapsed() >= deadline => {
                // Watchdog fired: kill the wedged child and reap it.
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();

    match status {
        Some(status) => Ok(Output {
            status,
            stdout,
            stderr,
        }),
        None => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "headless Chrome render timed out after {} s (real time); child process killed",
                deadline.as_secs()
            ),
        )),
    }
}

/// Render `url` in headless Chrome and return the final DOM as HTML.
///
/// Runs the blocking `Command` on Tokio's blocking thread pool under a hard
/// real-time deadline ([`CHROME_DUMP_DOM_DEADLINE_MS`]); the child is killed
/// when the deadline expires. Any staged Chrome profile clone is removed
/// before returning, regardless of outcome.
pub(crate) async fn fetch_rendered_dom_with_chrome(url: &str) -> Result<String, String> {
    let browser_binary = chrome_binary();
    let user_data_dir = first_non_empty_env_var(&["UNINEWS_CHROME_USER_DATA_DIR"]);
    let profile_dir = first_non_empty_env_var(&["UNINEWS_CHROME_PROFILE_DIR"]);
    let url = url.to_string();
    let browser_binary_for_error = browser_binary.clone();
    let url_for_error = url.clone();

    let output = tokio::task::spawn_blocking(move || {
        let staged_profile = if let Some(user_data_dir) = user_data_dir.as_ref() {
            let profile_name = profile_dir.as_deref().unwrap_or("Default");
            Some(clone_chrome_profile(Path::new(user_data_dir), profile_name))
        } else {
            None
        };

        let (effective_user_data_dir, effective_profile_dir, staged_root) = match staged_profile {
            Some(Ok((temp_root, profile_name))) => {
                (Some(temp_root.clone()), Some(profile_name), Some(temp_root))
            }
            Some(Err(err)) => return Err(io::Error::other(err)),
            None => (None, profile_dir, None),
        };

        let mut command = Command::new(&browser_binary);
        command
            .arg("--headless=new")
            .arg("--disable-gpu")
            .arg("--virtual-time-budget=15000")
            .arg("--dump-dom");

        if let Some(user_data_dir) = effective_user_data_dir.as_ref() {
            command.arg(format!("--user-data-dir={}", user_data_dir.display()));
        }

        if let Some(profile_dir) = effective_profile_dir.as_ref() {
            command.arg(format!("--profile-directory={}", profile_dir));
        }

        command
            .arg(&url)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let result = run_command_with_deadline(
            &mut command,
            Duration::from_millis(CHROME_DUMP_DOM_DEADLINE_MS),
        );

        if let Some(staged_root) = staged_root {
            let _ = fs::remove_dir_all(staged_root);
        }

        result
    })
    .await
    .map_err(|err| format!("Chrome browser fallback task failed: {}", err))?
    .map_err(|err| format!("Failed to launch Chrome browser fallback: {}", err))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            summarize_body(stdout.as_ref(), 400)
        } else {
            "unknown error".to_string()
        };

        return Err(format!(
            "failed to render {} with {}: {}",
            url_for_error, browser_binary_for_error, detail
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether a Playwright error looks like "browser not installed" so we can
/// attempt a one-shot `install_browsers`.
fn is_browser_not_installed_error(err: &playwright_rs::Error) -> bool {
    matches!(err, playwright_rs::Error::BrowserNotInstalled { .. })
        || looks_like_browser_not_installed_message(&err.to_string())
}

/// Pure message classifier for missing-browser errors (string form).
///
/// Exposed as `#[doc(hidden)]` so tests can pin the retry trigger without
/// launching Playwright.
#[doc(hidden)]
pub fn looks_like_browser_not_installed_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("not installed") || lower.contains("browser_not_installed")
}

/// Launch Chromium via Playwright, navigate to `url`, wait for real content
/// when possible, and return the final HTML DOM.
///
/// On the first "browser not installed" error this process will attempt
/// `install_browsers(Some(&["chromium"]))` once (unless disabled via
/// [`UNINEWS_PLAYWRIGHT_AUTOINSTALL_ENV`]), then retry exactly once.
/// Concurrent scrapes await the same in-flight install rather than failing
/// with the original error. Requires Node.js on `PATH`.
///
/// This is the same render the bot-protection fallback chain uses
/// internally, exposed publicly so host applications can reuse it for
/// their own rendering needs (e.g. serving a [`crate::ContentFallback`]
/// hook) without duplicating the launch/wait/challenge-settle logic.
///
/// # Errors
///
/// Returns a human-readable `String` when the Playwright server cannot
/// start, Chromium cannot launch/install, navigation fails, the overall
/// budget ([`playwright_overall_budget_ms`]) expires, or the DOM is empty.
/// Callers treat any `Err` as "continue to archive.org".
pub async fn fetch_rendered_dom_with_playwright(url: &str) -> Result<String, String> {
    match fetch_rendered_dom_with_playwright_once(url).await {
        Ok(html) => Ok(html),
        Err(err) if looks_like_browser_not_installed_message(&err) => {
            if !playwright_autoinstall_enabled() {
                return Err(format!(
                    "{err} (auto-install disabled via {UNINEWS_PLAYWRIGHT_AUTOINSTALL_ENV}; \
                     install manually with: npx playwright@{PLAYWRIGHT_VERSION} install chromium)"
                ));
            }
            // Awaits any in-flight install started by a concurrent scrape
            // and reuses the cached outcome, then retries exactly once.
            ensure_playwright_browser_installed().await?;
            fetch_rendered_dom_with_playwright_once(url).await
        }
        Err(err) => Err(err),
    }
}

/// Run the one-shot `install_browsers(chromium)`, bounded by
/// [`PLAYWRIGHT_AUTOINSTALL_TIMEOUT`]; the outcome is cached process-wide
/// so concurrent and later scrapes share it.
async fn ensure_playwright_browser_installed() -> Result<(), String> {
    PLAYWRIGHT_BROWSER_INSTALL
        .get_or_init(|| async {
            eprintln!(
                "uninews: Playwright Chromium missing; installing via install_browsers (Playwright {PLAYWRIGHT_VERSION})…"
            );
            // install_browsers downloads and executes hundreds of MB of
            // browser binaries (plus system packages on Linux via a silent
            // --with-deps) with no built-in timeout — bound it here.
            match tokio::time::timeout(
                PLAYWRIGHT_AUTOINSTALL_TIMEOUT,
                install_browsers(Some(&["chromium"])),
            )
            .await
            {
                Ok(Ok(())) => Ok(()),
                Ok(Err(install_err)) => Err(format!(
                    "Playwright Chromium is not installed and auto-install failed: {install_err}. \
                     Install manually with: npx playwright@{PLAYWRIGHT_VERSION} install chromium"
                )),
                Err(_) => Err(format!(
                    "Playwright Chromium auto-install timed out after {} s. \
                     Install manually with: npx playwright@{PLAYWRIGHT_VERSION} install chromium",
                    PLAYWRIGHT_AUTOINSTALL_TIMEOUT.as_secs()
                )),
            }
        })
        .await
        .clone()
}

/// One Playwright render attempt, wrapped in the overall wall-clock budget
/// ([`playwright_overall_budget_ms`]) so every internal await — driver
/// spawn, context/page creation, content reads, cleanup — is bounded and a
/// wedged driver cannot hang a scrape forever.
async fn fetch_rendered_dom_with_playwright_once(url: &str) -> Result<String, String> {
    let budget = Duration::from_millis(playwright_overall_budget_ms());
    match tokio::time::timeout(budget, render_dom_attempt(url)).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "Playwright render of {url} timed out: exceeded the overall budget of {} ms \
             ({} ms configured timeout + {} ms grace)",
            budget.as_millis(),
            playwright_timeout().as_millis(),
            PLAYWRIGHT_OVERALL_GRACE_MS
        )),
    }
}

/// Launch the Playwright Node driver plus a headless Chromium instance.
async fn launch_playwright_browser() -> Result<SharedBrowser, String> {
    let timeout_ms = playwright_timeout().as_millis() as f64;

    let playwright = Playwright::launch()
        .await
        .map_err(|err| format!("Failed to start Playwright server: {err}"))?;

    let launch_options = LaunchOptions::new()
        .headless(true)
        .timeout(timeout_ms)
        .args(vec![
            "--disable-blink-features=AutomationControlled".to_string(),
            // Security downgrade: disables Chromium's sandbox. Required in
            // containers without user-namespace support (default Docker
            // seccomp/userns profile), where the sandboxed child cannot
            // start at all. Only render URLs you would open yourself.
            "--no-sandbox".to_string(),
            "--disable-dev-shm-usage".to_string(),
        ]);

    let browser = playwright
        .chromium()
        .launch_with_options(launch_options)
        .await
        .map_err(|err| {
            if is_browser_not_installed_error(&err) {
                format!(
                    "Playwright Chromium is not installed ({err}). \
                     Install with: npx playwright@{PLAYWRIGHT_VERSION} install chromium"
                )
            } else {
                format!("Failed to launch Playwright Chromium: {err}")
            }
        })?;

    Ok(SharedBrowser {
        _playwright: playwright,
        browser,
    })
}

/// Navigate to `url` in a fresh context on the shared browser, wait out
/// challenges where possible, and return the final HTML DOM.
///
/// Only the per-scrape [`playwright_rs::BrowserContext`] is closed at the
/// end — never the shared browser, which lives until process exit. When the
/// cached browser's connection died, one fresh uncached browser is launched
/// for this attempt (and dropped with it).
async fn render_dom_attempt(url: &str) -> Result<String, String> {
    let timeout = playwright_timeout();
    let timeout_ms = timeout.as_millis() as f64;

    // `fresh` keeps a re-launched browser (and its driver) alive for the
    // whole attempt when the cached one died; dropping the `Playwright`
    // handle would kill the Node driver process mid-scrape.
    let fresh;
    let cell = shared_browser_cell();
    let browser = {
        // A failed launch is not cached: the next scrape retries and
        // surfaces the same error again.
        let shared = cell.get_or_try_init(launch_playwright_browser).await?;
        if shared.browser.is_connected() {
            shared.browser.clone()
        } else {
            fresh = launch_playwright_browser().await?;
            fresh.browser.clone()
        }
    };

    let context = browser
        .new_context_with_options(
            BrowserContextOptions::builder()
                .user_agent(BROWSER_USER_AGENT.to_string())
                .viewport(Viewport {
                    width: 1440,
                    height: 900,
                })
                .locale("en-US".to_string())
                .build(),
        )
        .await
        .map_err(|err| format!("Failed to create Playwright browser context: {err}"))?;

    let result = async {
        let page = context
            .new_page()
            .await
            .map_err(|err| format!("Failed to open Playwright page: {err}"))?;

        let goto_options = GotoOptions::new()
            .timeout(timeout)
            .wait_until(WaitUntil::DomContentLoaded);

        page.goto(url, Some(goto_options))
            .await
            .map_err(|err| format!("Playwright navigation failed for {url}: {err}"))?;

        // Give Cloudflare JS challenges a chance to complete and the article
        // body to appear. Timeouts here are non-fatal — we still dump the DOM.
        let wait_options = WaitForOptions::builder()
            .state(WaitForState::Visible)
            .timeout(timeout_ms)
            .build();
        let _ = page
            .locator("article, main, [data-testid='postBody'], .articleBody")
            .wait_for(Some(wait_options))
            .await;

        // If still on a challenge interstitial, wait briefly for the title to
        // change away from Cloudflare's "Just a moment…".
        let title = page.title().await.unwrap_or_default();
        if title.to_ascii_lowercase().contains("just a moment") {
            let settle = Duration::from_secs(8).min(timeout);
            tokio::time::sleep(settle).await;
            let _ = page.wait_for_load_state(Some(WaitUntil::NetworkIdle)).await;
        }

        let html = page
            .content()
            .await
            .map_err(|err| format!("Failed to read Playwright page content: {err}"))?;

        if html.trim().is_empty() {
            return Err("Playwright returned an empty DOM".to_string());
        }

        Ok(html)
    }
    .await;

    // Best-effort cleanup of the per-scrape context only; the SHARED browser
    // stays open for the next scrape. Ignore close errors once we have (or
    // failed to get) HTML.
    let _ = context.close().await;

    result
}
