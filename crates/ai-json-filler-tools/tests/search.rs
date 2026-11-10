use ai_json_filler_core::Tool;
use ai_json_filler_tools::{SearchTool, WebMemory};
use serde_json::json;

#[tokio::test]
async fn spec_is_object_with_query() {
    let tool = SearchTool::tavily("key");
    let spec = tool.spec();
    assert_eq!(spec.name, "web_search_tavily");
    // required must contain query
    let required = spec.parameters["required"].as_array().unwrap();
    assert!(required.iter().any(|v| v == "query"));
}

#[tokio::test]
async fn page_gt_1_on_tavily_errors() {
    let tool = SearchTool::tavily("k");
    let out = tool.call(json!({ "query": "q", "page": 2 })).await.unwrap();
    assert!(out.is_error);
}

/// A base URL that refuses connections immediately, so "network was used" is a
/// deterministic, offline-friendly signal.
const DEAD_BASE_URL: &str = "http://127.0.0.1:1";

fn hit(title: &str, url: &str, snippet: &str) -> serde_json::Value {
    json!({ "title": title, "url": url, "snippet": snippet })
}

#[tokio::test]
async fn cache_hit_returns_without_http() {
    let memory = WebMemory::new();
    memory.record_search(
        "searxng",
        "q",
        1,
        5,
        &[hit("T", "https://example.com/a", "C")],
    );
    let tool = SearchTool::searxng(DEAD_BASE_URL).with_memory(memory);

    let out = tool.call(json!({ "query": "q" })).await.unwrap();
    assert!(!out.is_error, "{}", out.content);
    let value: serde_json::Value = serde_json::from_str(&out.content).unwrap();
    assert_eq!(value["cached"], true);
    assert_eq!(value["provider"], "searxng");
    assert_eq!(value["query"], "q");
    assert_eq!(value["results"][0]["url"], "https://example.com/a");
    assert_eq!(value["results"][0]["snippet"], "C");
}

#[tokio::test]
async fn refresh_bypasses_cache() {
    let memory = WebMemory::new();
    memory.record_search(
        "searxng",
        "q",
        1,
        5,
        &[hit("T", "https://example.com/a", "C")],
    );
    let tool = SearchTool::searxng(DEAD_BASE_URL).with_memory(memory);

    let out = tool
        .call(json!({ "query": "q", "refresh": true }))
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("SEARCH_ERROR"), "{}", out.content);
}

#[tokio::test]
async fn cache_miss_uses_network() {
    let tool = SearchTool::searxng(DEAD_BASE_URL);
    let out = tool.call(json!({ "query": "q" })).await.unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("SEARCH_ERROR"), "{}", out.content);
    assert_eq!(tool.memory().len(), 0);
}

#[test]
fn spec_advertises_refresh() {
    let spec = SearchTool::tavily("k").spec();
    assert_eq!(spec.parameters["properties"]["refresh"]["type"], "boolean");
    let required = spec.parameters["required"].as_array().unwrap();
    assert!(!required.iter().any(|v| v == "refresh"));
}
