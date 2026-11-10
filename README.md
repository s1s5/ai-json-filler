# ai-json-filler

`JsonSchema` を derive した構造体を指定すると、LLM に **不足フィールドだけを部分更新で埋めさせる** Rust ライブラリ。

一括で全フィールドを生成させるのではなく、

1. 出力スキーマの `required` を外した「部分更新スキーマ」を `save_result` ツールとして提示する
2. LLM が分かった項目だけを保存する
3. 必須が揃うまで検証エラーを日本語で差し戻す
4. 揃ったら `serde` で構造体へデシリアライズする

というループで動く。小さめのモデルでも、調査と保存を交互に進められる。

## クレート構成

```
ai-json-filler-core   <-  ai-json-filler-genai
ai-json-filler-core   <-  ai-json-filler-tools
```

| クレート | 役割 |
|---|---|
| `ai-json-filler-core` | `FillSchema`（部分スキーマ・検証）、`FillSession`（`save_result` の状態機械）、`Tool` / `ToolSet` / `typed_tool`、日本語フィードバック。LLM も HTTP も知らない |
| `ai-json-filler-genai` | `genai` の `ChatRequest` / `ChatResponse` を使った実行ループ。`fill::<T>()` を提供 |
| `ai-json-filler-tools` | Tavily / SearXNG 検索、Crawl4AI / Tavily Extract 取得を `Tool` として実装 |

core は特定のエージェントフレームワークに依存しない。genai 以外（rig、rmcp、自前ループ）からも `FillSession` の 4 ステップで使える。

## 最小例（genai）

```rust
use ai_json_filler_genai::fill;
use genai::Client;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
struct CompanyProfile {
    /// 会社名
    name: String,
    /// 従業員数
    employees: u32,
    /// 本社所在地
    headquarters: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::builder().build()?;
    let outcome = fill::<CompanyProfile>(
        &client,
        "qwen3:8b",
        "トヨタ自動車の会社情報を調べてください",
    )
    .max_rounds(10)
    .run()
    .await?;

    println!("{:#?}", outcome.value);
    Ok(())
}
```

カスタムエンドポイント（Ollama の LAN サーバ等）を使う場合は `fill_target` に `genai::ServiceTarget` を渡す。`crates/ai-json-filler-genai/examples/fill_struct.rs` を参照。

## ツールを使う

```rust
use ai_json_filler_genai::fill;
use ai_json_filler_tools::WebTools;

let web = WebTools::from_env(); // TAVILY_API_KEY / SEARXNG_URL / CRAWL4AI_URL
if web.is_empty() {
    eprintln!("Web ツールが未設定です。TAVILY_API_KEY などを設定してください");
}

let mut builder = fill::<CompanyProfile>(&client, "qwen3:8b", "調べてください");
for tool in web.tools() {
    builder = builder.tool(tool);
}
let outcome = builder.run().await?;
```

既定では 1 機能 1 ツール（`web_search` / `web_fetch`）だけを登録する。プロバイダ別名（`web_search_tavily` 等）も欲しい場合だけ `WebTools::builder().provider_aliases(true)` を使う。

`tool()` は `ai-json-filler-core::Tool`（定義＋実行ハンドラ）を受け取る。`genai::chat::Tool` は定義だけを持ち、実行ハンドラを持たない。既に genai のツール定義とハンドラがある場合は `.genai_tool(tool, handler)` で登録できる。

## ターンとラウンド

`GenaiFillBuilder` は 2 つの上限を持つ。

- **`max_turns`**（既定 3）: ツール呼び出しの無い assistant 応答の上限。1 回で 1 ターン進む。ターンが進むと `max_rounds` は 0 に戻る。ターン境界では必ず「JSON はまだ埋まっていない」ことと現在の検証エラーを user プロンプトで伝える。
- **`max_rounds`**（既定 10）: 1 ターン内のツール呼び出し応答の上限。

```rust
let outcome = fill::<CompanyProfile>(&client, "qwen3:8b", "調べて")
    .max_turns(3)
    .max_rounds(10)
    .run()
    .await?;
```

両方とも 1 未満は 1 に丸められる。

## core を他のフレームワークに組み込む

