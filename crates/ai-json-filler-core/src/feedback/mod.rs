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

use items::{
    Section, apply_budget, assemble, collect_extra, collect_missing, collect_value_errors,
};
use text::saved_line;

#[cfg(test)]
pub(crate) use crate::schema::{collect_extra_keys, strip_extra_keys};

/// 引数が空の保存ツールに対する案内
pub(crate) fn empty_args_feedback(save_tool_name: &str) -> String {
    format!(
        "VALIDATION_ERROR: {save_tool_name} の引数が空でした。埋まっているフィールドを1つ以上含めて再送してください。"
    )
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
