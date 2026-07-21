//! Shared, process-wide `reqwest` clients.
//!
//! Building a `reqwest::Client` allocates a connection pool, TLS state, and
//! a default header map; doing it per request wastes all of that and defeats
//! keep-alive. Uninews therefore lazily builds two process-wide clients and
//! hands out `&'static` references:
//!
//! - [`web_client`] — for fetching article HTML from news sites. Forces
//!   HTTP/1.1 (HTTP/2 caused stream errors against some CDNs in the past)
//!   and sends a browser User-Agent to avoid trivial bot-walls.
//! - [`api_client`] — for JSON API calls (X API v2, X web GraphQL,
//!   archive.org). HTTP/2 allowed.
//!
//! Both clients apply conservative connect/read timeouts so a hung or
//! trickling server cannot block a scrape forever (availability / DoS
//! hardening). A hung headless-Chrome fallback is bounded separately by
//! Chrome's own `--virtual-time-budget`.

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::Client;

use crate::util::BROWSER_USER_AGENT;

/// Maximum time to wait for the TCP+TLS handshake to complete.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum total time for a single request, including the response body.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Process-wide client for article HTML fetches (HTTP/1.1 + browser UA).
pub(crate) fn web_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(BROWSER_USER_AGENT)
            .http1_only()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("static reqwest web client configuration must be valid")
    })
}

/// Process-wide client for JSON API calls (X API, GraphQL, archive.org).
pub(crate) fn api_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(BROWSER_USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("static reqwest API client configuration must be valid")
    })
}
