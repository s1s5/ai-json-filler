//! Tools used in array mode: `append`, `update`, and `remove`.
//!
//! Array mode keeps the accumulated value as an array of flat objects. Instead
//! of replacing the whole array on every save, the model mutates it through
//! three narrow tools. Each tool takes a JSON object, as most providers expect.

use serde_json::{Value, json};

use crate::{FillSchema, ToolSpec};

pub const DEFAULT_APPEND_TOOL_NAME: &str = "append";
pub const DEFAULT_UPDATE_TOOL_NAME: &str = "update";
pub const DEFAULT_REMOVE_TOOL_NAME: &str = "remove";

pub(crate) const APPEND_DESCRIPTION: &str = "配列の末尾に要素を追加する。\
     items には1件以上の要素を入れる。要素は部分的でよく、必須フィールドが\
     揃っていなくてもよい。既存の要素は変更しない。";

pub(crate) const UPDATE_DESCRIPTION: &str = "指定した index の要素を部分更新する。\
     patch に渡したフィールドだけが既存の要素にマージされる。\
     配列全体を送り直す必要はない。";

pub(crate) const REMOVE_DESCRIPTION: &str = "指定した index の要素を削除する。\
     削除後、それより後ろの要素の index は1つ前に詰まる。";

/// Names of the three tools exposed in array mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayToolNames {
    pub append: String,
    pub update: String,
    pub remove: String,
}

impl Default for ArrayToolNames {
    fn default() -> Self {
        Self {
            append: DEFAULT_APPEND_TOOL_NAME.to_string(),
            update: DEFAULT_UPDATE_TOOL_NAME.to_string(),
            remove: DEFAULT_REMOVE_TOOL_NAME.to_string(),
        }
    }
}

impl ArrayToolNames {
    /// Trim every name. Returns `None` when a name is empty or two names
    /// collide, so the caller can keep its current names.
    pub(crate) fn normalized(self) -> Option<Self> {
        let append = self.append.trim().to_string();
        let update = self.update.trim().to_string();
        let remove = self.remove.trim().to_string();
        if append.is_empty() || update.is_empty() || remove.is_empty() {
            return None;
        }
        if append == update || append == remove || update == remove {
            return None;
        }
        Some(Self {
            append,
            update,
            remove,
        })
    }

    pub(crate) fn all(&self) -> [&str; 3] {
        [&self.append, &self.update, &self.remove]
    }
}

/// Build the three tool specs. `partial` is the partial schema of the array
/// (root `type: array`); its `items` is reused as the element/patch schema.
pub(crate) fn specs(schema: &FillSchema, names: &ArrayToolNames) -> Vec<ToolSpec> {
    let partial = schema.partial();
    let items = partial
        .get("items")
        .cloned()
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));

    let append = with_defs(
        json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "minItems": 1,
                    "items": items.clone()
                }
            },
            "required": ["items"],
            "additionalProperties": false
        }),
        partial,
    );
    let update = with_defs(
        json!({
            "type": "object",
            "properties": {
                "index": { "type": "integer", "minimum": 0 },
                "patch": items
            },
            "required": ["index", "patch"],
            "additionalProperties": false
        }),
        partial,
    );
    let remove = json!({
        "type": "object",
        "properties": {
            "index": { "type": "integer", "minimum": 0 }
        },
        "required": ["index"],
        "additionalProperties": false
    });

    vec![
        ToolSpec {
            name: names.append.clone(),
            description: APPEND_DESCRIPTION.to_string(),
            parameters: append,
        },
        ToolSpec {
            name: names.update.clone(),
            description: UPDATE_DESCRIPTION.to_string(),
            parameters: update,
        },
        ToolSpec {
            name: names.remove.clone(),
            description: REMOVE_DESCRIPTION.to_string(),
            parameters: remove,
        },
    ]
}

/// If the partial schema keeps its item object in `$defs` (or draft-07
/// `definitions`), copy those definitions into the tool schema root so the
/// `$ref` still resolves.
fn with_defs(mut schema: Value, partial: &Value) -> Value {
    if let Some(obj) = schema.as_object_mut() {
        for key in ["$defs", "definitions"] {
            if let Some(defs) = partial.get(key) {
                obj.insert(key.to_string(), defs.clone());
            }
        }
    }
    schema
}

