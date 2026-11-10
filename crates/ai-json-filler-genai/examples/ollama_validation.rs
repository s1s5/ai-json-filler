//! Real-LLM validation for the OpenAI-compatible Ollama（mint-ornith）endpoint.
//!
//! Run:
//!   cargo run -p ai-json-filler-genai --example ollama_validation 2>&1 | tee /tmp/aijf-mint-validation.log

use std::sync::Arc;

use ai_json_filler_core::{FillError, Tool, ToolOutput, typed_tool};
use ai_json_filler_genai::{ChatBackend, GenaiBackend, fill_with_backend};
use genai::adapter::AdapterKind;
use genai::chat::{ChatRequest, ChatResponse};
use genai::resolver::{AuthData, Endpoint};
use genai::{Client, ModelIden, ServiceTarget};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
struct WeatherReport {
    /// 都市名
    city: String,
    /// 気温（摂氏）
    temperature_c: i32,
    /// 天気
    condition: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
struct DetailedReport {
    /// 都市名
    city: String,
    /// 気温（摂氏）
    temperature_c: i32,
    /// 湿度（%）
    humidity_percent: i32,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
struct WeatherArgs {
    /// 都市名
    city: String,
}

struct LoggingBackend {
    inner: GenaiBackend,
}

impl ChatBackend for LoggingBackend {
    fn chat<'a>(
        &'a self,
        request: ChatRequest,
    ) -> ai_json_filler_core::BoxFuture<'a, Result<ChatResponse, FillError>> {
        Box::pin(async move {
            eprintln!("\n--- LLM REQUEST ---");
            eprintln!("system:\n{}", request.system.as_deref().unwrap_or(""));
            eprintln!(
                "tools: {} 個",
                request.tools.as_ref().map(Vec::len).unwrap_or(0)
            );
            for (i, message) in request.messages.iter().enumerate() {
                eprintln!("[{i}] {:?}", message.role);
                for text in message.content.texts() {
                    eprintln!("    text: {text}");
                }
                for call in message.content.tool_calls() {
                    eprintln!("    tool_call: {} {}", call.fn_name, call.fn_arguments);
                }
                for response in message.content.tool_responses() {
                    eprintln!(
                        "    tool_response: call_id={} content={}",
                        response.call_id, response.content
                    );
                }
            }

            let response = self.inner.chat(request).await?;

            eprintln!("--- LLM RESPONSE ---");
            eprintln!("text: {:?}", response.first_text());
            for call in response.tool_calls() {
                eprintln!("tool_call: {} {}", call.fn_name, call.fn_arguments);
            }
            eprintln!("usage: {:?}", response.usage);
            Ok(response)
        })
    }
}

fn target() -> ServiceTarget {
    ServiceTarget {
        endpoint: Endpoint::from_owned("http://100.78.108.50:11434/v1/".to_string()),
        auth: AuthData::from_single("not-needed"),
        model: ModelIden::new(AdapterKind::OpenAI, "ornith-1.5:35b"),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::builder().build()?;
    let backend: Arc<dyn ChatBackend> = Arc::new(LoggingBackend {
        inner: GenaiBackend::new(client, target()),
    });

    println!("########## Scenario A: basic fill ##########");
    match fill_with_backend::<WeatherReport>(
        backend.clone(),
        "以下の事実だけを使って WeatherReport を埋めてください。東京は晴れで、気温は22度です。",
    )
    .max_rounds(6)
    .run()
    .await
    {
        Ok(outcome) => println!("RESULT OK: {:?}\nraw={}", outcome.value, outcome.raw),
        Err(e) => println!("RESULT ERR: {e}"),
    }

    println!("\n########## Scenario B: seed preserves existing field ##########");
    match fill_with_backend::<WeatherReport>(
        backend.clone(),
        "大阪は雨で、気温は18度です。気温と天気を埋めてください。都市名は既に保存済みです。",
    )
    .seed(json!({ "city": "大阪" }))
    .max_rounds(6)
    .run()
    .await
    {
        Ok(outcome) => println!("RESULT OK: {:?}\nraw={}", outcome.value, outcome.raw),
        Err(e) => println!("RESULT ERR: {e}"),
    }

    println!("\n########## Scenario C: validation feedback loop ##########");
    match fill_with_backend::<DetailedReport>(
        backend.clone(),
        "東京は晴れで、気温は22度です。湿度は分かりません。絶対に推測しないでください。分かる項目だけ save_result で保存してください。",
    )
    .max_rounds(4)
    .run()
    .await
    {
        Ok(outcome) => println!("RESULT OK (model filled humidity): {:?}\nraw={}", outcome.value, outcome.raw),
        Err(FillError::Incomplete { partial }) => {
            println!("RESULT INCOMPLETE (expected): partial={partial}")
        }
        Err(e) => println!("RESULT ERR: {e}"),
    }

    println!("\n########## Scenario D: custom tool call ##########");
    let weather_tool: Arc<dyn Tool> = Arc::new(typed_tool::<WeatherArgs, _>(
        "get_weather",
        "指定した都市の現在の天気を返す",
        |args| {
            Box::pin(async move {
                let value = json!({
                    "city": args.city,
                    "temperature_c": 22,
                    "condition": "晴れ"
                });
                Ok(ToolOutput::success(value.to_string()))
            })
        },
    ));

    match fill_with_backend::<WeatherReport>(
        backend.clone(),
        "get_weather ツールで東京の天気を調べ、その結果だけを使って WeatherReport を埋めてください。",
    )
    .tool(weather_tool)
    .max_rounds(6)
    .run()
    .await
    {
        Ok(outcome) => println!("RESULT OK: {:?}\nraw={}", outcome.value, outcome.raw),
        Err(e) => println!("RESULT ERR: {e}"),
    }

    Ok(())
}
