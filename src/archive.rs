//! archive.org Wayback Machine fallback.
//!
//! When a page cannot be scraped directly — because a bot-protection wall
//! (Cloudflare challenge, JavaScript-required interstitial, WAF 403/429)
//! stands in the way, or because the fetch failed hard (connection error,
//! timeout, 5xx) — uninews asks the Wayback Machine availability API for the
//! latest snapshot of the URL and scrapes the snapshot instead.
//!
//! The fallback is **enabled by default**; set `UNINEWS_ARCHIVE_FALLBACK=0`
//! (or `false`/`no`/`off`) to disable it. Every step is reported through
//! [`ScrapeEvent::ArchiveFallbackStarted`],
//! [`ScrapeEvent::ArchiveSnapshotFound`], and
//! [`ScrapeEvent::ArchiveSnapshotNotFound`].

use std::env;

use reqwest::header::HeaderMap;
use serde::Deserialize;

use crate::http::api_client;

/// Environment variable that toggles the archive.org fallback.
///
/// The fallback is on by default; set the variable to `0`, `false`, `no`,
/// or `off` (any case) to disable it.
pub const UNINEWS_ARCHIVE_FALLBACK_ENV: &str = "UNINEWS_ARCHIVE_FALLBACK";

/// Whether the archive.org Wayback Machine fallback is enabled.
///
/// Enabled by default; disabled when [`UNINEWS_ARCHIVE_FALLBACK_ENV`] is set
/// to `0`, `false`, `no`, or `off` (case-insensitive, surrounding whitespace
/// ignored). Any other value (including a set-but-empty one) keeps the
/// fallback enabled.
pub fn archive_fallback_enabled() -> bool {
    match env::var(UNINEWS_ARCHIVE_FALLBACK_ENV) {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

/// Only the first chunk of a response body is scanned for bot-protection
/// markers: challenge interstitials announce themselves in `<title>`/`<head>`
/// or the first visible elements, and lowercasing megabytes of HTML per
/// scrape would be wasted work.
const BOT_PROTECTION_SCAN_BYTES: usize = 64 * 1024;

/// Phrases that strongly indicate a bot-protection interstitial rather than
/// real article content. Deliberately distinctive to avoid false positives
/// on legitimate articles (e.g. we match `<title>just a moment`, the
/// Cloudflare challenge title, not the plain English phrase).
const BOT_PROTECTION_MARKERS: &[&str] = &[
    "<title>just a moment",
    "cf-browser-verification",
    "cf-chl",
    "_cf_chl",
    "cf-turnstile",
    "attention required! | cloudflare",
    "checking your browser",
    "ddos protection by cloudflare",
    "enable javascript and cookies to continue",
    "verify you are human",
    "ddos-guard",
];

/// Heuristically detect a bot-protection wall (Cloudflare and friends).
///
/// Returns `true` when:
///
/// - the body (first 64 KiB) contains a known challenge-page marker, or
/// - the status is 401/403/429 and the server identifies as Cloudflare
///   (`Server` header or a `cf-ray` header is present).
///
/// Exposed (as `pub` + `#[doc(hidden)]`) so integration tests can exercise
/// the heuristics directly.
#[doc(hidden)]
pub fn looks_like_bot_protection(status: u16, headers: &HeaderMap, body: &str) -> bool {
    let scan_end = body
        .char_indices()
        .take_while(|(index, _)| *index <= BOT_PROTECTION_SCAN_BYTES)
        .last()
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0);
    let haystack = body[..scan_end].to_ascii_lowercase();

    if BOT_PROTECTION_MARKERS
        .iter()
        .any(|marker| haystack.contains(marker))
    {
        return true;
    }

    if matches!(status, 401 | 403 | 429) {
        let server_is_cloudflare = headers
            .get("server")
            .and_then(|value| value.to_str().ok())
            .map(|server| server.to_ascii_lowercase().contains("cloudflare"))
            .unwrap_or(false);
        if server_is_cloudflare || headers.contains_key("cf-ray") {
            return true;
        }
    }

    false
}

/// A usable Wayback Machine snapshot of a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveSnapshot {
    /// Full snapshot URL, upgraded to HTTPS
    /// (`https://web.archive.org/web/<timestamp>/<original-url>`).
    pub url: String,
    /// Snapshot timestamp in `yyyyMMddhhmmss` form.
    pub timestamp: String,
}

#[derive(Debug, Deserialize)]
struct WaybackAvailabilityResponse {
    archived_snapshots: Option<WaybackSnapshots>,
}

#[derive(Debug, Deserialize)]
struct WaybackSnapshots {
    closest: Option<WaybackSnapshot>,
}

#[derive(Debug, Deserialize)]
struct WaybackSnapshot {
    available: Option<bool>,
    url: Option<String>,
    timestamp: Option<String>,
    status: Option<String>,
}

/// Parse the body of the Wayback availability API into an
/// [`ArchiveSnapshot`], if it describes a usable snapshot.
///
/// A snapshot is usable when it is marked `available`, its capture status is
/// `200`, and it carries both a URL and a timestamp. `http://` snapshot URLs
/// are upgraded to `https://`.
///
/// Exposed (as `pub` + `#[doc(hidden)]`) so integration tests can exercise
/// the parsing rules without network access.
#[doc(hidden)]
pub fn parse_availability_response(body: &str) -> Option<ArchiveSnapshot> {
    let response: WaybackAvailabilityResponse = serde_json::from_str(body).ok()?;
    let snapshot = response.archived_snapshots?.closest?;

    if snapshot.available != Some(true) || snapshot.status.as_deref() != Some("200") {
        return None;
    }

    let url = snapshot.url?;
    let timestamp = snapshot.timestamp?;
    if url.trim().is_empty() || timestamp.trim().is_empty() {
        return None;
    }

    // The API sometimes returns plain-http URLs; always upgrade to TLS.
    let url = url
        .strip_prefix("http://")
        .map(|rest| format!("https://{}", rest))
        .unwrap_or(url);

    Some(ArchiveSnapshot { url, timestamp })
}

/// Ask the Wayback Machine for the latest usable snapshot of `url`.
///
/// Returns `Ok(None)` when archive.org has no usable snapshot. The
/// availability endpoint returns the snapshot closest to the current time,
/// i.e. the most recent capture.
pub(crate) async fn latest_snapshot(url: &str) -> Result<Option<ArchiveSnapshot>, String> {
    let endpoint =
        reqwest::Url::parse_with_params("https://archive.org/wayback/available", &[("url", url)])
            .map_err(|error| format!("Failed to build archive.org availability URL: {}", error))?;

    let response = api_client()
        .get(endpoint)
        .send()
        .await
        .map_err(|error| format!("Failed to query archive.org availability API: {}", error))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("Failed to read archive.org response body: {}", error))?;

    if !status.is_success() {
        return Err(format!(
            "archive.org availability API returned HTTP {}",
            status
        ));
    }

    Ok(parse_availability_response(&body))
}
