use std::sync::Arc;

use ai_json_filler_core::{FillSession, Tool};
use schemars::JsonSchema;
use serde::Deserialize;

use ai_json_filler_tools::{WebMemory, WebTools};

fn names(tools: &[Arc<dyn Tool>]) -> Vec<String> {
    tools.iter().map(|t| t.spec().name.clone()).collect()
}

#[tokio::test]
async fn empty_builder_has_no_tools() {
    let web = WebTools::builder().build();
    assert!(web.is_empty());
    assert!(web.tools().is_empty());
}

#[tokio::test]
async fn tavily_registers_default_names_only() {
    let web = WebTools::builder().tavily_key("k").build();
    assert_eq!(
        names(&web.tools()),
        vec!["web_search".to_string(), "web_fetch".to_string()]
    );
}

#[tokio::test]
async fn provider_aliases_adds_names() {
    let web = WebTools::builder()
        .tavily_key("k")
        .provider_aliases(true)
        .build();
    assert_eq!(
        names(&web.tools()),
        vec![
            "web_search".to_string(),
            "web_fetch".to_string(),
            "web_search_tavily".to_string(),
            "web_fetch_tavily".to_string(),
        ]
    );
}

#[tokio::test]
async fn searxng_without_tavily_becomes_default() {
    let web = WebTools::builder().searxng_url("http://localhost/").build();
    assert_eq!(names(&web.tools()), vec!["web_search".to_string()]);
}

#[tokio::test]
async fn crawl4ai_without_tavily_becomes_default() {
    let web = WebTools::builder()
        .crawl4ai_url("http://localhost/")
        .build();
    assert_eq!(names(&web.tools()), vec!["web_fetch".to_string()]);
}

#[tokio::test]
async fn blank_values_are_ignored() {
    let web = WebTools::builder()
        .tavily_key("   ")
        .searxng_url("")
        .build();
    assert!(web.is_empty());
}

#[tokio::test]
async fn from_lookup_reads_fixed_values() {
    // Use fixed lookups; never touches std::env.
    let web = WebTools::from_lookup(|k| match k {
        "TAVILY_API_KEY" => Some("k".to_string()),
        _ => None,
    });
    assert_eq!(
        names(&web.tools()),
        vec!["web_search".to_string(), "web_fetch".to_string()]
    );
}

#[derive(Deserialize, JsonSchema)]
struct Company {
    name: String,
}

#[tokio::test]
async fn web_tools_register_into_session() {
    let company: Company = serde_json::from_str(r#"{"name":"Acme"}"#).unwrap();
    assert_eq!(company.name, "Acme");

    let tools = WebTools::builder().tavily_key("k").build().tools();
    let mut session: FillSession<Company> = FillSession::new("...").unwrap();
    session.add_tool(tools[0].clone());
    let specs = session.tool_specs();
    assert!(specs.iter().any(|s| s.name == "web_search"));
}

fn sample_hit() -> serde_json::Value {
    serde_json::json!({"title": "T", "url": "https://example.com", "snippet": "C"})
}

#[tokio::test]
async fn builder_accepts_external_memory() {
    let memory = WebMemory::new();
    let web = WebTools::builder()
        .tavily_key("k")
        .memory(memory.clone())
        .build();

    assert!(web.memory().is_empty());
    memory.record_search("tavily", "q", 1, 5, &[sample_hit()]);
    assert_eq!(web.memory().len(), 1);
    assert_eq!(web.memory().search_count(), 1);
}

#[tokio::test]
async fn clear_empties_shared_memory() {
    let web = WebTools::builder().tavily_key("k").build();
    web.memory()
        .record_search("tavily", "q", 1, 5, &[sample_hit()]);
    web.memory()
        .record_fetch("tavily", "https://example.com", "body");

    assert_eq!(web.clear(), 2);
    assert!(web.memory().is_empty());
    assert!(web.snapshot().searches.is_empty());
}

#[tokio::test]
async fn snapshot_exposes_records() {
    let web = WebTools::builder().tavily_key("k").build();
    web.memory()
        .record_search("tavily", "q", 1, 5, &[sample_hit()]);

    let snapshot = web.snapshot();
    assert_eq!(snapshot.searches.len(), 1);
    assert_eq!(snapshot.searches[0].query, "q");
    assert!(snapshot.fetches.is_empty());
}

#[tokio::test]
async fn built_tools_share_the_builders_memory() {
    // The registry is unreachable, so a successful answer proves the cache hit
    // came from the injected store rather than from the network.
    let memory = WebMemory::new();
    memory.record_search("searxng", "q", 1, 5, &[sample_hit()]);
    let web = WebTools::builder()
        .searxng_url("http://127.0.0.1:1")
        .memory(memory)
        .build();

    let tool = web
        .tools()
        .into_iter()
        .find(|t| t.spec().name == "web_search")
        .expect("web_search should be registered");
    let out = tool
        .call(serde_json::json!({ "query": "q" }))
        .await
        .unwrap();

    assert!(!out.is_error, "{}", out.content);
    let value: serde_json::Value = serde_json::from_str(&out.content).unwrap();
    assert_eq!(value["cached"], true);
}
