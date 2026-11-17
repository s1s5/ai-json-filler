use crate::{ArrayToolNames, FillError, RootKind};

pub(crate) const SAVE_DESCRIPTION: &str = "調査結果を JSON で保存する。引数は 1 つのオブジェクトで、\
     分かった項目だけを入れてよい（オブジェクトはキー単位で統合、配列は全体を置き換える）。\
     キーに `.` や `[0]` を付けず、入れ子は JSON の入れ子で書く。\
     検証エラーが返ったら、指摘されたキーを直して再送する。";

pub(crate) const LANGUAGE_RULES: &str = "【言語】\n\
     - 出力は必ず日本語で書くこと\n\
     - 中国語（簡体字）や英語の単語を混ぜない。固有名詞・数値・単位のみ英語可\n\
     - 意味の通らない単語や記号の羅列を書かない。不確実な箇所は「不明」と書く";

/// Which part of the prompt a caller can replace or disable.
///
/// The order of the variants is the order [`crate::FillSession::prompt_sections`]
/// reports them. [`Self::SYSTEM`] is the order the sections are joined in
/// `system_prompt()`; it is not the declaration order, because the tool hints
/// come before the language rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PromptSection {
    /// 【保存の手順】, naming the save tools that the current mode exposes.
    SaveInstructions,
    /// 【保存する JSON の形】 / 【要素の形】 followed by the field list.
    Guide,
    /// 【言語】.
    LanguageRules,
    /// The concatenation of every registered `Tool::prompt_hint()`.
    ToolHints,
    /// The first user message of the session.
    StartMessage,
    /// The user message inserted when the loop forces a save.
    SaveOnlyMessage,
}

impl PromptSection {
    /// Every section, in declaration order.
    pub const ALL: [Self; 6] = [
        Self::SaveInstructions,
        Self::Guide,
        Self::LanguageRules,
        Self::ToolHints,
        Self::StartMessage,
        Self::SaveOnlyMessage,
    ];

    /// The sections `system_prompt()` is made of, in joined order.
    ///
    /// The guide comes last, after the language rules: that is where the
    /// pre-section `system_prompt()` appended it, and the default prompt must
    /// keep the same bytes.
    pub const SYSTEM: [Self; 4] = [
        Self::SaveInstructions,
        Self::ToolHints,
        Self::LanguageRules,
        Self::Guide,
    ];
}

impl std::fmt::Display for PromptSection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::SaveInstructions => "保存の手順",
            Self::Guide => "フィールド一覧",
            Self::LanguageRules => "言語",
            Self::ToolHints => "ツールヒント",
            Self::StartMessage => "開始メッセージ",
            Self::SaveOnlyMessage => "強制保存メッセージ",
        };
        f.write_str(name)
    }
}

/// Where a rendered section came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PromptSectionSource {
    /// The built-in text.
    #[default]
    Default,
    /// Replaced by `with_prompt_section`.
    Custom,
    /// Removed by `without_prompt_section`.
    Disabled,
}

/// The current state of one prompt section.
///
/// `rendered` is what the model actually receives, separators included, so
/// concatenating the [`PromptSection::SYSTEM`] sections reproduces
/// `system_prompt()`. `template` is the raw string the caller wrote, before
/// placeholder substitution; it is `None` unless `source` is
/// [`PromptSectionSource::Custom`].
#[derive(Debug)]
pub struct PromptSectionView<'a> {
    pub source: PromptSectionSource,
    pub template: Option<&'a str>,
    pub rendered: String,
}

/// How much of the field list appears in the system prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GuideMode {
    /// No field list at all.
    Off,
    /// The field list, without the sentence that names the save tools.
    Fields,
    /// The field list preceded by a sentence that names the save tools.
    #[default]
    FieldsWithToolNames,
}

/// Warnings about a prompt that is likely to mislead the model.
///
/// The audit never fails: renaming a tool is legitimate, so a name it does not
/// recognise may be intentional. Callers that want a hard guarantee can assert
/// `prompt_audit().warnings.is_empty()` in their own tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptAudit {
    pub warnings: Vec<String>,
}

impl PromptAudit {
    pub(crate) fn warn(&mut self, warning: String) {
        if !self.warnings.contains(&warning) {
            self.warnings.push(warning);
        }
    }

