//! Integration tests for the plain-web pipeline in `src/web.rs`, exercised
//! through the public `universal_scrape` entry point against loopback
//! servers (hermetic — Playwright and archive.org are disabled so the
//! plain-fetch behavior is isolated, and UNINEWS_LLM_CLIENT points at a
//! bogus provider so the LLM stage can never make a live call even if a
//! stage boundary shifts — this dev shell may export real LLM keys).

use std::env;
use std::io::Write;
use std::net::TcpListener;

use uninews::{universal_scrape, UNINEWS_ARCHIVE_FALLBACK_ENV, UNINEWS_PLAYWRIGHT_ENV};

/// RAII helper: temporarily override an env var, restore on drop.
struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = env::var(key).ok();
        unsafe {
            env::set_var(key, value);
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

/// Spawn a loopback HTTP server that answers one request with `status` and
/// a body of exactly `body_bytes` 'a' characters. Returns the server URL.
fn spawn_body_server(status: &str, body_bytes: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback server");
    let addr = listener.local_addr().expect("local addr");
    let status = status.to_string();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut request = [0u8; 1024];
        let _ = std::io::Read::read(&mut stream, &mut request);
        let header = format!(
            "HTTP/1.1 {}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            status, body_bytes
        );
        stream.write_all(header.as_bytes()).expect("write header");
        let chunk = [b'a'; 65536];
        let mut remaining = body_bytes;
        while remaining > 0 {
            let n = remaining.min(chunk.len());
            stream.write_all(&chunk[..n]).expect("write body chunk");
            remaining -= n;
        }
    });
    format!("http://{}", addr)
}

/// The bounded body reader (`read_body_bounded` in src/web.rs):
///
/// 1. A response body larger than the 16 MiB cap must fail the fetch with
///    a clear size-limit error (memory-exhaustion protection), not an OOM
///    abort. The failure classifies as a network failure so the archive.org
///    fallback would engage — disabled here to keep the test hermetic.
/// 2. A body just under the cap is accepted (the size check is `>`, not
///    `>=`); served with a 500 so the pipeline stops before the LLM stage.
///
/// One test for both scenarios: they mutate the same process-wide env vars,
/// so running them sequentially under one set of guards avoids holding a
/// std::Mutex across `.await` (clippy::await_holding_lock).
#[tokio::test]
async fn bounded_body_reader_caps_oversize_and_accepts_under_cap() {
    let _pw = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_ENV, "0");
    let _archive = EnvVarGuard::set(UNINEWS_ARCHIVE_FALLBACK_ENV, "0");
    let _llm = EnvVarGuard::set("UNINEWS_LLM_CLIENT", "definitely-not-a-provider");

    // Scenario 1: over the cap -> size-limit error.
    let url = spawn_body_server("200 OK", 17 * 1024 * 1024);
    let post = universal_scrape(&url, "english", None).await;
    assert!(
        post.error.contains("16 MiB limit"),
        "expected the size-limit error, got: {}",
        post.error
    );

    // Scenario 2: under the cap -> any error EXCEPT the size-limit one.
    let url = spawn_body_server("500 Internal Server Error", 1024);
    let post = universal_scrape(&url, "english", None).await;
    assert!(
        !post.error.is_empty(),
        "a 500 with unparseable body must surface an error"
    );
    assert!(
        !post.error.contains("MiB limit"),
        "a 1 KiB body must not trip the size cap, got: {}",
        post.error
    );
}
