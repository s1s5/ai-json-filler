use std::marker::PhantomData;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::feedback::{FeedbackOptions, empty_args_feedback, format_validation_feedback};
use crate::give_up::{self, DEFAULT_GIVE_UP_TOOL_NAME, GiveUp};
use crate::prompt::{SAVE_DESCRIPTION, build_system_prompt};
use crate::schema::{collect_extra_keys, deep_merge, strip_extra_keys};
use crate::{DEFAULT_SAVE_TOOL_NAME, FillError, FillSchema, Tool, ToolSet, ToolSpec};

pub enum SessionResult {
    Continue {
        content: String,
        is_error: bool,
    },
    Done {
        content: String,
    },
    Impossible {
        reason: String,
        missing_fields: Vec<String>,
    },
}

pub struct FillOutcome<T> {
    pub value: T,
    pub raw: Value,
    pub warnings: Vec<String>,
    pub save_calls: usize,
}

pub struct FillSession<T> {
    schema: FillSchema,
    prompt: String,
    accumulated: Value,
    tools: ToolSet,
    save_tool_name: String,
    give_up: Option<GiveUp>,
    give_up_tool_name: String,
    give_up_min_tool_calls: Option<usize>,
    tool_calls: usize,
    feedback: FeedbackOptions,
    warnings: Vec<String>,
    save_calls: usize,
    done: bool,
    _marker: PhantomData<fn() -> T>,
}