    /// Drop every warning that mentions `name`.
    ///
    /// Use it for names the prompt mentions on purpose, such as a tool that
    /// was renamed in a migration message.
    pub fn allow(mut self, name: impl AsRef<str>) -> Self {
        let name = name.as_ref();
        if !name.is_empty() {
            self.warnings.retain(|warning| !warning.contains(name));
        }
        self
    }
}

/// Everything the rendering functions need, borrowed from the session.
pub(crate) struct PromptParts<'a> {
    pub save: &'a str,
    pub names: &'a ArrayToolNames,
    pub give_up: &'a str,
    pub kind: RootKind,
    pub guide: GuideMode,
    /// The rendered field list, already empty when the guide is off.
    pub fields: String,
    /// `Tool::prompt_hint()` of every registered tool, joined by blank lines.
    pub hints: String,
}

/// The built-in text of one section.
///
/// This is the only place that knows the default wording. The session calls it
/// for every section that has not been overridden, so `system_prompt()` and
/// `prompt_section()` cannot drift apart.
pub(crate) fn render_section(section: PromptSection, parts: &PromptParts<'_>) -> String {
    match section {
        PromptSection::SaveInstructions => section_block(section, save_instructions(parts)),
        PromptSection::Guide => {
            if parts.guide == GuideMode::Off {
                return String::new();
            }
            let with_tool_names = parts.guide == GuideMode::FieldsWithToolNames;
            section_block(section, guide_body(parts, with_tool_names))
        }
        PromptSection::LanguageRules => section_block(section, LANGUAGE_RULES.to_string()),
        PromptSection::ToolHints => section_block(section, parts.hints.clone()),
        PromptSection::StartMessage => section_block(section, start_message_body(parts)),
        PromptSection::SaveOnlyMessage => section_block(section, save_only_message_body(parts)),
    }
}

/// Wrap a section body with the separators the surrounding prompt expects.
///
/// An empty body stays empty: a section that renders to nothing (no tool
/// hints, an empty field list) must not leave a blank line behind.
pub(crate) fn section_block(section: PromptSection, body: String) -> String {
    if body.is_empty() {
        return body;
    }
    match section {
        PromptSection::SaveInstructions | PromptSection::Guide => format!("\n\n{body}"),
        PromptSection::LanguageRules => format!("\n{body}"),
        PromptSection::ToolHints => format!("\n{body}\n"),
        PromptSection::StartMessage | PromptSection::SaveOnlyMessage => body,
    }
}

fn save_instructions(parts: &PromptParts<'_>) -> String {
    let save = parts.save;
    let give_up = parts.give_up;
    let mut out = String::from("【保存の手順】\n");
    match parts.kind {
        RootKind::Object => {
            out.push_str(&format!("- 調査結果は {save} ツールで部分的に保存する\n"));
            out.push_str("- 一度にすべて埋める必要はない。分かっただけで呼ぶ\n");
            out.push_str("- 検証エラーが返ったら、不足分を追加調査して再保存する\n");
            out.push_str("- 不明な項目は省略してよい。推測で値を埋めない\n");
        }
        RootKind::Array => {
            let append = &parts.names.append;
            let update = &parts.names.update;
            let remove = &parts.names.remove;
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
        "- 十分に調べても必須項目が埋められないと判断したら、{give_up} を reason 付きで呼ぶ\n"
    ));
    out.push_str(&format!(
        "- {give_up} の乱用は禁止。調査ツールを試した後に限る\n"
    ));
    match parts.kind {
        RootKind::Object => {
            out.push_str(&format!(
                "- ツール呼び出しが無い応答を返さない。必ず {save} か {give_up} を呼ぶ\n"
            ));
        }
        RootKind::Array => {
            out.push_str(&format!(
                "- ツール呼び出しが無い応答を返さない。必ず {} / {} / {} か {give_up} を呼ぶ\n",
                parts.names.append, parts.names.update, parts.names.remove
            ));
        }
    }
    out
}

