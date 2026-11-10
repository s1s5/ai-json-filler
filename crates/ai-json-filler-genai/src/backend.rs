use ai_json_filler_core::{BoxFuture, FillError};
use genai::{Client, ModelSpec};

/// Abstraction over a single chat turn.
///
/// Implemented by [`GenaiBackend`] for real providers and by test doubles that
/// replay scripted responses.
pub trait ChatBackend: Send + Sync {
    fn chat<'a>(
        &'a self,
        request: genai::chat::ChatRequest,
    ) -> BoxFuture<'a, Result<genai::chat::ChatResponse, FillError>>;
}

/// [`ChatBackend`] backed by a `genai` client and a resolved model spec.
pub struct GenaiBackend {
    client: Client,
    model: ModelSpec,
}

impl GenaiBackend {
    pub fn new(client: Client, model: impl Into<ModelSpec>) -> Self {
        Self {
            client,
            model: model.into(),
        }
    }
}

impl ChatBackend for GenaiBackend {
    fn chat<'a>(
        &'a self,
        request: genai::chat::ChatRequest,
    ) -> BoxFuture<'a, Result<genai::chat::ChatResponse, FillError>> {
        Box::pin(async move {
            self.client
                .exec_chat(self.model.clone(), request, None)
                .await
                .map_err(|e| FillError::Llm(e.to_string()))
        })
    }
}
