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

会話は core が作る開始メッセージで始まる（genai のループは `FillSession::start_message()` を 1 通目に置く）。Array モードでは `save_result` ではなく配列ツールを名指しする。

```
調査を開始し、append / update / remove のいずれかで結果を保存してください。調べても分からない場合は give_up を呼んでください
```

開始メッセージは `save_result` を指さない。過去の履歴や癖でモデルが `save_result` を呼んだ場合は、`save_result` が Object モード専用であることと配列ツールの使い方を 1 通で返す（`TOOL_ERROR`、`is_error: true`）。ツール名を変えていても案内は設定値から作る。なお、Array モードで `save_result` と同名の調査ツールを登録した場合は、そのツールが優先される。

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

## ツール予算（`with_tool_budget`）

`web_search` などの調査ツールは、放っておくと調査が終わらずタイムアウトしやすい。ツール名ごとの実行回数に上限を置ける。

```rust
use ai_json_filler_core::FillSession;

let session = FillSession::<CompanyProfile>::new("調べて")?
    .with_tool_budget("web_search", 5);
```

- 上限に達した呼び出しは**実行しない**（トークンも時間も使わない）。`SessionResult::Continue { is_error: false }` で「上限（5 回）に達しました。… `save_result` を呼び出して保存してください」と案内する。案内の保存ツール名は設定値から取る（Array モードでは `append` / `update` / `remove`）。
- ブロックした呼び出しは `tool_calls` にも数えない（`give_up` の足切りは進まない）。実行回数の内訳は `FillOutcome::tool_calls_by_name` で読める。これは調査ツールだけを数え、保存系ツールは含まない。
- `max == 0` は「上限なし」（設定しないのと同じ）。誤設定でセッションが詰まない側に倒している。空のツール名は無視する。
- `save_result` / `append` / `update` / `remove` / `give_up` は予算の対象外。保存と報告は止めない。
- `Tool::prompt_hint()` の「合計5回まで」は助言にすぎない。実際の上限は `with_tool_budget` で決める。両方を同じ値にしておくのが望ましい。
- ツール名は登録名と完全一致で照合する。未登録の名前への設定は保持され、登録後に効く。

`with_tool_budget` は core の API。genai ビルダーでは `.tool_budget("web_search", 5)` として同じ設定ができる（`with_give_up_after_tool_calls` は genai に公開していない）。

## 保存の強制（`force_save_after`）

調査が長引いて保存されずに終わるのを防ぐには、ラウンド数を閾値にして調査ツールを外す。

```rust
let outcome = fill::<CompanyProfile>(&client, "qwen3:8b", "調べて")
    .force_save_after(2)
    .run()
    .await?;
```

- 閾値以降のラウンドでは `request.tools` を保存系だけに差し替える。Object モードは `save_result`、Array モードは `append` / `update` / `remove`。どちらも `give_up` は残す。
- 閾値に達した最初のラウンドで、「これ以上は調査できません。…」という user メッセージを 1 回だけ差し込む（文面は `FillSession::save_only_message()`）。ターンが進んでラウンド数が 0 に戻っても再挿入しない。
- 閾値到達はラッチする。ツール無し応答でターンが進みラウンド数が 0 に戻っても、調査ツールを再公開したり allowlist を解除したりしない。
- `force_save_after(0)` は最初のリクエストから強制する。未設定（既定）なら従来どおり全ラウンドで同じツール一覧。
- 強制中は `FillSession::set_tool_allowlist` により、一覧から外したツールの**実行も拒否**する（モデルが名前を覚えていて呼んできた場合に備える）。
- 早すぎる閾値は情報不足のまま保存させる。`force_save_after(0)` では調査ツールが最初から使えないため、モデルは自分の知識で埋めることになる。
- core から使う場合は `FillSession::save_only_tool_specs()` を `tool_specs()` の代わりに渡し、併せて `set_tool_allowlist(Some(names))` を設定する。

## core を他のフレームワークに組み込む

ホストは次の 4 つだけを使う。

1. `session.system_prompt()` を system プロンプトにする
2. `session.tool_specs()` を LLM のツール定義に変換する
3. 返ってきた tool call を `session.call(name, args).await` に渡す
4. `SessionResult::Done { .. }` / `SessionResult::Impossible { .. }` になったら `session.into_outcome()` を呼ぶ（`Ok` か `Err(FillError::Impossible)`）

LLM 無しで 4 ステップを実演する例が `crates/ai-json-filler-core/examples/manual_loop.rs`。`&self` でツールを呼ぶフレームワーク向けに、`Arc<tokio::sync::Mutex<Option<FillSession<T>>>>` で包んで `Done` で `take()` する例が `crates/ai-json-filler-core/examples/session_tool.rs` にある。

