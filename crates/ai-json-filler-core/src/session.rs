use std::collections::{BTreeMap, BTreeSet};
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
use crate::prompt::{
    GuideMode, PromptAudit, PromptParts, PromptSection, PromptSectionSource, PromptSectionView,
    SAVE_DESCRIPTION, has_placeholder, render_section, substitute,
};
use crate::schema::{GuideOptions, collect_extra_keys, deep_merge, strip_extra_keys};
use crate::{
    ArrayToolNames, DEFAULT_SAVE_TOOL_NAME, FillError, FillSchema, RootKind, Tool, ToolSet,
    ToolSpec,
};

/// プロンプト構成の版。`SAVE_DESCRIPTION`・ガイド見出し・
/// ゴール案内・予算メッセージ・開始メッセージ・強制保存メッセージを変えたら
/// 手で上げる。
pub const PROMPT_VERSION: &str = "save-contract-v4";

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

/// What the caller changed about the prompt sections.
///
/// A section is in at most one of the two sets: replacing a disabled section
/// enables it again, and disabling a replaced section drops the template.
#[derive(Debug, Default)]
struct PromptOverrides {
    sections: BTreeMap<PromptSection, String>,
    disabled: BTreeSet<PromptSection>,
}

impl PromptOverrides {
    fn replace(&mut self, section: PromptSection, template: String) {
        self.disabled.remove(&section);
        self.sections.insert(section, template);
    }

    fn disable(&mut self, section: PromptSection) {
        self.sections.remove(&section);
        self.disabled.insert(section);
    }

    fn reset(&mut self, section: PromptSection) {
        self.sections.remove(&section);
        self.disabled.remove(&section);
    }

    fn template(&self, section: PromptSection) -> Option<&str> {
        self.sections.get(&section).map(String::as_str)
    }

    fn has_template(&self, section: PromptSection) -> bool {
        self.sections.contains_key(&section)
    }

    fn is_disabled(&self, section: PromptSection) -> bool {
        self.disabled.contains(&section)
    }
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
    /// この結果を生成したプロンプトの版（例: `"save-contract-v4+stock-3"`）
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
    guide_mode: GuideMode,
    guide_options: GuideOptions,
    overrides: PromptOverrides,
    explicit_save_tool_name: bool,
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
            guide_mode: GuideMode::default(),
            guide_options: GuideOptions::default(),
            overrides: PromptOverrides::default(),
            explicit_save_tool_name: false,
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
    /// `true` is [`GuideMode::FieldsWithToolNames`], `false` is
    /// [`GuideMode::Off`]; use [`Self::with_guide_mode`] to keep the list but
    /// drop the sentence that names the save tools.
    pub fn with_schema_guide(mut self, enabled: bool) -> Self {
        self.guide_mode = if enabled {
            GuideMode::FieldsWithToolNames
        } else {
            GuideMode::Off
        };
        self
    }

    /// How much of the field list the system prompt carries.
    ///
    /// Defaults to [`GuideMode::FieldsWithToolNames`]. A `Guide` section that
    /// was replaced with [`Self::with_prompt_section`] wins over the mode.
    pub fn with_guide_mode(mut self, mode: GuideMode) -> Self {
        self.guide_mode = mode;
        self
    }

    pub fn guide_mode(&self) -> GuideMode {
        self.guide_mode
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

    /// The task text given to [`Self::new`] / [`Self::with_seed`].
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// Replace the text of one prompt section.
    ///
    /// Placeholders in `template` are filled from the current settings, so a
    /// caller does not have to know which mode the session runs in:
    ///
    /// | placeholder | becomes |
    /// |---|---|
    /// | `{save}` | the save tool name (object mode only) |
    /// | `{append}` `{update}` `{remove}` | the array tool names (array mode only) |
    /// | `{give_up}` | the give-up tool name |
    /// | `{fields}` | the field list ([`PromptSection::Guide`] only) |
    ///
    /// An unknown placeholder, a placeholder that belongs to the other mode, or
    /// a guide template without `{fields}` is a [`FillError`] here, at
    /// construction time, rather than a prompt the model silently misreads.
    /// Only identifier-shaped braces are placeholders, so JSON examples such as
    /// `{"name": "x"}` are copied through as written.
    ///
    /// Unlike the other `with_*` setters this returns a `Result`. That
    /// asymmetry is deliberate: a wrong section is not something to ignore.
    pub fn with_prompt_section(
        mut self,
        section: PromptSection,
        template: impl Into<String>,
    ) -> Result<Self, FillError> {
        let template = template.into();
        if section == PromptSection::Guide && !has_placeholder(&template, "fields") {
            return Err(FillError::MissingFieldsPlaceholder { section });
        }
        let parts = self.prompt_parts();
        substitute(&template, section, &parts)?;
        self.overrides.replace(section, template);
        Ok(self)
    }

    /// Drop a section from the prompt.
    ///
    /// A replaced section is dropped as well; use [`Self::reset_prompt_section`]
    /// to go back to the built-in text.
    pub fn without_prompt_section(mut self, section: PromptSection) -> Self {
        self.overrides.disable(section);
        self
    }

    /// Restore the built-in text of a section.
    pub fn reset_prompt_section(mut self, section: PromptSection) -> Self {
        self.overrides.reset(section);
        self
    }

    /// The current state of one section.
    ///
    /// `rendered` is rebuilt from the current tool names and guide settings
    /// every time, through the same path `system_prompt()` uses, so the two
    /// cannot drift apart.
    pub fn prompt_section(&self, section: PromptSection) -> PromptSectionView<'_> {
        let parts = self.prompt_parts();
        self.section_view(section, &parts)
    }

