use ai_json_filler_core::{FillSession, SessionResult};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize, JsonSchema)]
struct Company {
    name: String,
    employees: u32,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut session = FillSession::<Company>::new("会社情報を調べて")?;
    assert_eq!(session.tool_specs()[0].name, "save_result");

    match session.call("save_result", json!({"name": "トヨタ"})).await {
        SessionResult::Continue { .. } => {}
        SessionResult::Done { .. } => return Err("早すぎる完了".into()),
        SessionResult::Impossible { reason, .. } => return Err(format!("不可能: {reason}").into()),
    }

    match session
        .call("save_result", json!({"employees": 370000}))
        .await
    {
        SessionResult::Done { content } => println!("{content}"),
        SessionResult::Continue { content, .. } => return Err(content.into()),
        SessionResult::Impossible { reason, .. } => return Err(format!("不可能: {reason}").into()),
    }

    let outcome = session.into_outcome()?;
    println!("{} {}", outcome.value.name, outcome.value.employees);
    Ok(())
}