/// The heading and preamble for the field list, followed by `parts.fields`.
///
/// Returns an empty string when the field list is empty, so no empty heading is
/// left behind. With `with_tool_names == false` the preamble keeps the path
/// guidance but drops the sentence that names the save tools (that is
/// [`GuideMode::Fields`]).
fn guide_body(parts: &PromptParts<'_>, with_tool_names: bool) -> String {
    if parts.fields.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    match parts.kind {
        RootKind::Object => {
            out.push_str("【保存する JSON の形】\n");
            if with_tool_names {
                out.push_str(&format!(
                    "{} の引数は 1 つのオブジェクトです。キーに `.` や `[0]` を付けず、値はそのままの型で書いてください。\n\n",
                    parts.save
                ));
            } else {
                out.push_str(
                    "キーに `.` や `[0]` を付けず、値はそのままの型で書いてください。\n\n",
                );
            }
        }
        RootKind::Array => {
            out.push_str("【要素の形】\n");
            if with_tool_names {
                out.push_str(&format!(
                    "{} の items、{} の patch には、次の形のオブジェクトを入れてください。入れ子にはせず、キーに `.` や `[0]` を付けないでください。\n\n",
                    parts.names.append, parts.names.update
                ));
            } else {
                out.push_str(
                    "items と patch には、次の形のオブジェクトを入れてください。入れ子にはせず、キーに `.` や `[0]` を付けないでください。\n\n",
                );
            }
        }
    }
    out.push_str(&parts.fields);
    out.push('\n');
    out
}

fn start_message_body(parts: &PromptParts<'_>) -> String {
    let save = parts.save;
    let give_up = parts.give_up;
    match parts.kind {
        RootKind::Object => format!(
            "調査を開始し、{save} ツールで結果を保存してください。調べても分からない場合は {give_up} を呼んでください"
        ),
        RootKind::Array => format!(
            "調査を開始し、{} / {} / {} のいずれかで結果を保存してください。調べても分からない場合は {give_up} を呼んでください",
            parts.names.append, parts.names.update, parts.names.remove
        ),
    }
}

fn save_only_message_body(parts: &PromptParts<'_>) -> String {
    let save = parts.save;
    let give_up = parts.give_up;
    match parts.kind {
        RootKind::Object => format!(
            "これ以上は調査できません。分かった範囲で {save} を呼んでください。\n分からない項目は {give_up} で報告してください。"
        ),
        RootKind::Array => format!(
            "これ以上は調査できません。分かった範囲で {} / {} / {} のいずれかで保存してください。\n分からない項目は {give_up} で報告してください。",
            parts.names.append, parts.names.update, parts.names.remove
        ),
    }
}

/// Replace the placeholders in `template` in a single pass.
///
/// Placeholders are `{save}`, `{append}`, `{update}`, `{remove}`, `{give_up}`,
/// and `{fields}` (guide only). A placeholder that is unknown, that belongs to
/// the other mode, or that is not allowed in `section` is an error, so a typo
/// cannot silently reach the model.
///
/// Only identifier-shaped braces are placeholders: the text between `{` and
/// `}` is trimmed, and it counts only when it is non-empty and made up of
/// ASCII lowercase letters and `_`. Everything else (`{"name": "x"}`, `{1}`,
/// `{Save}`, `{}`) is copied through as written, so a template may contain
/// JSON examples. There is no escape for a literal `{save}`.
pub(crate) fn substitute(
    template: &str,
    section: PromptSection,
    parts: &PromptParts<'_>,
) -> Result<String, FillError> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            // 閉じていない `{` は散文の波括弧としてそのまま残す。
            out.push('{');
            rest = after;
            continue;
        };
        let inner = &after[..end];
        let name = inner.trim();
        if !is_placeholder_name(name) {
            // プレースホルダではない（JSON の例など）。書かれたとおりに出す。
            out.push('{');
            out.push_str(inner);
            out.push('}');
            rest = &after[end + 1..];
            continue;
        }
        match name {
            "save" => match parts.kind {
                RootKind::Object => out.push_str(parts.save),
                RootKind::Array => return Err(unavailable(section, name)),
            },
            "append" | "update" | "remove" => match parts.kind {
                RootKind::Array => out.push_str(match name {
                    "append" => &parts.names.append,
                    "update" => &parts.names.update,
                    _ => &parts.names.remove,
                }),
                RootKind::Object => return Err(unavailable(section, name)),
            },
            "give_up" => out.push_str(parts.give_up),
            "fields" if section == PromptSection::Guide => out.push_str(&parts.fields),
            _ => return Err(unavailable(section, name)),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Whether `name` (already trimmed) is a placeholder name and not prose.
fn is_placeholder_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_')
}

/// Whether `template` contains the placeholder `{name}` (spacing allowed).
pub(crate) fn has_placeholder(template: &str, name: &str) -> bool {
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return false;
        };
        if after[..end].trim() == name {
            return true;
        }
        rest = &after[end + 1..];
    }
    false
}

