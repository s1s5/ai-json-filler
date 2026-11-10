use ai_json_filler_core::Tool;
use ai_json_filler_tools::SearchTool;
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
