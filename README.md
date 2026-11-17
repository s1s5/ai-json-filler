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

## 指定できる JSON の形

充填対象は次の 2 モードのどちらか。モードはスキーマのルート型から自動判定する。

| モード | ルート | 要素・フィールド | 例 |
|---|---|---|---|
| Object（既定） | フラットな object | スカラー、またはスカラー配列 | `struct Company { name: String, tags: Vec<String> }` |
| Array（`is_array`） | object の配列 | 各要素はフラットな object | `Vec<Row>` |

次の形は構築時に `FillError::UnsupportedSchemaShape { path, reason }` で拒否する。

- ネストした object
- Object モードのフィールドとしての object 配列
- 配列の配列
- map 型（`additionalProperties` がスキーマ）
- 型未指定のフィールド

Object モードのスカラー配列は、保存のたびに**配列全体を置換**する。部分的な追記はできない。

## Array モード（`is_array`）

`Vec<Row>` のように object の配列を指定すると Array モードになる。`save_result` の代わりに、次の 3 ツールが公開される。

| ツール | 引数 | 役割 |
|---|---|---|
| `append` | `{"items": [{...}, ...]}` | 末尾に要素を追加。要素は部分的でよい |
| `update` | `{"index": 0, "patch": {...}}` | 指定要素に `patch` をキー単位でマージ |
| `remove` | `{"index": 0}` | 指定要素を削除。後ろの index は前に詰まる |

```rust
#[derive(Debug, Deserialize, JsonSchema)]
struct Row {
    /// 名前
    name: String,
    /// 備考
    note: String,
}

let outcome = fill::<Vec<Row>>(&client, "qwen3:8b", "行を調べて埋めてください")
    .run()
    .await?;
println!("{:#?}", outcome.value);
```

- 部分要素のまま `append` し、後から `update` で必須フィールドを埋められる。
- `remove` の後は index がずれる。`update` / `remove` は現在の index を指定する。
- 空配列がスキーマ上妥当なら、空で完了し得る。最低 1 件を要求する場合は `minItems: 1` をスキーマに入れるか、`with_goal_empty(GoalEmpty::Fail)` を使う。
- ツール名は `ArrayToolNames` で変更できる（`with_array_tool_names` / `array_tool_names`）。

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

## 取得結果のキャッシュ

`WebTools::builder()` が作るツール群は 1 つの `WebMemory` を共有する。`web_search` は検索結果を、`web_fetch` は取得した本文を**切り捨て前の全文**で保持する。

- 同じキー（検索は provider / query / page / 件数、取得は URL）の 2 回目以降はネットワークに出ず、保持済みの内容を返す。応答 JSON には `cached: true` が付く。
- `web_fetch` は全文を持つので、`offset` を進めた続きの取得も再取得なしで返る。
- 最新の情報が要るときだけ `refresh: true` を付ける。再取得して履歴に追記する（古いレコードは残る）。
- エラー応答は記録しない。

```rust
let web = WebTools::from_env();
// ... fill を実行 ...

let hits = web.memory().search_hits("決算"); // 全検索結果を横断
let latest = web.memory().latest_fetch("https://example.com/ir");
let snapshot = web.snapshot(); // searches / fetches のコピー

let removed = web.clear(); // 全件削除（削除件数）
```

外部公開 API: `memory()` / `searches()` / `fetches()` / `latest_search()` / `latest_fetch()` / `search_hits()` / `snapshot()`。

`clear()` が消すのは**ツール層のキャッシュだけ**で、LLM の会話履歴は消えない。以後の検索・取得は再びネットワークに出る。

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

## プロンプトに入るフィールド一覧

`system_prompt()` は末尾（`【言語】` の後）に、保存すべきフィールドの一覧を付ける。ツール定義（`parameters`）を読まないモデルでも、キー名・型・必須/任意が分かるようにするため。

```text
【保存する JSON の形】
save_result の引数は 1 つのオブジェクトです。キーに `.` や `[0]` を付けず、値はそのままの型で書いてください。

- employees: integer | null  (format: uint32, min: 0)  従業員数
- name: string  (必須)  会社名
- tags: array<string>  (必須)  タグ
```

Array モードでは見出しが `【要素の形】` になり、`append` の `items` / `update` の `patch` に入れる要素の形として同じ一覧を出す。要素の `$ref`（`#/$defs/Row`）はローカル解決してから列挙する。

- 一覧を消す: `FillSession::with_schema_guide(false)`（既定 `true`）。差し戻すと従来のプロンプトに戻る。
- 出力を絞る: `FillSession::with_guide_options(GuideOptions { .. })`。現在値は `guide_options()` で読める。

```rust
use ai_json_filler_core::GuideOptions;

let session = FillSession::<CompanyProfile>::new("調べて")?
    .with_guide_options(GuideOptions {
        max_description_chars: 80, // 0（既定）は無制限
        max_fields: Some(20),      // None（既定）は無制限。必須は削らない
    });
```

`max_description_chars` は既定で無制限。説明文は「何を入れるか」の唯一の手掛かりなので、切るかどうかは呼び出し側が決める。切ったときは `…` が付く。`max_fields` は**任意フィールドだけ**を制限し、超えた分は `…他 N 件（任意フィールド）` と表示する。必須フィールドは常に全部出る。

