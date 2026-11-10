use ai_json_filler_tools::WebMemory;
use serde_json::json;

fn sample_hits() -> Vec<serde_json::Value> {
    vec![
        json!({"title": "Alpha", "url": "https://a.example", "snippet": "first"}),
        json!({"title": "Beta", "url": "https://b.example", "snippet": "second"}),
        json!({"title": "NoUrl", "url": "", "snippet": "ignored"}),
    ]
}

#[test]
fn empty_memory() {
    let m = WebMemory::new();
    assert_eq!(m.len(), 0);
    assert!(m.is_empty());
    assert!(m.searches().is_empty());
    assert!(m.fetches().is_empty());
}

#[test]
fn ids_increment() {
    let m = WebMemory::new();
    m.record_search("tavily", "q1", 1, 5, &sample_hits());
    m.record_search("tavily", "q2", 1, 5, &sample_hits());
    let ids: Vec<u64> = m.searches().iter().map(|s| s.id).collect();
    assert_eq!(ids, vec![1, 2]);
}

#[test]
fn latest_search_matches_key() {
    let m = WebMemory::new();
    m.record_search("tavily", "rust", 1, 5, &sample_hits());
    let hit = m.latest_search("tavily", "rust", 1, 5).expect("should hit");
    assert_eq!(hit.query, "rust");
    // Results without a URL are dropped.
    assert_eq!(hit.results.len(), 2);
}

#[test]
fn latest_search_ignores_other_provider() {
    let m = WebMemory::new();
    m.record_search("tavily", "rust", 1, 5, &sample_hits());
    assert!(m.latest_search("searxng", "rust", 1, 5).is_none());
}

#[test]
fn latest_search_ignores_page_and_count() {
    let m = WebMemory::new();
    m.record_search("tavily", "rust", 1, 5, &sample_hits());
    assert!(m.latest_search("tavily", "rust", 2, 5).is_none());
    assert!(m.latest_search("tavily", "rust", 1, 10).is_none());
}

#[test]
fn latest_search_trims_query() {
    let m = WebMemory::new();
    m.record_search("tavily", "rust", 1, 5, &sample_hits());
    assert!(m.latest_search("tavily", " rust ", 1, 5).is_some());
}

#[test]
fn latest_search_prefers_most_recent() {
    let m = WebMemory::new();
    m.record_search(
        "tavily",
        "rust",
        1,
        5,
        &[json!({"title": "old", "url": "https://old", "snippet": "s"})],
    );
    m.record_search(
        "tavily",
        "rust",
        1,
        5,
        &[json!({"title": "new", "url": "https://new", "snippet": "s"})],
    );
    let hit = m.latest_search("tavily", "rust", 1, 5).unwrap();
    assert_eq!(hit.results[0].title, "new");
}

#[test]
fn latest_fetch_returns_most_recent() {
    let m = WebMemory::new();
    m.record_fetch("tavily", "https://x.example", "first");
    m.record_fetch("tavily", "https://x.example", "second");
    let hit = m.latest_fetch("https://x.example").unwrap();
    assert_eq!(hit.content, "second");
}

#[test]
fn latest_fetch_trims_url() {
    let m = WebMemory::new();
    m.record_fetch("tavily", "https://x.example", "c");
    assert!(m.latest_fetch(" https://x.example ").is_some());
}

#[test]
fn search_hits_scans_all_records() {
    let m = WebMemory::new();
    m.record_search(
        "tavily",
        "rust",
        1,
        5,
        &[json!({"title": "Rust 1", "url": "https://a", "snippet": "x"})],
    );
    m.record_search(
        "tavily",
        "tokio",
        1,
        5,
        &[json!({"title": "Tokio", "url": "https://b", "snippet": "y"})],
    );
    let hits = m.search_hits("tokio");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].title, "Tokio");
}

#[test]
fn search_hits_is_case_insensitive() {
    let m = WebMemory::new();
    m.record_search(
        "tavily",
        "RUST",
        1,
        5,
        &[json!({"title": "Rust", "url": "https://a", "snippet": "x"})],
    );
    assert_eq!(m.search_hits("rust").len(), 1);
}

#[test]
fn snapshot_is_a_copy() {
    let m = WebMemory::new();
    m.record_search("tavily", "rust", 1, 5, &sample_hits());
    let snap = m.snapshot();
    m.record_search("tavily", "rust", 1, 5, &sample_hits());
    assert_eq!(snap.searches.len(), 1);
}

#[test]
fn clear_returns_removed_and_empties() {
    let m = WebMemory::new();
    m.record_search("tavily", "rust", 1, 5, &sample_hits());
    m.record_fetch("tavily", "https://x", "c");
    assert_eq!(m.clear(), 2);
    assert!(m.is_empty());
    assert!(m.searches().is_empty());
}

#[test]
fn memory_is_shared_by_clone() {
    let m = WebMemory::new();
    let clone = m.clone();
    clone.record_search("tavily", "rust", 1, 5, &sample_hits());
    assert_eq!(m.search_count(), 1);
    assert_eq!(clone.search_count(), 1);
}
