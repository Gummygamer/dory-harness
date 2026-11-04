//! One real search per service. Ignored by default: each run spends a credit (Tavily) or a request
//! (Brave). Run with the key set in the environment:
//!
//!     TAVILY_API_KEY=... cargo test -p dory-agent --test web_live -- --ignored --nocapture

use std::sync::atomic::AtomicBool;

use dory_agent::web::{Brave, SearchProvider, Tavily};

#[test]
#[ignore = "spends one Tavily credit; needs TAVILY_API_KEY"]
fn tavily_live_search_returns_hits() {
    let key = std::env::var("TAVILY_API_KEY").expect("TAVILY_API_KEY is not set");
    let provider = Tavily::new(&key, "https://api.tavily.com");
    let hits = provider
        .search(
            "serde_json preserve_order feature release",
            3,
            &AtomicBool::new(false),
        )
        .expect("the live Tavily search failed");
    assert!(!hits.is_empty(), "the live search returned no hits");
    for hit in &hits {
        println!(
            "tavily | {} | {} | {} bytes of excerpt",
            hit.title,
            hit.url,
            hit.content.len()
        );
    }
}

#[test]
#[ignore = "spends one Brave request; needs BRAVE_SEARCH_API_KEY"]
fn brave_live_search_returns_hits() {
    let key = std::env::var("BRAVE_SEARCH_API_KEY").expect("BRAVE_SEARCH_API_KEY is not set");
    let provider = Brave::new(&key, "https://api.search.brave.com");
    let hits = provider
        .search(
            "serde_json preserve_order feature release",
            3,
            &AtomicBool::new(false),
        )
        .expect("the live Brave search failed");
    assert!(!hits.is_empty(), "the live search returned no hits");
    for hit in &hits {
        println!(
            "brave | {} | {} | {} bytes of excerpt",
            hit.title,
            hit.url,
            hit.content.len()
        );
    }
}
