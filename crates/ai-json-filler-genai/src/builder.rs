use std::marker::PhantomData;
use std::sync::Arc;

use ai_json_filler_core::{
    ArrayToolNames, BoxFuture, ExtraKeys, FeedbackOptions, FillError, FillOutcome, FillSession,
    GoalEmpty, GuideMode, GuideOptions, PromptSection, SessionResult, Tool, ToolOutput, ToolSpec,
};
use genai::chat::{ChatMessage, ChatRequest, ContentPart, MessageContent, ToolResponse};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::backend::{ChatBackend, GenaiBackend};
use crate::convert::to_genai_tool;

/// Start a fill using the default provider resolution for `model`.
pub fn fill<T>(
    client: &genai::Client,
    model: impl Into<genai::ModelSpec>,
    prompt: impl Into<String>,
) -> GenaiFillBuilder<T>
where
    T: DeserializeOwned + JsonSchema,
{
    let backend = Arc::new(GenaiBackend::new(client.clone(), model));
    fill_with_backend(backend, prompt)
}

/// Start a fill against an explicit [`genai::ServiceTarget`] (custom endpoint/auth).
pub fn fill_target<T>(
    client: &genai::Client,
    target: genai::ServiceTarget,
    prompt: impl Into<String>,
) -> GenaiFillBuilder<T>
where
    T: DeserializeOwned + JsonSchema,
{
    fill(client, target, prompt)
}

/// Start a fill using a caller-supplied [`ChatBackend`].
pub fn fill_with_backend<T>(
    backend: Arc<dyn ChatBackend>,
    prompt: impl Into<String>,
) -> GenaiFillBuilder<T>
where
    T: DeserializeOwned + JsonSchema,
{
    GenaiFillBuilder {
        backend,
        prompt: prompt.into(),
        seed: None,
        tools: Vec::new(),
        save_tool_name: None,
        give_up_tool_name: None,
        array_tool_names: None,
        strict_tool_names: false,
        goal_fields: Vec::new(),
        goal_empty: None,
        extra_keys: None,
        schema_guide: None,
        guide_mode: None,
        guide_options: None,
        feedback_options: None,
        prompt_sections: Vec::new(),
        disabled_sections: Vec::new(),
        tool_budgets: Vec::new(),
        prompt_version: None,
        force_save_after: None,
        max_turns: 3,
        max_rounds: 10,
        _marker: PhantomData,
    }
}

/// Builder for a [`FillSession`] driven through a [`ChatBackend`].
///
/// `max_turns` counts assistant responses without a tool call (turn boundaries).
/// `max_rounds` counts tool-call responses inside one turn and resets to 0 when
/// the turn changes.
pub struct GenaiFillBuilder<T> {
    backend: Arc<dyn ChatBackend>,
    prompt: String,
    seed: Option<Value>,
    tools: Vec<Arc<dyn Tool>>,
    save_tool_name: Option<String>,
    give_up_tool_name: Option<String>,
    array_tool_names: Option<ArrayToolNames>,
    /// Set by the `try_*` name setters: a name that cannot be applied is then
    /// an error instead of a warning.
    strict_tool_names: bool,
    goal_fields: Vec<String>,
    goal_empty: Option<GoalEmpty>,
    extra_keys: Option<ExtraKeys>,
    schema_guide: Option<bool>,
    guide_mode: Option<GuideMode>,
    guide_options: Option<GuideOptions>,
    feedback_options: Option<FeedbackOptions>,
    prompt_sections: Vec<(PromptSection, String)>,
    disabled_sections: Vec<PromptSection>,
    tool_budgets: Vec<(String, usize)>,
    prompt_version: Option<String>,
    force_save_after: Option<usize>,
    max_turns: usize,
    max_rounds: usize,
    _marker: PhantomData<fn() -> T>,
}

