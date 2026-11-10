use ai_json_filler_genai::fill;
use genai::Client;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
#[allow(dead_code)] // 表示は Debug 経由なのでフィールド直接参照は無い
struct CompanyProfile {
    /// 会社名（日本語）
    name: String,
    /// 従業員数
    employees: u32,
    /// 本社所在地
    headquarters: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = std::env::var("MODEL").unwrap_or_else(|_| "qwen3:8b".to_string());

    let client = Client::builder().build()?;
    let outcome = fill::<CompanyProfile>(&client, model, "トヨタ自動車の会社情報を調べてください")
        .max_rounds(10)
        .run()
        .await?;

    println!("{:#?}", outcome.value);
    Ok(())
}
