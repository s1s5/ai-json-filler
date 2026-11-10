use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use ai_json_filler_core::{BoxFuture, FillError, ToolOutput};
use ai_json_filler_genai::{ChatBackend, fill_with_backend};
use genai::ModelIden;
use genai::adapter::AdapterKind;
use genai::chat::{
    ChatRequest, ChatResponse, ContentPart, MessageContent, Tool, ToolCall, ToolName, Usage,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct Company {
    name: String,
    employees: u32,
}

struct ScriptedBackend {
    responses: Mutex<VecDeque<ChatResponse>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl ScriptedBackend {
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl ChatBackend for ScriptedBackend {
    fn chat<'a>(&'a self, request: ChatRequest) -> BoxFuture<'a, Result<ChatResponse, FillError>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| FillError::Llm("script exhausted".to_string()))
        })
    }
}

fn tool_call(id: &str, name: &str, args: Value) -> ToolCall {
    ToolCall {
        call_id: id.to_string(),
        fn_name: name.to_string(),
        fn_arguments: args,
        thought_signatures: None,
    }
}

fn model_iden() -> ModelIden {
    ModelIden::new(AdapterKind::Ollama, "test")
}

fn response(calls: Vec<ToolCall>) -> ChatResponse {
    let model = model_iden();
    ChatResponse {
        content: MessageContent::from_parts(
            calls
                .into_iter()
                .map(ContentPart::ToolCall)
                .collect::<Vec<_>>(),
        ),
        reasoning_content: None,
        model_iden: model.clone(),
        provider_model_iden: model,
        stop_reason: None,
        usage: Usage::default(),
        captured_raw_body: None,
        response_id: None,
    }
}

fn text_response(text: &str) -> ChatResponse {
    let model = model_iden();
    ChatResponse {
        content: MessageContent::from_text(text),
        reasoning_content: None,
        model_iden: model.clone(),
        provider_model_iden: model,
        stop_reason: None,
        usage: Usage::default(),
        captured_raw_body: None,
        response_id: None,
    }
}

#[tokio::test]
async fn fills_struct_in_two_rounds() {
    let backend = Arc::new(ScriptedBackend::new(vec![
        response(vec![tool_call(
            "1",
            "save_result",
            json!({"name": "トヨタ"}),
        )]),
        response(vec![tool_call(
            "2",
            "save_result",
            json!({"employees": 370000}),
        )]),
    ]));

    let outcome = fill_with_backend::<Company>(backend, "調べて")
        .run()
        .await
        .unwrap();

    assert_eq!(
        outcome.value,
        Company {
            name: "トヨタ".to_string(),
            employees: 370000,
        }
    );
    assert_eq!(outcome.save_calls, 2);
}

#[tokio::test]
async fn text_only_response_is_fed_back() {
    let backend = Arc::new(ScriptedBackend::new(vec![
        text_response("まだです"),
        response(vec![tool_call(
            "1",
            "save_result",
            json!({"name": "トヨタ", "employees": 370000}),
        )]),
    ]));

    let outcome = fill_with_backend::<Company>(backend.clone(), "調べて")
        .run()
        .await
        .unwrap();

    assert_eq!(outcome.value.employees, 370000);

    let requests = backend.requests();
    assert_eq!(requests.len(), 2);
    let fed_back = requests[1].messages.iter().any(|message| {
        message
            .content
            .texts()
            .iter()
            .any(|text| text.contains("ツールを呼び出してください"))
    });
    assert!(
        fed_back,
        "text-only response should be fed back to the model"
    );
}

#[tokio::test]
async fn max_rounds_returns_incomplete() {
    let backend = Arc::new(ScriptedBackend::new(vec![
        response(vec![tool_call(
            "1",
            "save_result",
            json!({"name": "トヨタ"}),
        )]),
        response(vec![tool_call(
            "2",
            "save_result",
            json!({"employees": 370000}),
        )]),
    ]));

    let error = match fill_with_backend::<Company>(backend, "調べて")
        .max_rounds(1)
        .run()
        .await
    {
        Ok(_) => panic!("expected FillError::Incomplete"),
        Err(error) => error,
    };

    match error {
        FillError::Incomplete { partial } => {
            assert_eq!(partial["name"], "トヨタ");
            assert!(partial.get("employees").is_none());
        }
        other => panic!("expected FillError::Incomplete, got {other:?}"),
    }
}

#[tokio::test]
async fn custom_save_tool_name() {
    let backend = Arc::new(ScriptedBackend::new(vec![response(vec![tool_call(
        "1",
        "store_fields",
        json!({"name": "トヨタ", "employees": 370000}),
    )])]));

    let outcome = fill_with_backend::<Company>(backend.clone(), "調べて")
        .save_tool_name("store_fields")
        .run()
        .await
        .unwrap();

    assert_eq!(outcome.value.employees, 370000);

    let requests = backend.requests();
    assert!(
        requests[0]
            .tools
            .as_ref()
            .unwrap()
            .iter()
            .any(|tool| matches!(&tool.name, ToolName::Custom(name) if name == "store_fields"))
    );
    assert!(requests[0].messages.iter().any(|message| {
        message
            .content
            .texts()
            .iter()
            .any(|text| text.contains("store_fields"))
    }));
}

