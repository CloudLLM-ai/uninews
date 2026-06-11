use std::env;

use uninews::universal_scrape;

const DIARIOBITCOIN_X_ARTICLE_STATUS_URL: &str =
    "https://x.com/DiarioBitcoin/status/2034263054754726116";

fn has_x_credentials() -> bool {
    (env::var("X_API_KEY").is_ok() && env::var("X_API_SECRET").is_ok())
        || (env::var("DBTC_TWITTER_API_KEY").is_ok() && env::var("DBTC_TWITTER_API_SECRET").is_ok())
}

#[tokio::test]
#[ignore = "requires live X credentials, network access, and OPEN_AI_SECRET"]
async fn reads_linked_article_from_x_status() {
    if !has_x_credentials() {
        eprintln!("skipping live X article test: set X_API_KEY and X_API_SECRET");
        return;
    }

    let post = universal_scrape(DIARIOBITCOIN_X_ARTICLE_STATUS_URL, "english", None).await;

    assert!(
        post.error.is_empty(),
        "unexpected scrape error: {}",
        post.error
    );
    assert!(
        !post.title.trim().is_empty(),
        "expected a non-empty article title for {}",
        DIARIOBITCOIN_X_ARTICLE_STATUS_URL
    );
    assert!(
        !post
            .content
            .contains("I'm sorry, I can't transform this content into Markdown format"),
        "expected linked article content instead of link-only tweet markdown fallback"
    );
    assert!(
        !post.content.trim().is_empty(),
        "expected non-empty article content for {}",
        DIARIOBITCOIN_X_ARTICLE_STATUS_URL
    );
}