ホストは次の 4 つだけを使う。

1. `session.system_prompt()` を system プロンプトにする
2. `session.tool_specs()` を LLM のツール定義に変換する
3. 返ってきた tool call を `session.call(name, args).await` に渡す
4. `SessionResult::Done { .. }` / `SessionResult::Impossible { .. }` になったら `session.into_outcome()` を呼ぶ（`Ok` か `Err(FillError::Impossible)`）

LLM 無しで 4 ステップを実演する例が `crates/ai-json-filler-core/examples/manual_loop.rs`。`&self` でツールを呼ぶフレームワーク向けに、`Arc<tokio::sync::Mutex<Option<FillSession<T>>>>` で包んで `Done` で `take()` する例が `crates/ai-json-filler-core/examples/session_tool.rs` にある。

## 不可能の報告

LLM が「これ以上調べても値が分からない」と判断したときは、`give_up` ツールで理由付きの打ち切りを宣言できる。ループ上限（`max_turns` / `max_rounds`）による打ち切りと区別できるのが目的。

- 既定名は `give_up`（`ai_json_filler_core::DEFAULT_GIVE_UP_TOOL_NAME`）。core は `FillSession::with_give_up_tool_name("cannot_complete")`、genai は `.give_up_tool_name("cannot_complete")` で変更できる。
- `reason` は必須。空・空白のみ・非オブジェクト引数は `SessionResult::Continue` で差し戻され、セッションは終端しない。`missing_fields` は任意（文字列配列）。
- 受理されると `SessionResult::Impossible { reason, missing_fields }` が返り、以降の呼び出しも同じ結果になる。`is_done()` / `is_aborted()` は `true` になる。
- `run()` は `Err(FillError::Impossible { reason, missing_fields, partial })` を返す。`partial` には打ち切り時点までの累積が入る。上限到達は従来どおり `FillError::Incomplete { partial }`。
- 登録済みの調査ツールがある場合、未設定でも「調査ツール 1 回以上」を要求する。`with_give_up_after_tool_calls(n)` で回数を変更でき、`with_give_up_after_tool_calls(0)` で無効化できる。ツール 0 本なら即座に受理される。genai ビルダーには公開していない。

```rust
use ai_json_filler_core::FillError;

match fill::<CompanyProfile>(&client, "qwen3:8b", "調べて").run().await {
    Ok(outcome) => println!("{:?}", outcome.value),
    Err(FillError::Impossible {
        reason,
        missing_fields,
        ..
    }) => {
        eprintln!("不可能: {reason}（未取得: {}）", missing_fields.join(", "));
    }
    Err(e) => return Err(e.into()),
}
```

## 設計上の注意

- **保存ツール名**: 既定は `save_result`（`ai_json_filler_core::DEFAULT_SAVE_TOOL_NAME`）。core は `FillSession::with_save_tool_name("store_fields")`、genai は `.save_tool_name("store_fields")` で変更できる。空文字・空白のみの名前は無視され、既定のままになる。

- **seed**: `FillSession::with_seed` は既存の部分データを受け取る。`required` 不足だけを許容し、型違い・enum 違反・余計キーは `FillError::InvalidSeed` で拒否する。
- **余計キー**: `save_result` が送ってきた余計キーは検証前に除去し、`FillOutcome::warnings` に残す。
- **`additionalProperties`**: schemars 1 は素の struct に `additionalProperties: false` を付けないため、`FillSchema::for_type` 側で `properties` を持つオブジェクトに注入する。`anyOf` / `oneOf` は再帰し、`allOf` は `unevaluatedProperties: false` を使う。
- **フィードバック**: 日本語固定。`FeedbackOptions` で件数・文字数上限を変更できる。
- **バージョン**: `genai = "0.7"`（キャレット）。マイナー更新で API が変わった場合は `ai-json-filler-genai` だけを直す。
- **未実装**: 英語フィードバック、トークン予算、自動リトライ、ストリーミング、rounds / usage の返却。

## 検証

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo check --workspace --examples
```

core が LLM / HTTP に依存していないこと:

```bash
cargo tree -p ai-json-filler-core -e normal | grep -E 'genai|reqwest|tokio'
```

何も出なければ OK。
