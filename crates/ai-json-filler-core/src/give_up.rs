use serde_json::{Value, json};

use crate::ToolSpec;

/// Default name of the tool the model calls to declare the goal unreachable.
pub const DEFAULT_GIVE_UP_TOOL_NAME: &str = "give_up";

/// Reason and missing fields reported by the model when it gives up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GiveUp {
    pub reason: String,
    pub missing_fields: Vec<String>,
}

pub(crate) const DESCRIPTION: &str = "これ以上調査しても必須項目を埋められないと判断したときに呼ぶ。\
     reason に不可能な理由を、missing_fields に埋められなかった項目を書く。\
     推測で値を埋めるより、このツールで正直に報告する。";

pub(crate) fn spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: name.to_string(),
        description: DESCRIPTION.to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "minLength": 1,
                    "description": "目標を達成できない理由（日本語・1〜2文）"
                },
                "missing_fields": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "埋められなかったフィールドのパス（例: employees）"
                }
            },
            "required": ["reason"],
            "additionalProperties": false
        }),
    }
}

pub(crate) fn parse(args: &Value) -> Result<GiveUp, String> {
    let obj = args
        .as_object()
        .ok_or_else(|| "引数は JSON オブジェクトで送ってください。".to_string())?;
    let reason = obj
        .get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if reason.is_empty() {
        return Err(
            "reason が空です。不可能と判断した理由を日本語で書いて再送してください。".to_string(),
        );
    }
    let missing_fields = obj
        .get("missing_fields")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok(GiveUp {
        reason: reason.to_string(),
        missing_fields,
    })
}