#[tokio::test]
async fn max_turns_returns_incomplete() {
    let backend = Arc::new(ScriptedBackend::new(vec![
        text_response("まだです"),
        text_response("まだ分かりません"),
    ]));

    let error = match fill_with_backend::<Company>(backend.clone(), "調べて")
        .max_turns(2)
        .run()
        .await
    {
        Ok(_) => panic!("expected FillError::Incomplete"),
        Err(error) => error,
    };

    assert!(matches!(error, FillError::Incomplete { .. }));
    assert_eq!(backend.requests().len(), 2);

    let feedback = backend.requests().iter().skip(1).any(|request| {
        request.messages.iter().any(|message| {
            message
                .content
                .texts()
                .iter()
                .any(|text| text.contains("JSONはまだ埋まっていません"))
        })
    });
    assert!(feedback, "turn feedback must say the JSON is incomplete");
}

#[tokio::test]
async fn max_rounds_resets_after_turn() {
    let backend = Arc::new(ScriptedBackend::new(vec![
        response(vec![tool_call(
            "1",
            "save_result",
            json!({"name": "トヨタ"}),
        )]),
        text_response("まだです"),
        response(vec![tool_call("2", "lookup", json!({"q": "x"}))]),
        response(vec![tool_call(
            "3",
            "save_result",
            json!({"employees": 370000}),
        )]),
    ]));

    let outcome = fill_with_backend::<Company>(backend.clone(), "調べて")
        .max_turns(2)
        .max_rounds(2)
        .run()
        .await
        .unwrap();

    assert_eq!(outcome.value.employees, 370000);
    assert_eq!(backend.requests().len(), 4);
}

#[tokio::test]
async fn genai_tool_registers_definition_and_handler() {
    let backend = Arc::new(ScriptedBackend::new(vec![
        response(vec![tool_call("1", "lookup", json!({"city": "東京"}))]),
        response(vec![tool_call(
            "2",
            "save_result",
            json!({"name": "東京", "employees": 1}),
        )]),
    ]));

    let tool = Tool::new(ToolName::Custom("lookup".to_string()))
        .with_description("都市を調べる")
        .with_schema(json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }));

    let outcome = fill_with_backend::<Company>(backend.clone(), "調べて")
        .genai_tool(tool, |args| {
            Box::pin(async move {
                Ok(ToolOutput::success(format!(
                    "{{\"name\":\"{}\"}}",
                    args["city"].as_str().unwrap_or("?")
                )))
            })
        })
        .run()
        .await
        .unwrap();

    assert_eq!(outcome.value.name, "東京");
    assert!(
        backend.requests()[0]
            .tools
            .as_ref()
            .unwrap()
            .iter()
            .any(|tool| matches!(&tool.name, ToolName::Custom(name) if name == "lookup"))
    );
    assert!(backend.requests()[1].messages.iter().any(|message| {
        message
            .content
            .tool_responses()
            .iter()
            .any(|response| response.content.contains("東京"))
    }));
}

#[tokio::test]
async fn give_up_returns_impossible() {
    let backend = Arc::new(ScriptedBackend::new(vec![
        response(vec![tool_call(
            "1",
            "save_result",
            json!({"name": "トヨタ"}),
        )]),
        response(vec![tool_call(
            "2",
            "give_up",
            json!({"reason": "従業員数は非公開", "missing_fields": ["employees"]}),
        )]),
    ]));

    let error = match fill_with_backend::<Company>(backend, "調べて").run().await {
        Ok(_) => panic!("expected FillError::Impossible"),
        Err(error) => error,
    };

    match error {
        FillError::Impossible {
            reason,
            missing_fields,
            partial,
        } => {
            assert_eq!(reason, "従業員数は非公開");
            assert_eq!(missing_fields, vec!["employees".to_string()]);
            assert_eq!(partial["name"], "トヨタ");
            assert!(partial.get("employees").is_none());
        }
        other => panic!("expected FillError::Impossible, got {other:?}"),
    }
}

#[tokio::test]
async fn give_up_tool_is_advertised() {
    let backend = Arc::new(ScriptedBackend::new(vec![response(vec![tool_call(
        "1",
        "save_result",
        json!({"name": "トヨタ", "employees": 370000}),
    )])]));

    fill_with_backend::<Company>(backend.clone(), "調べて")
        .run()
        .await
        .unwrap();

    let requests = backend.requests();
    assert!(
        requests[0]
            .tools
            .as_ref()
            .unwrap()
            .iter()
            .any(|tool| matches!(&tool.name, ToolName::Custom(name) if name == "give_up"))
    );
    assert!(requests[0].messages.iter().any(|message| {
        message
            .content
            .texts()
            .iter()
            .any(|text| text.contains("give_up") && text.contains("分からない場合"))
    }));
}

#[tokio::test]
async fn custom_give_up_tool_name() {
    let backend = Arc::new(ScriptedBackend::new(vec![response(vec![tool_call(
        "1",
        "cannot_complete",
        json!({"reason": "根拠が見つからない"}),
    )])]));

    let error = match fill_with_backend::<Company>(backend.clone(), "調べて")
        .give_up_tool_name("cannot_complete")
        .run()
        .await
    {
        Ok(_) => panic!("expected FillError::Impossible"),
        Err(error) => error,
    };
    match error {
        FillError::Impossible { reason, .. } => assert_eq!(reason, "根拠が見つからない"),
        other => panic!("expected FillError::Impossible, got {other:?}"),
    }

    let tools = backend.requests()[0].tools.clone().unwrap();
    assert!(
        tools
            .iter()
            .any(|tool| matches!(&tool.name, ToolName::Custom(name) if name == "cannot_complete"))
    );
    assert!(
        !tools
            .iter()
            .any(|tool| matches!(&tool.name, ToolName::Custom(name) if name == "give_up"))
    );
}
