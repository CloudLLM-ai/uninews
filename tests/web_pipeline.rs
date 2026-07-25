//! Integration tests for the plain-web pipeline in `src/web.rs`, exercised
//! through the public `universal_scrape` entry point against loopback
//! servers (hermetic — Playwright and archive.org are disabled so the
//! plain-fetch behavior is isolated).

use std::env;
use std::io::Write;
use std::net::TcpListener;
use std::sync::Mutex;

use uninews::{universal_scrape, UNINEWS_ARCHIVE_FALLBACK_ENV, UNINEWS_PLAYWRIGHT_ENV};

/// Serializes tests that mutate process-wide env vars.
static ENV_LOCK: Mutex<()> = Mutex::new(());

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

/// A response body larger than the 16 MiB cap must fail the fetch with a
/// clear size-limit error (memory-exhaustion protection), not an OOM abort.
/// The failure classifies as a network failure so the archive.org fallback
/// would engage — disabled here to keep the test hermetic.
#[tokio::test]
async fn oversized_response_body_fails_with_size_limit_error() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _pw = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_ENV, "0");
    let _archive = EnvVarGuard::set(UNINEWS_ARCHIVE_FALLBACK_ENV, "0");

    let url = spawn_body_server("200 OK", 17 * 1024 * 1024);
    let post = universal_scrape(&url, "english", None).await;

    assert!(
        post.error.contains("16 MiB limit"),
        "expected the size-limit error, got: {}",
        post.error
    );
}

/// A body just under the cap is accepted by the bounded reader (the size
/// check is `>`, not `>=`). Served with a 500 so the pipeline stops before
/// the LLM conversion stage — and UNINEWS_LLM_CLIENT is pointed at a bogus
/// provider anyway, so even if a stage boundary shifts the LLM stage fails
/// fast instead of making a live call (this shell may export real keys).
/// The assertion is only that the bounded reader did not reject the body.
#[tokio::test]
async fn body_under_the_cap_is_not_rejected_by_the_bounded_reader() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _pw = EnvVarGuard::set(UNINEWS_PLAYWRIGHT_ENV, "0");
    let _archive = EnvVarGuard::set(UNINEWS_ARCHIVE_FALLBACK_ENV, "0");
    let _llm = EnvVarGuard::set("UNINEWS_LLM_CLIENT", "definitely-not-a-provider");

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
