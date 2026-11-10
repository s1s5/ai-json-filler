use ai_json_filler_core::ToolSpec;
use ai_json_filler_genai::ChatBackend;
use ai_json_filler_genai::{GenaiBackend, to_genai_tool};
use genai::ModelIden;
use genai::ServiceTarget;
use genai::adapter::AdapterKind;
use genai::chat::ToolName;
use genai::resolver::{AuthData, Endpoint};
use genai::{Client, ModelSpec};
use serde_json::json;

#[test]
fn to_genai_tool_keeps_fields() {
    let spec = ToolSpec {
        name: "web_search".into(),
        description: "検索".into(),
        parameters: json!({"type": "object"}),
    };

    let tool = to_genai_tool(&spec);

    assert_eq!(tool.name, ToolName::Custom("web_search".to_string()));
    assert_eq!(tool.description, Some("検索".to_string()));
    assert_eq!(tool.schema, Some(json!({"type": "object"})));
}

#[test]
fn backend_is_object_safe() {
    let client = Client::builder().build().unwrap();
    let spec: ModelSpec = ServiceTarget {
        endpoint: Endpoint::from_owned("http://127.0.0.1:11434/".to_string()),
        auth: AuthData::from_single("ollama"),
        model: ModelIden::new(AdapterKind::Ollama, "test"),
    }
    .into();

    let _: Box<dyn ChatBackend> = Box::new(GenaiBackend::new(client, spec));
}
