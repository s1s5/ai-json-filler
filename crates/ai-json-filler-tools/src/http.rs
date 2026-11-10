use std::time::Duration;

use ai_json_filler_core::FillError;
use serde_json::Value;

pub(crate) fn json_error_snippet(status: reqwest::StatusCode, body: &str) -> String {
    let snippet: String = body.chars().take(300).collect();
    format!("{status}: {snippet}")
}

pub(crate) fn build_client(timeout_secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs.max(1)))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Send a request and decode a JSON body.
///
/// Transport failures and non-2xx responses come back as a short message that
/// callers wrap in a tool error. Keeping this in one place makes the search and
/// fetch providers behave identically.
pub(crate) async fn read_json(
    response: Result<reqwest::Response, reqwest::Error>,
) -> Result<Value, String> {
    let response = response.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(json_error_snippet(status, &body));
    }
    response.json().await.map_err(|e| e.to_string())
}

pub(crate) fn to_tool_error(name: &str, message: impl Into<String>) -> FillError {
    FillError::Tool {
        name: name.to_string(),
        message: message.into(),
    }
}