## 余計キーの扱い

`save_result` / `append` / `update` に、スキーマが宣言していないキーが混ざったときの既定は**拒否**。捨てて保存すると、データが欠けたまま「保存しました」と返ってしまうため。

- `ExtraKeys::Reject`（既定）: 候補をコミットせず `SessionResult::Continue` を返す。余計なキーを名指しし、正しいキー（末尾の `.` を外した名前 → 編集距離 2 以内の近いプロパティ）を提案する。
- `ExtraKeys::Strip`: 余計キーを捨て、他が妥当なら `Done`。捨てたキーは `FillOutcome::warnings` に `余計なキーを無視しました: confidence.` と件数で残る。

```rust
use ai_json_filler_core::{ExtraKeys, FillSession};

let session = FillSession::<CompanyProfile>::new("調べて")?
    .with_extra_keys(ExtraKeys::Strip); // 既定は ExtraKeys::Reject
```

`append` / `update` / `remove` の**引数**に含まれる余計キー（`{"items": [...], "comment": "x"}` の `comment`）は、ポリシーに関係なく拒否する。ツール定義が `additionalProperties: false` を宣言しているため。

## ゴール項目（`with_goal_fields`）

`required` はスキーマ検証の担当で、`Option<T>` / `#[serde(default)]` のフィールドは 1 つも入らなくても完了してしまう。ゴール項目はその上に「値が存在すること」だけを要求する。`required` でないフィールドも完了条件にできる。

```rust
use ai_json_filler_core::{FillSession, GoalEmpty};

let session = FillSession::<CompanyProfile>::new("調べて")?
    .with_goal_fields(["employees", "headquarters"])?; // 構築時に検証される
```

- 記法は `field`（Object モード）と `[].field`（Array モード）だけ。それ以外の記法、ルート種別との不一致（Object モードの `[].x`、Array モードの素の `x`）、スキーマに無い名前は構築時に `FillError::InvalidGoalPath` / `FillError::UnknownGoalField` で拒否する。永久に充足しないゴールを作らないため。
- 充足 = 値が存在し、`null` でなく、`""` でない。`" "` / `0` / `false` / `[]` / `{}` は充足。意味・範囲の正しさは判定しない（スキーマ検証の担当）。
- ゴール未達でも保存はコミットする（部分コミット）。`done` は立たず、不足一覧と「次に取るべき操作」が返る。Array モードで既存要素に不足がある場合は `update` を案内し、`append` は要素が増えるので使わないよう明示する。
- `[].field`（`GoalPath::EveryElement`）は**現在存在する要素**をすべて検査する。要素数の保証はしない（0 件なら何も検査しない）。最低 1 件を要求するには `with_goal_empty(GoalEmpty::Fail)` を使う。件数の上限・下限は扱わない。`GoalEmpty` の既定は `Pass`。
- 不足一覧は 5 行まで。超えたら `…他 N 件`。ツール無し応答への差し戻し（`progress_feedback()`）にも同じ一覧と案内を出す。
- 案内のツール名は設定値（`save_tool_name` / `array_tool_names`）から取る。
- ゴール未設定なら従来と完全に同じ挙動。埋められない項目をゴールにすると `max_turns` / `max_rounds` まで進んで `FillError::Incomplete` になるので、`give_up` と併用する。

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

- **形状制約**: ルートはフラット object またはフラット object の配列。ネスト object / object 配列 / map / 型未指定は `FillError::UnsupportedSchemaShape` で構築時に拒否する。`$ref` はローカル参照（`#/...`）のみ解決する。
- **Array モード**: 既定のツール名は `append` / `update` / `remove`。`ArrayToolNames` で変更できる。各ツールの引数スキーマには、`items` の `$ref` を解決できるよう `$defs` / `definitions` をコピーする。`save_calls` は3ツールの呼び出し回数も数える。
- **seed**: `FillSession::with_seed` は既存の部分データを受け取る。`required` 不足だけを許容し、型違い・enum 違反・余計キーは `FillError::InvalidSeed` で拒否する。
- **余計キー**: 既定は `ExtraKeys::Reject`（保存しない）。`FillSession::with_extra_keys(ExtraKeys::Strip)` にすると従来どおり除去して保存し、`FillOutcome::warnings` に `余計なキーを無視しました: confidence.` と件数を残す。配列要素内の余計キーは `[0].extra` のようにパス付きで記録する。`append` / `update` / `remove` の引数側の余計キーは、ポリシーに関係なく拒否する。
- **`additionalProperties`**: schemars 1 は素の struct に `additionalProperties: false` を付けないため、`FillSchema::for_type` 側で `properties` を持つオブジェクトに注入する。`anyOf` / `oneOf` は再帰し、`allOf` は `unevaluatedProperties: false` を使う。配列ルートでは `items` の object にも適用する。
- **フィードバック**: 日本語固定。`FeedbackOptions` で件数・文字数上限を変更できる。
- **バージョン**: `genai = "0.7"`（キャレット）。マイナー更新で API が変わった場合は `ai-json-filler-genai` だけを直す。
- **未実装**: 英語フィードバック、トークン予算、自動リトライ、ストリーミング、rounds / usage の返却、配列の一括 update / remove、キー付きマージ。

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
