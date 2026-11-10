//! SearXNG + Crawl4AI + Ollama（mint-ornith）の E2E 検証。
//!
//! ケース定義: `thoughts/2026-11-10-ai-json-filler/10-web-e2e-cases.md`
//!
//! 実行:
//!   cargo run -p ai-json-filler-genai --example ollama_web_validation 2>&1 | tee /tmp/aijf-web-e2e.log
//!   cargo run -p ai-json-filler-genai --example ollama_web_validation -- a c g
//!
//! 環境変数:
//!   MINT_ENDPOINT  既定 http://100.78.108.50:11434/v1/
//!   MINT_MODEL     既定 ornith-1.5:35b
//!   SEARXNG_URL    既定 http://searxng.localhost
//!   CRAWL4AI_URL   既定 http://crawl4ai.localhost
//!
//! Tavily は使わない（`WebTools::builder()` に Tavily キーを渡さない）。

use std::process::ExitCode;
use std::sync::Arc;

use ai_json_filler_core::{BoxFuture, FillError, FillOutcome, Tool};
use ai_json_filler_genai::{ChatBackend, GenaiBackend, fill_with_backend};
use ai_json_filler_tools::WebTools;
use genai::adapter::AdapterKind;
use genai::chat::{ChatRequest, ChatResponse};
use genai::resolver::{AuthData, Endpoint};
use genai::{Client, ModelIden, ServiceTarget};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// バックエンド（ログ付き）
// ---------------------------------------------------------------------------

struct LoggingBackend {
    inner: Arc<GenaiBackend>,
    case: String,
}

impl ChatBackend for LoggingBackend {
    fn chat<'a>(&'a self, request: ChatRequest) -> BoxFuture<'a, Result<ChatResponse, FillError>> {
        Box::pin(async move {
            eprintln!(
                "\n[{}] --- REQUEST: {} messages, {} tools ---",
                self.case,
                request.messages.len(),
                request.tools.as_ref().map(Vec::len).unwrap_or(0)
            );
            if let Some(last) = request.messages.last() {
                for text in last.content.texts() {
                    eprintln!("[{}] user: {}", self.case, truncate(text, 240));
                }
                for response in last.content.tool_responses() {
                    eprintln!(
                        "[{}] tool_response: {}",
                        self.case,
                        truncate(&response.content, 240)
                    );
                }
            }

            let response = self.inner.chat(request).await?;

            if let Some(text) = response.first_text().filter(|t| !t.is_empty()) {
                eprintln!("[{}] text: {}", self.case, truncate(text, 240));
            }
            for call in response.tool_calls() {
                eprintln!(
                    "[{}] call: {} {}",
                    self.case,
                    call.fn_name,
                    truncate(&call.fn_arguments.to_string(), 400)
                );
            }
            eprintln!("[{}] usage: {:?}", self.case, response.usage);
            Ok(response)
        })
    }
}

