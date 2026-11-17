use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::feedback::{
    FeedbackOptions, budget_message, empty_args_feedback, extra_key_path,
    format_validation_feedback, goal_feedback, goal_section, rejected_extra_keys_feedback,
    save_action,
};
use crate::give_up::{self, DEFAULT_GIVE_UP_TOOL_NAME, GiveUp};
use crate::goal::{self, GoalEmpty, GoalPath, GoalReport};
use crate::prompt::{SAVE_DESCRIPTION, build_system_prompt, guide_section};
use crate::schema::{GuideOptions, collect_extra_keys, deep_merge, strip_extra_keys};
use crate::{
    ArrayToolNames, DEFAULT_SAVE_TOOL_NAME, FillError, FillSchema, RootKind, Tool, ToolSet,
    ToolSpec,
};

/// プロンプト構成の版。`SAVE_DESCRIPTION`・ガイド見出し・
/// ゴール案内・予算メッセージを変えたら手で上げる。
pub const PROMPT_VERSION: &str = "save-contract-v3";

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

/// What a save does when it carries keys the schema does not declare.
///
/// Dropping them silently loses data while still reporting success, so the
/// default is [`ExtraKeys::Reject`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExtraKeys {
    /// Drop the extra keys and save the rest when nothing else is wrong.
    Strip,
    /// Refuse the save, name the keys, and suggest the closest property.
    #[default]
    Reject,
}

#[non_exhaustive]
pub struct FillOutcome<T> {
    pub value: T,
    pub raw: Value,
    pub warnings: Vec<String>,
    pub save_calls: usize,
    /// 実行した調査ツール呼び出しの内訳（保存系ツールとブロックされた呼び出しは含まない）
    ///
    /// 数え方は `tool_calls` と同じで、give_up の足切りに使う総数の
    /// 内訳になる。ツール予算の効果測定用。
    pub tool_calls_by_name: BTreeMap<String, usize>,
    /// この結果を生成したプロンプトの版（例: `"save-contract-v3+stock-3"`）
    pub prompt_version: String,
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
    tool_calls_by_name: BTreeMap<String, usize>,
    tool_budgets: BTreeMap<String, usize>,
    tool_allowlist: Option<Vec<String>>,
    feedback: FeedbackOptions,
    extra_keys: ExtraKeys,
    goal_fields: Vec<GoalPath>,
    goal_empty: GoalEmpty,
    warnings: Vec<String>,
    save_calls: usize,
    prompt_version: String,
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
            tool_calls_by_name: BTreeMap::new(),
            tool_budgets: BTreeMap::new(),
            tool_allowlist: None,
            feedback: FeedbackOptions::default(),
            extra_keys: ExtraKeys::default(),
            goal_fields: Vec::new(),
            goal_empty: GoalEmpty::default(),
            warnings: Vec::new(),
            save_calls: 0,
            prompt_version: PROMPT_VERSION.to_string(),
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

    /// How saves that carry undeclared keys are handled. Defaults to
    /// [`ExtraKeys::Reject`].
    pub fn with_extra_keys(mut self, policy: ExtraKeys) -> Self {
        self.extra_keys = policy;
        self
    }

    pub fn extra_keys(&self) -> ExtraKeys {
        self.extra_keys
    }