/// A parsed tool argument plus the keys the wrapper does not accept.
///
/// The wrapper schemas declare `additionalProperties: false`, so unknown keys
/// are reported to the model instead of being dropped silently.
pub(crate) struct Parsed<T> {
    pub value: T,
    pub extras: Vec<String>,
}

impl<T> Parsed<T> {
    fn new(value: T, extras: Vec<String>) -> Self {
        Self { value, extras }
    }
}

pub(crate) fn parse_append(args: &Value) -> Result<Parsed<Vec<Value>>, String> {
    let obj = args
        .as_object()
        .ok_or_else(|| "引数は JSON オブジェクトで送ってください。".to_string())?;
    let extras = extra_keys(obj, &["items"]);
    let items = obj
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "items は JSON 配列で送ってください。".to_string())?;
    if items.is_empty() {
        return Err("items が空です。1件以上の要素を送ってください。".to_string());
    }
    if items.iter().any(|item| !item.is_object()) {
        return Err("items の各要素は JSON オブジェクトで送ってください。".to_string());
    }
    Ok(Parsed::new(items.clone(), extras))
}

pub(crate) fn parse_update(args: &Value) -> Result<Parsed<(usize, Value)>, String> {
    let obj = args
        .as_object()
        .ok_or_else(|| "引数は JSON オブジェクトで送ってください。".to_string())?;
    let extras = extra_keys(obj, &["index", "patch"]);
    let index = parse_index(obj)?;
    let patch = obj
        .get("patch")
        .and_then(Value::as_object)
        .ok_or_else(|| "patch は JSON オブジェクトで送ってください。".to_string())?;
    if patch.is_empty() {
        return Err("patch が空です。更新するフィールドを送ってください。".to_string());
    }
    Ok(Parsed::new((index, Value::Object(patch.clone())), extras))
}

pub(crate) fn parse_remove(args: &Value) -> Result<Parsed<usize>, String> {
    let obj = args
        .as_object()
        .ok_or_else(|| "引数は JSON オブジェクトで送ってください。".to_string())?;
    let extras = extra_keys(obj, &["index"]);
    let index = parse_index(obj)?;
    Ok(Parsed::new(index, extras))
}

/// Keys of the wrapper argument that the tool does not declare.
fn extra_keys(obj: &serde_json::Map<String, Value>, allowed: &[&str]) -> Vec<String> {
    let mut extras: Vec<String> = obj
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .cloned()
        .collect();
    extras.sort();
    extras
}

/// Feedback for a wrapper call that carried undeclared keys.
pub(crate) fn extra_args_message(tool: &str, extras: &[String], allowed: &str) -> String {
    format!(
        "{tool} の引数に使えないキーがあります: {}（使えるのは {allowed} だけです）",
        extras.join(", ")
    )
}

fn parse_index(obj: &serde_json::Map<String, Value>) -> Result<usize, String> {
    let raw = obj
        .get("index")
        .ok_or_else(|| "index を指定してください。".to_string())?;
    let index = raw
        .as_u64()
        .ok_or_else(|| "index は0以上の整数で送ってください。".to_string())?;
    usize::try_from(index).map_err(|_| "index が大きすぎます。".to_string())
}

pub(crate) fn range_error(index: usize, len: usize) -> String {
    format!("VALIDATION_ERROR: index {index} は範囲外です（現在 {len} 件）。")
}

pub(crate) fn appended_message(first: usize, count: usize, len: usize) -> String {
    if count == 1 {
        format!("追加しました（[{first}]、現在{len}件）")
    } else {
        let last = first + count - 1;
        format!("追加しました（[{first}]〜[{last}]、現在{len}件）")
    }
}

pub(crate) fn updated_message(index: usize) -> String {
    format!("更新しました（[{index}]）")
}

pub(crate) fn removed_message(index: usize, len: usize) -> String {
    format!("削除しました（[{index}]、現在{len}件）")
}
