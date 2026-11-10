use std::sync::Arc;

use ai_json_filler_core::{FillSession, Tool};
use schemars::JsonSchema;
use serde::Deserialize;

use ai_json_filler_tools::WebTools;

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
