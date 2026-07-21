//! X.com / Twitter support.
//!
//! This module implements scraping of tweets, threads, and X Articles:
//!
//! - **Tweets & threads** via the official X API v2 (OAuth 2.0 app-only
//!   bearer token from `X_API_KEY` / `X_API_SECRET`, with compatibility
//!   fallback for `DBTC_TWITTER_API_KEY` / `DBTC_TWITTER_API_SECRET`).
//! - **X Articles** embedded in the v2 tweet payload when available,
//!   otherwise via X's web GraphQL `TweetResultByRestId` endpoint using a
//!   guest token, with a final HTML/headless-Chrome fallback for guest-wall
//!   cases (handled by [`crate::web`]).
//! - **Link-only posts** are followed to the external article, which is then
//!   scraped like any other web URL.

use reqwest::Client;
use serde::Deserialize;

use crate::http::api_client;
use crate::llm::convert_content_to_markdown;
use crate::util::{first_non_empty_env_var, summarize_body};
use crate::web::scrape_web_url_with_title_override;
use crate::Post;

/// Public, well-known bearer token embedded in X's own web client. Used for
/// guest-token GraphQL requests; not a secret.
const X_WEB_BEARER_TOKEN: &str =
    "AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs%3D1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA";

/// Query ID of X's web GraphQL `TweetResultByRestId` operation.
const X_WEB_TWEET_RESULT_BY_REST_ID_QUERY_ID: &str = "zy39CwTyYhU-_0LP7dljjg";

/// A single tweet returned by the Twitter/X API v2.
#[derive(Deserialize, Debug)]
pub struct XTweet {
    pub(crate) id: String,
    pub(crate) text: String,
    pub(crate) created_at: Option<String>,
    pub(crate) author_id: Option<String>,
    pub(crate) conversation_id: Option<String>,
    pub(crate) article: Option<XArticleMeta>,
    pub(crate) entities: Option<XEntities>,
}

