use jsonschema::JsonType;
use jsonschema::paths::Location;
use serde_json::Value;

use super::FeedbackOptions;

pub(super) use crate::schema::display_path;

pub(super) fn depth(loc: &Location) -> usize {
    loc.iter().count()
}

pub(super) fn type_label(t: JsonType) -> &'static str {
    match t {
        JsonType::Null => "ヌル(null)",
        JsonType::Boolean => "真偽値(boolean)",
        JsonType::Integer => "整数(integer)",
        JsonType::Number => "数値(number)",
        JsonType::String => "文字列(string)",
        JsonType::Array => "配列(array)",
        JsonType::Object => "オブジェクト(object)",
    }
}

fn actual_type_label(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "真偽値(boolean)",
        Value::Number(_) => "数値(number)",
        Value::String(_) => "文字列(string)",
        Value::Array(_) => "配列(array)",
        Value::Object(_) => "オブジェクト(object)",
    }
}

/// 「実際の型 + プレビュー」
pub(super) fn actual(value: &Value, opts: &FeedbackOptions) -> String {
    match value {
        Value::Null => "null".to_string(),
        other => format!(
            "{} {}",
            actual_type_label(other),
            value_preview(other, opts.max_value_chars)
        ),
    }
}

pub(super) fn value_preview(value: &Value, limit: usize) -> String {
    let raw = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
    truncate_chars(&raw, limit)
}

pub(super) fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push('…');
    out
}

/// 数値リミットの表示（整数なら小数点を出さない）
pub(super) fn scalar(value: &Value) -> String {
    value.to_string()
}

pub(super) fn str_len(value: &Value) -> usize {
    value.as_str().map(|s| s.chars().count()).unwrap_or(0)
}

pub(super) fn array_len(value: &Value) -> usize {
    value.as_array().map(|a| a.len()).unwrap_or(0)
}

pub(super) fn object_len(value: &Value) -> usize {
    value.as_object().map(|o| o.len()).unwrap_or(0)
}

pub(super) fn property_label(property: &Value) -> String {
    match property {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub(super) fn saved_line(accumulated: &Value) -> String {
    let mut keys: Vec<&str> = accumulated
        .as_object()
        .map(|obj| obj.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    if keys.is_empty() {
        "保存済み: なし".to_string()
    } else {
        format!("保存済み: {}", keys.join(", "))
    }
}