## プロンプト表面マップ

どの文字列がモデルに届き、どのノブで制御できるか。順序は固定で、並べ替えは提供しない（保存手順が言語ルールの後ろに来ると壊れるため）。

表の順序がそのまま system プロンプトの順序（`PromptSection::SYSTEM`）。

| 部分 | 届く先 | 既定のノブ | 差し替え / 無効化 |
|---|---|---|---|
| タスク文 | system の先頭 | `FillSession::new(prompt)` | `prompt()` で読める。差し替えは不可 |
| 【保存の手順】 | system | ツール名から自動 | `PromptSection::SaveInstructions` |
| ツールヒント | system | `Tool::prompt_hint()` | `PromptSection::ToolHints` |
| 【言語】 | system | 日本語固定 | `PromptSection::LanguageRules` |
| 【保存する JSON の形】/【要素の形】 | system の末尾（【言語】の後） | `with_schema_guide` / `with_guide_mode` / `with_guide_options` | `PromptSection::Guide`（テンプレートに `{fields}` 必須） |
| 開始メッセージ | 最初の user | モードのツール名から自動 | `PromptSection::StartMessage` |
| 強制保存メッセージ | `force_save_after` 到達時の user | モードのツール名から自動 | `PromptSection::SaveOnlyMessage` |

### セクションの差し替え

```rust
use ai_json_filler_core::{FillSession, GuideMode, PromptSection};

let session = FillSession::<CompanyProfile>::new("調べて")?
    // ツール名はプレースホルダで受ける。モードは書かなくてよい。
    .with_prompt_section(
        PromptSection::StartMessage,
        "調査を始め、{save} で保存してください。無理なら {give_up} を呼んでください",
    )?
    // 一覧は残し、ツール名の一文だけ落とす
    .with_guide_mode(GuideMode::Fields)
    // 言語ルールを消す
    .without_prompt_section(PromptSection::LanguageRules);
```

- プレースホルダは `{save}`（Object モード）、`{append}` `{update}` `{remove}`（Array モード）、`{give_up}`、`{fields}`（`Guide` 専用）。モードに合わないもの・未知のもの・`{fields}` の無い `Guide` は `with_prompt_section` の時点で `FillError` になる。壊れた文面が黙ってモデルに届くことはない。
- プレースホルダと見なすのは**識別子形**（`{` と `}` の間を trim して、空でなく英小文字と `_` だけ）の波括弧だけ。`{"name": "x"}` のような JSON の例や `{1}`・`{}`・`{Save}` はそのままの文字として出力される。`{ save }` は trim されて `save` として置換される。`{save}` という文字列そのものを出したいときのエスケープは無い。
- 置換は 1 パスで、置換した値が再度展開されることはない。
- `without_prompt_section` は既定文面を消す。`with_prompt_section` で差し替えたセクションも消える。`reset_prompt_section` で既定に戻る。
- `StartMessage` を無効化すると **user メッセージを 1 つも送らない**（system だけで始まる）。user ターンを必須にするプロバイダでは無効化しないこと。
- 既定の文面をそのまま残したいセクションは触らない。差し替えていないセクションは従来どおりに描画される。
- genai ビルダーでは `.prompt_section(...)` / `.without_prompt_section(...)` / `.guide_mode(...)`。検証は `run()` がセッションを組み立てるときに行われる（モードが型パラメータで決まるため）。

### 現在の状態を読む

設定は write-only にしない。差し替えた文面・無効化したセクション・実際に届く文面はすべて読み返せる。

```rust
use ai_json_filler_core::{PromptSection, PromptSectionSource};

let view = session.prompt_section(PromptSection::StartMessage);
assert_eq!(view.source, PromptSectionSource::Custom); // Default / Custom / Disabled
assert_eq!(view.template, Some("{save} で保存してください")); // 生テンプレート（未置換）
assert_eq!(view.rendered, "save_result で保存してください"); // 実際に届く文面

assert!(session.is_prompt_section_disabled(PromptSection::LanguageRules));
for (section, view) in session.prompt_sections() {
    println!("{section:?}: {:?} / {} bytes", view.source, view.rendered.len());
}
```

- `rendered` は毎回、現在のツール名・ガイド設定から組み立てる。ツール名を後から変えても追随する。
- `PromptSection::SYSTEM` の 4 セクションの `rendered` を連結すると `system_prompt()` と一致する（表示と実送信がズレない）。`StartMessage` / `SaveOnlyMessage` は `start_message()` / `save_only_message()` と一致する。
- `template` は生の文字列。置換後が欲しいときは `rendered` を使う。

