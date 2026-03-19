use std::env;

use uninews::universal_scrape;

const DIARIOBITCOIN_THREAD_URL: &str = "https://x.com/DiarioBitcoin/status/2034234106385661952";

fn has_x_credentials() -> bool {
    (env::var("X_API_KEY").is_ok() && env::var("X_API_SECRET").is_ok())
        || (env::var("DBTC_TWITTER_API_KEY").is_ok() && env::var("DBTC_TWITTER_API_SECRET").is_ok())
}

#[tokio::test]
#[ignore = "requires live X credentials and network access"]
async fn reads_live_diariobitcoin_x_thread() {
    if !has_x_credentials() {
        eprintln!("skipping live X thread test: set X_API_KEY and X_API_SECRET");
        return;
    }

    let post = universal_scrape(DIARIOBITCOIN_THREAD_URL, "english", None).await;

    assert!(
        post.error.is_empty() || post.error.contains("OPEN_AI_SECRET"),
        "unexpected scrape error: {}",
        post.error
    );
    assert!(
        !post.title.trim().is_empty(),
        "expected a non-empty title for {}",
        DIARIOBITCOIN_THREAD_URL
    );
    assert!(
        !post.content.trim().is_empty(),
        "expected non-empty content for {}",
        DIARIOBITCOIN_THREAD_URL
    );
    assert!(
        post.author
            .as_deref()
            .is_some_and(|author| !author.trim().is_empty()),
        "expected author metadata for {}",
        DIARIOBITCOIN_THREAD_URL
    );
    assert!(
        post.publication_date.is_some(),
        "expected publication date metadata for {}",
        DIARIOBITCOIN_THREAD_URL
    );
}