fn unavailable(section: PromptSection, placeholder: &str) -> FillError {
    FillError::UnknownPromptPlaceholder {
        section,
        placeholder: format!("{{{placeholder}}}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts<'a>(
        kind: RootKind,
        save: &'a str,
        names: &'a ArrayToolNames,
        give_up: &'a str,
        fields: &str,
    ) -> PromptParts<'a> {
        PromptParts {
            save,
            names,
            give_up,
            kind,
            guide: GuideMode::default(),
            fields: fields.to_string(),
            hints: String::new(),
        }
    }

    #[test]
    fn guide_section_object_uses_save_tool_name() {
        let names = ArrayToolNames::default();
        let parts = parts(
            RootKind::Object,
            "store_fields",
            &names,
            "give_up",
            "- name: string  (必須)  会社名",
        );
        let section = guide_body(&parts, true);
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
        let parts = parts(
            RootKind::Array,
            "save_result",
            &names,
            "give_up",
            "- market_key: string  (必須)",
        );
        let section = guide_body(&parts, true);
        assert!(section.contains("【要素の形】"), "{section}");
        assert!(section.contains("add_rows の items"), "{section}");
        assert!(section.contains("patch_row の patch"), "{section}");
        assert!(!section.contains("save_result"), "{section}");
    }

    #[test]
    fn guide_section_can_drop_tool_names() {
        let names = ArrayToolNames::default();
        let object = parts(
            RootKind::Object,
            "store_fields",
            &names,
            "give_up",
            "- a: string",
        );
        let section = guide_body(&object, false);
        assert!(section.contains("【保存する JSON の形】"), "{section}");
        assert!(!section.contains("store_fields"), "{section}");
        assert!(section.contains("- a: string"), "{section}");
        assert!(
            section.contains("キーに `.` や `[0]` を付けず"),
            "{section}"
        );
    }

    #[test]
    fn guide_section_is_empty_when_fields_empty() {
        let names = ArrayToolNames::default();
        let object = parts(RootKind::Object, "save_result", &names, "give_up", "");
        let array = parts(RootKind::Array, "save_result", &names, "give_up", "");
        assert!(guide_body(&object, true).is_empty());
        assert!(guide_body(&array, true).is_empty());
    }

    #[test]
    fn save_instructions_name_the_mode_tools() {
        let names = ArrayToolNames::default();
        let object = parts(RootKind::Object, "save_result", &names, "give_up", "");
        let text = save_instructions(&object);
        assert!(text.contains("save_result ツールで部分的に保存"), "{text}");
        assert!(!text.contains("append"), "{text}");

        let array = parts(RootKind::Array, "save_result", &names, "give_up", "");
        let text = save_instructions(&array);
        assert!(text.contains("append: 配列の末尾"), "{text}");
        assert!(!text.contains("save_result"), "{text}");
    }

    #[test]
    fn substitute_fills_mode_placeholders() {
        let names = ArrayToolNames {
            append: "add_rows".to_string(),
            update: "patch_row".to_string(),
            remove: "drop_row".to_string(),
        };
        let array = parts(RootKind::Array, "save_result", &names, "cannot", "f");
        let rendered = substitute(
            "{append}/{update}/{remove} then {give_up}",
            PromptSection::StartMessage,
            &array,
        )
        .expect("placeholders");
        assert_eq!(rendered, "add_rows/patch_row/drop_row then cannot");

        let object = parts(RootKind::Object, "store", &names, "cannot", "f");
        let rendered = substitute("{save} or {give_up}", PromptSection::StartMessage, &object)
            .expect("placeholders");
        assert_eq!(rendered, "store or cannot");
    }

    #[test]
    fn substitute_rejects_foreign_mode_placeholder() {
        let names = ArrayToolNames::default();
        let object = parts(RootKind::Object, "save_result", &names, "give_up", "f");
        assert!(substitute("{append}", PromptSection::StartMessage, &object).is_err());

        let array = parts(RootKind::Array, "save_result", &names, "give_up", "f");
        assert!(substitute("{save}", PromptSection::StartMessage, &array).is_err());
    }

    #[test]
    fn substitute_rejects_unknown_placeholder() {
        let names = ArrayToolNames::default();
        let object = parts(RootKind::Object, "save_result", &names, "give_up", "f");
        assert!(substitute("{savee}", PromptSection::StartMessage, &object).is_err());
        // `{fields}` は Guide 専用。
        assert!(substitute("{fields}", PromptSection::StartMessage, &object).is_err());
    }

    #[test]
    fn substitute_leaves_non_placeholder_braces_alone() {
        let names = ArrayToolNames::default();
        let object = parts(RootKind::Object, "save_result", &names, "give_up", "f");

        let rendered = substitute(
            r#"例: {"name": "x"}。{save} を呼ぶ"#,
            PromptSection::StartMessage,
            &object,
        )
        .expect("JSON example is literal");
        assert_eq!(rendered, r#"例: {"name": "x"}。save_result を呼ぶ"#);

        for literal in ["{1}", "{}", "{Save}", "{ save }"] {
            let expected = if literal == "{ save }" {
                "save_result"
            } else {
                literal
            };
            let rendered =
                substitute(literal, PromptSection::StartMessage, &object).expect("literal braces");
            assert_eq!(rendered, expected, "{literal}");
        }
    }

    #[test]
    fn has_placeholder_ignores_spacing_and_prose() {
        assert!(has_placeholder("{fields}", "fields"));
        assert!(has_placeholder("a { fields } b", "fields"));
        assert!(!has_placeholder(r#"{"fields": 1}"#, "fields"));
        assert!(!has_placeholder("{fields }x", "field"));
    }

    #[test]
    fn substitute_keeps_unclosed_braces() {
        let names = ArrayToolNames::default();
        let object = parts(RootKind::Object, "save_result", &names, "give_up", "f");
        assert_eq!(
            substitute("a {save", PromptSection::StartMessage, &object).unwrap(),
            "a {save"
        );
        assert_eq!(
            substitute("a } b", PromptSection::StartMessage, &object).unwrap(),
            "a } b"
        );
    }

    #[test]
    fn substitute_keeps_double_braces() {
        let names = ArrayToolNames::default();
        let object = parts(RootKind::Object, "save_result", &names, "give_up", "f");
        // 外側の `{` から最初の `}` までが `{save` で、識別子形ではない。
        // エスケープ記法ではないので、そのまま残る。
        assert_eq!(
            substitute("{{save}}", PromptSection::StartMessage, &object).unwrap(),
            "{{save}}"
        );
    }

    #[test]
    fn save_description_forbids_path_keys() {
        assert!(
            SAVE_DESCRIPTION.contains("キーに `.` や `[0]` を付けず"),
            "{SAVE_DESCRIPTION}"
        );
    }

    #[test]
    fn save_description_mentions_array_replacement() {
        assert!(
            SAVE_DESCRIPTION.contains("配列は全体を置き換える"),
            "{SAVE_DESCRIPTION}"
        );
    }

    #[test]
    fn save_description_has_no_merge_wording() {
        assert!(!SAVE_DESCRIPTION.contains("マージ"), "{SAVE_DESCRIPTION}");
        assert!(
            !SAVE_DESCRIPTION.contains("部分的に保存"),
            "{SAVE_DESCRIPTION}"
        );
    }

    #[test]
    fn save_description_keeps_retry_hint() {
        assert!(
            SAVE_DESCRIPTION.contains("検証エラーが返ったら"),
            "{SAVE_DESCRIPTION}"
        );
        assert!(SAVE_DESCRIPTION.contains("再送"), "{SAVE_DESCRIPTION}");
    }

    #[test]
    fn sections_join_into_the_system_prompt_shape() {
        let names = ArrayToolNames::default();
        let mut p = parts(
            RootKind::Object,
            "save_result",
            &names,
            "give_up",
            "- a: string",
        );
        p.hints = "【ヒント】".to_string();
        let mut out = String::new();
        for section in PromptSection::SYSTEM {
            out.push_str(&render_section(section, &p));
        }
        assert!(out.starts_with("\n\n【保存の手順】\n"), "{out}");

        // 旧実装と同じ並び: 保存の手順 → ツールヒント → 言語 → ガイド。
        let order = [
            "【保存の手順】",
            "【ヒント】",
            "【言語】",
            "【保存する JSON の形】",
        ];
        let mut cursor = 0usize;
        for heading in order {
            let at = out[cursor..]
                .find(heading)
                .unwrap_or_else(|| panic!("{heading} missing or out of order: {out}"));
            cursor += at;
        }
    }
}
