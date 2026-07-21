//! Headless-Chrome rendering fallback.
//!
//! Some pages (notably X Articles behind a guest wall) only render their
//! content after JavaScript execution. When the plain HTTP fetch yields no
//! usable body, uninews can shell out to a local Chrome/Chromium binary in
//! headless mode (`--dump-dom`) and re-parse the rendered DOM.
//!
//! If `UNINEWS_CHROME_USER_DATA_DIR` (and optionally
//! `UNINEWS_CHROME_PROFILE_DIR`) point at a logged-in Chrome profile, the
//! profile is cloned into a temporary directory first so the real profile is
//! never mutated or locked by the headless run.
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
use std::time::{SystemTime, UNIX_EPOCH};

use crate::util::{first_non_empty_env_var, summarize_body};

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
