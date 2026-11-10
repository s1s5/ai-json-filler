use jsonschema::error::{TypeKind, ValidationError, ValidationErrorKind};
use serde_json::Value;

use super::FeedbackOptions;
use super::text::{
    actual, array_len, object_len, property_label, scalar, str_len, truncate_chars, type_label,
    value_preview,
};

/// `anyOf` / `oneOf` で要約する分岐の上限
const MAX_BRANCHES: usize = 3;
/// `enum` で列挙する選択肢の上限
const MAX_ENUM_OPTIONS: usize = 5;
/// `pattern` を表示する最大文字数
const MAX_PATTERN_CHARS: usize = 60;

/// 1 件のエラーを日本語の説明文にする（パスは含めない）
pub(super) fn describe_error(error: &ValidationError<'_>, opts: &FeedbackOptions) -> String {
    let instance = error.instance();
    let preview = value_preview(instance, opts.max_value_chars);

    match error.kind() {
        ValidationErrorKind::Required { property } => {
            format!("必須フィールド {} がありません", property_label(property))
        }
        ValidationErrorKind::Type { kind } => {
            let expected = match kind {
                TypeKind::Single(t) => type_label(*t).to_string(),
                TypeKind::Multiple(set) => {
                    let names: Vec<&str> = set.iter().map(type_label).collect();
                    format!("{} のいずれか", names.join(" または "))
                }
            };
            format!("{expected}が必要です（実際: {}）", actual(instance, opts))
        }
        ValidationErrorKind::Enum { options } => {
            let (shown, rest) = enum_options(options, opts);
            let mut out = format!("次のいずれかにしてください: {shown}");
            if rest > 0 {
                out.push_str(&format!(", …他 {rest} 件"));
            }
            out.push_str(&format!("（実際: {preview}）"));
            out
        }
        ValidationErrorKind::Constant { expected_value } => format!(
            "{} 固定です（実際: {}）",
            value_preview(expected_value, opts.max_value_chars),
            preview
        ),
        ValidationErrorKind::Minimum { limit } => {
            format!("{} 以上が必要です（実際: {preview}）", scalar(limit))
        }
        ValidationErrorKind::Maximum { limit } => {
            format!("{} 以下が必要です（実際: {preview}）", scalar(limit))
        }
        ValidationErrorKind::ExclusiveMinimum { limit } => {
            format!(
                "{} より大きい値が必要です（実際: {preview}）",
                scalar(limit)
            )
        }
        ValidationErrorKind::ExclusiveMaximum { limit } => {
            format!(
                "{} より小さい値が必要です（実際: {preview}）",
                scalar(limit)
            )
        }
        ValidationErrorKind::MultipleOf { multiple_of } => {
            format!("{multiple_of} の倍数が必要です（実際: {preview}）")
        }
        ValidationErrorKind::MinLength { limit } => format!(
            "{limit}文字以上が必要です（実際: {}文字）",
            str_len(instance)
        ),
        ValidationErrorKind::MaxLength { limit } => format!(
            "{limit}文字以下にしてください（実際: {}文字）",
            str_len(instance)
        ),
        ValidationErrorKind::Pattern { pattern } => format!(
            "パターン {} に一致しません（実際: {preview}）",
            truncate_chars(pattern, MAX_PATTERN_CHARS)
        ),
        ValidationErrorKind::Format { format } => {
            format!("形式 {format} に一致しません（実際: {preview}）")
        }
        ValidationErrorKind::ContentEncoding { content_encoding } => {
            format!("{content_encoding} として不正な文字列です")
        }
        ValidationErrorKind::ContentMediaType { content_media_type } => {
            format!("{content_media_type} として不正な内容です")
        }
        ValidationErrorKind::FromUtf8 { .. } => "文字コードが不正です".to_string(),
        ValidationErrorKind::MinItems { limit } => {
            format!("{limit}件以上必要です（実際: {}件）", array_len(instance))
        }
        ValidationErrorKind::MaxItems { limit } => {
            format!(
                "{limit}件以下にしてください（実際: {}件）",
                array_len(instance)
            )
        }
        ValidationErrorKind::UniqueItems => "要素が重複しています".to_string(),
        ValidationErrorKind::Contains => "条件に一致する要素がありません".to_string(),
        ValidationErrorKind::AdditionalItems { limit } => {
            format!("要素数が多すぎます（最大 {limit} 件）")
        }
        ValidationErrorKind::UnevaluatedItems { unexpected } => {
            format!("想定外の要素があります: {}", unexpected.join(", "))
        }
        ValidationErrorKind::MinProperties { limit } => format!(
            "プロパティが{limit}件以上必要です（実際: {}件）",
            object_len(instance)
        ),
        ValidationErrorKind::MaxProperties { limit } => format!(
            "プロパティが{limit}件以下になるようにしてください（実際: {}件）",
            object_len(instance)
        ),
        ValidationErrorKind::PropertyNames { error } => {
            format!("プロパティ名が不正です（{error}）")
        }
        ValidationErrorKind::AdditionalProperties { unexpected }
        | ValidationErrorKind::UnevaluatedProperties { unexpected } => {
            format!("スキーマに無いキーです: {}", unexpected.join(", "))
        }
        ValidationErrorKind::AnyOf { context } => {
            format!(
                "次のいずれの形式にも一致しません{}",
                describe_branches(context, opts)
            )
        }
        ValidationErrorKind::OneOfNotValid { context } => format!(
            "次のいずれか1つに一致する必要があります{}",
            describe_branches(context, opts)
        ),
        ValidationErrorKind::OneOfMultipleValid { .. } => {
            "複数の形式に同時に一致しています（1つだけにしてください）".to_string()
        }
        ValidationErrorKind::Not { .. } => "この値は許可されていません".to_string(),
        ValidationErrorKind::FalseSchema => "このフィールドには値を入れられません".to_string(),
        ValidationErrorKind::Custom { keyword, message } => {
            format!("検証エラー（{keyword}）: {message}")
        }
        other => fallback_message(other.keyword(), &error.to_string()),
    }
}

/// 未対応 kind のフォールバック（英語原文を落とさない）
pub(super) fn fallback_message(keyword: &str, raw: &str) -> String {
    format!("検証エラー（{keyword}）: {raw}")
}

/// `anyOf` / `oneOf` の分岐を先頭 1 件ずつ要約する
fn describe_branches(context: &[Vec<ValidationError<'static>>], opts: &FeedbackOptions) -> String {
    let mut out = String::new();
    for (index, branch) in context.iter().take(MAX_BRANCHES).enumerate() {
        let Some(first) = branch.first() else {
            continue;
        };
        out.push_str(&format!(
            "\n    - 形式{}: {}",
            index + 1,
            describe_error(first, opts)
        ));
    }
    out
}

fn enum_options(options: &Value, opts: &FeedbackOptions) -> (String, usize) {
    let Some(values) = options.as_array() else {
        return (value_preview(options, opts.max_value_chars), 0);
    };
    let take = values.len().min(MAX_ENUM_OPTIONS);
    let shown = values[..take]
        .iter()
        .map(|v| value_preview(v, opts.max_value_chars))
        .collect::<Vec<_>>()
        .join(", ");
    (shown, values.len() - take)
}