fn truncate(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

// ---------------------------------------------------------------------------
// 共通
// ---------------------------------------------------------------------------

struct Ctx {
    backend: Arc<GenaiBackend>,
    tools: Vec<Arc<dyn Tool>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Expect {
    Ok,
    Impossible,
}

#[derive(PartialEq)]
enum Verdict {
    Pass,
    Fail,
    Incomplete,
}

struct CaseReport {
    case: String,
    verdict: Verdict,
    detail: String,
}

impl CaseReport {
    fn line(&self) -> String {
        let label = match self.verdict {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::Incomplete => "INCOMPLETE",
        };
        format!("{:<3} {} {}", self.case, label, self.detail)
    }
}

async fn run_case<T>(
    ctx: &Ctx,
    case: &str,
    prompt: &str,
    seed: Option<Value>,
    max_rounds: usize,
    expect: Expect,
    check: impl FnOnce(&Result<FillOutcome<T>, FillError>) -> Vec<String>,
) -> CaseReport
where
    T: DeserializeOwned + JsonSchema,
{
    let backend: Arc<dyn ChatBackend> = Arc::new(LoggingBackend {
        inner: ctx.backend.clone(),
        case: case.to_string(),
    });

    let mut builder = fill_with_backend::<T>(backend, prompt)
        .max_turns(3)
        .max_rounds(max_rounds);
    if let Some(seed) = seed {
        builder = builder.seed(seed);
    }
    for tool in &ctx.tools {
        builder = builder.tool(tool.clone());
    }

    let result = builder.run().await;

    let detail = match &result {
        Ok(outcome) => format!("raw={}", outcome.raw),
        Err(FillError::Impossible {
            reason,
            missing_fields,
            partial,
        }) => format!("reason={reason} missing={missing_fields:?} partial={partial}"),
        Err(error) => format!("{error}"),
    };

    let (verdict, failures) = match (expect, &result) {
        (_, Err(FillError::Incomplete { .. })) => (
            Verdict::Incomplete,
            vec!["上限到達（判定不能）".to_string()],
        ),
        (Expect::Ok, Ok(_)) => {
            let failures = check(&result);
            let verdict = if failures.is_empty() {
                Verdict::Pass
            } else {
                Verdict::Fail
            };
            (verdict, failures)
        }
        (Expect::Ok, Err(FillError::Impossible { .. })) => (
            Verdict::Fail,
            vec!["成功を期待したが give_up された".to_string()],
        ),
        (Expect::Ok, Err(error)) => (Verdict::Fail, vec![format!("エラー: {error}")]),
        (Expect::Impossible, Err(FillError::Impossible { .. })) => {
            let failures = check(&result);
            let verdict = if failures.is_empty() {
                Verdict::Pass
            } else {
                Verdict::Fail
            };
            (verdict, failures)
        }
        (Expect::Impossible, Ok(_)) => (
            Verdict::Fail,
            vec!["give_up を期待したが Ok（捏造の可能性）".to_string()],
        ),
        (Expect::Impossible, Err(error)) => (Verdict::Fail, vec![format!("エラー: {error}")]),
    };

    let detail = if failures.is_empty() {
        detail
    } else {
        format!("{detail} / {}", failures.join("; "))
    };

    CaseReport {
        case: case.to_string(),
        verdict,
        detail,
    }
}

// ---------------------------------------------------------------------------
// A. 基本 — 清水寺
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct TempleInfo {
    /// 寺院名
    name: String,
    /// 所在地（都道府県と市区町村）
    location: String,
    /// 創建年（西暦）
    founded_year: u32,
    /// 世界遺産登録年（西暦）
    unesco_year: u32,
}

async fn case_a(ctx: &Ctx) -> CaseReport {
    run_case::<TempleInfo>(
        ctx,
        "A",
        "清水寺について調べ、TempleInfo を埋めてください。所在地は都道府県と市区町村まで含めてください。創建年と世界遺産登録年は西暦の数値で入れてください。",
        None,
        10,
        Expect::Ok,
        |result| {
            let mut failures = Vec::new();
            if let Ok(outcome) = result {
                let v = &outcome.value;
                if !v.name.contains("清水寺") {
                    failures.push(format!("name={}", v.name));
                }
                if !v.location.contains("京都") {
                    failures.push(format!("location={}", v.location));
                }
                if v.founded_year != 778 {
                    failures.push(format!("founded_year={}", v.founded_year));
                }
                if v.unesco_year != 1994 {
                    failures.push(format!("unesco_year={}", v.unesco_year));
                }
            }
            failures
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// B. 数値 — トヨタ自動車
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct CompanyProfile {
    /// 会社名
    name: String,
    /// 設立年（西暦）
    founded_year: u32,
    /// 本社所在地（都道府県と市区町村）
    headquarters: String,
    /// 連結従業員数（人）
    employees: u32,
}

async fn case_b(ctx: &Ctx) -> CaseReport {
    run_case::<CompanyProfile>(
        ctx,
        "B",
        "トヨタ自動車の会社概要を調べ、CompanyProfile を埋めてください。employees は連結の従業員数（人）です。数値は検索結果に書かれている値をそのまま使ってください。",
        None,
        10,
        Expect::Ok,
        |result| {
            let mut failures = Vec::new();
            if let Ok(outcome) = result {
                let v = &outcome.value;
                if !(380_000..=388_000).contains(&v.employees) {
                    failures.push(format!("employees={}", v.employees));
                }
                if !v.headquarters.contains("愛知") {
                    failures.push(format!("headquarters={}", v.headquarters));
                }
                if !v.name.contains("トヨタ") {
                    failures.push(format!("name={}", v.name));
                }
                if v.founded_year != 1937 {
                    failures.push(format!("founded_year={}", v.founded_year));
                }
            }
            failures
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// C. ネスト配列 — 証券コード
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct CompanyBrief {
    /// 会社名
    name: String,
    /// 証券コード（4桁）
    ticker: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TickerList {
    /// 上場市場名（例: プライム）
    market: String,
    /// 対象企業
    companies: Vec<CompanyBrief>,
}

async fn case_c(ctx: &Ctx) -> CaseReport {
    run_case::<TickerList>(
        ctx,
        "C",
        "トヨタ自動車、本田技研工業、日産自動車の3社について、東京証券取引所の証券コードを調べて TickerList を埋めてください。market は上場市場名です。",
        None,
        10,
        Expect::Ok,
        |result| {
            let mut failures = Vec::new();
            if let Ok(outcome) = result {
                let v = &outcome.value;
                if !v.market.contains("プライム") {
                    failures.push(format!("market={}", v.market));
                }
                if v.companies.len() != 3 {
                    failures.push(format!("companies.len()={}", v.companies.len()));
                }
                let expect = [
                    ("7203", "トヨタ"),
                    ("7267", "本田"),
                    ("7201", "日産"),
                ];
                for (ticker, name) in expect {
                    match v.companies.iter().find(|c| c.ticker == ticker) {
                        Some(company) => {
                            let alias = if ticker == "7267" { "ホンダ" } else { name };
                            if !company.name.contains(name) && !company.name.contains(alias) {
                                failures.push(format!("{ticker} の会社名={}", company.name));
                            }
                        }
                        None => failures.push(format!("{ticker} が無い")),
                    }
                }
            }
            failures
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// D. seed 保持 — 札幌の天気
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct WeatherReport {
    /// 都市名
    city: String,
    /// 気温（摂氏）
    temperature_c: i32,
    /// 天気
    condition: String,
}

async fn case_d(ctx: &Ctx) -> CaseReport {
    run_case::<WeatherReport>(
        ctx,
        "D",
        "札幌の今日の天気を調べて、気温と天気を保存してください。都市名は既に保存済みです。",
        Some(json!({ "city": "札幌" })),
        10,
        Expect::Ok,
        |result| {
            let mut failures = Vec::new();
            if let Ok(outcome) = result {
                let v = &outcome.value;
                if v.city != "札幌" {
                    failures.push(format!("city={}", v.city));
                }
                if !(-30..=40).contains(&v.temperature_c) {
                    failures.push(format!("temperature_c={}", v.temperature_c));
                }
                if v.condition.trim().is_empty() {
                    failures.push("condition が空".to_string());
                }
            }
            failures
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// E. 架空製品 — give_up
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
struct ProductSpec {
    /// 製品名
    name: String,
    /// メーカー名
    manufacturer: String,
    /// 発売年（西暦）
    release_year: u32,
    /// 価格（円）
    price_jpy: u32,
}

async fn case_e(ctx: &Ctx) -> CaseReport {
    run_case::<ProductSpec>(
        ctx,
        "E",
        "架空の製品「ホログラム炊飯器 HRX-9000」の仕様を調べて ProductSpec を埋めてください。実在しない製品の場合は、推測で埋めずに give_up ツールで理由を報告してください。",
        None,
        8,
        Expect::Impossible,
        |result| {
            let mut failures = Vec::new();
            if let Err(FillError::Impossible {
                reason,
                missing_fields,
                ..
            }) = result
            {
                if reason.trim().is_empty() {
                    failures.push("reason が空".to_string());
                }
                if missing_fields.is_empty() {
                    failures.push("missing_fields が空".to_string());
                }
            }
            failures
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// F. 非公表の数値 — 部分保存 → give_up
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
struct ExamStats {
    /// 大学名
    university: String,
    /// 年度（西暦）
    year: u32,
    /// 受験者数（人）
    applicants: u32,
    /// 全受験者の平均点
    average_score: f64,
}

async fn case_f(ctx: &Ctx) -> CaseReport {
    run_case::<ExamStats>(
        ctx,
        "F",
        "東京大学の2026年度一般選抜（前期日程）について、受験者数と全受験者の平均点を調べて ExamStats を埋めてください。平均点が公表されていない場合は、推測せずに give_up ツールで理由を報告してください。",
        None,
        12,
        Expect::Impossible,
        |result| {
            let mut failures = Vec::new();
            if let Err(FillError::Impossible {
                reason, partial, ..
            }) = result
            {
                if reason.trim().is_empty() {
                    failures.push("reason が空".to_string());
                }
                let hints = [
                    "非公表",
                    "公表されていない",
                    "公表していない",
                    "不明",
                    "確認でき",
                ];
                if !hints.iter().any(|hint| reason.contains(hint)) {
                    failures.push(format!("reason に非公表の言及なし: {reason}"));
                }
                let has_saved = ["applicants", "year", "university"]
                    .iter()
                    .any(|key| partial.get(*key).is_some());
                if !has_saved {
                    failures.push("既知項目が1つも部分保存されていない".to_string());
                }
                if let Some(name) = partial.get("university").and_then(Value::as_str)
                    && !name.contains("東京大学")
                {
                    failures.push(format!("partial.university={name}"));
                }
                if let Some(year) = partial.get("year").and_then(Value::as_u64)
                    && year != 2026
                {
                    failures.push(format!("partial.year={year}"));
                }
            }
            failures
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// G. 2段ネスト — 2024年ノーベル物理学賞
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct NobelAward {
    /// 賞の名前
    prize: String,
    /// 受賞年
    year: u32,
    /// 受賞者
    laureates: Vec<Laureate>,
    /// 授賞式
    ceremony: Ceremony,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct Laureate {
    /// 氏名
    name: String,
    /// 所属
    affiliation: Affiliation,
    /// 受賞理由の要約
    contribution: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct Affiliation {
    /// 機関名
    institution: String,
    /// 国名
    country: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct Ceremony {
    /// 都市
    city: String,
    /// 日付（例: 12月10日）
    date: String,
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

async fn case_g(ctx: &Ctx) -> CaseReport {
    run_case::<NobelAward>(
        ctx,
        "G",
        "2024年のノーベル物理学賞について調べ、NobelAward を埋めてください。受賞者は2名で、それぞれの所属機関と国、受賞理由の要約を入れてください。授賞式の都市と日付も入れてください。",
        None,
        12,
        Expect::Ok,
        |result| {
            let mut failures = Vec::new();
            if let Ok(outcome) = result {
                let v = &outcome.value;
                if !v.prize.contains("物理学") {
                    failures.push(format!("prize={}", v.prize));
                }
                if v.year != 2024 {
                    failures.push(format!("year={}", v.year));
                }
                if v.laureates.len() != 2 {
                    failures.push(format!("laureates.len()={}", v.laureates.len()));
                }
                let hopfield = v.laureates.iter().find(|l| {
                    contains_any(&l.name, &["ホップフィールド", "Hopfield"])
                });
                let hinton = v
                    .laureates
                    .iter()
                    .find(|l| contains_any(&l.name, &["ヒントン", "Hinton"]));
                match hopfield {
                    Some(l) => {
                        if !contains_any(&l.affiliation.institution, &["プリンストン", "Princeton"]) {
                            failures.push(format!(
                                "Hopfield institution={}",
                                l.affiliation.institution
                            ));
                        }
                        if !contains_any(
                            &l.affiliation.country,
                            &["アメリカ", "米国", "USA", "United States"],
                        ) {
                            failures.push(format!("Hopfield country={}", l.affiliation.country));
                        }
                        if l.contribution.trim().is_empty() {
                            failures.push("Hopfield contribution が空".to_string());
                        }
                    }
                    None => failures.push("Hopfield が見つからない".to_string()),
                }
                match hinton {
                    Some(l) => {
                        if !contains_any(&l.affiliation.institution, &["トロント", "Toronto"]) {
                            failures.push(format!(
                                "Hinton institution={}",
                                l.affiliation.institution
                            ));
                        }
                        if !contains_any(&l.affiliation.country, &["カナダ", "Canada"]) {
                            failures.push(format!("Hinton country={}", l.affiliation.country));
                        }
                        if l.contribution.trim().is_empty() {
                            failures.push("Hinton contribution が空".to_string());
                        }
                    }
                    None => failures.push("Hinton が見つからない".to_string()),
                }
                if !v.ceremony.city.contains("ストックホルム") {
                    failures.push(format!("ceremony.city={}", v.ceremony.city));
                }
                if v.ceremony.date.trim().is_empty() {
                    failures.push("ceremony.date が空".to_string());
                }
            }
            failures
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// H. 複数ソース＋計算 — トヨタ2025年3月期
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct FinancialSummary {
    /// 会社名
    company: String,
    /// 会計年度（2025年3月期なら 2025）
    fiscal_year: u32,
    /// 売上高（億円）
    revenue_oku_yen: f64,
    /// 営業利益（億円）
    operating_income_oku_yen: f64,
    /// 営業利益率（%）
    operating_margin_percent: f64,
    /// 連結従業員数（人）
    employees: u32,
    /// 参照した情報源の URL
    sources: Vec<String>,
}

async fn case_h(ctx: &Ctx) -> CaseReport {
    run_case::<FinancialSummary>(
        ctx,
        "H",
        "トヨタ自動車の2025年3月期（2025年度）の連結売上高と営業利益を調べ、営業利益率を計算して FinancialSummary を埋めてください。従業員数も入れてください。値は複数の情報源で確認し、参照した URL を sources に2件以上入れてください。金額の単位は億円です。",
        None,
        12,
        Expect::Ok,
        |result| {
            let mut failures = Vec::new();
            if let Ok(outcome) = result {
                let v = &outcome.value;
                if !v.company.contains("トヨタ") {
                    failures.push(format!("company={}", v.company));
                }
                if v.fiscal_year != 2025 {
                    failures.push(format!("fiscal_year={}", v.fiscal_year));
                }
                if !(478_000.0..=482_500.0).contains(&v.revenue_oku_yen) {
                    failures.push(format!("revenue_oku_yen={}", v.revenue_oku_yen));
                }
                if !(47_500.0..=48_500.0).contains(&v.operating_income_oku_yen) {
                    failures.push(format!(
                        "operating_income_oku_yen={}",
                        v.operating_income_oku_yen
                    ));
                }
                if !(9.0..=10.5).contains(&v.operating_margin_percent) {
                    failures.push(format!(
                        "operating_margin_percent={}",
                        v.operating_margin_percent
                    ));
                } else {
                    let derived =
                        v.operating_income_oku_yen / v.revenue_oku_yen * 100.0;
                    if (derived - v.operating_margin_percent).abs() > 0.2 {
                        failures.push(format!(
                            "利益率が計算と不一致: 申告={} 計算={derived:.2}",
                            v.operating_margin_percent
                        ));
                    }
                }
                if !(380_000..=388_000).contains(&v.employees) {
                    failures.push(format!("employees={}", v.employees));
                }
                if v.sources.len() < 2 {
                    failures.push(format!("sources.len()={}", v.sources.len()));
                }
                for source in &v.sources {
                    if !source.starts_with("http") {
                        failures.push(format!("sources に URL でない値: {source}"));
                    }
                }
            }
            failures
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    let endpoint = env_or("MINT_ENDPOINT", "http://100.78.108.50:11434/v1/");
    let model = env_or("MINT_MODEL", "ornith-1.5:35b");
    let searxng = env_or("SEARXNG_URL", "http://searxng.localhost");
    let crawl4ai = env_or("CRAWL4AI_URL", "http://crawl4ai.localhost");

    let selected: Vec<String> = std::env::args()
        .skip(1)
        .flat_map(|arg| {
            arg.split(',')
                .map(|s| s.trim().to_lowercase())
                .collect::<Vec<_>>()
        })
        .filter(|s| !s.is_empty())
        .collect();

    let target = ServiceTarget {
        endpoint: Endpoint::from_owned(endpoint.clone()),
        auth: AuthData::from_single("not-needed"),
        model: ModelIden::new(AdapterKind::OpenAI, model.clone()),
    };
    let client = match Client::builder().build() {
        Ok(client) => client,
        Err(error) => {
            eprintln!("client build failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let backend = Arc::new(GenaiBackend::new(client, target));

    let web = WebTools::builder()
        .searxng_url(searxng.clone())
        .crawl4ai_url(crawl4ai.clone())
        .build();
    if web.is_empty() {
        eprintln!("Web ツールが空です（SEARXNG_URL / CRAWL4AI_URL を確認）");
        return ExitCode::FAILURE;
    }

    println!("endpoint: {endpoint}");
    println!("model:    {model}");
    println!("search:   {searxng}");
    println!("fetch:    {crawl4ai}");
    println!("tools:    {}", web.tools().len());
    if let Ok(key) = std::env::var("TAVILY_API_KEY")
        && !key.trim().is_empty()
    {
        println!("note: TAVILY_API_KEY は WebTools に渡していません（SearXNG 固定）");
    }
    println!();

    let ctx = Ctx {
        backend,
        tools: web.tools(),
    };

    let run = |case: &str| selected.is_empty() || selected.iter().any(|s| s == case);

    let mut reports: Vec<CaseReport> = Vec::new();
    if run("a") {
        reports.push(case_a(&ctx).await);
    }
    if run("b") {
        reports.push(case_b(&ctx).await);
    }
    if run("c") {
        reports.push(case_c(&ctx).await);
    }
    if run("d") {
        reports.push(case_d(&ctx).await);
    }
    if run("e") {
        reports.push(case_e(&ctx).await);
    }
    if run("f") {
        reports.push(case_f(&ctx).await);
    }
    if run("g") {
        reports.push(case_g(&ctx).await);
    }
    if run("h") {
        reports.push(case_h(&ctx).await);
    }

    println!("\n================ SUMMARY ================");
    for report in &reports {
        println!("{}", report.line());
    }
    let passed = reports
        .iter()
        .filter(|r| r.verdict == Verdict::Pass)
        .count();
    let failed = reports
        .iter()
        .filter(|r| r.verdict == Verdict::Fail)
        .count();
    let incomplete = reports
        .iter()
        .filter(|r| r.verdict == Verdict::Incomplete)
        .count();
    println!(
        "pass={passed} fail={failed} incomplete={incomplete} total={}",
        reports.len()
    );

    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
