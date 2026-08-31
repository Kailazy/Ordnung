//! Live smoke test for the free-text record lookup. Ignored by default: it
//! needs a real `DISCOGS_TOKEN` and hits the network.
//!
//! Run with: `DISCOGS_TOKEN=… cargo test -p ordnung-core --test record_search_live -- --ignored --nocapture`
use ordnung_core::discogs::Client;

#[test]
#[ignore = "network + token"]
fn free_text_lookup_returns_usable_records() {
    let token = std::env::var("DISCOGS_TOKEN").expect("DISCOGS_TOKEN");
    let c = Client::new(token, "Ordnung/0.1 +https://kailazy.github.io/Ordnung/");
    let page = c
        .search_records("metro area", 1, 10)
        .expect("search should succeed");
    assert!(!page.hits.is_empty(), "expected hits for a well-known record");
    assert!(page.items > 0, "pagination should report a total");
    for h in page.hits.iter().take(5) {
        println!(
            "{} | {} - {} | {} · {} · {} {} · {}",
            h.release_id, h.artist, h.title, h.year, h.format, h.label, h.catno, h.country
        );
        assert!(h.release_id > 0, "every hit needs a concrete release id");
    }
    // The whole point of the split: an artist should come back separated from
    // the title rather than joined with " - ".
    assert!(
        page.hits.iter().any(|h| !h.artist.is_empty()),
        "at least one hit should have a parsed artist"
    );
}

/// An empty query must not spend a request.
#[test]
fn blank_query_short_circuits_without_network() {
    let c = Client::new("not-a-real-token", "Ordnung/0.1 +test");
    let page = c.search_records("   ", 1, 10).expect("blank is not an error");
    assert!(page.hits.is_empty());
    assert_eq!(page.items, 0);
}
