use serde_json::Value;

use crate::prompt::PromptSection;
use crate::validation::ValidationIssue;

#[derive(Debug, thiserror::Error)]
pub enum FillError {
    #[error("root schema must be a flat object or an array of flat objects")]
    UnsupportedRootSchema,
    #[error("unsupported schema shape at {path}: {reason}")]
    UnsupportedSchemaShape { path: String, reason: String },
    #[error("invalid schema: {0}")]
    Schema(String),
    #[error("invalid seed: {issues:?}")]
    InvalidSeed { issues: Vec<ValidationIssue> },
    #[error("invalid goal path: {path}（`field` または `[].field` の形で指定してください）")]
    InvalidGoalPath { path: String },
    #[error("unknown or unavailable prompt placeholder {placeholder} in section '{section}'")]
    UnknownPromptPlaceholder {
        section: PromptSection,
        placeholder: String,
    },
    #[error("prompt section '{section}' must contain {{fields}}")]
    MissingFieldsPlaceholder { section: PromptSection },
    #[error("tool name '{name}' is not usable: {reason}")]
    InvalidToolName { name: String, reason: String },
    #[error("unknown goal field: {path}（スキーマに存在しません）")]
    UnknownGoalField { path: String },
    #[error("fill is not complete")]
    Incomplete { partial: Value },
    #[error("目標達成は不可能と判断されました: {reason}")]
    Impossible {
        reason: String,
        missing_fields: Vec<String>,
        partial: Value,
    },
    #[error("failed to deserialize: {0}")]
    Deserialize(#[from] serde_json::Error),
    #[error("unknown tool: {name}")]
    UnknownTool { name: String },
    #[error("tool '{name}' failed: {message}")]
    Tool { name: String, message: String },
    #[error("LLM call failed: {0}")]
    Llm(String),
}
