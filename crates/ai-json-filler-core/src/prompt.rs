use crate::ToolSet;

pub(crate) const SAVE_DESCRIPTION: &str = "調査結果を部分的に保存する。渡したフィールドのみが既存データにマージされる。\
     必須フィールドが全て揃うと自動的に検証が成功する。\
     検証エラーが返った場合は、その不足フィールドを調査して再度呼び出せ。";

pub(crate) const LANGUAGE_RULES: &str = "【言語】\n\
     - 出力は必ず日本語で書くこと\n\
     - 中国語（簡体字）や英語の単語を混ぜない。固有名詞・数値・単位のみ英語可\n\
     - 意味の通らない単語や記号の羅列を書かない。不確実な箇所は「不明」と書く";

pub(crate) fn build_system_prompt(
    base: &str,
    save_tool_name: &str,
    give_up_tool_name: &str,
    tools: &ToolSet,
) -> String {
    let mut out = String::from(base);
    out.push_str("\n\n【保存の手順】\n");
    out.push_str(&format!(
        "- 調査結果は {save_tool_name} ツールで部分的に保存する\n"
    ));
    out.push_str("- 一度にすべて埋める必要はない。分かっただけで呼ぶ\n");
    out.push_str("- 検証エラーが返ったら、不足分を追加調査して再保存する\n");
    out.push_str("- 不明な項目は省略してよい。推測で値を埋めない\n");
    out.push_str(&format!(
        "- 十分に調べても必須項目が埋められないと判断したら、{give_up_tool_name} を reason 付きで呼ぶ\n"
    ));
    out.push_str(&format!(
        "- {give_up_tool_name} の乱用は禁止。調査ツールを試した後に限る\n"
    ));
    out.push_str(&format!(
        "- ツール呼び出しが無い応答を返さない。必ず {save_tool_name} か {give_up_tool_name} を呼ぶ\n"
    ));

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