    /// Every section, in [`PromptSection::ALL`] order.
    ///
    /// Useful for snapshots and diffs: the six entries are always the same six.
    pub fn prompt_sections(&self) -> Vec<(PromptSection, PromptSectionView<'_>)> {
        let parts = self.prompt_parts();
        PromptSection::ALL
            .iter()
            .map(|&section| (section, self.section_view(section, &parts)))
            .collect()
    }

    pub fn is_prompt_section_disabled(&self, section: PromptSection) -> bool {
        self.overrides.is_disabled(section)
    }

    /// Look for prompt mistakes that would mislead the model.
    ///
    /// This never fails: a renamed tool is legitimate, so an unfamiliar name
    /// is reported, not rejected. The default configuration reports nothing;
    /// asserting `prompt_audit().warnings.is_empty()` in a test pins that.
    pub fn prompt_audit(&self) -> PromptAudit {
        let system = self.system_prompt();
        let start = self.start_message();
        let save_only = self.save_only_message();
        let mut audit = PromptAudit::default();

        let advertised: Vec<&str> = match self.schema.root_kind() {
            RootKind::Object => vec![self.save_tool_name.as_str()],
            RootKind::Array => self.array_tool_names.all().to_vec(),
        };
        for name in &advertised {
            if !system.contains(name) {
                audit.warn(format!(
                    "保存ツール名 '{name}' がシステムプロンプトに含まれていません"
                ));
            }
            if !start.contains(name) {
                audit.warn(format!(
                    "保存ツール名 '{name}' が開始メッセージに含まれていません"
                ));
            }
            if !save_only.contains(name) {
                audit.warn(format!(
                    "保存ツール名 '{name}' が強制保存メッセージに含まれていません"
                ));
            }
        }

        if !system.contains(&self.give_up_tool_name) && !start.contains(&self.give_up_tool_name) {
            audit.warn(format!(
                "give-up ツール名 '{}' がプロンプトに含まれていません",
                self.give_up_tool_name
            ));
        }

        if self.schema.is_array() {
            let save = self.save_tool_name.as_str();
            if self.explicit_save_tool_name {
                audit.warn(format!(
                    "save_tool_name '{save}' は配列モードでは使われません（設定ミス）"
                ));
            } else if !self.tools.contains(save)
                && (system.contains(save) || start.contains(save) || save_only.contains(save))
            {
                audit.warn(format!(
                    "配列モードでは使われない保存ツール名 '{save}' がプロンプトに含まれています"
                ));
            }
        }

        audit
    }

    /// Run [`Self::prompt_audit`] now and merge the warnings into
    /// [`Self::warnings`], so they end up in [`FillOutcome::warnings`].
    pub fn with_prompt_audit(mut self) -> Self {
        for warning in self.prompt_audit().warnings {
            self.push_warning(warning);
        }
        self
    }