#[derive(Deserialize, Debug)]
pub struct XArticleMeta {
    pub(crate) title: Option<String>,
    pub(crate) plain_text: Option<String>,
    pub(crate) preview_text: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct XUrlEntity {
    pub(crate) url: Option<String>,
    pub(crate) expanded_url: Option<String>,
    pub(crate) unwound_url: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct XEntities {
    pub(crate) urls: Option<Vec<XUrlEntity>>,
}

/// Author information from the Twitter/X API v2 `includes.users` array.
#[derive(Deserialize, Debug)]
struct XUser {
    name: String,
    username: String,
    profile_image_url: Option<String>,
}

/// The `includes` block that accompanies expanded API responses.
#[derive(Deserialize, Debug)]
struct XIncludes {
    users: Option<Vec<XUser>>,
}

/// Top-level response for a single-tweet lookup (`GET /2/tweets/:id`).
#[derive(Deserialize, Debug)]
struct XTweetResponse {
    data: Option<XTweet>,
    includes: Option<XIncludes>,
    errors: Option<Vec<serde_json::Value>>,
}

/// Top-level response for a recent-search query (`GET /2/tweets/search/recent`).
#[derive(Deserialize, Debug)]
struct XSearchResponse {
    data: Option<Vec<XTweet>>,
}

/// Response returned by the X app-only token exchange endpoint.
#[derive(Deserialize, Debug)]
struct XBearerTokenResponse {
    token_type: String,
    access_token: String,
}

#[derive(Deserialize, Debug)]
struct XGuestActivateResponse {
    guest_token: String,
}

#[derive(Deserialize, Debug)]
struct XWebTweetResultResponse {
    data: Option<XWebTweetResultData>,
    errors: Option<Vec<serde_json::Value>>,
}

#[derive(Deserialize, Debug)]
struct XWebTweetResultData {
    #[serde(rename = "tweetResult")]
    tweet_result: Option<XWebTweetResultEnvelope>,
}

#[derive(Deserialize, Debug)]
struct XWebTweetResultEnvelope {
    result: Option<XWebTweetResult>,
}

#[derive(Deserialize, Debug)]
struct XWebTweetResult {
    article: Option<XWebArticleEnvelope>,
}

#[derive(Deserialize, Debug)]
struct XWebArticleEnvelope {
    #[serde(rename = "article_results")]
    article_results: Option<XWebArticleResults>,
}

#[derive(Deserialize, Debug)]
struct XWebArticleResults {
    result: Option<XWebArticle>,
}

#[derive(Deserialize, Debug)]
struct XWebArticle {
    title: Option<String>,
    plain_text: Option<String>,
    content_state: Option<XWebArticleContentState>,
    cover_media: Option<XWebArticleCoverMedia>,
}

#[derive(Deserialize, Debug)]
struct XWebArticleContentState {
    blocks: Option<Vec<XWebArticleBlock>>,
}

#[derive(Deserialize, Debug)]
struct XWebArticleBlock {
    text: Option<String>,
}

#[derive(Deserialize, Debug)]
struct XWebArticleCoverMedia {
    media_info: Option<XWebArticleMediaInfo>,
}

#[derive(Deserialize, Debug)]
struct XWebArticleMediaInfo {
    original_img_url: Option<String>,
}

/// Returns `true` when `url` belongs to X.com or Twitter.com.
///
/// # Examples
///
/// ```text
/// // These are X/Twitter URLs:
/// //   https://x.com/user/status/1234567890
/// //   https://twitter.com/user/status/1234567890
/// ```
pub fn is_x_url(url: &str) -> bool {
    url.starts_with("https://x.com/") || url.starts_with("https://twitter.com/")
}

/// Returns `true` for X Article URLs (`/i/article/<id>`).
pub fn is_x_article_url(url: &str) -> bool {
    url.contains("x.com/i/article/") || url.contains("twitter.com/i/article/")
}

/// Extracts the numeric tweet ID from an X.com or Twitter.com status URL.
///
/// Supports trailing query-strings and fragments:
/// - `https://x.com/user/status/1234567890` → `Some("1234567890")`
/// - `https://twitter.com/user/status/1234567890?s=20` → `Some("1234567890")`
///
/// Returns `None` if no numeric ID can be found after `/status/`.
pub fn extract_tweet_id(url: &str) -> Option<String> {
    // Strip query-string and fragment before searching for the ID.
    let clean = url.split('?').next().unwrap_or(url);
    let clean = clean.split('#').next().unwrap_or(clean);

    const STATUS: &str = "/status/";
    if let Some(pos) = clean.find(STATUS) {
        let after = &clean[pos + STATUS.len()..];
        let id: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !id.is_empty() {
            return Some(id);
        }
    }
    None
}

/// Pull a human-readable error message out of an X API / GraphQL error body.
pub(crate) fn x_api_error_message(body: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;

    if let Some(message) = parsed.get("error").and_then(|value| value.as_str()) {
        return Some(message.to_string());
    }

    if let Some(message) = parsed.get("detail").and_then(|value| value.as_str()) {
        return Some(message.to_string());
    }

    parsed
        .get("errors")
        .and_then(|value| value.as_array())
        .and_then(|errors| errors.first())
        .and_then(|error| {
            error
                .get("detail")
                .or_else(|| error.get("message"))
                .and_then(|value| value.as_str())
        })
        .map(ToString::to_string)
}

/// Whether verbose X debug dumps are enabled (`UNINEWS_DEBUG_X_JSON=1`).
fn x_debug_enabled() -> bool {
    matches!(
        std::env::var("UNINEWS_DEBUG_X_JSON").ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

/// Dump a labelled payload to stderr when X debug mode is enabled.
pub(crate) fn x_debug_dump(label: &str, body: &str) {
    if x_debug_enabled() {
        eprintln!("--- {} ---\n{}\n--- end {} ---", label, body, label);
    }
}

/// Dump a labelled HTTP response (status + headers + body) to stderr when X
/// debug mode is enabled.
pub(crate) fn x_debug_dump_http_response(
    label: &str,
    url: &str,
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: &str,
) {
    if !x_debug_enabled() {
        return;
    }

    eprintln!("--- {} ---", label);
    eprintln!("url: {}", url);
    eprintln!("status: {}", status);
    for (name, value) in headers {
        eprintln!(
            "header {}: {}",
            name.as_str(),
            value.to_str().unwrap_or("<non-utf8>")
        );
    }
    eprintln!();
    eprintln!("{}", body);
    eprintln!("--- end {} ---", label);
}

fn x_url_is_status_link(url: &str) -> bool {
    url.contains("/status/")
}

fn normalize_text_url_token(token: &str) -> Option<String> {
    let trimmed = token.trim_matches(|c: char| {
        matches!(
            c,
            '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | '.' | ';' | ':'
        )
    });

    if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        return Some(trimmed.to_string());
    }

    None
}

fn x_text_urls(tweet: &XTweet) -> Vec<String> {
    let mut urls = Vec::new();

    if let Some(entity_urls) = tweet
        .entities
        .as_ref()
        .and_then(|entities| entities.urls.as_ref())
    {
        for url in entity_urls {
            for candidate in [&url.url, &url.expanded_url, &url.unwound_url]
                .into_iter()
                .flatten()
            {
                let candidate = candidate.trim();
                if !candidate.is_empty() && !urls.iter().any(|url| url == candidate) {
                    urls.push(candidate.to_string());
                }
            }
        }
    }

    for token in tweet.text.split_whitespace() {
        if let Some(candidate) = normalize_text_url_token(token) {
            if !urls.iter().any(|url| url == &candidate) {
                urls.push(candidate);
            }
        }
    }

    urls
}

pub fn x_linked_article_url(tweet: &XTweet) -> Option<String> {
    x_text_urls(tweet).into_iter().find(|candidate| {
        !candidate.is_empty()
            && !candidate.starts_with("https://t.co/")
            && !candidate.starts_with("http://t.co/")
            && !x_url_is_status_link(candidate)
    })
}

async fn resolve_url_redirect(client: &Client, url: &str) -> Option<String> {
    let response = client.get(url).send().await.ok()?;
    let final_url = response.url().as_str().trim().to_string();

    if final_url.is_empty()
        || final_url.starts_with("https://t.co/")
        || final_url.starts_with("http://t.co/")
        || x_url_is_status_link(&final_url)
    {
        return None;
    }

    Some(final_url)
}

async fn resolve_x_linked_article_url(client: &Client, tweet: &XTweet) -> Option<String> {
    if let Some(article_url) = x_linked_article_url(tweet) {
        return Some(article_url);
    }

    for candidate in x_text_urls(tweet) {
        if let Some(resolved_url) = resolve_url_redirect(client, &candidate).await {
            return Some(resolved_url);
        }
    }

    None
}

fn x_text_without_urls(tweet: &XTweet) -> String {
    let mut text = tweet.text.clone();

    for candidate in x_text_urls(tweet) {
        text = text.replace(&candidate, " ");
    }

    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn x_post_is_link_only(tweet: &XTweet) -> bool {
    x_text_without_urls(tweet).trim().is_empty()
}

pub fn x_article_plain_text(article: &XArticleMeta) -> Option<String> {
    article
        .plain_text
        .as_deref()
        .or(article.preview_text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

/// Detect X's guest-wall boilerplate ("This page is not supported", …) in a
/// raw HTML body.
pub fn x_article_body_unavailable(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("this page is not supported")
        || lower.contains("please visit the author's profile")
        || lower.contains("javascript is not available")
}

fn x_web_article_body(article: &XWebArticle) -> Option<String> {
    if let Some(plain_text) = article.plain_text.as_ref() {
        let plain_text = plain_text.trim();
        if !plain_text.is_empty() {
            return Some(plain_text.to_string());
        }
    }

    let blocks = article.content_state.as_ref()?.blocks.as_ref()?;
    let block_text = blocks
        .iter()
        .filter_map(|block| block.text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n\n");

    if block_text.trim().is_empty() {
        None
    } else {
        Some(block_text)
    }
}

pub fn parse_x_web_article_post(
    body: &str,
    title_override: Option<&str>,
    publication_date: Option<String>,
    author: Option<String>,
) -> Result<Post, String> {
    let response: XWebTweetResultResponse = serde_json::from_str(body).map_err(|error| {
        format!(
            "Failed to parse X web GraphQL article response: {} ({})",
            error,
            summarize_body(body, 400)
        )
    })?;

    if let Some(errors) = response.errors.as_ref() {
        if !errors.is_empty() {
            let message = errors
                .first()
                .and_then(|error| error.get("message").or_else(|| error.get("detail")))
                .and_then(|value| value.as_str())
                .unwrap_or("Unknown X web GraphQL error");
            return Err(format!("X web GraphQL error: {}", message));
        }
    }

    let article = response
        .data
        .and_then(|data| data.tweet_result)
        .and_then(|tweet_result| tweet_result.result)
        .and_then(|tweet_result| tweet_result.article)
        .and_then(|article| article.article_results)
        .and_then(|results| results.result)
        .ok_or_else(|| "X web GraphQL response did not include an article payload.".to_string())?;

    let content = x_web_article_body(&article)
        .ok_or_else(|| "X web GraphQL response did not include article body text.".to_string())?;

    let title = article
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            title_override
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "X article".to_string());

    let featured_image_url = article
        .cover_media
        .and_then(|media| media.media_info)
        .and_then(|media_info| media_info.original_img_url)
        .unwrap_or_default();

    Ok(Post {
        title,
        content,
        featured_image_url,
        publication_date,
        author,
        error: String::new(),
    })
}

async fn resolve_x_guest_token(client: &Client) -> Result<String, String> {
    let response = client
        .post("https://api.x.com/1.1/guest/activate.json")
        .header("Authorization", format!("Bearer {}", X_WEB_BEARER_TOKEN))
        .header("x-twitter-active-user", "yes")
        .header("x-twitter-client-language", "en")
        .send()
        .await
        .map_err(|error| format!("Failed to activate X guest token: {}", error))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("Failed to read X guest token response: {}", error))?;
    x_debug_dump("X guest token JSON", &body);

    if !status.is_success() {
        let message = x_api_error_message(&body).unwrap_or_else(|| summarize_body(&body, 400));
        return Err(format!(
            "Failed to activate X guest token (status {}): {}",
            status, message
        ));
    }

    let response: XGuestActivateResponse = serde_json::from_str(&body).map_err(|error| {
        format!(
            "Failed to parse X guest token response: {} ({})",
            error,
            summarize_body(&body, 400)
        )
    })?;

    Ok(response.guest_token)
}

async fn scrape_x_article_from_web_graphql(
    client: &Client,
    tweet_id: &str,
    title_override: Option<&str>,
    publication_date: Option<String>,
    author: Option<String>,
) -> Result<Post, String> {
    let guest_token = resolve_x_guest_token(client).await?;
    let endpoint = format!(
        "https://x.com/i/api/graphql/{}/TweetResultByRestId",
        X_WEB_TWEET_RESULT_BY_REST_ID_QUERY_ID
    );

    let variables = serde_json::json!({
        "tweetId": tweet_id,
        "withCommunity": false,
        "includePromotedContent": false,
        "withVoice": false
    })
    .to_string();
    let features = serde_json::json!({
        "withArticleRichContentState": true,
        "withArticlePlainText": true,
        "withArticleSummaryText": false,
        "withArticleVoiceOver": false
    })
    .to_string();
    let field_toggles = serde_json::json!({
        "withArticleRichContentState": true,
        "withArticlePlainText": true,
        "withArticleSummaryText": false,
        "withArticleVoiceOver": false
    })
    .to_string();

    let endpoint = reqwest::Url::parse_with_params(
        &endpoint,
        &[
            ("variables", variables),
            ("features", features),
            ("fieldToggles", field_toggles),
        ],
    )
    .map_err(|error| format!("Failed to build X web GraphQL URL: {}", error))?;

    let response = client
        .get(endpoint)
        .header("Authorization", format!("Bearer {}", X_WEB_BEARER_TOKEN))
        .header("x-guest-token", guest_token)
        .header("x-twitter-active-user", "yes")
        .header("x-twitter-client-language", "en")
        .send()
        .await
        .map_err(|error| format!("Failed to fetch X article via web GraphQL: {}", error))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("Failed to read X web GraphQL response body: {}", error))?;
    x_debug_dump("X web GraphQL JSON", &body);

    if !status.is_success() {
        let message = x_api_error_message(&body).unwrap_or_else(|| summarize_body(&body, 400));
        return Err(format!(
            "X web GraphQL returned HTTP {}: {}",
            status, message
        ));
    }

    parse_x_web_article_post(&body, title_override, publication_date, author)
}

async fn resolve_x_bearer_token(client: &Client) -> Result<String, String> {
    let api_key = first_non_empty_env_var(&["X_API_KEY", "DBTC_TWITTER_API_KEY"]);
    let api_secret = first_non_empty_env_var(&["X_API_SECRET", "DBTC_TWITTER_API_SECRET"]);

    let (api_key, api_secret) = match (api_key, api_secret) {
        (Some(api_key), Some(api_secret)) => (api_key, api_secret),
        _ => {
            return Err(
                "Please provide both X_API_KEY and X_API_SECRET (or DBTC_TWITTER_API_KEY and DBTC_TWITTER_API_SECRET).".into(),
            );
        }
    };

    let token_resp = client
        .post("https://api.x.com/oauth2/token")
        .basic_auth(api_key, Some(api_secret))
        .header(
            "Content-Type",
            "application/x-www-form-urlencoded;charset=UTF-8",
        )
        .body("grant_type=client_credentials")
        .send()
        .await
        .map_err(|e| format!("Failed to obtain X bearer token: {}", e))?;

    let status = token_resp.status();
    let body = token_resp
        .text()
        .await
        .map_err(|e| format!("Failed to read X bearer token response: {}", e))?;

    if !status.is_success() {
        let message = x_api_error_message(&body).unwrap_or(body);
        return Err(format!(
            "Failed to obtain X bearer token (status {}): {}",
            status, message
        ));
    }

    let token_data: XBearerTokenResponse = serde_json::from_str(&body)
        .map_err(|e| format!("Failed to parse X bearer token response: {} ({})", e, body))?;

    if !token_data.token_type.eq_ignore_ascii_case("bearer") {
        return Err(format!(
            "X token exchange returned unsupported token type: {}",
            token_data.token_type
        ));
    }

    Ok(token_data.access_token)
}

/// Fetches a tweet or X thread via the Twitter/X API v2 and returns a [`Post`].
///
/// # Authentication
///
/// Uses `X_API_KEY` and `X_API_SECRET` to exchange for an OAuth 2.0
/// app-only Bearer Token before calling the X API.
///
/// # Thread handling
///
/// When the fetched tweet is the root of a thread (or part of one), the
/// function also queries the recent-search endpoint to collect all tweets
/// in the same conversation that were posted by the same author, then
/// sorts them chronologically so the thread reads naturally.
///
/// > **Note:** The recent-search endpoint only covers the last 7 days.
/// > Tweets older than 7 days are still returned as a single-tweet post.
///
/// # Errors
///
/// All errors are non-fatal and are returned inside [`Post::error`].
pub(crate) async fn scrape_x_url(
    url: &str,
    language: &str,
    context_window_tokens: Option<usize>,
) -> Post {
    // ── 1. Extract the tweet ID from the URL ─────────────────────────────────
    let tweet_id = match extract_tweet_id(url) {
        Some(id) => id,
        None => {
            return Post {
                title: String::new(),
                content: String::new(),
                featured_image_url: String::new(),
                publication_date: None,
                author: None,
                error: format!("Could not extract a tweet ID from the URL: {}", url),
            };
        }
    };

    // ── 2. Use the shared API HTTP client ───────────────────────────────────
    let client = api_client();

    // ── 3. Resolve the Bearer Token ──────────────────────────────────────────
    let bearer_token = match resolve_x_bearer_token(client).await {
        Ok(token) => token,
        Err(error) => {
            return Post {
                title: String::new(),
                content: String::new(),
                featured_image_url: String::new(),
                publication_date: None,
                author: None,
                error,
            };
        }
    };

    let auth_header = format!("Bearer {}", bearer_token);

    // ── 4. Fetch the root tweet ───────────────────────────────────────────────
    let root_tweet_url = format!(
        "https://api.x.com/2/tweets/{}?tweet.fields=created_at,author_id,conversation_id,text,entities,article&expansions=author_id&user.fields=name,username,profile_image_url",
        tweet_id
    );
    let root_resp = match client
        .get(&root_tweet_url)
        .header("Authorization", &auth_header)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return Post {
                title: String::new(),
                content: String::new(),
                featured_image_url: String::new(),
                publication_date: None,
                author: None,
                error: format!("Failed to call X API: {}", e),
            };
        }
    };

    let root_status = root_resp.status();
    let root_body = match root_resp.text().await {
        Ok(body) => body,
        Err(e) => {
            return Post {
                title: String::new(),
                content: String::new(),
                featured_image_url: String::new(),
                publication_date: None,
                author: None,
                error: format!("Failed to read X API response body: {}", e),
            };
        }
    };
    x_debug_dump("X root tweet JSON", &root_body);

    if !root_status.is_success() {
        let message =
            x_api_error_message(&root_body).unwrap_or_else(|| summarize_body(&root_body, 400));
        return Post {
            title: String::new(),
            content: String::new(),
            featured_image_url: String::new(),
            publication_date: None,
            author: None,
            error: format!("X API returned HTTP {}: {}", root_status, message),
        };
    }

    let root_data: XTweetResponse = match serde_json::from_str(&root_body) {
        Ok(d) => d,
        Err(e) => {
            return Post {
                title: String::new(),
                content: String::new(),
                featured_image_url: String::new(),
                publication_date: None,
                author: None,
                error: format!(
                    "Failed to parse X API response: {} ({})",
                    e,
                    summarize_body(&root_body, 400)
                ),
            };
        }
    };

    // Surface API-level errors (e.g. tweet not found, bad credentials).
    if let Some(errors) = &root_data.errors {
        if !errors.is_empty() {
            let msg = errors
                .first()
                .and_then(|e| e.get("detail").or_else(|| e.get("message")))
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown X API error");
            return Post {
                title: String::new(),
                content: String::new(),
                featured_image_url: String::new(),
                publication_date: None,
                author: None,
                error: format!("X API error: {}", msg),
            };
        }
    }

    let root_tweet = match root_data.data {
        Some(t) => t,
        None => {
            return Post {
                title: String::new(),
                content: String::new(),
                featured_image_url: String::new(),
                publication_date: None,
                author: None,
                error: format!(
                    "X API returned no tweet data. Response body: {}",
                    summarize_body(&root_body, 400)
                ),
            };
        }
    };

    // Resolve the author's display name and profile image.
    let author_info = root_data
        .includes
        .as_ref()
        .and_then(|inc| inc.users.as_ref())
        .and_then(|users| users.first());

    let author_display = author_info.map(|u| format!("@{} ({})", u.username, u.name));
    let profile_image = author_info
        .and_then(|u| u.profile_image_url.clone())
        .unwrap_or_default();

    let author_id = root_tweet.author_id.clone().unwrap_or_default();
    let conversation_id = root_tweet
        .conversation_id
        .clone()
        .unwrap_or_else(|| root_tweet.id.clone());

    if x_post_is_link_only(&root_tweet) {
        let article_title_override = root_tweet
            .article
            .as_ref()
            .and_then(|article| article.title.as_deref());
        let embedded_article_body = root_tweet.article.as_ref().and_then(x_article_plain_text);

        if let Some(content) = embedded_article_body {
            let scraped_article_post = Post {
                title: article_title_override
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .unwrap_or("X article")
                    .to_string(),
                content,
                featured_image_url: profile_image.clone(),
                publication_date: root_tweet.created_at.clone(),
                author: author_display.clone(),
                error: String::new(),
            };

            return match convert_content_to_markdown(
                scraped_article_post.clone(),
                language,
                context_window_tokens,
            )
            .await
            {
                Ok(markdown_post) => markdown_post,
                Err(err) => Post {
                    error: err,
                    ..scraped_article_post
                },
            };
        }

        if let Some(article_url) = resolve_x_linked_article_url(client, &root_tweet).await {
            if is_x_article_url(&article_url) {
                match scrape_x_article_from_web_graphql(
                    client,
                    &root_tweet.id,
                    article_title_override,
                    root_tweet.created_at.clone(),
                    author_display.clone(),
                )
                .await
                {
                    Ok(scraped_article_post) => {
                        return match convert_content_to_markdown(
                            scraped_article_post.clone(),
                            language,
                            context_window_tokens,
                        )
                        .await
                        {
                            Ok(markdown_post) => markdown_post,
                            Err(err) => Post {
                                error: err,
                                ..scraped_article_post
                            },
                        };
                    }
                    Err(graphql_error) => {
                        let article_post = scrape_web_url_with_title_override(
                            &article_url,
                            language,
                            article_title_override,
                            context_window_tokens,
                        )
                        .await;
                        if article_post.error.is_empty() {
                            return article_post;
                        }

                        return Post {
                            title: article_post.title,
                            content: article_post.content,
                            featured_image_url: article_post.featured_image_url,
                            publication_date: article_post.publication_date,
                            author: article_post.author,
                            error: format!(
                                "Failed to scrape linked X article {} via X web GraphQL: {}. HTML fallback failed: {}",
                                article_url, graphql_error, article_post.error
                            ),
                        };
                    }
                }
            }

            let article_post = scrape_web_url_with_title_override(
                &article_url,
                language,
                article_title_override,
                context_window_tokens,
            )
            .await;
            if article_post.error.is_empty() {
                return article_post;
            }

            return Post {
                title: article_post.title,
                content: article_post.content,
                featured_image_url: article_post.featured_image_url,
                publication_date: article_post.publication_date,
                author: article_post.author,
                error: format!(
                    "Failed to scrape linked article {}: {}",
                    article_url, article_post.error
                ),
            };
        }
    }

    // ── 5. Collect the full thread ────────────────────────────────────────────
    // Seed the list with the root tweet.
    let mut thread_tweets: Vec<(String, String)> = vec![(
        root_tweet.created_at.clone().unwrap_or_default(),
        root_tweet.text.clone(),
    )];

    // Try to fetch the rest of the conversation from the recent-search endpoint.
    // This only covers the last 7 days; for older tweets we fall back to the
    // single tweet already captured above.
    let search_url = format!(
        "https://api.x.com/2/tweets/search/recent?query=conversation_id%3A{}&tweet.fields=created_at,author_id,text,entities&max_results=100",
        conversation_id
    );
    if let Ok(search_resp) = client
        .get(&search_url)
        .header("Authorization", &auth_header)
        .send()
        .await
    {
        if let Ok(search_body) = search_resp.text().await {
            x_debug_dump("X recent search JSON", &search_body);
            if let Ok(search_data) = serde_json::from_str::<XSearchResponse>(&search_body) {
                if let Some(tweets) = search_data.data {
                    for t in tweets {
                        // Only include tweets from the same author (i.e. the thread,
                        // not replies from other users). Guard against an empty
                        // author_id (which would match any tweet lacking the field).
                        let same_author = !author_id.is_empty()
                            && t.author_id.as_deref() == Some(author_id.as_str());
                        if same_author && t.id != root_tweet.id {
                            thread_tweets.push((t.created_at.unwrap_or_default(), t.text));
                        }
                    }
                }
            }
        }
    }

    // Sort chronologically so the thread reads oldest → newest.
    thread_tweets.sort_by(|a, b| a.0.cmp(&b.0));

    // ── 6. Assemble the Post ──────────────────────────────────────────────────
    let title = format!(
        "{}: {}",
        author_display.as_deref().unwrap_or("X post"),
        root_tweet.text.chars().take(80).collect::<String>()
    );

    let content = thread_tweets
        .iter()
        .map(|(ts, text)| {
            if ts.is_empty() {
                text.clone()
            } else {
                format!("[{}] {}", ts, text)
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let scraped_post = Post {
        title,
        content,
        featured_image_url: profile_image,
        publication_date: root_tweet.created_at,
        author: author_display,
        error: String::new(),
    };

    // ── 7. AI Markdown conversion & optional translation ──────────────────────
    match convert_content_to_markdown(scraped_post.clone(), language, context_window_tokens).await {
        Ok(markdown_post) => markdown_post,
        Err(err) => Post {
            error: err,
            ..scraped_post
        },
    }
}
