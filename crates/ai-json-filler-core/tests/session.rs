use std::sync::Arc;

use ai_json_filler_core::{
    BoxFuture, FeedbackOptions, FillError, FillSession, SessionResult, Tool, ToolOutput, ToolSpec,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct Company {
    name: String,
    employees: u32,
}

fn new_session() -> FillSession<Company> {
    FillSession::new("会社情報を調べて").expect("session")
}

struct DummyTool;

impl Tool for DummyTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "lookup".to_string(),
            description: "ダミーの調査ツール".to_string(),
            parameters: json!({"type": "object", "properties": {}}),
        }
    }

    fn call<'a>(&'a self, _args: Value) -> BoxFuture<'a, Result<ToolOutput, FillError>> {
        Box::pin(async move { Ok(ToolOutput::success("調査しました")) })
    }
}

fn with_dummy_tool() -> FillSession<Company> {
    let mut session = new_session();
    session.add_tool(Arc::new(DummyTool));
    session
}

#[tokio::test]
async fn completes_in_two_saves() {
    let mut session = new_session();
    match session.call("save_result", json!({"name": "トヨタ"})).await {
        SessionResult::Continue { is_error, .. } => assert!(!is_error),
        SessionResult::Done { .. } => panic!("should not be done"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }
    assert!(!session.is_done());

    match session
        .call("save_result", json!({"employees": 370000}))
        .await
    {
        SessionResult::Done { content } => assert!(content.contains("保存しました")),
        SessionResult::Continue { content, .. } => panic!("still incomplete: {content}"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }

    let outcome = session.into_outcome().expect("outcome");
    assert_eq!(
        outcome.value,
        Company {
            name: "トヨタ".to_string(),
            employees: 370000,
        }
    );
    assert_eq!(outcome.save_calls, 2);
    assert!(outcome.warnings.is_empty());
    assert_eq!(outcome.raw, json!({"name": "トヨタ", "employees": 370000}));
}

#[tokio::test]
async fn extra_key_is_stripped() {
    let mut session = new_session();
    let result = session
        .call(
            "save_result",
            json!({"name": "トヨタ", "employees": 370000, "extra": true}),
        )
        .await;

    assert!(matches!(result, SessionResult::Done { .. }));
    assert_eq!(
        session.snapshot(),
        &json!({"name": "トヨタ", "employees": 370000})
    );
    assert_eq!(
        session.warnings(),
        &["余計なキーを無視しました: extra".to_string()]
    );
}

#[tokio::test]
async fn warnings_accumulate_before_completion() {
    let mut session = new_session();
    let first = session
        .call("save_result", json!({"name": "トヨタ", "extra": 1}))
        .await;
    assert!(matches!(first, SessionResult::Continue { .. }));
    assert_eq!(session.warnings().len(), 1);

    let second = session
        .call("save_result", json!({"employees": 370000}))
        .await;
    assert!(matches!(second, SessionResult::Done { .. }));

    let outcome = session.into_outcome().expect("outcome");
    assert_eq!(outcome.warnings.len(), 1);
    assert!(outcome.warnings[0].contains("extra"));
}

#[tokio::test]
async fn unknown_tool_message() {
    let mut session = new_session();
    match session.call("nope", json!({})).await {
        SessionResult::Continue { content, is_error } => {
            assert!(is_error);
            assert!(content.contains("利用できません"), "{content}");
        }
        SessionResult::Done { .. } => panic!("unexpected done"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }
}

#[tokio::test]
async fn feedback_options_are_used() {
    let mut session = new_session().with_feedback_options(FeedbackOptions {
        max_errors: 0,
        ..FeedbackOptions::default()
    });
    match session.call("save_result", json!({"name": "トヨタ"})).await {
        SessionResult::Continue { content, .. } => {
            assert!(content.contains("…他 1 件"), "{content}");
        }
        SessionResult::Done { .. } => panic!("unexpected done"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }
}

#[test]
fn seed_allows_missing_required() {
    let session = FillSession::<Company>::with_seed("調べて", json!({"name": "トヨタ"}));
    assert!(session.is_ok());
}

#[test]
fn seed_rejects_type_error() {
    match FillSession::<Company>::with_seed("調べて", json!({"employees": "多い"})) {
        Err(FillError::InvalidSeed { .. }) => {}
        Err(other) => panic!("unexpected: {other:?}"),
        Ok(_) => panic!("seed must be rejected"),
    }
}

#[test]
fn seed_rejects_non_object() {
    match FillSession::<Company>::with_seed("調べて", json!([1, 2])) {
        Err(FillError::InvalidSeed { .. }) => {}
        Err(other) => panic!("unexpected: {other:?}"),
        Ok(_) => panic!("seed must be rejected"),
    }
}

#[test]
fn incomplete_returns_partial() {
    let session = new_session();
    match session.into_outcome() {
        Err(FillError::Incomplete { partial }) => assert_eq!(partial, json!({})),
        Err(other) => panic!("unexpected: {other:?}"),
        Ok(_) => panic!("outcome must be incomplete"),
    }
}

#[tokio::test]
async fn custom_save_tool_name() {
    let mut session = new_session().with_save_tool_name("store_fields");

    assert_eq!(session.save_tool_name(), "store_fields");
    assert_eq!(session.tool_specs()[0].name, "store_fields");
    assert!(session.system_prompt().contains("store_fields"));

    match session.call("save_result", json!({"name": "トヨタ"})).await {
        SessionResult::Continue { content, is_error } => {
            assert!(is_error);
            assert!(content.contains("利用できません"), "{content}");
        }
        SessionResult::Done { .. } => panic!("unexpected done"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }

    match session
        .call(
            "store_fields",
            json!({"name": "トヨタ", "employees": 370000}),
        )
        .await
    {
        SessionResult::Done { content } => assert!(content.contains("保存しました")),
        SessionResult::Continue { content, .. } => panic!("still incomplete: {content}"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }

    let outcome = session.into_outcome().expect("outcome");
    assert_eq!(outcome.value.name, "トヨタ");
}

#[test]
fn progress_feedback_lists_missing_fields() {
    let session = new_session();
    let feedback = session.progress_feedback();
    assert!(
        feedback.contains("JSONはまだ埋まっていません"),
        "{feedback}"
    );
    assert!(feedback.contains("required"), "{feedback}");
    assert!(feedback.contains("save_result"), "{feedback}");
}

#[tokio::test]
async fn give_up_ends_session() {
    let mut session = new_session();
    assert!(session.system_prompt().contains("give_up"));
    assert!(matches!(
        session.call("save_result", json!({"name": "トヨタ"})).await,
        SessionResult::Continue { .. }
    ));
    assert!(!session.is_done());
    assert!(!session.is_aborted());

    let args = json!({
        "reason": "従業員数は非公開",
        "missing_fields": ["employees"],
    });
    match session.call("give_up", args.clone()).await {
        SessionResult::Impossible {
            reason,
            missing_fields,
        } => {
            assert_eq!(reason, "従業員数は非公開");
            assert_eq!(missing_fields, vec!["employees".to_string()]);
        }
        _ => panic!("give_up must end the session"),
    }

    assert!(session.is_done());
    assert!(session.is_aborted());
    assert_eq!(
        session
            .give_up_record()
            .map(|record| record.reason.as_str()),
        Some("従業員数は非公開")
    );

    // The declaration is idempotent.
    match session.call("give_up", args).await {
        SessionResult::Impossible { reason, .. } => {
            assert_eq!(reason, "従業員数は非公開");
        }
        _ => panic!("second give_up must return Impossible"),
    }

    match session.into_outcome() {
        Err(FillError::Impossible {
            reason,
            missing_fields,
            partial,
        }) => {
            assert_eq!(reason, "従業員数は非公開");
            assert_eq!(missing_fields, vec!["employees".to_string()]);
            assert_eq!(partial, json!({"name": "トヨタ"}));
        }
        Err(other) => panic!("unexpected: {other:?}"),
        Ok(_) => panic!("outcome must be impossible"),
    }
}

#[tokio::test]
async fn give_up_preserves_partial() {
    let mut session = new_session();
    session.call("save_result", json!({"name": "トヨタ"})).await;
    assert!(matches!(
        session
            .call("give_up", json!({"reason": "残りは非公開"}))
            .await,
        SessionResult::Impossible { .. }
    ));

    match session.into_outcome() {
        Err(FillError::Impossible { partial, .. }) => {
            assert_eq!(partial["name"], "トヨタ");
            assert!(partial.get("employees").is_none());
        }
        Err(other) => panic!("unexpected: {other:?}"),
        Ok(_) => panic!("outcome must be impossible"),
    }
}

#[tokio::test]
async fn give_up_requires_reason() {
    let mut session = new_session();
    for args in [json!({}), json!({"reason": "   "})] {
        match session.call("give_up", args).await {
            SessionResult::Continue { content, .. } => {
                assert!(content.contains("reason"), "{content}");
            }
            _ => panic!("give_up without reason must be rejected"),
        }
        assert!(!session.is_done());
        assert!(!session.is_aborted());
    }
    assert!(matches!(
        session.into_outcome(),
        Err(FillError::Incomplete { .. })
    ));
}

#[tokio::test]
async fn give_up_rejects_non_object() {
    let mut session = new_session();
    match session.call("give_up", json!([1, 2])).await {
        SessionResult::Continue { content, .. } => {
            assert!(content.contains("JSON オブジェクト"), "{content}");
        }
        _ => panic!("non-object arguments must be rejected"),
    }
    assert!(!session.is_aborted());
}

#[tokio::test]
async fn give_up_after_save_returns_saved() {
    let mut session = new_session();
    assert!(matches!(
        session
            .call(
                "save_result",
                json!({"name": "トヨタ", "employees": 370000})
            )
            .await,
        SessionResult::Done { .. }
    ));

    match session.call("give_up", json!({"reason": "不要"})).await {
        SessionResult::Done { content } => assert!(content.contains("保存済み"), "{content}"),
        _ => panic!("give_up after save must stay Done"),
    }
    assert!(!session.is_aborted());
    assert!(session.into_outcome().is_ok());
}

#[tokio::test]
async fn custom_give_up_tool_name() {
    let mut session = new_session().with_give_up_tool_name("cannot_complete");

    assert_eq!(session.give_up_tool_name(), "cannot_complete");
    let specs = session.tool_specs();
    assert!(specs.iter().any(|spec| spec.name == "cannot_complete"));
    assert!(!specs.iter().any(|spec| spec.name == "give_up"));
    assert!(session.system_prompt().contains("cannot_complete"));

    match session
        .call("cannot_complete", json!({"reason": "根拠が見つからない"}))
        .await
    {
        SessionResult::Impossible { reason, .. } => assert_eq!(reason, "根拠が見つからない"),
        _ => panic!("custom give-up tool must work"),
    }

    let mut other = new_session().with_give_up_tool_name("cannot_complete");
    match other.call("give_up", json!({"reason": "x"})).await {
        SessionResult::Continue { content, is_error } => {
            assert!(is_error);
            assert!(content.contains("利用できません"), "{content}");
        }
        _ => panic!("default name must no longer be reachable"),
    }
}

#[tokio::test]
async fn give_up_name_collision_ignored() {
    let session = new_session().with_give_up_tool_name("save_result");
    assert_eq!(session.give_up_tool_name(), "give_up");

    let session = new_session().with_save_tool_name("give_up");
    assert_eq!(session.save_tool_name(), "save_result");

    let names: Vec<String> = session
        .tool_specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect();
    assert_eq!(
        names,
        vec!["save_result".to_string(), "give_up".to_string()]
    );
}

#[tokio::test]
async fn give_up_after_tool_calls_gate() {
    let mut session = with_dummy_tool().with_give_up_after_tool_calls(2);

    match session.call("give_up", json!({"reason": "不明"})).await {
        SessionResult::Continue { content, .. } => {
            assert!(content.contains("VALIDATION_ERROR"), "{content}");
            assert!(content.contains("2回"), "{content}");
        }
        _ => panic!("gate must reject the first give_up"),
    }

    assert!(matches!(
        session.call("lookup", json!({})).await,
        SessionResult::Continue { .. }
    ));
    match session.call("give_up", json!({"reason": "不明"})).await {
        SessionResult::Continue { content, .. } => {
            assert!(content.contains("現在1回"), "{content}");
        }
        _ => panic!("gate must reject the second give_up"),
    }

    assert!(matches!(
        session.call("lookup", json!({})).await,
        SessionResult::Continue { .. }
    ));
    assert!(matches!(
        session.call("give_up", json!({"reason": "不明"})).await,
        SessionResult::Impossible { .. }
    ));
}

#[tokio::test]
async fn give_up_auto_gate_with_tools() {
    let mut session = with_dummy_tool();
    assert!(matches!(
        session.call("give_up", json!({"reason": "不明"})).await,
        SessionResult::Continue { .. }
    ));
    assert!(!session.is_aborted());

    assert!(matches!(
        session.call("lookup", json!({})).await,
        SessionResult::Continue { .. }
    ));
    assert!(matches!(
        session.call("give_up", json!({"reason": "不明"})).await,
        SessionResult::Impossible { .. }
    ));
}

#[tokio::test]
async fn give_up_auto_gate_without_tools() {
    let mut session = new_session();
    assert!(matches!(
        session
            .call("give_up", json!({"reason": "調査手段が無い"}))
            .await,
        SessionResult::Impossible { .. }
    ));
}