    /// 呼び出し側の版を連結する（例: `"stock-3"` →
    /// `"save-contract-v4+stock-3"`）。空文字・空白のみは無視する。
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
    /// used by the give-up tool or by one of the array tools. An ignored name
    /// is reported through [`Self::warnings`]; use
    /// [`Self::try_with_save_tool_name`] to get an error instead.
    ///
    /// The name is unused in array mode: the start message and the advertised
    /// tools all come from [`ArrayToolNames`] there. Because a collision is
    /// resolved by ignoring whichever name is applied second, the genai
    /// builder applies the array names first in array mode so that the names
    /// that actually appear in the tool list win.
    pub fn with_save_tool_name(mut self, name: impl Into<String>) -> Self {
        let name = name.into().trim().to_string();
        if let Some(reason) = self.save_name_problem(&name) {
            self.push_warning(format!("保存ツール名 '{name}' を無視しました: {reason}"));
        } else {
            self.save_tool_name = name;
            self.explicit_save_tool_name = true;
        }
        self
    }

    /// [`Self::with_save_tool_name`], but an unusable name is an error.
    pub fn try_with_save_tool_name(mut self, name: impl Into<String>) -> Result<Self, FillError> {
        let name = name.into().trim().to_string();
        if let Some(reason) = self.save_name_problem(&name) {
            return Err(FillError::InvalidToolName { name, reason });
        }
        self.save_tool_name = name;
        self.explicit_save_tool_name = true;
        Ok(self)
    }

    fn save_name_problem(&self, name: &str) -> Option<String> {
        if name.is_empty() {
            return Some("空の名前は使えません".to_string());
        }
        if name == self.give_up_tool_name.as_str() {
            return Some("give-up ツール名と衝突しています".to_string());
        }
        if self.array_tool_names.all().contains(&name) {
            return Some("配列ツール名と衝突しています".to_string());
        }
        None
    }

    pub fn save_tool_name(&self) -> &str {
        &self.save_tool_name
    }

    /// Override the three tool names used in array mode.
    ///
    /// Empty names, duplicate names, and names that collide with the save or
    /// give-up tool are ignored **as a set**: one bad name keeps all three at
    /// their defaults rather than leaving a half-renamed trio. The ignored call
    /// is reported through [`Self::warnings`].
    pub fn with_array_tool_names(mut self, names: ArrayToolNames) -> Self {
        match self.array_names_problem(&names) {
            Some(reason) => self.push_warning(format!("配列ツール名を無視しました: {reason}")),
            None => self.array_tool_names = names.normalized().expect("checked above"),
        }
        self
    }

    /// [`Self::with_array_tool_names`], but an unusable set is an error.
    pub fn try_with_array_tool_names(mut self, names: ArrayToolNames) -> Result<Self, FillError> {
        if let Some(reason) = self.array_names_problem(&names) {
            return Err(FillError::InvalidToolName {
                name: format!("{}/{}/{}", names.append, names.update, names.remove),
                reason,
            });
        }
        self.array_tool_names = names.normalized().expect("checked above");
        Ok(self)
    }