    /// Declare the fields that must hold a value before the session finishes.
    ///
    /// Notation is `field` in object mode and `[].field` in array mode. A
    /// mismatch with the root kind, any other notation, or a name the schema
    /// does not declare is an error at construction time: an impossible goal
    /// would keep the session from ever finishing.
    ///
    /// A field counts as filled when the value exists, is not `null`, and is
    /// not `""`. `" "`, `0`, `false`, `[]`, and `{}` all count as filled.
    pub fn with_goal_fields<I, S>(mut self, fields: I) -> Result<Self, FillError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut paths = Vec::new();
        for spec in fields {
            let path = GoalPath::parse(spec.as_ref(), self.schema.root_kind())?;
            goal::check_against_schema(&path, &self.schema)?;
            paths.push(path);
        }
        self.goal_fields = paths;
        Ok(self)
    }

    /// Whether an array session may finish with zero elements.
    ///
    /// Defaults to [`GoalEmpty::Pass`]. [`GoalEmpty::Fail`] only requires at
    /// least one element; it is not a general item-count limit, and it has no
    /// effect in object mode.
    pub fn with_goal_empty(mut self, policy: GoalEmpty) -> Self {
        self.goal_empty = policy;
        self
    }

    /// The declared goal fields, in the order they were given.
    pub fn goal_fields(&self) -> &[GoalPath] {
        &self.goal_fields
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

    /// 呼び出し側の版を連結する（例: `"stock-3"` →
    /// `"save-contract-v3+stock-3"`）。空文字・空白のみは無視する。
    pub fn with_prompt_version(mut self, suffix: impl Into<String>) -> Self {
        let suffix = suffix.into();
        let trimmed = suffix.trim();
        if !trimmed.is_empty() {
            self.prompt_version = format!("{}+{trimmed}", self.prompt_version);
        }
        self
    }

    /// このセッションが記録するプロンプト版。既定は [`PROMPT_VERSION`]。
    pub fn prompt_version(&self) -> &str {
        &self.prompt_version
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

    /// Cap how many times `tool` may be executed. `0` means "no limit".
    ///
    /// The cap is checked before the tool runs: the call that would exceed it
    /// is not executed (no token or network cost) and is answered with
    /// [`SessionResult::Continue`] carrying `is_error: false`. Blocked calls do
    /// not count toward the give-up gate.
    ///
    /// Saving is never capped: `save_result` / `append` / `update` / `remove`
    /// and the give-up tool are dispatched before the budget is consulted, so a
    /// budget set on their names has no effect. A budget on a name that is not
    /// registered yet is kept and applies once the tool is added.
    ///
    /// `0` is a no-op rather than "block this tool": a misconfiguration should
    /// not wedge the session. An empty name is ignored.
    pub fn with_tool_budget(mut self, tool: impl Into<String>, max: usize) -> Self {
        let tool = tool.into();
        let trimmed = tool.trim();
        if trimmed.is_empty() {
            return self;
        }
        if max == 0 {
            self.tool_budgets.remove(trimmed);
            return self;
        }
        self.tool_budgets.insert(trimmed.to_string(), max);
        self
    }

    /// The per-tool caps currently in effect.
    pub fn tool_budgets(&self) -> &BTreeMap<String, usize> {
        &self.tool_budgets
    }

    /// Restrict which investigation tools may run.
    ///
    /// `Some(names)` makes `call` refuse any other registered tool, so a model
    /// that remembers a tool removed from the advertised list cannot keep
    /// using it. `None` removes the restriction. Save and give-up tools are
    /// never restricted.
    pub fn set_tool_allowlist(&mut self, names: Option<Vec<String>>) {
        self.tool_allowlist = names;
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

    /// Tool definitions that only save, plus the give-up tool.
    ///
    /// Object mode exposes [`Self::save_tool_name`]; Array mode exposes the
    /// three array tools. Investigation tools registered with [`Self::add_tool`]
    /// are not included. A loop can swap this in for [`Self::tool_specs`] once
    /// it wants to stop the model from investigating further.
    pub fn save_only_tool_specs(&self) -> Vec<ToolSpec> {
        let mut specs = match self.schema.root_kind() {
            RootKind::Object => vec![ToolSpec {
                name: self.save_tool_name.clone(),
                description: SAVE_DESCRIPTION.to_string(),
                parameters: self.schema.partial().clone(),
            }],
            RootKind::Array => crate::array::specs(&self.schema, &self.array_tool_names),
        };
        let give_up_spec = give_up::spec(&self.give_up_tool_name);
        if !specs.iter().any(|spec| spec.name == give_up_spec.name) {
            specs.push(give_up_spec);
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
        let action = save_action(
            self.schema.root_kind(),
            &self.save_tool_name,
            &self.array_tool_names,
        );
        match self.schema.validate(&self.accumulated) {
            Ok(()) => match self.unsatisfied_goals(&self.accumulated) {
                Some(report) => {
                    goal_feedback(&report, &self.save_tool_name, &self.array_tool_names)
                }
                None => format!("JSONはまだ完了していません。{action}結果を保存してください。"),
            },
            Err(issues) => {
                let lines: Vec<String> = issues
                    .iter()
                    .map(|issue| format!("- {}: {} ({})", issue.path, issue.keyword, issue.message))
                    .collect();
                let mut out = format!(
                    "JSONはまだ埋まっていません。現在の検証エラー:\n{}",
                    lines.join("\n")
                );
                // ゴール不足もここに出さないと、ツール無し応答が空回りする。
                let mut has_goal_section = false;
                if let Some(report) = self.unsatisfied_goals(&self.accumulated) {
                    out.push('\n');
                    out.push_str(&goal_section(
                        &report,
                        &self.save_tool_name,
                        &self.array_tool_names,
                    ));
                    has_goal_section = true;
                }
                // ゴール節が「次に取るべき操作」を既に出しているので、
                // 同じことを二度書かない。
                if has_goal_section {
                    out.push_str(&format!(
                        "\nどうしても埋められない場合は {} を呼んで理由を報告してください。",
                        self.give_up_tool_name
                    ));
                } else {
                    out.push_str(&format!(
                        "\n{action}不足分を保存してください。\nどうしても埋められない場合は {} を呼んで理由を報告してください。",
                        self.give_up_tool_name
                    ));
                }
                out
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
        if let Some(allowed) = &self.tool_allowlist
            && !allowed.iter().any(|allowed_name| allowed_name == name)
        {
            return SessionResult::Continue {
                content: format!(
                    "TOOL_ERROR: ツール '{name}' は現在利用できません。保存してください。"
                ),
                is_error: true,
            };
        }
        if !self.tools.contains(name) {
            return SessionResult::Continue {
                content: format!("TOOL_ERROR: ツール '{name}' は利用できません"),
                is_error: true,
            };
        }
        // 予算超過は「実行しない」で返す。トークンも時間も使わず、give_up の
        // 足切り（ツール呼び出し総数）も進めない。
        if let Some(max) = self.tool_budgets.get(name).copied() {
            let used = self.tool_calls_by_name.get(name).copied().unwrap_or(0);
            if used >= max {
                return SessionResult::Continue {
                    content: budget_message(
                        name,
                        max,
                        self.schema.root_kind(),
                        &self.save_tool_name,
                        &self.array_tool_names,
                    ),
                    is_error: false,
                };
            }
        }
        self.tool_calls += 1;
        *self.tool_calls_by_name.entry(name.to_string()).or_insert(0) += 1;
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
        let parsed = match crate::array::parse_append(&args) {
            Ok(parsed) => parsed,
            Err(message) => return validation_error(message),
        };
        if let Some(result) =
            reject_extra_args(&self.array_tool_names.append, &parsed.extras, "items")
        {
            return result;
        }
        let items = parsed.value;
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
        let parsed = match crate::array::parse_update(&args) {
            Ok(parsed) => parsed,
            Err(message) => return validation_error(message),
        };
        if let Some(result) = reject_extra_args(
            &self.array_tool_names.update,
            &parsed.extras,
            "index, patch",
        ) {
            return result;
        }
        let (index, patch) = parsed.value;
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
        let parsed = match crate::array::parse_remove(&args) {
            Ok(parsed) => parsed,
            Err(message) => return validation_error(message),
        };
        if let Some(result) =
            reject_extra_args(&self.array_tool_names.remove, &parsed.extras, "index")
        {
            return result;
        }
        let index = parsed.value;
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

    /// Validate a candidate, apply the extra-key policy, and commit it.
    ///
    /// `prefix` is the operation result shown above validation feedback (for
    /// example, `追加しました（[0]）`). It is empty for object-mode saves and is
    /// never shown when the save is rejected, because nothing was committed.
    fn commit_candidate(&mut self, candidate: Value, prefix: &str) -> SessionResult {
        if self.schema.validate(&candidate).is_ok() {
            return self.finish_if_complete(candidate, prefix);
        }

        let errors: Vec<_> = self.schema.validator().iter_errors(&candidate).collect();
        let extras = collect_extra_keys(&errors);
        if extras.is_empty() {
            let feedback = format_validation_feedback(&errors, &candidate, &[], &self.feedback);
            self.accumulated = candidate;
            return SessionResult::Continue {
                content: format!("{prefix}{feedback}"),
                is_error: false,
            };
        }

        // Reject でも Strip でも「何が余計だったか」は同じに取れる。
        // Reject では書き換えた候補を捨てる（append が非冪等なのでコミットしない）。
        let mut stripped = candidate;
        let removed = strip_extra_keys(&mut stripped, &extras);

        match self.extra_keys {
            ExtraKeys::Reject => {
                for (loc, key) in &removed {
                    self.push_warning(format!(
                        "余計なキーを拒否しました: {}",
                        extra_key_path(loc, key)
                    ));
                }
                // コミットしていないので `prefix` は出さない。
                let feedback =
                    rejected_extra_keys_feedback(&removed, &self.schema, &self.accumulated);
                SessionResult::Continue {
                    content: feedback,
                    is_error: false,
                }
            }
            ExtraKeys::Strip => {
                for (loc, key) in &removed {
                    self.push_warning(format!(
                        "余計なキーを無視しました: {}",
                        extra_key_path(loc, key)
                    ));
                }
                self.push_warning(format!("余計なキーを {} 件捨てました", removed.len()));

                let errors: Vec<_> = self.schema.validator().iter_errors(&stripped).collect();
                if errors.is_empty() {
                    return self.finish_if_complete(stripped, prefix);
                }

                let feedback =
                    format_validation_feedback(&errors, &stripped, &removed, &self.feedback);
                self.accumulated = stripped;
                SessionResult::Continue {
                    content: format!("{prefix}{feedback}"),
                    is_error: false,
                }
            }
        }
    }

    /// The goals the candidate does not satisfy, or `None` when all are met.
    fn unsatisfied_goals(&self, candidate: &Value) -> Option<GoalReport> {
        if self.goal_fields.is_empty() && self.goal_empty == GoalEmpty::Pass {
            return None;
        }
        let report = goal::missing(&self.goal_fields, self.goal_empty, candidate);
        (!report.is_satisfied()).then_some(report)
    }

    /// Commit a schema-valid candidate and finish, unless a goal is unmet.
    ///
    /// An unmet goal still commits the candidate (partial commit): the model
    /// can then fix the saved value with `update` / `save_result` instead of
    /// appending it again. `done` is not set, so the session keeps accepting
    /// calls.
    fn finish_if_complete(&mut self, candidate: Value, prefix: &str) -> SessionResult {
        if let Some(report) = self.unsatisfied_goals(&candidate) {
            self.accumulated = candidate;
            return SessionResult::Continue {
                content: format!(
                    "{prefix}{}",
                    goal_feedback(&report, &self.save_tool_name, &self.array_tool_names)
                ),
                is_error: false,
            };
        }
        self.accumulated = candidate;
        self.done = true;
        SessionResult::Done {
            content: self.done_content(),
        }
    }

    fn push_warning(&mut self, warning: String) {
        if !self.warnings.contains(&warning) {
            self.warnings.push(warning);
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
                tool_calls_by_name: self.tool_calls_by_name,
                prompt_version: self.prompt_version,
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

/// Unknown keys of an `append` / `update` / `remove` argument.
///
/// The wrapper schemas declare `additionalProperties: false`, so this is
/// rejected whatever [`ExtraKeys`] says: the policy is about the data being
/// filled, not about malformed tool arguments.
fn reject_extra_args(tool: &str, extras: &[String], allowed: &str) -> Option<SessionResult> {
    if extras.is_empty() {
        return None;
    }
    Some(validation_error(crate::array::extra_args_message(
        tool, extras, allowed,
    )))
}