### プロンプト監査（`prompt_audit`）

壊してはいけない契約（保存ツールの存在・`give_up`・強制保存メッセージ）を検査する。**エラーにはしない**。ツール名の変更は正当なので、見知らぬ名前は警告に留める。

```rust
let audit = session.prompt_audit();
assert!(audit.warnings.is_empty(), "{:?}", audit.warnings); // 既定構成は無警告

// 移行メッセージなど、意図して出している名前は除外できる
let audit = session.prompt_audit().allow("save_result");
```

- 警告の例: 配列モードなのに `save_result` がプロンプトに残っている（今回のバグと同根。保存名を変えた場合も既定名の残骸を検査する）、公開している保存ツール名が文面に無い、`give_up` がどこにも無い、`save_tool_name` を配列モードで設定している。
- `without_prompt_section` で無効化したセクションは検査しない（送らないものを警告しても意味がない）。
- 同名の調査ツールが登録されている `save_result` は正当なので警告しない。
- genai の `run()` は監査を自動で回し、結果を `FillOutcome::warnings`（`Ok` 経路）に合流させる。意図して出している名前は `.prompt_audit_allow("name")`（core は `with_prompt_audit_allowed([...])`）で除外できる。
- **`FillError::Impossible` / `Incomplete` では警告が付かない**（`FillOutcome` 自体が返らないため）。CI で退行を検出したいときは `prompt_audit()` を直接呼ぶ。

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
- 一覧は残してツール名の一文だけ消す: `FillSession::with_guide_mode(GuideMode::Fields)`。genai は `.guide_mode(...)`。既定は `GuideMode::FieldsWithToolNames`。
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

## プロンプト版（`prompt_version`）

どのプロンプト構成で生成した結果かを `FillOutcome::prompt_version` に残す。DB の列にそのまま入れられる。

- 既定値は `ai_json_filler_core::PROMPT_VERSION`（現在 `"save-contract-v4"`）。呼び出し側の版はサフィックスとして連結される。
- core は `FillSession::with_prompt_version("stock-3")`、genai は `.prompt_version("stock-3")` で指定する。空文字・空白のみは無視され、既定のままになる。
- 結果は `"save-contract-v4+stock-3"` の形。`FillSession::prompt_version()` でセッション側からも読める。

```rust
let outcome = fill::<CompanyProfile>(&client, "qwen3:8b", "調べて")
    .prompt_version("stock-3")
    .run()
    .await?;
// outcome.prompt_version == "save-contract-v4+stock-3"
```

**版を上げる規律**: `SAVE_DESCRIPTION`・`LANGUAGE_RULES`・ガイドの見出し（`guide_body`）・`goal_feedback` の文面・`budget_message`・`start_message`・`save_only_message` を変えたら `PROMPT_VERSION` を上げる。テンプレートがスキーマ・ツール名・タスク文で動的なためハッシュでは検出できない。`v4` は既定の文面を変えていない。開始メッセージ・強制保存メッセージが genai 側にあった時代は版に反映されておらず、core の所有へ移したうえで版の管理対象に入れたことを示すための版上げ（出力の変更ではない）。上げ忘れは PR のチェック項目で防ぐ。

差し替え API（`with_prompt_section` / `without_prompt_section`）と `GuideMode` の追加は**既定の出力を変えない**ので版を上げない。既定文面そのものを変えたときだけ上げる。

## 設計上の注意

- **保存ツール名**: 既定は `save_result`（`ai_json_filler_core::DEFAULT_SAVE_TOOL_NAME`）。core は `FillSession::with_save_tool_name("store_fields")`、genai は `.save_tool_name("store_fields")` で変更できる。空文字・空白のみの名前は無視され、既定のままになる。Array モードではこの名前は使わない（ツール一覧・開始メッセージ・強制保存メッセージは `ArrayToolNames` から作る）。give-up 名・配列ツール名と衝突した場合は無視され、**その旨が `warnings()` / `FillOutcome::warnings` に残る**（無言で落とさない）。衝突は「後から適用した側が負ける」ので、genai ビルダーは Array モードで配列名を先に適用する（配列名が勝つ）。設定ミスを警告ではなくエラーにしたい場合は `try_with_save_tool_name` / `try_with_array_tool_names` / `try_with_give_up_tool_name`（genai は `try_save_tool_name` など）を使う。genai の `try_*` は**その名前だけ**を厳格にする（`.try_give_up_tool_name(...)` を足しても、保存名・配列名は警告のまま）。

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
