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

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct Row {
    name: String,
    note: String,
}

#[tokio::test]
async fn fills_vec_struct_with_append_and_update() {
    let backend = Arc::new(ScriptedBackend::new(vec![
        response(vec![tool_call(
            "1",
            "append",
            json!({"items": [{"name": "A"}]}),
        )]),
        response(vec![tool_call(
            "2",
            "update",
            json!({"index": 0, "patch": {"note": "x"}}),
        )]),
    ]));

    let outcome = fill_with_backend::<Vec<Row>>(backend, "調べて")
        .run()
        .await
        .unwrap();

    assert_eq!(
        outcome.value,
        vec![Row {
            name: "A".to_string(),
            note: "x".to_string(),
        }]
    );
    assert_eq!(outcome.save_calls, 2);
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct CompanyOpt {
    name: String,
    employees: Option<u32>,
}

#[tokio::test]
async fn builder_forwards_goal_fields() {
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

    let outcome = match fill_with_backend::<CompanyOpt>(backend.clone(), "調べて")
        .goal_fields(["employees"])
        .run()
        .await
    {
        Ok(outcome) => outcome,
        Err(err) => panic!("expected success, got {err}"),
    };

    assert_eq!(
        outcome.value,
        CompanyOpt {
            name: "トヨタ".to_string(),
            employees: Some(370000),
        }
    );
    assert_eq!(outcome.save_calls, 2, "1回目では完了しないはず");
    let requests = backend.requests();
    assert_eq!(requests.len(), 2);
}

#[tokio::test]
async fn builder_rejects_unknown_goal_field() {
    let backend = Arc::new(ScriptedBackend::new(vec![]));
    match fill_with_backend::<CompanyOpt>(backend, "調べて")
        .goal_fields(["nope"])
        .run()
        .await
    {
        Ok(_) => panic!("unknown goal field must be rejected"),
        Err(err) => assert!(
            matches!(err, ai_json_filler_core::FillError::UnknownGoalField { .. }),
            "{err}"
        ),
    }
}

struct CountingLookup {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl ai_json_filler_core::Tool for CountingLookup {
    fn spec(&self) -> ai_json_filler_core::ToolSpec {
        ai_json_filler_core::ToolSpec {
            name: "lookup".to_string(),
            description: "調べる".to_string(),
            parameters: json!({"type":"object","properties":{}}),
        }
    }

    fn call<'a>(
        &'a self,
        _args: Value,
    ) -> ai_json_filler_core::BoxFuture<'a, Result<ai_json_filler_core::ToolOutput, FillError>>
    {
        Box::pin(async move {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ai_json_filler_core::ToolOutput::success("調べました"))
        })
    }
}

#[tokio::test]
async fn builder_forwards_tool_budget() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let calls = Arc::new(AtomicUsize::new(0));
    let backend = Arc::new(ScriptedBackend::new(vec![
        response(vec![tool_call("1", "lookup", json!({}))]),
        response(vec![tool_call("2", "lookup", json!({}))]),
        response(vec![tool_call(
            "3",
            "save_result",
            json!({"name": "x", "employees": 1}),
        )]),
    ]));

    let outcome = fill_with_backend::<Company>(backend.clone(), "調べて")
        .tool(Arc::new(CountingLookup {
            calls: calls.clone(),
        }))
        .tool_budget("lookup", 1)
        .run()
        .await;

    assert!(outcome.is_ok(), "should complete");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "2回目は実行されない");
    assert_eq!(outcome.unwrap().tool_calls_by_name.get("lookup"), Some(&1));
    let requests = backend.requests();
    let last = requests.last().unwrap();
    let mut text = String::new();
    for message in &last.messages {
        for chunk in message.content.texts() {
            text.push_str(chunk);
            text.push('\n');
        }
        for response in message.content.tool_responses() {
            text.push_str(&response.content);
            text.push('\n');
        }
    }
    assert!(text.contains("上限（1 回）"), "{text}");
}

