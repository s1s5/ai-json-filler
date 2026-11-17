//! 検証エラーを LLM 向けの日本語フィードバックへ変換する
//!
//! 目的:
//! - 1 回の `save_result` 呼び出しで、不足・型違い・余計キーをまとめて伝える
//! - 小型モデルでも次の行動が決まる日本語にする
//! - 余計キーを累積から除去して「一度送ると二度と消せない」デッドロックを断つ
//!
//! [`format_validation_feedback`] / [`strip_extra_keys`] は純関数。
//! 呼び出し側（`runner`）は累積の clone を候補として渡し、成功したときだけ
//! 累積へコミットする。`ValidationError` は候補を借用するため、
//! 除去は「所有型へ抜き出す → 借用を切る → 書き換える」の順で行う。

mod describe;
mod items;
#[cfg(test)]
mod tests;
mod text;

use jsonschema::error::ValidationError;
use jsonschema::paths::Location;
use serde_json::Value;

use crate::ArrayToolNames;
use crate::FillSchema;
use crate::RootKind;
use crate::goal::{GoalMiss, GoalReport};
use items::{
    Section, apply_budget, assemble, collect_extra, collect_missing, collect_value_errors,
};
use text::saved_line;

#[cfg(test)]
pub(crate) use crate::schema::{collect_extra_keys, strip_extra_keys};

/// ゴール不足の一覧に出す行数の上限
const MAX_GOAL_LINES: usize = 5;

/// 保存を促す句（`save_result を呼び出して` / `append / update / remove のいずれかで`）
///
/// ツール無し応答への差し戻しと、ツール予算に達したときの案内で共有する。
/// ツール名は設定値から取るので、カスタム名でもそのまま通る。
pub(crate) fn save_action(
    root_kind: RootKind,
    save_tool_name: &str,
    names: &ArrayToolNames,
) -> String {
    match root_kind {
        RootKind::Object => format!("{save_tool_name} を呼び出して"),
        RootKind::Array => format!(
            "{} / {} / {} のいずれかで",
            names.append, names.update, names.remove
        ),
    }
}

/// ツール予算に達したときの案内
///
/// ブロックした呼び出しへの応答なので、`is_error: false` で返す前提の文面。
/// 何回で止まったかと、次に取るべき保存操作を 1 文で示す。
pub(crate) fn budget_message(
    tool: &str,
    max: usize,
    root_kind: RootKind,
    save_tool_name: &str,
    names: &ArrayToolNames,
) -> String {
    format!(
        "{tool} は上限（{max} 回）に達しました。これ以上呼ばず、分かった範囲で {}保存してください。",
        save_action(root_kind, save_tool_name, names)
    )
}

/// ゴール未達のフィードバック
///
/// 不足一覧に加えて「次に取るべき操作」を返す。Array モードでは `append` の
/// やり直しを促さず、コミット済みの要素を `update` で直すよう案内する。
pub(crate) fn goal_feedback(
    report: &GoalReport,
    save_tool_name: &str,
    names: &ArrayToolNames,
) -> String {
    format!(
        "JSONはまだ完成していません。{}",
        goal_section(report, save_tool_name, names)
    )
}

/// 不足一覧と操作案内（`progress_feedback` からも使う）
pub(crate) fn goal_section(
    report: &GoalReport,
    save_tool_name: &str,
    names: &ArrayToolNames,
) -> String {
    if report.empty {
        return format!(
            "要素が 1 件もありません。\n{} で1件以上を追加してください。",
            names.append
        );
    }

    let mut out = String::from("次の項目が空です:");
    for miss in report.misses.iter().take(MAX_GOAL_LINES) {
        out.push_str("\n- ");
        out.push_str(&miss.line());
    }
    let truncated = report.misses.len() > MAX_GOAL_LINES;
    if truncated {
        out.push_str(&format!(
            "\n…他 {} 件",
            report.misses.len() - MAX_GOAL_LINES
        ));
    }
    out.push('\n');
    out.push_str(&next_action(report, save_tool_name, names, truncated));
    out
}

/// 不足の種類に応じた「次に取るべき操作」
fn next_action(
    report: &GoalReport,
    save_tool_name: &str,
    names: &ArrayToolNames,
    truncated: bool,
) -> String {
    let mut fields: Vec<&str> = Vec::new();
    let mut indices: Vec<usize> = Vec::new();
    let mut element = false;
    for miss in &report.misses {
        match miss {
            GoalMiss::Field(field) => push_once(&mut fields, field),
            GoalMiss::Element { index, field } => {
                element = true;
                push_once(&mut fields, field);
                indices.push(*index);
            }
        }
    }
    indices.sort_unstable();
    indices.dedup();

    if !element {
        if truncated || fields.len() > 3 {
            return format!("{save_tool_name} に不足している項目を入れて再送してください。");
        }
        return format!(
            "{save_tool_name} に {} を入れて再送してください。",
            fields.join(" と ")
        );
    }

    if truncated || indices.len() > 3 || fields.len() > 3 {
        return format!(
            "{} で不足している要素の patch に値を入れてください。\n{} すると要素が増えるので使わないでください。",
            names.update, names.append
        );
    }
    let index_list: Vec<String> = indices.iter().map(usize::to_string).collect();
    format!(
        "{} で index {} の patch に {} を入れてください。\n{} すると要素が増えるので使わないでください。",
        names.update,
        index_list.join(" と "),
        fields.join(" と "),
        names.append
    )
}