impl<T> GenaiFillBuilder<T>
where
    T: DeserializeOwned + JsonSchema,
{
    pub fn seed(mut self, seed: Value) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Register an executable tool from `ai-json-filler-core`.
    ///
    /// `genai::chat::Tool` only carries a definition; the handler is required
    /// to execute tool calls inside the fill loop. Use [`Self::genai_tool`] if
    /// you already have a `genai::chat::Tool` plus a handler.
    pub fn tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Register a `genai::chat::Tool` definition together with its handler.
    ///
    /// This is the bridge for callers that already build genai tool
    /// definitions. The handler receives the JSON arguments and returns the
    /// text content that is sent back to the model.
    pub fn genai_tool<H>(mut self, tool: genai::chat::Tool, handler: H) -> Self
    where
        H: Fn(Value) -> BoxFuture<'static, Result<ToolOutput, FillError>> + Send + Sync + 'static,
    {
        let spec = ToolSpec {
            name: tool.name.as_str().to_string(),
            description: tool.description.unwrap_or_default(),
            parameters: tool
                .schema
                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
        };
        self.tools
            .push(Arc::new(GenaiToolHandler { spec, handler }));
        self
    }

    /// Maximum number of consecutive tool-call responses inside a turn.
    ///
    /// The counter resets to 0 when a response without a tool call advances
    /// the turn.
    pub fn max_rounds(mut self, rounds: usize) -> Self {
        self.max_rounds = rounds.max(1);
        self
    }

    /// Maximum number of assistant responses without a tool call.
    ///
    /// Each such response advances the turn and resets the round counter.
    pub fn max_turns(mut self, turns: usize) -> Self {
        self.max_turns = turns.max(1);
        self
    }

    /// Override the name of the tool used for partial saves (default: `save_result`).
    ///
    /// Unused in array mode: the advertised tools and the loop's start message
    /// come from [`ArrayToolNames`] there. When the name collides with one of
    /// the array tool names, the array names win in array mode (they are
    /// applied first) and this name is ignored.
    pub fn save_tool_name(mut self, name: impl Into<String>) -> Self {
        self.save_tool_name = Some(name.into());
        self
    }

    /// [`Self::save_tool_name`], but a name that cannot be used is an error
    /// when [`Self::run`] starts instead of a warning.
    pub fn try_save_tool_name(mut self, name: impl Into<String>) -> Self {
        self.strict_tool_names = true;
        self.save_tool_name = Some(name.into());
        self
    }

    /// Override the name of the tool that declares the goal unreachable
    /// (default: `give_up`).
    pub fn give_up_tool_name(mut self, name: impl Into<String>) -> Self {
        self.give_up_tool_name = Some(name.into());
        self
    }

    /// [`Self::give_up_tool_name`], with strict validation (see
    /// [`Self::try_save_tool_name`]).
    pub fn try_give_up_tool_name(mut self, name: impl Into<String>) -> Self {
        self.strict_tool_names = true;
        self.give_up_tool_name = Some(name.into());
        self
    }

    /// Override the names of the three tools used in array mode.
    pub fn array_tool_names(mut self, names: ArrayToolNames) -> Self {
        self.array_tool_names = Some(names);
        self
    }

    /// [`Self::array_tool_names`], with strict validation (see
    /// [`Self::try_save_tool_name`]).
    pub fn try_array_tool_names(mut self, names: ArrayToolNames) -> Self {
        self.strict_tool_names = true;
        self.array_tool_names = Some(names);
        self
    }

    /// Require these fields to be filled before the session completes.
    ///
    /// Each entry is `field` (object mode) or `[].field` (array mode). Names are
    /// validated against the schema when the session is created.
    pub fn goal_fields<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.goal_fields = fields.into_iter().map(Into::into).collect();
        self
    }

    /// Policy for arrays with zero elements (default: [`GoalEmpty::Pass`]).
    pub fn goal_empty(mut self, policy: GoalEmpty) -> Self {
        self.goal_empty = Some(policy);
        self
    }

    /// Policy for keys outside the schema (default: [`ExtraKeys::Reject`]).
    pub fn extra_keys(mut self, policy: ExtraKeys) -> Self {
        self.extra_keys = Some(policy);
        self
    }

    /// Include the schema field list in the system prompt (default: `true`).
    pub fn schema_guide(mut self, enabled: bool) -> Self {
        self.schema_guide = Some(enabled);
        self
    }

    /// How much of the field list the system prompt carries
    /// (default: [`GuideMode::FieldsWithToolNames`]).
    ///
    /// [`GuideMode::Fields`] keeps the list but drops the sentence that names
    /// the save tools.
    pub fn guide_mode(mut self, mode: GuideMode) -> Self {
        self.guide_mode = Some(mode);
        self
    }

    /// Tune how much of each field's doc comment the guide includes.
    pub fn guide_options(mut self, options: GuideOptions) -> Self {
        self.guide_options = Some(options);
        self
    }

    /// Limits for the validation feedback the model gets after a failed save.
    pub fn feedback_options(mut self, options: FeedbackOptions) -> Self {
        self.feedback_options = Some(options);
        self
    }

    /// Replace the text of one prompt section.
    ///
    /// Placeholders (`{save}`, `{append}`, `{update}`, `{remove}`, `{give_up}`,
    /// and `{fields}` in the guide) are filled from the current settings, so
    /// the template does not have to know the mode. They are checked when
    /// [`Self::run`] builds the session, because the mode comes from the type
    /// parameter.
    pub fn prompt_section(mut self, section: PromptSection, template: impl Into<String>) -> Self {
        self.prompt_sections.push((section, template.into()));
        self
    }

    /// Drop a prompt section. A section that was also replaced by
    /// [`Self::prompt_section`] is dropped, because this is applied last.
    pub fn without_prompt_section(mut self, section: PromptSection) -> Self {
        self.disabled_sections.push(section);
        self
    }

    /// Cap how many times `tool` may run before the model is told to save.
    ///
    /// `max == 0` removes the cap.
    pub fn tool_budget(mut self, tool: impl Into<String>, max: usize) -> Self {
        self.tool_budgets.push((tool.into(), max));
        self
    }

    /// 呼び出し側のプロンプト版を連結する（例: `"stock-3"` →
    /// `"save-contract-v4+stock-3"`）。空文字・空白のみは無視される。
    ///
    /// 連結結果は [`FillOutcome::prompt_version`] から読める。
    pub fn prompt_version(mut self, suffix: impl Into<String>) -> Self {
        self.prompt_version = Some(suffix.into());
        self
    }

    /// Stop allowing investigation once `rounds` tool-call rounds have passed.
    ///
    /// From that round on, the request advertises only the save tools and
    /// `give_up`, and a single user message asks the model to save what it has
    /// found. `0` forces saving from the first request. Unset by default, so
    /// the tool list stays the same in every round.
    pub fn force_save_after(mut self, rounds: usize) -> Self {
        self.force_save_after = Some(rounds);
        self
    }

    pub async fn run(self) -> Result<FillOutcome<T>, FillError> {
        let max_turns = self.max_turns.max(1);
        let max_rounds = self.max_rounds.max(1);
        let force_save_after = self.force_save_after;

        let mut session = match self.seed {
            Some(seed) => FillSession::<T>::with_seed(self.prompt, seed)?,
            None => FillSession::<T>::new(self.prompt)?,
        };
        // 名前の衝突は「後から適用した側が無視される」。Array モードで実際に
        // 使われるのは配列名なので、先に適用して勝たせる。
        let strict = self.strict_tool_names;
        if session.is_array() {
            if let Some(names) = self.array_tool_names {
                session = if strict {
                    session.try_with_array_tool_names(names)?
                } else {
                    session.with_array_tool_names(names)
                };
            }
            if let Some(name) = self.save_tool_name {
                session = if strict {
                    session.try_with_save_tool_name(name)?
                } else {
                    session.with_save_tool_name(name)
                };
            }
        } else {
            if let Some(name) = self.save_tool_name {
                session = if strict {
                    session.try_with_save_tool_name(name)?
                } else {
                    session.with_save_tool_name(name)
                };
            }
            if let Some(names) = self.array_tool_names {
                session = if strict {
                    session.try_with_array_tool_names(names)?
                } else {
                    session.with_array_tool_names(names)
                };
            }
        }
        if let Some(name) = self.give_up_tool_name {
            session = if strict {
                session.try_with_give_up_tool_name(name)?
            } else {
                session.with_give_up_tool_name(name)
            };
        }
        if !self.goal_fields.is_empty() {
            session = session.with_goal_fields(self.goal_fields)?;
        }
        if let Some(policy) = self.goal_empty {
            session = session.with_goal_empty(policy);
        }
        if let Some(policy) = self.extra_keys {
            session = session.with_extra_keys(policy);
        }
        if let Some(enabled) = self.schema_guide {
            session = session.with_schema_guide(enabled);
        }
        if let Some(mode) = self.guide_mode {
            session = session.with_guide_mode(mode);
        }
        if let Some(options) = self.guide_options {
            session = session.with_guide_options(options);
        }
        if let Some(options) = self.feedback_options {
            session = session.with_feedback_options(options);
        }
        for (section, template) in self.prompt_sections {
            session = session.with_prompt_section(section, template)?;
        }
        for section in self.disabled_sections {
            session = session.without_prompt_section(section);
        }
        for (tool, max) in self.tool_budgets {
            session = session.with_tool_budget(tool, max);
        }
        if let Some(suffix) = self.prompt_version {
            session = session.with_prompt_version(suffix);
        }
        for tool in self.tools {
            session.add_tool(tool);
        }
        // 監査は登録済みのツール名も見るので、ツール追加の後に行う。
        session = session.with_prompt_audit();
        let mut messages: Vec<ChatMessage> = vec![ChatMessage::user(session.start_message())];
        let force_save_message = session.save_only_message();

        let mut turns = 0usize;
        let mut rounds = 0usize;
        let mut nudged = false;
        // 閾値到達をラッチする。ツール無し応答で `rounds` は 0 に戻るので、
        // 毎周計算し直すと調査ツールと allowlist が復活してしまう。
        let mut forced = false;

        loop {
            if turns >= max_turns {
                break;
            }

            let forcing = forced || force_save_after.is_some_and(|threshold| rounds >= threshold);
            forced = forcing;
            if forcing && !nudged {
                messages.push(ChatMessage::user(force_save_message.clone()));
                nudged = true;
            }

            let mut request = ChatRequest::from_system(session.system_prompt());
            request.messages = messages.clone();
            let specs = if forcing {
                session.save_only_tool_specs()
            } else {
                session.tool_specs()
            };
            if forcing {
                let names = specs.iter().map(|spec| spec.name.clone()).collect();
                session.set_tool_allowlist(Some(names));
            } else {
                session.set_tool_allowlist(None);
            }
            request.tools = Some(specs.iter().map(to_genai_tool).collect());

            let response = self.backend.chat(request).await?;
            let calls = response.tool_calls();

            if calls.is_empty() {
                // A text-only response advances the turn. Tell the model
                // explicitly that the JSON is still incomplete, and reset the
                // per-turn round counter.
                turns += 1;
                rounds = 0;
                let text = response.first_text().unwrap_or("");
                messages.push(ChatMessage::user(format!(
                    "ツールを呼び出してください。\n{}\nあなたのテキスト応答: {text}",
                    session.progress_feedback()
                )));
                if turns >= max_turns {
                    break;
                }
                continue;
            }

            // A tool-call response is one round inside the current turn.
            rounds += 1;

            let mut parts: Vec<ContentPart> = Vec::new();
            if let Some(text) = response.first_text().filter(|text| !text.is_empty()) {
                parts.push(ContentPart::Text(text.to_string()));
            }
            for call in &calls {
                parts.push(ContentPart::ToolCall((*call).clone()));
            }
            messages.push(ChatMessage::assistant(MessageContent::from_parts(parts)));

            for call in calls {
                match session.call(&call.fn_name, call.fn_arguments.clone()).await {
                    SessionResult::Continue { content, .. } => {
                        messages.push(ChatMessage::from(vec![ToolResponse {
                            call_id: call.call_id.clone(),
                            fn_name: Some(call.fn_name.clone()),
                            content,
                        }]));
                    }
                    SessionResult::Done { .. } => {
                        return session.into_outcome();
                    }
                    SessionResult::Impossible { .. } => {
                        return session.into_outcome();
                    }
                }
            }

            if rounds >= max_rounds {
                break;
            }
        }

        session.into_outcome()
    }
}

struct GenaiToolHandler<H> {
    spec: ToolSpec,
    handler: H,
}

impl<H> Tool for GenaiToolHandler<H>
where
    H: Fn(Value) -> BoxFuture<'static, Result<ToolOutput, FillError>> + Send + Sync,
{
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<ToolOutput, FillError>> {
        (self.handler)(args)
    }
}
