use super::{describe::fallback_message, text::display_path, *};

use jsonschema::paths::{Location, LocationSegment};
use serde_json::{Value, json};

fn feedback(schema: &Value, instance: &Value) -> String {
    feedback_with_removed(schema, instance, &[])
}

fn feedback_with_removed(
    schema: &Value,
    instance: &Value,
    removed: &[(Location, String)],
) -> String {
    let validator = jsonschema::validator_for(schema).expect("schema must be valid");
    let errors: Vec<_> = validator.iter_errors(instance).collect();
    format_validation_feedback(&errors, instance, removed, &FeedbackOptions::default())
}

fn extras_of(schema: &Value, instance: &mut Value) -> Vec<(Location, String)> {
    let validator = jsonschema::validator_for(schema).expect("schema must be valid");
    let extras = {
        let errors: Vec<_> = validator.iter_errors(instance).collect();
        collect_extra_keys(&errors)
    };
    strip_extra_keys(instance, &extras)
}

fn loc(segments: &[LocationSegment<'_>]) -> Location {
    let mut out = Location::new();
    for segment in segments {
        out = out.join(segment.clone());
    }
    out
}

// --- S1: フォーマッタ ---

#[test]
fn formats_missing() {
    let schema = json!({
        "type": "object",
        "properties": { "title": {"type": "string"} },
        "required": ["title"]
    });
    let text = feedback(&schema, &json!({}));
    assert!(text.contains("- (root): title"), "text:\n{text}");
    assert!(text.contains("[不足]"), "text:\n{text}");
}

#[test]
fn formats_type_error() {
    let schema = json!({
        "type": "object",
        "properties": { "n": {"type": "number"} }
    });
    let text = feedback(&schema, &json!({ "n": "x" }));
    assert!(
        text.contains("- n: 数値(number)が必要です"),
        "text:\n{text}"
    );
    assert!(text.contains("[値]"), "text:\n{text}");
}

#[test]
fn groups_extra_keys() {
    let schema = json!({
        "type": "object",
        "properties": { "title": {"type": "string"} },
        "additionalProperties": false
    });
    let mut instance = json!({ "title": "x", "extra": 1 });
    let removed = extras_of(&schema, &mut instance);
    let out = feedback_with_removed(&schema, &instance, &removed);
    assert!(out.contains("[余計]"), "out:\n{out}");
    assert!(out.contains("余計"), "out:\n{out}");
    assert!(out.contains("extra"), "out:\n{out}");
    assert!(out.contains("保存済み: title"), "out:\n{out}");
}

#[test]
fn respects_max_errors() {
    let mut props: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut required: Vec<String> = Vec::new();
    for i in 0..20 {
        let name = format!("p{i}");
        required.push(name.clone());
        props.insert(name, json!({ "type": "string" }));
    }
    let schema = json!({ "type": "object", "properties": props, "required": required });

    let out = feedback(&schema, &json!({}));
    assert!(out.contains("（20件）"), "out:\n{out}");
    assert!(out.contains("…他 8 件"), "out:\n{out}");
    assert_eq!(out.matches("- (root): ").count(), 1, "out:\n{out}");
}

#[test]
fn respects_max_chars() {
    let props: serde_json::Map<String, Value> = (0..20)
        .map(|i| (format!("p{i}"), json!({ "type": "number" })))
        .collect();
    let instance: serde_json::Map<String, Value> = (0..20)
        .map(|i| (format!("p{i}"), json!("x".repeat(200))))
        .collect();
    let schema = json!({ "type": "object", "properties": props });
    let validator = jsonschema::validator_for(&schema).expect("schema must be valid");
    let instance = Value::Object(instance);
    let errors: Vec<_> = validator.iter_errors(&instance).collect();
    let opts = FeedbackOptions {
        max_chars: 400,
        ..FeedbackOptions::default()
    };
    let text = format_validation_feedback(&errors, &instance, &[], &opts);
    assert!(text.chars().count() <= 400, "len: {}", text.chars().count());
    assert!(text.contains("…他"), "text:\n{text}");
}

#[test]
fn saved_line_sorted() {
    let text = feedback(
        &json!({ "type": "object", "properties": { "a": { "type": "string" } } }),
        &json!({ "a": "x" }),
    );
    assert!(text.contains("保存済み"), "text:\n{text}");
    assert!(text.contains("a"), "text:\n{text}");
}

#[test]
fn strip_root_extra() {
    // --- S3: 余計キーの除去 ---
    let schema = json!({
        "type": "object",
        "properties": { "title": {"type": "string"} },
        "additionalProperties": false
    });
    let mut instance = json!({ "title": "x", "foo": 1 });
    let removed = extras_of(&schema, &mut instance);
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].1, "foo");
    assert_eq!(instance, json!({ "title": "x" }));
}

#[test]
fn display_text_root_and_array() {
    assert_eq!(display_path(&Location::new()), "(root)");
    let path = loc(&[
        LocationSegment::Property("title".into()),
        LocationSegment::Index(0),
        LocationSegment::Property("nested".into()),
    ]);
    assert_eq!(display_path(&path), "title[0].nested");
}

#[test]
fn fallback_raw() {
    let text = fallback_message("someKeyword", "raw english message");
    assert!(text.contains("someKeyword"));
    assert!(text.contains("raw english message"));
}
