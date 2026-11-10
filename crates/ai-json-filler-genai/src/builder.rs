use std::marker::PhantomData;
use std::sync::Arc;

use ai_json_filler_core::{
    BoxFuture, FillError, FillOutcome, FillSession, SessionResult, Tool, ToolOutput, ToolSpec,
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
    pub fn save_tool_name(mut self, name: impl Into<String>) -> Self {
        self.save_tool_name = Some(name.into());
        self
    }

    /// Override the name of the tool that declares the goal unreachable
    /// (default: `give_up`).
    pub fn give_up_tool_name(mut self, name: impl Into<String>) -> Self {
        self.give_up_tool_name = Some(name.into());
        self
    }

    pub async fn run(self) -> Result<FillOutcome<T>, FillError> {
        let max_turns = self.max_turns.max(1);
        let max_rounds = self.max_rounds.max(1);

        let mut session = match self.seed {
            Some(seed) => FillSession::<T>::with_seed(self.prompt, seed)?,
            None => FillSession::<T>::new(self.prompt)?,
        };
        if let Some(name) = self.save_tool_name {
            session = session.with_save_tool_name(name);
        }
        if let Some(name) = self.give_up_tool_name {
            session = session.with_give_up_tool_name(name);
        }
        for tool in self.tools {
            session.add_tool(tool);
        }
        let save_tool_name = session.save_tool_name().to_string();
        let give_up_tool_name = session.give_up_tool_name().to_string();

        let mut messages: Vec<ChatMessage> = vec![ChatMessage::user(format!(
            "調査を開始し、{save_tool_name} ツールで結果を保存してください。調べても分からない場合は {give_up_tool_name} を呼んでください"
        ))];

        let mut turns = 0usize;
        let mut rounds = 0usize;

        loop {
            if turns >= max_turns {
                break;
            }

            let mut request = ChatRequest::from_system(session.system_prompt());
            request.messages = messages.clone();
            request.tools = Some(session.tool_specs().iter().map(to_genai_tool).collect());

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
