use std::marker::PhantomData;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

use jsonschema::paths::Location;

use crate::feedback::{FeedbackOptions, empty_args_feedback, format_validation_feedback};
use crate::give_up::{self, DEFAULT_GIVE_UP_TOOL_NAME, GiveUp};
use crate::prompt::{SAVE_DESCRIPTION, build_system_prompt, guide_section};
use crate::schema::{GuideOptions, collect_extra_keys, deep_merge, display_path, strip_extra_keys};
use crate::{
    ArrayToolNames, DEFAULT_SAVE_TOOL_NAME, FillError, FillSchema, RootKind, Tool, ToolSet,
    ToolSpec,
};

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
    array_tool_names: ArrayToolNames,
    give_up: Option<GiveUp>,
    give_up_tool_name: String,
    give_up_min_tool_calls: Option<usize>,
    tool_calls: usize,
    feedback: FeedbackOptions,
    warnings: Vec<String>,
    save_calls: usize,
    done: bool,
    schema_guide: bool,
    guide_options: GuideOptions,
    _marker: PhantomData<fn() -> T>,
}

impl<T> FillSession<T>
where
    T: DeserializeOwned + JsonSchema,
{
    pub fn new(prompt: impl Into<String>) -> Result<Self, FillError> {
        let schema = FillSchema::for_type::<T>()?;
        let seed = match schema.root_kind() {
            RootKind::Object => Value::Object(Default::default()),
            RootKind::Array => Value::Array(Vec::new()),
        };
        Self::from_parts(prompt, seed, schema)
    }

    pub fn with_seed(prompt: impl Into<String>, seed: Value) -> Result<Self, FillError> {
        let schema = FillSchema::for_type::<T>()?;
        Self::from_parts(prompt, seed, schema)
    }

    fn from_parts(
        prompt: impl Into<String>,
        seed: Value,
        schema: FillSchema,
    ) -> Result<Self, FillError> {
        if let Err(issues) = schema.validate_seed(&seed) {
            return Err(FillError::InvalidSeed { issues });
        }
        Ok(Self {
            schema,
            prompt: prompt.into(),
            accumulated: seed,
            tools: ToolSet::new(),
            save_tool_name: DEFAULT_SAVE_TOOL_NAME.to_string(),
            array_tool_names: ArrayToolNames::default(),
            give_up: None,
            give_up_tool_name: DEFAULT_GIVE_UP_TOOL_NAME.to_string(),
            give_up_min_tool_calls: None,
            tool_calls: 0,
            feedback: FeedbackOptions::default(),
            warnings: Vec::new(),
            save_calls: 0,
            done: false,
            schema_guide: true,
            guide_options: GuideOptions::default(),
            _marker: PhantomData,
        })
    }

    pub fn with_feedback_options(mut self, options: FeedbackOptions) -> Self {
        self.feedback = options;
        self
    }

    /// Whether the system prompt lists the fields to fill. Defaults to `true`.
    ///
    /// Turn it off to get the prompt as it was before the field guide existed.
    pub fn with_schema_guide(mut self, enabled: bool) -> Self {
        self.schema_guide = enabled;
        self
    }

    /// Set the limits used when rendering the field list. Defaults to
    /// unlimited descriptions and unlimited fields.
    pub fn with_guide_options(mut self, options: GuideOptions) -> Self {
        self.guide_options = options;
        self
    }

    pub fn guide_options(&self) -> &GuideOptions {
        &self.guide_options
    }

    /// Override the name of the tool used for partial saves.
    ///
    /// Empty or whitespace-only names are ignored, and so is a name already
    /// used by the give-up tool.
    pub fn with_save_tool_name(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        let trimmed = name.trim();
        if !trimmed.is_empty()
            && trimmed != self.give_up_tool_name
            && !self.array_tool_names.all().contains(&trimmed)
        {
            self.save_tool_name = trimmed.to_string();
        }
        self
    }

    pub fn save_tool_name(&self) -> &str {
        &self.save_tool_name
    }

    /// Override the three tool names used in array mode.
    ///
    /// Empty names, duplicate names, and names that collide with the save or
    /// give-up tool are ignored as a set.
    pub fn with_array_tool_names(mut self, names: ArrayToolNames) -> Self {
        let Some(names) = names.normalized() else {
            return self;
        };
        if names
            .all()
            .iter()
            .any(|name| *name == self.save_tool_name || *name == self.give_up_tool_name)
        {
            return self;
        }
        self.array_tool_names = names;
        self
    }

    pub fn array_tool_names(&self) -> &ArrayToolNames {
        &self.array_tool_names
    }

    /// The top-level shape this session fills.
    pub fn root_kind(&self) -> RootKind {
        self.schema.root_kind()
    }

    /// Whether this session fills an array of flat objects.
    pub fn is_array(&self) -> bool {
        self.schema.is_array()
    }

    /// Override the name of the tool that declares the goal unreachable.
    ///
    /// Empty or whitespace-only names are ignored, and so is a name already
    /// used by the save tool.
    pub fn with_give_up_tool_name(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        let trimmed = name.trim();
        if !trimmed.is_empty()
            && trimmed != self.save_tool_name
            && !self.array_tool_names.all().contains(&trimmed)
        {
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
        let mut out = build_system_prompt(
            &self.prompt,
            self.schema.root_kind(),
            &self.save_tool_name,
            &self.give_up_tool_name,
            &self.array_tool_names,
            &self.tools,
        );
        if self.schema_guide {
            let fields = self.schema.guide_fields(&self.guide_options);
            out.push_str(&guide_section(
                self.schema.root_kind(),
                &self.save_tool_name,
                &self.array_tool_names,
                &fields,
            ));
        }
        out
    }

    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        let mut specs = match self.schema.root_kind() {
            RootKind::Object => vec![ToolSpec {
                name: self.save_tool_name.clone(),
                description: SAVE_DESCRIPTION.to_string(),
                parameters: self.schema.partial().clone(),
            }],
            RootKind::Array => crate::array::specs(&self.schema, &self.array_tool_names),
        };
        let mut reserved: Vec<String> = specs.iter().map(|spec| spec.name.clone()).collect();
        reserved.push(self.save_tool_name.clone());
        let give_up_spec = give_up::spec(&self.give_up_tool_name);
        if !reserved.contains(&give_up_spec.name) {
            reserved.push(give_up_spec.name.clone());
            specs.push(give_up_spec);
        }
        for spec in self.tools.specs() {
            if !reserved.contains(&spec.name) {
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
    /// validation issues, so the model knows why it must call a mutation tool.
    pub fn progress_feedback(&self) -> String {
        let action = match self.schema.root_kind() {
            RootKind::Object => format!("{} を呼び出して", self.save_tool_name),
            RootKind::Array => format!(
                "{} / {} / {} のいずれかで",
                self.array_tool_names.append,
                self.array_tool_names.update,
                self.array_tool_names.remove
            ),
        };
        match self.schema.validate(&self.accumulated) {
            Ok(()) => format!("JSONはまだ完了していません。{action}結果を保存してください。"),
            Err(issues) => {
                let lines: Vec<String> = issues
                    .iter()
                    .map(|issue| format!("- {}: {} ({})", issue.path, issue.keyword, issue.message))
                    .collect();
                format!(
                    "JSONはまだ埋まっていません。現在の検証エラー:\n{}\n{action}不足分を保存してください。\nどうしても埋められない場合は {} を呼んで理由を報告してください。",
                    lines.join("\n"),
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
        if self.schema.is_array() {
            if name == self.array_tool_names.append {
                return self.append(args);
            }
            if name == self.array_tool_names.update {
                return self.update(args);
            }
            if name == self.array_tool_names.remove {
                return self.remove(args);
            }
        } else if name == self.save_tool_name {
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
        self.commit_candidate(candidate, "")
    }

    fn append(&mut self, args: Value) -> SessionResult {
        self.save_calls += 1;
        let items = match crate::array::parse_append(&args) {
            Ok(items) => items,
            Err(message) => return validation_error(message),
        };
        let mut candidate = self.accumulated.clone();
        let Some(array) = candidate.as_array_mut() else {
            return validation_error("内部状態が配列ではありません。".to_string());
        };
        let first = array.len();
        array.extend(items.iter().cloned());
        let len = array.len();
        let prefix = format!(
            "{}\n",
            crate::array::appended_message(first, items.len(), len)
        );
        self.commit_candidate(candidate, &prefix)
    }

    fn update(&mut self, args: Value) -> SessionResult {
        self.save_calls += 1;
        let (index, patch) = match crate::array::parse_update(&args) {
            Ok(update) => update,
            Err(message) => return validation_error(message),
        };
        let len = self.accumulated.as_array().map(Vec::len).unwrap_or(0);
        if index >= len {
            return SessionResult::Continue {
                content: crate::array::range_error(index, len),
                is_error: false,
            };
        }
        let mut candidate = self.accumulated.clone();
        let Some(array) = candidate.as_array_mut() else {
            return validation_error("内部状態が配列ではありません。".to_string());
        };
        deep_merge(&mut array[index], &patch);
        let prefix = format!("{}\n", crate::array::updated_message(index));
        self.commit_candidate(candidate, &prefix)
    }

    fn remove(&mut self, args: Value) -> SessionResult {
        self.save_calls += 1;
        let index = match crate::array::parse_remove(&args) {
            Ok(index) => index,
            Err(message) => return validation_error(message),
        };
        let len = self.accumulated.as_array().map(Vec::len).unwrap_or(0);
        if index >= len {
            return SessionResult::Continue {
                content: crate::array::range_error(index, len),
                is_error: false,
            };
        }
        let mut candidate = self.accumulated.clone();
        let Some(array) = candidate.as_array_mut() else {
            return validation_error("内部状態が配列ではありません。".to_string());
        };
        array.remove(index);
        let prefix = format!("{}\n", crate::array::removed_message(index, array.len()));
        self.commit_candidate(candidate, &prefix)
    }

    /// Validate a candidate, strip extra keys, and commit it.
    ///
    /// `prefix` is the operation result shown above validation feedback (for
    /// example, `追加しました（[0]）`). It is empty for object-mode saves.
    fn commit_candidate(&mut self, mut candidate: Value, prefix: &str) -> SessionResult {
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

        for (loc, key) in &removed {
            let warning = warning_message(loc, key);
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
            content: format!("{prefix}{feedback}"),
            is_error: false,
        }
    }

    fn done_content(&self) -> String {
        match &self.accumulated {
            Value::Array(items) => format!("保存しました（{}件）", items.len()),
            Value::Object(_) => {
                let keys: Vec<&str> = self
                    .accumulated
                    .as_object()
                    .map(|o| o.keys().map(String::as_str).collect())
                    .unwrap_or_default();
                format!("保存しました（保存済み: {}）", keys.join(", "))
            }
            _ => "保存しました".to_string(),
        }
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

fn validation_error(message: String) -> SessionResult {
    SessionResult::Continue {
        content: format!("VALIDATION_ERROR: {message}"),
        is_error: false,
    }
}

fn warning_message(loc: &Location, key: &str) -> String {
    let path = display_path(loc);
    if path == "(root)" {
        format!("余計なキーを無視しました: {key}")
    } else {
        format!("余計なキーを無視しました: {path}.{key}")
    }
}
