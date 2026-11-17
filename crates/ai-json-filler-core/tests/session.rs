use std::sync::Arc;

use ai_json_filler_core::{
    BoxFuture, ExtraKeys, FeedbackOptions, FillError, FillSession, GoalEmpty, GuideOptions,
    SessionResult, Tool, ToolOutput, ToolSpec,
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
async fn strip_extra_key_still_completes() {
    let mut session = new_session().with_extra_keys(ExtraKeys::Strip);
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
    assert!(
        session
            .warnings()
            .iter()
            .any(|warning| warning == "余計なキーを無視しました: extra"),
        "{:?}",
        session.warnings()
    );
    assert!(
        session
            .warnings()
            .iter()
            .any(|warning| warning == "余計なキーを 1 件捨てました"),
        "{:?}",
        session.warnings()
    );
}

#[tokio::test]
async fn reject_extra_key_does_not_commit_object() {
    let mut session = new_session();
    assert_eq!(session.extra_keys(), ExtraKeys::Reject);

    match session
        .call("save_result", json!({"name": "トヨタ", "extra": true}))
        .await
    {
        SessionResult::Continue { content, is_error } => {
            assert!(!is_error);
            assert!(content.contains("保存しませんでした"), "{content}");
        }
        _ => panic!("extra key must not finish the session"),
    }

    // 候補はコミットしない。
    assert_eq!(session.snapshot(), &json!({}));
    assert!(
        session
            .warnings()
            .iter()
            .any(|warning| warning.contains("拒否しました") && warning.contains("extra")),
        "{:?}",
        session.warnings()
    );
    assert!(matches!(
        session.into_outcome(),
        Err(FillError::Incomplete { partial }) if partial == json!({})
    ));
}

#[tokio::test]
async fn reject_extra_key_names_the_correct_field() {
    let mut session = new_session();
    match session
        .call("save_result", json!({"name": "トヨタ", "employees.": 1}))
        .await
    {
        SessionResult::Continue { content, .. } => {
            assert!(
                content.contains("正しいキーは `employees` です（末尾の `.` を外す）"),
                "{content}"
            );
        }
        _ => panic!("expected continue"),
    }

    // 提案どおりに直して送り直せば保存できる（拒否時は候補ごと捨てているので、
    // name も一緒に送り直す）。
    match session
        .call(
            "save_result",
            json!({"name": "トヨタ", "employees": 370000}),
        )
        .await
    {
        SessionResult::Done { .. } => {}
        other => {
            let _ = other;
            panic!("expected done")
        }
    }
    assert_eq!(
        session.snapshot(),
        &json!({"name": "トヨタ", "employees": 370000})
    );
}

#[tokio::test]
async fn reject_without_similar_property_has_no_suggestion() {
    let mut session = new_session();
    match session
        .call(
            "save_result",
            json!({"name": "トヨタ", "totally_unrelated": 1}),
        )
        .await
    {
        SessionResult::Continue { content, .. } => {
            assert!(content.contains("totally_unrelated"), "{content}");
            assert!(!content.contains("正しいキー"), "{content}");
            assert!(!content.contains("間違いではありませんか"), "{content}");
        }
        _ => panic!("expected continue"),
    }
    assert_eq!(session.snapshot(), &json!({}));
}

#[tokio::test]
async fn strip_warning_includes_count() {
    let mut session = new_session().with_extra_keys(ExtraKeys::Strip);
    let _ = session
        .call("save_result", json!({"name": "トヨタ", "foo": 1, "bar": 2}))
        .await;

    assert!(
        session
            .warnings()
            .iter()
            .any(|warning| warning == "余計なキーを 2 件捨てました"),
        "{:?}",
        session.warnings()
    );
    // employees はまだ不足しているので終端しない。
    assert!(!session.is_done());
    assert_eq!(session.snapshot(), &json!({"name": "トヨタ"}));
}

#[tokio::test]
async fn warnings_accumulate_before_completion() {
    let mut session = new_session().with_extra_keys(ExtraKeys::Strip);
    let first = session
        .call("save_result", json!({"name": "トヨタ", "extra": 1}))
        .await;
    assert!(matches!(first, SessionResult::Continue { .. }));
    assert_eq!(session.warnings().len(), 2);

    let second = session
        .call("save_result", json!({"employees": 370000}))
        .await;
    assert!(matches!(second, SessionResult::Done { .. }));

    let outcome = session.into_outcome().expect("outcome");
    assert_eq!(outcome.warnings.len(), 2);
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

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct Row {
    name: String,
    note: String,
}

fn array_session() -> FillSession<Vec<Row>> {
    FillSession::new("行を埋めて").expect("session")
}

#[tokio::test]
async fn array_mode_advertises_three_tools() {
    let session = array_session();
    let specs = session.tool_specs();
    let names: Vec<String> = specs.iter().map(|spec| spec.name.clone()).collect();
    assert!(names.iter().any(|n| n == "append"), "{names:?}");
    assert!(names.iter().any(|n| n == "update"), "{names:?}");
    assert!(names.iter().any(|n| n == "remove"), "{names:?}");
    assert!(!names.iter().any(|n| n == "save_result"), "{names:?}");
}

#[tokio::test]
async fn array_tool_specs_are_valid_schemas() {
    let session = array_session();
    for spec in session.tool_specs() {
        jsonschema::validator_for(&spec.parameters)
            .unwrap_or_else(|err| panic!("{}: {err}", spec.name));
    }
}

#[tokio::test]
async fn array_tool_specs_include_defs_for_ref_items() {
    let session = array_session();
    let specs = session.tool_specs();
    let append = specs.iter().find(|spec| spec.name == "append").unwrap();
    let update = specs.iter().find(|spec| spec.name == "update").unwrap();
    assert!(append.parameters.get("$defs").is_some());
    assert!(update.parameters.get("$defs").is_some());
}

#[tokio::test]
async fn append_partial_then_update_completes() {
    let mut session = array_session();
    match session
        .call("append", json!({"items": [{"name": "A"}]}))
        .await
    {
        SessionResult::Continue { content, .. } => {
            assert!(content.contains("追加しました"), "{content}");
            assert!(
                content.contains("[0].note") || content.contains("note"),
                "{content}"
            );
        }
        other => {
            let _ = other;
            panic!("expected continue")
        }
    }
    assert!(!session.is_done());

    match session
        .call("update", json!({"index": 0, "patch": {"note": "x"}}))
        .await
    {
        SessionResult::Done { content } => assert!(content.contains("1件"), "{content}"),
        other => {
            let _ = other;
            panic!("expected done")
        }
    }

    let outcome = session.into_outcome().expect("outcome");
    assert_eq!(
        outcome.value,
        vec![Row {
            name: "A".to_string(),
            note: "x".to_string(),
        }]
    );
    assert_eq!(outcome.save_calls, 2);
}

#[tokio::test]
async fn append_multiple_items() {
    let mut session = array_session();
    let result = session
        .call(
            "append",
            json!({"items": [
                {"name": "A", "note": "a"},
                {"name": "B", "note": "b"}
            ]}),
        )
        .await;
    assert!(matches!(result, SessionResult::Done { .. }));
    let outcome = session.into_outcome().expect("outcome");
    assert_eq!(outcome.value.len(), 2);
    assert_eq!(outcome.value[1].name, "B");
}

#[tokio::test]
async fn update_merges_only_given_fields() {
    let mut session = array_session();
    let _ = session
        .call("append", json!({"items": [{"name": "A"}]}))
        .await;
    let result = session
        .call("update", json!({"index": 0, "patch": {"note": "x"}}))
        .await;
    assert!(matches!(result, SessionResult::Done { .. }));
    assert_eq!(session.snapshot(), &json!([{"name": "A", "note": "x"}]));
}

#[tokio::test]
async fn remove_shifts_indices() {
    let mut session = array_session();
    let _ = session
        .call(
            "append",
            json!({"items": [{"name": "A"}, {"name": "B"}, {"name": "C"}]}),
        )
        .await;
    let removed = session.call("remove", json!({"index": 1})).await;
    assert!(matches!(removed, SessionResult::Continue { .. }));
    assert_eq!(session.snapshot(), &json!([{"name": "A"}, {"name": "C"}]));

    // The element that was at index 2 is now at index 1.
    let _ = session
        .call("update", json!({"index": 1, "patch": {"note": "c"}}))
        .await;
    let result = session
        .call("update", json!({"index": 0, "patch": {"note": "a"}}))
        .await;
    assert!(matches!(result, SessionResult::Done { .. }));
    assert_eq!(
        session.snapshot(),
        &json!([{"name": "A", "note": "a"}, {"name": "C", "note": "c"}])
    );
}

#[tokio::test]
async fn remove_out_of_range_is_rejected() {
    let mut session = array_session();
    let _ = session
        .call("append", json!({"items": [{"name": "A"}]}))
        .await;
    let before = session.snapshot().clone();
    match session.call("remove", json!({"index": 5})).await {
        SessionResult::Continue { content, .. } => {
            assert!(content.contains("範囲外"), "{content}");
        }
        other => {
            let _ = other;
            panic!("expected continue")
        }
    }
    assert_eq!(session.snapshot(), &before);
}

#[tokio::test]
async fn extra_key_in_append_is_stripped() {
    let mut session = array_session().with_extra_keys(ExtraKeys::Strip);
    let result = session
        .call("append", json!({"items": [{"name": "A", "extra": true}]}))
        .await;
    assert!(matches!(result, SessionResult::Continue { .. }));
    assert_eq!(session.snapshot(), &json!([{"name": "A"}]));
    assert!(
        session
            .warnings()
            .iter()
            .any(|warning| warning.contains("[0].extra")),
        "{:?}",
        session.warnings()
    );
}

#[tokio::test]
async fn reject_suggests_nearest_property() {
    let mut session = array_session();
    match session
        .call(
            "append",
            json!({"items": [{"name": "A", "note": "x", "name_0": "dup"}]}),
        )
        .await
    {
        SessionResult::Continue { content, .. } => {
            assert!(content.contains("`[0].name_0`"), "{content}");
            assert!(
                content.contains("→ `name` の間違いではありませんか"),
                "{content}"
            );
        }
        _ => panic!("expected continue"),
    }
    assert_eq!(session.snapshot(), &json!([]));
    assert!(!session.is_done());
}

#[tokio::test]
async fn reject_append_does_not_duplicate_items() {
    let mut session = array_session();
    assert!(matches!(
        session
            .call(
                "append",
                json!({"items": [{"name": "A", "note": "a", "bogus": 1}]})
            )
            .await,
        SessionResult::Continue { .. }
    ));
    assert_eq!(session.snapshot(), &json!([]));

    match session
        .call("append", json!({"items": [{"name": "A", "note": "a"}]}))
        .await
    {
        SessionResult::Done { content } => assert!(content.contains("1件"), "{content}"),
        other => {
            let _ = other;
            panic!("expected done")
        }
    }

    let outcome = session.into_outcome().expect("outcome");
    assert_eq!(outcome.value.len(), 1);
    assert_eq!(outcome.value[0].name, "A");
}

#[tokio::test]
async fn wrapper_extra_key_is_rejected() {
    // Strip を選んでいても、wrapper の宣言と違う引数は拒否する。
    for policy in [ExtraKeys::Reject, ExtraKeys::Strip] {
        let mut session = array_session().with_extra_keys(policy);
        match session
            .call(
                "append",
                json!({"items": [{"name": "A", "note": "a"}], "comment": "hi"}),
            )
            .await
        {
            SessionResult::Continue { content, .. } => {
                assert!(content.contains("使えないキー"), "{policy:?}: {content}");
                assert!(content.contains("comment"), "{policy:?}: {content}");
                assert!(content.contains("items"), "{policy:?}: {content}");
            }
            other => {
                let _ = other;
                panic!("{policy:?}: wrapper extra key must be rejected")
            }
        }
        assert_eq!(session.snapshot(), &json!([]));
        assert!(!session.is_done());
    }
}

#[tokio::test]
async fn array_mode_seed_then_update() {
    let mut session =
        FillSession::<Vec<Row>>::with_seed("行を埋めて", json!([{"name": "A"}])).expect("session");
    let result = session
        .call("update", json!({"index": 0, "patch": {"note": "x"}}))
        .await;
    assert!(matches!(result, SessionResult::Done { .. }));
    let outcome = session.into_outcome().expect("outcome");
    assert_eq!(outcome.value[0].note, "x");
}

#[tokio::test]
async fn array_mode_rejects_save_result() {
    let mut session = array_session();
    match session.call("save_result", json!({"name": "A"})).await {
        SessionResult::Continue { content, is_error } => {
            assert!(is_error);
            assert!(content.contains("利用できません"), "{content}");
        }
        other => {
            let _ = other;
            panic!("expected tool error")
        }
    }
}

#[tokio::test]
async fn array_mode_custom_tool_names() {
    use ai_json_filler_core::ArrayToolNames;
    let names = ArrayToolNames {
        append: "add_rows".to_string(),
        update: "patch_row".to_string(),
        remove: "drop_row".to_string(),
    };
    let session = array_session().with_array_tool_names(names);
    let specs = session.tool_specs();
    let names: Vec<String> = specs.iter().map(|spec| spec.name.clone()).collect();
    assert!(names.iter().any(|n| n == "add_rows"), "{names:?}");
    assert!(names.iter().any(|n| n == "patch_row"), "{names:?}");
    assert!(names.iter().any(|n| n == "drop_row"), "{names:?}");

    // カスタム名はツール一覧だけでなくプロンプト本文にも反映される
    let prompt = session.system_prompt();
    assert!(!prompt.contains(" update "), "{prompt}");
    assert!(prompt.contains("patch_row を使い"), "{prompt}");
}

#[test]
fn system_prompt_contains_guide() {
    let object = new_session().system_prompt();
    assert!(object.contains("【保存する JSON の形】"), "{object}");
    assert!(object.contains("- name: string  (必須)"), "{object}");
    assert!(object.contains("- employees: integer"), "{object}");

    let array = array_session().system_prompt();
    assert!(array.contains("【要素の形】"), "{array}");
    assert!(array.contains("append の items"), "{array}");
    assert!(array.contains("- name: string  (必須)"), "{array}");
    assert!(array.contains("- note: string  (必須)"), "{array}");
}

#[test]
fn with_schema_guide_false_excludes_guide() {
    let with_guide = new_session().system_prompt();
    let without_guide = new_session().with_schema_guide(false).system_prompt();

    assert!(
        with_guide.contains("【保存する JSON の形】"),
        "{with_guide}"
    );
    assert!(
        !without_guide.contains("【保存する JSON の形】"),
        "{without_guide}"
    );
    assert!(!without_guide.contains("- name: string"), "{without_guide}");
    // The rest of the prompt is unchanged.
    assert!(without_guide.contains("【保存の手順】"), "{without_guide}");
    assert!(without_guide.contains("【言語】"), "{without_guide}");
    assert!(without_guide.len() < with_guide.len());
}

#[derive(Debug, Deserialize, JsonSchema)]
#[allow(dead_code)]
struct DescribedCompany {
    /// 会社名です。とても長い説明がここに続きます。
    name: String,
}

#[test]
fn with_guide_options_changes_output() {
    let session = FillSession::<DescribedCompany>::new("調べて").expect("session");
    let full = session.system_prompt();
    assert!(full.contains("とても長い説明"), "{full}");

    let session = session.with_guide_options(GuideOptions {
        max_description_chars: 10,
        max_fields: None,
    });
    assert_eq!(session.guide_options().max_description_chars, 10);
    let truncated = session.system_prompt();
    assert!(truncated.contains("会社名です。…"), "{truncated}");
    assert!(!truncated.contains("とても"), "{truncated}");
}

#[tokio::test]
async fn reject_lists_allowed_keys_when_nothing_is_similar() {
    let mut session = new_session();
    let result = session
        .call("save_result", serde_json::json!({"name": "x", "zzzzzz": 1}))
        .await;
    let content = match result {
        SessionResult::Continue { content, .. } => content,
        SessionResult::Done { content } => panic!("expected continue, got done: {content}"),
        SessionResult::Impossible { reason, .. } => {
            panic!("expected continue, got impossible: {reason}")
        }
    };
    assert!(content.contains("`zzzzzz` は使えません"), "{content}");
    assert!(content.contains("使えるキー: employees, name"), "{content}");
    assert!(
        content.contains("この呼び出しは保存していません"),
        "{content}"
    );
    assert!(!content.contains("末尾の `.`"), "{content}");
}

// --- 修正 06: ゴール項目（with_goal_fields） ---

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct GoalProfile {
    name: Option<String>,
    employees: Option<u32>,
}

fn goal_profile_session() -> FillSession<GoalProfile> {
    FillSession::new("会社を調べて").expect("session")
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct GoalRow {
    market_key: String,
    sales_share: Option<String>,
}

fn goal_array_session(fields: &[&str]) -> FillSession<Vec<GoalRow>> {
    FillSession::new("行を埋めて")
        .expect("session")
        .with_goal_fields(fields.iter().copied())
        .expect("goals")
}

#[tokio::test]
async fn goal_field_blocks_done_object() {
    let mut session = goal_profile_session()
        .with_goal_fields(["employees"])
        .expect("goals");
    assert_eq!(session.goal_fields().len(), 1);
    assert_eq!(session.goal_fields()[0].field(), "employees");

    match session.call("save_result", json!({"name": "トヨタ"})).await {
        SessionResult::Continue { content, is_error } => {
            assert!(!is_error, "{content}");
            assert!(content.contains("JSONはまだ完成していません"), "{content}");
            assert!(content.contains("- employees"), "{content}");
            assert!(
                content.contains("save_result に employees を入れて再送してください"),
                "{content}"
            );
        }
        SessionResult::Done { content } => panic!("goal must block done: {content}"),
        SessionResult::Impossible { reason, .. } => panic!("unexpected impossible: {reason}"),
    }
    assert!(!session.is_done());
    // 部分コミットはする。
    assert_eq!(session.snapshot(), &json!({"name": "トヨタ"}));
}

#[tokio::test]
async fn goal_field_allows_done_when_filled() {
    let mut session = goal_profile_session()
        .with_goal_fields(["employees"])
        .expect("goals");
    match session
        .call(
            "save_result",
            json!({"name": "トヨタ", "employees": 370000}),
        )
        .await
    {
        SessionResult::Done { content } => assert!(content.contains("保存しました"), "{content}"),
        SessionResult::Continue { content, .. } => panic!("expected done: {content}"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }
    let outcome = session.into_outcome().expect("outcome");
    assert_eq!(
        outcome.value,
        GoalProfile {
            name: Some("トヨタ".to_string()),
            employees: Some(370000),
        }
    );
}

#[tokio::test]
async fn goal_every_element_requires_all_rows() {
    let mut session = goal_array_session(&["[].sales_share"]);
    match session
        .call(
            "append",
            json!({"items": [
                {"market_key": "a", "sales_share": "10%"},
                {"market_key": "b"}
            ]}),
        )
        .await
    {
        SessionResult::Continue { content, .. } => {
            assert!(content.contains("- [1].sales_share"), "{content}");
            assert!(!content.contains("- [0].sales_share"), "{content}");
            assert!(
                content.contains("update で index 1 の patch に sales_share を入れてください"),
                "{content}"
            );
        }
        SessionResult::Done { content } => panic!("goal must block done: {content}"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }
    assert_eq!(
        session.snapshot(),
        &json!([{"market_key": "a", "sales_share": "10%"}, {"market_key": "b"}])
    );

    match session
        .call(
            "update",
            json!({"index": 1, "patch": {"sales_share": "20%"}}),
        )
        .await
    {
        SessionResult::Done { content } => assert!(content.contains("2件"), "{content}"),
        SessionResult::Continue { content, .. } => panic!("expected done: {content}"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }
}

#[tokio::test]
async fn goal_empty_string_counts_as_missing() {
    let mut session = goal_profile_session()
        .with_goal_fields(["name"])
        .expect("goals");
    match session.call("save_result", json!({"name": ""})).await {
        SessionResult::Continue { content, .. } => {
            assert!(content.contains("- name"), "{content}");
            assert!(!session.is_done());
        }
        SessionResult::Done { content } => panic!("`\"\"` must be missing: {content}"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }
}

#[tokio::test]
async fn goal_blank_string_counts_as_present() {
    let mut session = goal_profile_session()
        .with_goal_fields(["name"])
        .expect("goals");
    match session.call("save_result", json!({"name": " "})).await {
        SessionResult::Done { content } => assert!(content.contains("保存しました"), "{content}"),
        SessionResult::Continue { content, .. } => panic!("`\" \"` must be present: {content}"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }
}

#[tokio::test]
async fn goal_empty_zero_elements_pass_by_default() {
    let mut session = goal_array_session(&["[].sales_share"]);
    assert!(matches!(
        session
            .call("append", json!({"items": [{"market_key": "a"}]}))
            .await,
        SessionResult::Continue { .. }
    ));
    // 最後の要素を消すと 0 件。既定は Pass なので完了できる。
    match session.call("remove", json!({"index": 0})).await {
        SessionResult::Done { content } => assert!(content.contains("0件"), "{content}"),
        SessionResult::Continue { content, .. } => panic!("expected done: {content}"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }
    assert_eq!(session.snapshot(), &json!([]));
}

#[tokio::test]
async fn goal_empty_fail_requires_one_element() {
    let mut session = goal_array_session(&["[].sales_share"]).with_goal_empty(GoalEmpty::Fail);
    assert!(matches!(
        session
            .call("append", json!({"items": [{"market_key": "a"}]}))
            .await,
        SessionResult::Continue { .. }
    ));
    match session.call("remove", json!({"index": 0})).await {
        SessionResult::Continue { content, is_error } => {
            assert!(!is_error, "{content}");
            assert!(content.contains("要素が 1 件もありません"), "{content}");
            assert!(
                content.contains("append で1件以上を追加してください"),
                "{content}"
            );
        }
        SessionResult::Done { content } => panic!("Fail must block zero rows: {content}"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }
    assert!(!session.is_done());
    assert_eq!(session.snapshot(), &json!([]));
}

#[test]
fn invalid_goal_path_is_an_error() {
    for spec in ["a.b", "[0].x", "[]x", "[]", "", "employees[0]"] {
        match goal_profile_session().with_goal_fields([spec]) {
            Err(FillError::InvalidGoalPath { path }) => assert_eq!(path, spec),
            Ok(_) => panic!("{spec:?} must be rejected"),
            Err(other) => panic!("{spec:?}: unexpected error {other}"),
        }
    }

    for spec in ["sales_share", "[].a.b", "[].", "[].[].x"] {
        let session = FillSession::<Vec<GoalRow>>::new("行を埋めて").expect("session");
        match session.with_goal_fields([spec]) {
            Err(FillError::InvalidGoalPath { path }) => assert_eq!(path, spec),
            Ok(_) => panic!("{spec:?} must be rejected"),
            Err(other) => panic!("{spec:?}: unexpected error {other}"),
        }
    }
}

#[test]
fn unknown_goal_field_is_an_error() {
    match goal_profile_session().with_goal_fields(["nope"]) {
        Err(FillError::UnknownGoalField { path }) => assert_eq!(path, "nope"),
        Ok(_) => panic!("unknown field must be rejected"),
        Err(other) => panic!("unexpected error {other}"),
    }
    match FillSession::<Vec<GoalRow>>::new("行を埋めて")
        .expect("session")
        .with_goal_fields(["[].nope"])
    {
        Err(FillError::UnknownGoalField { path }) => assert_eq!(path, "[].nope"),
        Ok(_) => panic!("unknown field must be rejected"),
        Err(other) => panic!("unexpected error {other}"),
    }
    // 要素のフィールドを素の名前で指定しても、ルート種別が違うので InvalidGoalPath。
    match FillSession::<Vec<GoalRow>>::new("行を埋めて")
        .expect("session")
        .with_goal_fields(["market_key"])
    {
        Err(FillError::InvalidGoalPath { path }) => assert_eq!(path, "market_key"),
        Ok(_) => panic!("bare field in array mode must be rejected"),
        Err(other) => panic!("unexpected error {other}"),
    }
}

#[tokio::test]
async fn update_fixes_appended_element_after_partial_commit() {
    let mut session = goal_array_session(&["[].sales_share"]);
    match session
        .call("append", json!({"items": [{"market_key": "steel"}]}))
        .await
    {
        SessionResult::Continue { content, .. } => {
            assert!(content.contains("追加しました"), "{content}");
            assert!(content.contains("- [0].sales_share"), "{content}");
        }
        SessionResult::Done { content } => panic!("goal must block done: {content}"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }
    // ゴール未達でも append はコミットされている。
    assert_eq!(session.snapshot(), &json!([{"market_key": "steel"}]));

    match session
        .call(
            "update",
            json!({"index": 0, "patch": {"sales_share": "30%"}}),
        )
        .await
    {
        SessionResult::Done { content } => assert!(content.contains("1件"), "{content}"),
        SessionResult::Continue { content, .. } => panic!("update must finish: {content}"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }

    let outcome = session.into_outcome().expect("outcome");
    assert_eq!(outcome.value[0].sales_share.as_deref(), Some("30%"));
    assert_eq!(outcome.save_calls, 2);
}

#[tokio::test]
async fn goal_feedback_says_update_not_append() {
    let mut session = goal_array_session(&["[].sales_share"]);
    match session
        .call(
            "append",
            json!({"items": [{"market_key": "a"}, {"market_key": "b"}]}),
        )
        .await
    {
        SessionResult::Continue { content, .. } => {
            assert!(
                content.contains("update で index 0 と 1 の patch に sales_share を入れてください"),
                "{content}"
            );
            assert!(
                content.contains("append すると要素が増えるので使わないでください"),
                "{content}"
            );
        }
        SessionResult::Done { content } => panic!("goal must block done: {content}"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }
}

#[tokio::test]
async fn goal_feedback_uses_custom_tool_names() {
    let names = ai_json_filler_core::ArrayToolNames {
        append: "add_rows".to_string(),
        update: "patch_row".to_string(),
        remove: "drop_row".to_string(),
    };
    let mut session = goal_array_session(&["[].sales_share"]).with_array_tool_names(names);
    match session
        .call("add_rows", json!({"items": [{"market_key": "a"}]}))
        .await
    {
        SessionResult::Continue { content, .. } => {
            assert!(content.contains("patch_row で index 0"), "{content}");
            assert!(content.contains("add_rows すると要素が増える"), "{content}");
            assert!(!content.contains("update で"), "{content}");
        }
        SessionResult::Done { content } => panic!("goal must block done: {content}"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }
}

#[tokio::test]
async fn progress_feedback_lists_missing_goals() {
    let mut session = goal_array_session(&["[].sales_share"]);
    assert!(matches!(
        session
            .call("append", json!({"items": [{"market_key": "a"}]}))
            .await,
        SessionResult::Continue { .. }
    ));
    // 検証は通るがゴール未達の状態で、ツール無し応答への差し戻し。
    assert!(session.snapshot().as_array().is_some());
    let feedback = session.progress_feedback();
    assert!(
        feedback.contains("JSONはまだ完成していません"),
        "{feedback}"
    );
    assert!(feedback.contains("[0].sales_share"), "{feedback}");
    assert!(feedback.contains("update で index 0"), "{feedback}");
    assert!(feedback.contains("append すると要素が増える"), "{feedback}");
}

#[tokio::test]
async fn goal_feedback_limits_missing_lines() {
    let mut session = goal_array_session(&["[].sales_share"]);
    let items: Vec<Value> = (0..7)
        .map(|index| json!({"market_key": format!("k{index}")}))
        .collect();
    match session.call("append", json!({"items": items})).await {
        SessionResult::Continue { content, .. } => {
            assert!(content.contains("- [4].sales_share"), "{content}");
            assert!(!content.contains("- [5].sales_share"), "{content}");
            assert!(content.contains("…他 2 件"), "{content}");
            assert!(
                content.contains("update で不足している要素の patch に値を入れてください"),
                "{content}"
            );
            assert!(
                content.contains("append すると要素が増えるので使わないでください"),
                "{content}"
            );
        }
        SessionResult::Done { content } => panic!("goal must block done: {content}"),
        SessionResult::Impossible { .. } => panic!("unexpected impossible"),
    }
}
