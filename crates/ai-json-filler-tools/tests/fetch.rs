use ai_json_filler_core::Tool;
use ai_json_filler_tools::fetch::{slice_content, validate_url};
use ai_json_filler_tools::{FetchTool, WebMemory};
use serde_json::json;

#[test]
fn rejects_loopback() {
    assert!(validate_url("http://127.0.0.1/", false).is_some());
}

#[test]
fn rejects_private() {
    assert!(validate_url("http://10.0.0.1/", false).is_some());
    assert!(validate_url("http://192.168.1.1/", false).is_some());
    assert!(validate_url("http://100.64.0.1/", false).is_some());
}

#[test]
fn rejects_metadata_ip() {
    assert!(validate_url("http://169.254.169.254/", false).is_some());
    assert!(validate_url("http://[::ffff:169.254.169.254]/", false).is_some());
}

#[test]
fn rejects_localhost_with_dot() {
    assert!(validate_url("http://localhost.:8080/", false).is_some());
}

#[test]
fn allows_public() {
    assert!(validate_url("https://example.com/", false).is_none());
}

#[test]
fn slice_returns_next_offset() {
    let content = "0123456789";
    let (r1, t1, off1) = slice_content(content, 0, 4);
    assert_eq!(r1, "0123");
    assert!(t1);
    assert_eq!(off1, Some(4));
    let (r2, t2, off2) = slice_content(content, 4, 4);
    assert_eq!(r2, "4567");
    assert!(t2);
    assert_eq!(off2, Some(8));
    let (r3, t3, off3) = slice_content(content, 8, 4);
    assert_eq!(r3, "89");
    assert!(!t3);
    assert_eq!(off3, None);
}

/// A base URL that refuses connections immediately, so "network was used" is a
/// deterministic, offline-friendly signal.
const DEAD_BASE_URL: &str = "http://127.0.0.1:1";
const PAGE_URL: &str = "https://example.com/article";

fn cached_tool(body: &str) -> FetchTool {
    let memory = WebMemory::new();
    memory.record_fetch("crawl4ai", PAGE_URL, body);
    FetchTool::crawl4ai(DEAD_BASE_URL).with_memory(memory)
}

fn parse(out: &ai_json_filler_core::ToolOutput) -> serde_json::Value {
    assert!(!out.is_error, "{}", out.content);
    serde_json::from_str(&out.content).unwrap()
}

#[tokio::test]
async fn cache_hit_offset_zero() {
    let tool = cached_tool("0123456789");
    let out = tool.call(json!({ "url": PAGE_URL })).await.unwrap();
    let value = parse(&out);
    assert_eq!(value["cached"], true);
    assert_eq!(value["content"], "0123456789");
    assert_eq!(value["truncated"], false);
    assert_eq!(value["next_offset"], serde_json::Value::Null);
}

#[tokio::test]
async fn cache_hit_offset_continues() {
    let tool = cached_tool("0123456789");
    let out = tool
        .call(json!({ "url": PAGE_URL, "offset": 5 }))
        .await
        .unwrap();
    let value = parse(&out);
    assert_eq!(value["cached"], true);
    assert_eq!(value["content"], "56789");
    assert_eq!(value["truncated"], false);
}

#[tokio::test]
async fn cache_hit_respects_max_chars() {
    let tool = cached_tool("0123456789");
    let out = tool
        .call(json!({ "url": PAGE_URL, "max_chars": 2 }))
        .await
        .unwrap();
    let value = parse(&out);
    assert_eq!(value["cached"], true);
    assert_eq!(value["content"], "01");
    assert_eq!(value["truncated"], true);
    assert_eq!(value["next_offset"], 2);
}

#[tokio::test]
async fn refresh_bypasses_cache() {
    let tool = cached_tool("0123456789");
    let out = tool
        .call(json!({ "url": PAGE_URL, "refresh": true }))
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("FETCH_ERROR"), "{}", out.content);
}

#[test]
fn spec_advertises_refresh() {
    let spec = FetchTool::crawl4ai("http://127.0.0.1:1").spec();
    assert_eq!(spec.parameters["properties"]["refresh"]["type"], "boolean");
    let required = spec.parameters["required"].as_array().unwrap();
    assert!(!required.iter().any(|v| v == "refresh"));
}
