use std::sync::Arc;

use ai_json_filler_core::{FillError, FillOutcome, FillSession, SessionResult};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::Mutex;

pub enum CallOutcome<T> {
    Continue {
        content: String,
        is_error: bool,
    },
    Done {
        content: String,
        outcome: FillOutcome<T>,
    },
    Impossible {
        reason: String,
        missing_fields: Vec<String>,
    },
}

pub struct SharedSession<T> {
    inner: Arc<Mutex<Option<FillSession<T>>>>,
}

impl<T> SharedSession<T>
where
    T: DeserializeOwned + JsonSchema,
{
    pub fn new(session: FillSession<T>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Some(session))),
        }
    }

    pub async fn call(&self, name: &str, args: Value) -> Result<CallOutcome<T>, FillError> {
        let mut guard = self.inner.lock().await;
        let session = guard.as_mut().ok_or(FillError::Incomplete {
            partial: Value::Null,
        })?;
        match session.call(name, args).await {
            SessionResult::Continue { content, is_error } => {
                Ok(CallOutcome::Continue { content, is_error })
            }
            SessionResult::Done { content } => {
                let session = guard.take().ok_or(FillError::Incomplete {
                    partial: Value::Null,
                })?;
                let outcome = session.into_outcome()?;
                Ok(CallOutcome::Done { content, outcome })
            }
            SessionResult::Impossible {
                reason,
                missing_fields,
            } => {
                let _ = guard.take();
                Ok(CallOutcome::Impossible {
                    reason,
                    missing_fields,
                })
            }
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct Company {
    name: String,
    employees: u32,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let shared = SharedSession::new(FillSession::<Company>::new("会社情報を調べて")?);

    match shared
        .call("save_result", json!({"name": "トヨタ"}))
        .await?
    {
        CallOutcome::Continue { .. } => {}
        CallOutcome::Done { .. } => return Err("早すぎる完了".into()),
        CallOutcome::Impossible { reason, .. } => return Err(format!("不可能: {reason}").into()),
    }

    match shared
        .call("save_result", json!({"employees": 370000}))
        .await?
    {
        CallOutcome::Done { outcome, .. } => {
            println!("{} {}", outcome.value.name, outcome.value.employees);
        }
        CallOutcome::Continue { content, .. } => return Err(content.into()),
        CallOutcome::Impossible { reason, .. } => return Err(format!("不可能: {reason}").into()),
    }

    Ok(())
}