fn push_once<'a>(list: &mut Vec<&'a str>, value: &'a str) {
    if !list.contains(&value) {
        list.push(value);
    }
}

/// 引数が空の保存ツールに対する案内
pub(crate) fn empty_args_feedback(save_tool_name: &str) -> String {
    format!(
        "VALIDATION_ERROR: {save_tool_name} の引数が空でした。埋まっているフィールドを1つ以上含めて再送してください。"
    )
}

/// 余計キーの表示パス（`extra` / `[0].extra`）
pub(crate) fn extra_key_path(loc: &Location, key: &str) -> String {
    let path = text::display_path(loc);
    if path == "(root)" {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

/// 余計キーを拒否したときのフィードバック
///
/// 候補はコミットしていないので「保存しませんでした」と明示し、正しいキー名を
/// 提案する。近い名前が無いキーは名指しだけに留める（推測を書かない）。
pub(crate) fn rejected_extra_keys_feedback(
    removed: &[(Location, String)],
    schema: &FillSchema,
    saved: &Value,
) -> String {
    let mut out = String::from("保存しませんでした。スキーマに無いキーがあります。");
    let mut allowed: Vec<String> = Vec::new();
    let mut notation_hint = false;
    for (loc, key) in removed {
        let path = extra_key_path(loc, key);
        match schema.suggest_property(loc, key) {
            Some(suggestion) => {
                // 末尾の `.` を外すだけで直る場合は、その直し方を名指しする。
                let trailing_dot = key.trim_end_matches('.') == suggestion && key != &suggestion;
                if trailing_dot {
                    notation_hint = true;
                    out.push_str(&format!(
                        "\n- `{path}` → 正しいキーは `{suggestion}` です（末尾の `.` を外す）"
                    ));
                } else {
                    out.push_str(&format!(
                        "\n- `{path}` → `{suggestion}` の間違いではありませんか"
                    ));
                }
            }
            None => {
                out.push_str(&format!("\n- `{path}` は使えません"));
                if key.contains('.') || key.contains('[') {
                    notation_hint = true;
                }
                // 同じ位置で使えるキーを集める（モデルがゼロから探さなくて済む）。
                if let Some(props) = schema.properties_at(loc) {
                    for name in props.keys() {
                        if !allowed.contains(name) {
                            allowed.push(name.clone());
                        }
                    }
                }
            }
        }
    }
    if !allowed.is_empty() {
        allowed.sort();
        out.push_str(&format!("\n\n使えるキー: {}", allowed.join(", ")));
    }
    if notation_hint {
        out.push_str("\n\nキーに `.` や `[0]` を付けず、そのままの名前で送り直してください。");
    } else {
        out.push_str("\n\nスキーマにあるキーだけで送り直してください。");
    }
    out.push_str("\nこの呼び出しは保存していません。");
    out.push('\n');
    out.push_str(&saved_line(saved));
    out
}

/// フィードバックの上限
#[derive(Debug, Clone, Copy)]
pub struct FeedbackOptions {
    /// 表示するエラー件数の上限
    pub max_errors: usize,
    /// 出力全体の文字数上限
    pub max_chars: usize,
    /// 値をプレビューする文字数上限
    pub max_value_chars: usize,
}

impl Default for FeedbackOptions {
    fn default() -> Self {
        Self {
            max_errors: 12,
            max_chars: 1600,
            max_value_chars: 80,
        }
    }
}

/// 検証エラー群を LLM 向けの日本語フィードバックに変換する
///
/// * `errors` — 余計キー除去**後**の検証エラー
/// * `accumulated` — コミット予定の累積（`保存済み:` の表示に使う）
/// * `removed` — [`strip_extra_keys`] が実際に除去した `(位置, キー)`
pub(crate) fn format_validation_feedback(
    errors: &[ValidationError<'_>],
    accumulated: &Value,
    removed: &[(Location, String)],
    opts: &FeedbackOptions,
) -> String {
    let total = errors.len() + removed.len();

    let missing = collect_missing(errors);
    let values = collect_value_errors(errors, opts);
    let extras = collect_extra(removed);

    // 件数の予算は「不足 → 余計 → 値」の順に割り当てる。
    // 値エラーは情報量が多く、余計キーは再送を止めるために必要なので優先する。
    let mut budget = opts.max_errors;
    let mut truncated = 0usize;
    let missing = apply_budget(missing, &mut budget, &mut truncated);
    let extras = apply_budget(extras, &mut budget, &mut truncated);
    let values = apply_budget(values, &mut budget, &mut truncated);

    let mut lines: Vec<(Section, String, usize)> = Vec::new();
    for item in missing.into_iter().chain(values).chain(extras) {
        lines.push((item.section, item.render(), item.count()));
    }

    let saved = saved_line(accumulated);

    // 文字数上限: 超えたら末尾の行から落とし、落とした件数を `…他 N 件` に足す。
    loop {
        let text = assemble(total, &lines, &saved, truncated);
        if text.chars().count() <= opts.max_chars || lines.is_empty() {
            return text;
        }
        if let Some((_, _, count)) = lines.pop() {
            truncated += count;
        }
    }
}