#[tokio::test]
async fn builder_forwards_extra_keys_strip() {
    let backend = Arc::new(ScriptedBackend::new(vec![response(vec![tool_call(
        "1",
        "save_result",
        json!({"name": "x", "employees": 1, "bogus": true}),
    )])]));

    let outcome = fill_with_backend::<Company>(backend, "調べて")
        .extra_keys(ai_json_filler_core::ExtraKeys::Strip)
        .run()
        .await
        .unwrap();
    assert_eq!(outcome.raw, json!({"name": "x", "employees": 1}));
    assert!(outcome.warnings.iter().any(|w| w.contains("bogus")));
}

#[tokio::test]
async fn builder_can_disable_schema_guide() {
    let backend = Arc::new(ScriptedBackend::new(vec![response(vec![tool_call(
        "1",
        "save_result",
        json!({"name": "x", "employees": 1}),
    )])]));
    let _ = fill_with_backend::<Company>(backend.clone(), "調べて")
        .schema_guide(false)
        .run()
        .await;
    let system = backend.requests()[0].system.clone().unwrap_or_default();
    assert!(!system.contains("保存する JSON の形"), "{system}");
}

fn tool_names(request: &ChatRequest) -> Vec<String> {
    request
        .tools
        .as_ref()
        .expect("request must carry tools")
        .iter()
        .map(|tool| tool.name.as_str().to_string())
        .collect()
}

fn nudge_count(request: &ChatRequest) -> usize {
    request
        .messages
        .iter()
        .filter(|message| {
            message
                .content
                .texts()
                .iter()
                .any(|text| text.contains("これ以上は調査できません"))
        })
        .count()
}

fn lookup_tool() -> Tool {
    Tool::new(ToolName::Custom("lookup".to_string()))
        .with_description("調べる")
        .with_schema(json!({"type": "object", "properties": {}}))
}

fn lookup_handler()
-> impl Fn(Value) -> BoxFuture<'static, Result<ToolOutput, FillError>> + Send + Sync {
    |_args: Value| Box::pin(async { Ok(ToolOutput::success("調べました")) })
}

#[tokio::test]
async fn force_save_after_swaps_tools() {
    let backend = Arc::new(ScriptedBackend::new(vec![
        response(vec![tool_call("1", "lookup", json!({}))]),
        response(vec![tool_call(
            "2",
            "save_result",
            json!({"name": "x", "employees": 1}),
        )]),
    ]));

    let outcome = fill_with_backend::<Company>(backend.clone(), "調べて")
        .genai_tool(lookup_tool(), lookup_handler())
        .force_save_after(1)
        .run()
        .await
        .unwrap();

    assert_eq!(outcome.value.employees, 1);
    let requests = backend.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        tool_names(&requests[0]).iter().any(|name| name == "lookup"),
        "the first round must still advertise investigation tools"
    );
    assert!(
        !tool_names(&requests[1]).iter().any(|name| name == "lookup"),
        "forced rounds must not advertise investigation tools"
    );
}

#[tokio::test]
async fn force_save_after_keeps_save_tool() {
    let backend = Arc::new(ScriptedBackend::new(vec![
        response(vec![tool_call("1", "lookup", json!({}))]),
        response(vec![tool_call(
            "2",
            "save_result",
            json!({"name": "x", "employees": 1}),
        )]),
    ]));

    fill_with_backend::<Company>(backend.clone(), "調べて")
        .genai_tool(lookup_tool(), lookup_handler())
        .force_save_after(1)
        .run()
        .await
        .unwrap();

    let forced = tool_names(&backend.requests()[1]);
    assert!(
        forced.iter().any(|name| name == "save_result"),
        "{forced:?}"
    );
    assert!(forced.iter().any(|name| name == "give_up"), "{forced:?}");
}

#[tokio::test]
async fn force_save_after_keeps_array_tools() {
    let backend = Arc::new(ScriptedBackend::new(vec![response(vec![tool_call(
        "1",
        "append",
        json!({"items": [{"name": "A", "note": "n"}]}),
    )])]));

    let outcome = fill_with_backend::<Vec<Row>>(backend.clone(), "調べて")
        .genai_tool(lookup_tool(), lookup_handler())
        .force_save_after(0)
        .run()
        .await
        .unwrap();

    assert_eq!(outcome.value.len(), 1);
    let names = tool_names(&backend.requests()[0]);
    for expected in ["append", "update", "remove", "give_up"] {
        assert!(
            names.iter().any(|name| name == expected),
            "missing {expected}: {names:?}"
        );
    }
    assert!(!names.iter().any(|name| name == "lookup"), "{names:?}");
}