    fn array_names_problem(&self, names: &ArrayToolNames) -> Option<String> {
        if names.clone().normalized().is_none() {
            return Some("名前が空か重複しています".to_string());
        }
        if names
            .all()
            .iter()
            .any(|name| *name == self.save_tool_name || *name == self.give_up_tool_name)
        {
            return Some("保存ツール名または give-up ツール名と衝突しています".to_string());
        }
        None
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
    /// used by the save tool. An ignored name is reported through
    /// [`Self::warnings`]; use [`Self::try_with_give_up_tool_name`] to get an
    /// error instead.
    pub fn with_give_up_tool_name(mut self, name: impl Into<String>) -> Self {
        let name = name.into().trim().to_string();
        if let Some(reason) = self.give_up_name_problem(&name) {
            self.push_warning(format!(
                "give-up ツール名 '{name}' を無視しました: {reason}"
            ));
        } else {
            self.give_up_tool_name = name;
        }
        self
    }

    /// [`Self::with_give_up_tool_name`], but an unusable name is an error.
    pub fn try_with_give_up_tool_name(
        mut self,
        name: impl Into<String>,
    ) -> Result<Self, FillError> {
        let name = name.into().trim().to_string();
        if let Some(reason) = self.give_up_name_problem(&name) {
            return Err(FillError::InvalidToolName { name, reason });
        }
        self.give_up_tool_name = name;
        Ok(self)
    }

    fn give_up_name_problem(&self, name: &str) -> Option<String> {
        if name.is_empty() {
            return Some("空の名前は使えません".to_string());
        }
        if name == self.save_tool_name.as_str() {
            return Some("保存ツール名と衝突しています".to_string());
        }
        if self.array_tool_names.all().contains(&name) {
            return Some("配列ツール名と衝突しています".to_string());
        }
        None
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

    /// The task text followed by every [`PromptSection::SYSTEM`] section.
    ///
    /// Built from the same rendering path as [`Self::prompt_section`], so the
    /// two always agree.
    pub fn system_prompt(&self) -> String {
        let parts = self.prompt_parts();
        let mut out = String::from(self.prompt.as_str());
        for section in PromptSection::SYSTEM {
            out.push_str(&self.section_text(section, &parts));
        }
        out
    }

    /// セッション開始時に user ロールで 1 回だけ送る文面。
    ///
    /// Object モードは [`Self::save_tool_name`] を、Array モードは
    /// [`ArrayToolNames`] の 3 ツールを名指しする。Array モードでは
    /// `save_result` がツール一覧に無いので、ここでも出さない。
    /// どちらのモードでも [`Self::give_up_tool_name`] を含める。
    pub fn start_message(&self) -> String {
        let parts = self.prompt_parts();
        self.section_text(PromptSection::StartMessage, &parts)
    }

    /// 調査を打ち切って保存に移る段階で 1 回だけ差し込む文面。
    ///
    /// Object モードは [`Self::save_tool_name`] を、Array モードは
    /// [`ArrayToolNames`] の 3 ツールを名指しする。
    pub fn save_only_message(&self) -> String {
        let parts = self.prompt_parts();
        self.section_text(PromptSection::SaveOnlyMessage, &parts)
    }

    /// Everything the prompt needs, borrowed from the session.
    fn prompt_parts(&self) -> PromptParts<'_> {
        // A replaced guide still needs the field list, whatever the mode says.
        let needs_fields =
            self.guide_mode != GuideMode::Off || self.overrides.has_template(PromptSection::Guide);
        PromptParts {
            save: &self.save_tool_name,
            names: &self.array_tool_names,
            give_up: &self.give_up_tool_name,
            kind: self.schema.root_kind(),
            guide: self.guide_mode,
            fields: if needs_fields {
                self.schema.guide_fields(&self.guide_options)
            } else {
                String::new()
            },
            hints: self.tool_hints(),
        }
    }

    fn tool_hints(&self) -> String {
        let mut hints: Vec<&str> = Vec::new();
        for tool in self.tools.tools() {
            if let Some(hint) = tool.prompt_hint() {
                hints.push(hint);
            }
        }
        hints.join("\n\n")
    }

    /// The text of one section, separators included.
    fn section_text(&self, section: PromptSection, parts: &PromptParts<'_>) -> String {
        if self.overrides.is_disabled(section) {
            return String::new();
        }
        if let Some(template) = self.overrides.template(section) {
            // `with_prompt_section` already rendered this template against the
            // same mode, and the mode never changes afterwards. Falling back to
            // an empty string keeps `system_prompt()` infallible.
            return match substitute(template, section, parts) {
                Ok(body) => crate::prompt::section_block(section, body),
                Err(_) => String::new(),
            };
        }
        render_section(section, parts)
    }

    fn section_view<'a>(
        &'a self,
        section: PromptSection,
        parts: &PromptParts<'_>,
    ) -> PromptSectionView<'a> {
        if self.is_prompt_section_disabled(section) {
            return PromptSectionView {
                source: PromptSectionSource::Disabled,
                template: None,
                rendered: String::new(),
            };
        }
        match self.overrides.template(section) {
            Some(template) => PromptSectionView {
                source: PromptSectionSource::Custom,
                template: Some(template),
                rendered: self.section_text(section, parts),
            },
            None => PromptSectionView {
                source: PromptSectionSource::Default,
                template: None,
                rendered: self.section_text(section, parts),
            },
        }
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
        // Array モードで save 名を予約すると、同名の調査ツールがリクエストから
        // 静かに消える（`call()` では実行できてしまう）。予約は Object モードだけ。
        if !self.schema.is_array() {
            reserved.push(self.save_tool_name.clone());
        }
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
            // 過去の履歴・癖・ファインチューニングで `save_result` を呼ぶ
            // モデルは残る。同名の調査ツールが無いなら、その 1 ラウンドを
            // 案内で回収する。
            if name == self.save_tool_name && !self.tools.contains(name) {
                let names = &self.array_tool_names;
                return SessionResult::Continue {
                    content: format!(
                        "TOOL_ERROR: ツール '{name}' は Object モード専用のため、このモードでは利用できません。{} / {} / {} のいずれかで保存してください。",
                        names.append, names.update, names.remove
                    ),
                    is_error: true,
                };
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