impl<T> FillSession<T>
where
    T: DeserializeOwned + JsonSchema,
{
    pub fn new(prompt: impl Into<String>) -> Result<Self, FillError> {
        Self::with_seed(prompt, Value::Object(Default::default()))
    }

    pub fn with_seed(prompt: impl Into<String>, seed: Value) -> Result<Self, FillError> {
        let schema = FillSchema::for_type::<T>()?;
        if let Err(issues) = schema.validate_seed(&seed) {
            return Err(FillError::InvalidSeed { issues });
        }
        Ok(Self {
            schema,
            prompt: prompt.into(),
            accumulated: seed,
            tools: ToolSet::new(),
            save_tool_name: DEFAULT_SAVE_TOOL_NAME.to_string(),
            give_up: None,
            give_up_tool_name: DEFAULT_GIVE_UP_TOOL_NAME.to_string(),
            give_up_min_tool_calls: None,
            tool_calls: 0,
            feedback: FeedbackOptions::default(),
            warnings: Vec::new(),
            save_calls: 0,
            done: false,
            _marker: PhantomData,
        })
    }

    pub fn with_feedback_options(mut self, options: FeedbackOptions) -> Self {
        self.feedback = options;
        self
    }

    /// Override the name of the tool used for partial saves.
    ///
    /// Empty or whitespace-only names are ignored, and so is a name already
    /// used by the give-up tool.
    pub fn with_save_tool_name(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        let trimmed = name.trim();
        if !trimmed.is_empty() && trimmed != self.give_up_tool_name {
            self.save_tool_name = trimmed.to_string();
        }
        self
    }

    pub fn save_tool_name(&self) -> &str {
        &self.save_tool_name
    }

    /// Override the name of the tool that declares the goal unreachable.
    ///
    /// Empty or whitespace-only names are ignored, and so is a name already
    /// used by the save tool.
    pub fn with_give_up_tool_name(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        let trimmed = name.trim();
        if !trimmed.is_empty() && trimmed != self.save_tool_name {
            self.give_up_tool_name = trimmed.to_string();
        }
        self
    }

    pub fn give_up_tool_name(&self) -> &str {
        &self.give_up_tool_name
    }

    /// Require at least `calls` investigation tool calls before a give-up is
    /// accepted. `Some(0)` disables the gate.
    ///
    /// When unset, the gate is 0 with no registered tools and 1 otherwise.
    pub fn with_give_up_after_tool_calls(mut self, calls: usize) -> Self {
        self.give_up_min_tool_calls = Some(calls);
        self
    }

    pub fn add_tool(&mut self, tool: Arc<dyn Tool>) -> &mut Self {
        self.tools.insert(tool);
        self
    }

    pub fn system_prompt(&self) -> String {
        build_system_prompt(
            &self.prompt,
            &self.save_tool_name,
            &self.give_up_tool_name,
            &self.tools,
        )
    }

    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        let mut specs = vec![ToolSpec {
            name: self.save_tool_name.clone(),
            description: SAVE_DESCRIPTION.to_string(),
            parameters: self.schema.partial().clone(),
        }];
        let give_up_spec = give_up::spec(&self.give_up_tool_name);
        if give_up_spec.name != self.save_tool_name {
            specs.push(give_up_spec);
        }
        for spec in self.tools.specs() {
            if spec.name != self.save_tool_name && spec.name != self.give_up_tool_name {
                specs.push(spec);
            }
        }
        specs
    }

    /// Whether the session reached a terminal state (saved or given up).
    pub fn is_done(&self) -> bool {
        self.done || self.give_up.is_some()
    }

    /// Whether the model declared the goal unreachable.
    pub fn is_aborted(&self) -> bool {
        self.give_up.is_some()
    }

    /// The give-up declaration accepted by the session, if any.
    pub fn give_up_record(&self) -> Option<&GiveUp> {
        self.give_up.as_ref()
    }

    pub fn snapshot(&self) -> &Value {
        &self.accumulated
    }
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Feedback for a turn that ended without a tool call.
    ///
    /// Always states that the JSON is still incomplete and lists the current
    /// validation issues, so the model knows why it must call the save tool.
    pub fn progress_feedback(&self) -> String {
        match self.schema.validate(&self.accumulated) {
            Ok(()) => format!(
                "JSONはまだ完了していません。{} を呼び出して結果を保存してください。",
                self.save_tool_name
            ),
            Err(issues) => {
                let lines: Vec<String> = issues
                    .iter()
                    .map(|issue| format!("- {}: {} ({})", issue.path, issue.keyword, issue.message))
                    .collect();
                format!(
                    "JSONはまだ埋まっていません。現在の検証エラー:\n{}\n{} を呼び出して、不足分を保存してください。\nどうしても埋められない場合は {} を呼んで理由を報告してください。",
                    lines.join("\n"),
                    self.save_tool_name,
                    self.give_up_tool_name
                )
            }
        }
    }

    pub async fn call(&mut self, name: &str, args: Value) -> SessionResult {
        if self.done {
            return SessionResult::Done {
                content: "保存済みです".to_string(),
            };
        }
        if let Some(give_up) = &self.give_up {
            return SessionResult::Impossible {
                reason: give_up.reason.clone(),
                missing_fields: give_up.missing_fields.clone(),
            };
        }
        if name == self.save_tool_name {
            return self.save(args);
        }
        if name == self.give_up_tool_name {
            return self.give_up(args);
        }
        if !self.tools.contains(name) {
            return SessionResult::Continue {
                content: format!("TOOL_ERROR: ツール '{name}' は利用できません"),
                is_error: true,
            };
        }
        self.tool_calls += 1;
        match self.tools.call(name, args).await {
            Ok(output) => SessionResult::Continue {
                content: output.content,
                is_error: output.is_error,
            },
            Err(e) => SessionResult::Continue {
                content: format!("TOOL_ERROR: {e}"),
                is_error: true,
            },
        }
    }

    fn give_up(&mut self, args: Value) -> SessionResult {
        let threshold = self
            .give_up_min_tool_calls
            .unwrap_or(if self.tools.is_empty() { 0 } else { 1 });
        if self.tool_calls < threshold {
            return SessionResult::Continue {
                content: format!(
                    "VALIDATION_ERROR: まだ調査ツールを{threshold}回以上呼んでいません（現在{}回）。先に調査ツールで確認してください。",
                    self.tool_calls
                ),
                is_error: false,
            };
        }
        match give_up::parse(&args) {
            Ok(record) => {
                let reason = record.reason.clone();
                let missing_fields = record.missing_fields.clone();
                self.give_up = Some(record);
                SessionResult::Impossible {
                    reason,
                    missing_fields,
                }
            }
            Err(message) => SessionResult::Continue {
                content: format!("VALIDATION_ERROR: {message}"),
                is_error: false,
            },
        }
    }

    fn save(&mut self, args: Value) -> SessionResult {
        self.save_calls += 1;

        let Some(obj) = args.as_object() else {
            return SessionResult::Continue {
                content: empty_args_feedback(&self.save_tool_name),
                is_error: false,
            };
        };
        if obj.is_empty() {
            return SessionResult::Continue {
                content: empty_args_feedback(&self.save_tool_name),
                is_error: false,
            };
        }

        let mut candidate = self.accumulated.clone();
        deep_merge(&mut candidate, &args);

        if self.schema.validate(&candidate).is_ok() {
            self.accumulated = candidate;
            self.done = true;
            return SessionResult::Done {
                content: self.done_content(),
            };
        }

        let errors: Vec<_> = self.schema.validator().iter_errors(&candidate).collect();
        let extras = collect_extra_keys(&errors);
        let removed = strip_extra_keys(&mut candidate, &extras);
        let errors: Vec<_> = self.schema.validator().iter_errors(&candidate).collect();

        for (_, key) in &removed {
            let warning = format!("余計なキーを無視しました: {key}");
            if !self.warnings.contains(&warning) {
                self.warnings.push(warning);
            }
        }

        if !removed.is_empty() && errors.is_empty() {
            self.accumulated = candidate;
            self.done = true;
            return SessionResult::Done {
                content: self.done_content(),
            };
        }

        let feedback = format_validation_feedback(&errors, &candidate, &removed, &self.feedback);
        self.accumulated = candidate;
        SessionResult::Continue {
            content: feedback,
            is_error: false,
        }
    }

    fn done_content(&self) -> String {
        let keys: Vec<&str> = self
            .accumulated
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        format!("保存しました（保存済み: {}）", keys.join(", "))
    }

    pub fn into_outcome(self) -> Result<FillOutcome<T>, FillError> {
        if self.done {
            let value: T = serde_json::from_value(self.accumulated.clone())?;
            return Ok(FillOutcome {
                value,
                raw: self.accumulated,
                warnings: self.warnings,
                save_calls: self.save_calls,
            });
        }
        if let Some(give_up) = self.give_up {
            return Err(FillError::Impossible {
                reason: give_up.reason,
                missing_fields: give_up.missing_fields,
                partial: self.accumulated,
            });
        }
        Err(FillError::Incomplete {
            partial: self.accumulated,
        })
    }
}