#[tokio::test]
async fn force_save_after_adds_nudge_once() {
    let backend = Arc::new(ScriptedBackend::new(vec![
        response(vec![tool_call("1", "lookup", json!({}))]),
        text_response("まだです"),
        response(vec![tool_call(
            "2",
            "save_result",
            json!({"name": "x", "employees": 1}),
        )]),
    ]));

    fill_with_backend::<Company>(backend.clone(), "調べて")
        .genai_tool(lookup_tool(), lookup_handler())
        .force_save_after(1)
        .max_turns(2)
        .run()
        .await
        .unwrap();

    let requests = backend.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(nudge_count(&requests[0]), 0);
    assert_eq!(
        nudge_count(&requests[1]),
        1,
        "the forced round must carry the nudge"
    );
    assert_eq!(
        nudge_count(&requests[2]),
        1,
        "the nudge must not be inserted again after a turn reset"
    );
}

#[tokio::test]
async fn force_save_after_zero_means_immediate() {
    let backend = Arc::new(ScriptedBackend::new(vec![response(vec![tool_call(
        "1",
        "save_result",
        json!({"name": "x", "employees": 1}),
    )])]));

    fill_with_backend::<Company>(backend.clone(), "調べて")
        .genai_tool(lookup_tool(), lookup_handler())
        .force_save_after(0)
        .run()
        .await
        .unwrap();

    let request = &backend.requests()[0];
    let names = tool_names(request);
    assert!(!names.iter().any(|name| name == "lookup"), "{names:?}");
    assert!(names.iter().any(|name| name == "save_result"), "{names:?}");
    assert_eq!(nudge_count(request), 1);
}

#[tokio::test]
async fn without_force_save_tools_are_stable() {
    let backend = Arc::new(ScriptedBackend::new(vec![
        response(vec![tool_call("1", "lookup", json!({}))]),
        response(vec![tool_call("2", "lookup", json!({}))]),
        response(vec![tool_call(
            "3",
            "save_result",
            json!({"name": "x", "employees": 1}),
        )]),
    ]));

    fill_with_backend::<Company>(backend.clone(), "調べて")
        .genai_tool(lookup_tool(), lookup_handler())
        .run()
        .await
        .unwrap();

    let requests = backend.requests();
    assert_eq!(requests.len(), 3);
    let expected = tool_names(&requests[0]);
    assert!(expected.iter().any(|name| name == "lookup"));
    for request in &requests {
        assert_eq!(tool_names(request), expected);
        assert_eq!(nudge_count(request), 0);
    }
}

#[tokio::test]
async fn forced_save_blocks_removed_tool_execution() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let calls = Arc::new(AtomicUsize::new(0));
    let backend = Arc::new(ScriptedBackend::new(vec![
        response(vec![tool_call("1", "lookup", json!({}))]),
        // 一覧から消えた後も、モデルが名前を覚えていて呼ぶケース
        response(vec![tool_call("2", "lookup", json!({}))]),
        response(vec![tool_call(
            "3",
            "save_result",
            json!({"name": "x", "employees": 1}),
        )]),
    ]));

    let outcome = fill_with_backend::<Company>(backend.clone(), "調べて")
        .tool(Arc::new(CountingLookup {
            calls: calls.clone(),
        }))
        .force_save_after(1)
        .run()
        .await;

    assert!(outcome.is_ok(), "保存まで到達するはず");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "強制後は実行されない（一覧から消すだけでは不十分）"
    );
    let requests = backend.requests();
    let mut text = String::new();
    for message in &requests.last().unwrap().messages {
        for response in message.content.tool_responses() {
            text.push_str(&response.content);
            text.push('\n');
        }
    }
    assert!(text.contains("現在利用できません"), "{text}");
}
