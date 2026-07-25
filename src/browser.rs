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

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use playwright_rs::{
    install_browsers, BrowserContextOptions, GotoOptions, LaunchOptions, Playwright, Viewport,
    WaitForOptions, WaitForState, WaitUntil, PLAYWRIGHT_VERSION,
};

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
    match env::var(UNINEWS_PLAYWRIGHT_TIMEOUT_MS_ENV) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(ms) if ms > 0 => Duration::from_millis(ms),
            _ => {
                eprintln!(
                    "uninews: invalid {}={:?}; using default {} ms",
                    UNINEWS_PLAYWRIGHT_TIMEOUT_MS_ENV, raw, DEFAULT_PLAYWRIGHT_TIMEOUT_MS
                );
                Duration::from_millis(DEFAULT_PLAYWRIGHT_TIMEOUT_MS)
            }
        },
        Err(_) => Duration::from_millis(DEFAULT_PLAYWRIGHT_TIMEOUT_MS),
    }
}

/// At most one automatic `install_browsers(chromium)` attempt per process.
static PLAYWRIGHT_BROWSER_INSTALL_ATTEMPTED: AtomicBool = AtomicBool::new(false);

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
    })?;

    Ok((temp_root, profile_name.to_string()))
}

/// Render `url` in headless Chrome and return the final DOM as HTML.
///
/// Runs the blocking `Command` on Tokio's blocking thread pool. Any staged
/// Chrome profile clone is removed before returning, regardless of outcome.
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

        command.arg(&url);
        let result = command.output();

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
/// `install_browsers(Some(&["chromium"]))` once, then retry. Requires
/// Node.js on `PATH`.
///
/// # Errors
///
/// Returns a human-readable `String` when the Playwright server cannot
/// start, Chromium cannot launch/install, navigation fails, or the DOM is
/// empty. Callers treat any `Err` as "continue to archive.org".
pub(crate) async fn fetch_rendered_dom_with_playwright(url: &str) -> Result<String, String> {
    match fetch_rendered_dom_with_playwright_once(url).await {
        Ok(html) => Ok(html),
        Err(err) if looks_like_browser_not_installed_message(&err) => {
            if PLAYWRIGHT_BROWSER_INSTALL_ATTEMPTED
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                eprintln!(
                    "uninews: Playwright Chromium missing; installing via install_browsers (Playwright {PLAYWRIGHT_VERSION})…"
                );
                if let Err(install_err) = install_browsers(Some(&["chromium"])).await {
                    return Err(format!(
                        "Playwright Chromium is not installed and auto-install failed: {install_err}. \
                         Install manually with: npx playwright@{PLAYWRIGHT_VERSION} install chromium"
                    ));
                }
                fetch_rendered_dom_with_playwright_once(url).await
            } else {
                Err(err)
            }
        }
        Err(err) => Err(err),
    }
}

async fn fetch_rendered_dom_with_playwright_once(url: &str) -> Result<String, String> {
    let timeout = playwright_timeout();
    let timeout_ms = timeout.as_millis() as f64;

    let playwright = Playwright::launch()
        .await
        .map_err(|err| format!("Failed to start Playwright server: {err}"))?;

    let launch_options = LaunchOptions::new()
        .headless(true)
        .timeout(timeout_ms)
        .args(vec![
            "--disable-blink-features=AutomationControlled".to_string(),
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

    let result = async {
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

    // Best-effort cleanup; ignore close errors once we have (or failed to get) HTML.
    let _ = browser.close().await;

    result
}
