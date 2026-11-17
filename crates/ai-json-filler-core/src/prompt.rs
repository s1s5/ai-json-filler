use crate::{ArrayToolNames, RootKind, ToolSet};

pub(crate) const SAVE_DESCRIPTION: &str = "調査結果を部分的に保存する。渡したフィールドのみが既存データにマージされる。\
     必須フィールドが全て揃うと自動的に検証が成功する。\
     検証エラーが返った場合は、その不足フィールドを調査して再度呼び出せ。";

pub(crate) const LANGUAGE_RULES: &str = "【言語】\n\
     - 出力は必ず日本語で書くこと\n\
     - 中国語（簡体字）や英語の単語を混ぜない。固有名詞・数値・単位のみ英語可\n\
     - 意味の通らない単語や記号の羅列を書かない。不確実な箇所は「不明」と書く";

pub(crate) fn build_system_prompt(
    base: &str,
    kind: RootKind,
    save_tool_name: &str,
    give_up_tool_name: &str,
    array_tool_names: &ArrayToolNames,
    tools: &ToolSet,
) -> String {
    let mut out = String::from(base);
    out.push_str("\n\n【保存の手順】\n");

    match kind {
        RootKind::Object => {
            out.push_str(&format!(
                "- 調査結果は {save_tool_name} ツールで部分的に保存する\n"
            ));
            out.push_str("- 一度にすべて埋める必要はない。分かっただけで呼ぶ\n");
            out.push_str("- 検証エラーが返ったら、不足分を追加調査して再保存する\n");
            out.push_str("- 不明な項目は省略してよい。推測で値を埋めない\n");
        }
        RootKind::Array => {
            let append = &array_tool_names.append;
            let update = &array_tool_names.update;
            let remove = &array_tool_names.remove;
            out.push_str("- 調査結果は配列として保存する。次の3ツールを使い分ける\n");
            out.push_str(&format!(
                "- {append}: 配列の末尾に要素を追加する。要素は部分的でよい。複数件を一度に追加できる\n"
            ));
            out.push_str(&format!(
                "- {update}: index の要素に patch をマージする。渡したフィールドだけが更新される\n"
            ));
            out.push_str(&format!(
                "- {remove}: index の要素を削除する。削除後、後ろの index は前に詰まる\n"
            ));
            out.push_str("- index は 0 始まり\n");
            out.push_str(&format!(
                "- 既存要素を書き換えるときは {update} を使い、配列全体を送り直さない\n"
            ));
            out.push_str("- 要素の順序を保つ。削除後の index のずれに注意する\n");
            out.push_str("- 不明な項目は省略してよい。推測で値を埋めない\n");
        }
    }

    out.push_str(&format!(
        "- 十分に調べても必須項目が埋められないと判断したら、{give_up_tool_name} を reason 付きで呼ぶ\n"
    ));
    out.push_str(&format!(
        "- {give_up_tool_name} の乱用は禁止。調査ツールを試した後に限る\n"
    ));
    match kind {
        RootKind::Object => {
            out.push_str(&format!(
                "- ツール呼び出しが無い応答を返さない。必ず {save_tool_name} か {give_up_tool_name} を呼ぶ\n"
            ));
        }
        RootKind::Array => {
            out.push_str(&format!(
                "- ツール呼び出しが無い応答を返さない。必ず {} / {} / {} か {give_up_tool_name} を呼ぶ\n",
                array_tool_names.append, array_tool_names.update, array_tool_names.remove
            ));
        }
    }

    for tool in tools.tools() {
        if let Some(hint) = tool.prompt_hint() {
            out.push('\n');
            out.push_str(hint);
            out.push('\n');
        }
    }

    out.push('\n');
    out.push_str(LANGUAGE_RULES);
    out
}

/// The heading and preamble for the field list, followed by `fields`.
///
/// Returns an empty string when `fields` is empty, so no empty heading is
/// left behind. Tool names appear here (not in `schema.rs`) because both the
/// save tool and the array tools can be renamed by the caller.
pub(crate) fn guide_section(
    kind: RootKind,
    save_tool: &str,
    array_names: &ArrayToolNames,
    fields: &str,
) -> String {
    if fields.is_empty() {
        return String::new();
    }

    let mut out = String::from("\n\n");
    match kind {
        RootKind::Object => {
            out.push_str("【保存する JSON の形】\n");
            out.push_str(&format!(
                "{save_tool} の引数は 1 つのオブジェクトです。キーに `.` や `[0]` を付けず、値はそのままの型で書いてください。\n\n"
            ));
        }
        RootKind::Array => {
            out.push_str("【要素の形】\n");
            out.push_str(&format!(
                "{} の items、{} の patch には、次の形のオブジェクトを入れてください。入れ子にはせず、キーに `.` や `[0]` を付けないでください。\n\n",
                array_names.append, array_names.update
            ));
        }
    }
    out.push_str(fields);
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_names() -> ArrayToolNames {
        ArrayToolNames::default()
    }

    #[test]
    fn guide_section_object_uses_save_tool_name() {
        let section = guide_section(
            RootKind::Object,
            "store_fields",
            &default_names(),
            "- name: string  (必須)  会社名",
        );
        assert!(section.contains("【保存する JSON の形】"), "{section}");
        assert!(section.contains("store_fields の引数"), "{section}");
        assert!(
            section.contains("- name: string  (必須)  会社名"),
            "{section}"
        );
    }

    #[test]
    fn guide_section_array_uses_custom_names() {
        let names = ArrayToolNames {
            append: "add_rows".to_string(),
            update: "patch_row".to_string(),
            remove: "drop_row".to_string(),
        };
        let section = guide_section(
            RootKind::Array,
            "save_result",
            &names,
            "- market_key: string  (必須)",
        );
        assert!(section.contains("【要素の形】"), "{section}");
        assert!(section.contains("add_rows の items"), "{section}");
        assert!(section.contains("patch_row の patch"), "{section}");
        assert!(!section.contains("save_result"), "{section}");
    }

    #[test]
    fn guide_section_is_empty_when_fields_empty() {
        assert!(guide_section(RootKind::Object, "save_result", &default_names(), "").is_empty());
        assert!(guide_section(RootKind::Array, "save_result", &default_names(), "").is_empty());
    }
}
