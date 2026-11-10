use std::collections::{BTreeMap, BTreeSet, HashSet};

use jsonschema::error::{ValidationError, ValidationErrorKind};
use jsonschema::paths::Location;
use serde_json::Value;

use super::FeedbackOptions;
use super::describe::describe_error;
use super::text::{depth, display_path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Section {
    Missing,
    Value,
    Extra,
}

impl Section {
    fn heading(self) -> &'static str {
        match self {
            Section::Missing => "[不足] 必須フィールドがありません",
            Section::Value => "[値] 値が不正です",
            Section::Extra => "[余計] スキーマに無いキーです（無視しました）",
        }
    }
}

/// 出力する 1 行分
#[derive(Debug, Clone)]
pub(super) struct Item {
    pub(super) section: Section,
    /// 表示するパス（`(root)` / `market.region` / `news[0]`）
    path: String,
    /// `Missing` / `Extra` はキー一覧、`Value` は説明文 1 件
    props: Vec<String>,
    /// 安定ソート用キー（深さ → パス → keyword → 詳細）
    sort: (usize, String, String, String),
}

impl Item {
    /// 件数（見出しの合計・上限計算に使う）
    pub(super) fn count(&self) -> usize {
        match self.section {
            Section::Value => 1,
            _ => self.props.len(),
        }
    }

    pub(super) fn render(&self) -> String {
        match self.section {
            Section::Value => format!("- {}: {}", self.path, self.props[0]),
            _ => format!("- {}: {}", self.path, self.props.join(", ")),
        }
    }
}

pub(super) fn assemble(
    total: usize,
    lines: &[(Section, String, usize)],
    saved: &str,
    truncated: usize,
) -> String {
    let mut out = format!("VALIDATION_ERROR: 検証に失敗しました（{total}件）");
    let mut current: Option<Section> = None;
    for (section, line, _) in lines {
        if current != Some(*section) {
            out.push('\n');
            out.push_str(section.heading());
            current = Some(*section);
        }
        out.push('\n');
        out.push_str(line);
    }
    out.push('\n');
    out.push_str(saved);
    if truncated > 0 {
        out.push('\n');
        out.push_str(&format!("…他 {truncated} 件"));
    }
    out
}

/// 予算を消費しながら項目を残す。溢れた件数は `truncated` に積む。
pub(super) fn apply_budget(
    items: Vec<Item>,
    budget: &mut usize,
    truncated: &mut usize,
) -> Vec<Item> {
    let mut out = Vec::new();
    for mut item in items {
        let count = item.count();
        if *budget == 0 {
            *truncated += count;
            continue;
        }
        if count > *budget {
            let keep = *budget;
            *truncated += count - keep;
            item.props.truncate(keep);
            *budget = 0;
            out.push(item);
        } else {
            *budget -= count;
            out.push(item);
        }
    }
    out
}

pub(super) fn collect_missing(errors: &[ValidationError<'_>]) -> Vec<Item> {
    let mut groups: BTreeMap<String, (usize, BTreeSet<String>)> = BTreeMap::new();
    for error in errors {
        let ValidationErrorKind::Required { property } = error.kind() else {
            continue;
        };
        let path = display_path(error.instance_path());
        let name = match property {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let depth = depth(error.instance_path());
        groups
            .entry(path)
            .or_insert((depth, BTreeSet::new()))
            .1
            .insert(name);
    }
    items_from_groups(groups, Section::Missing, "required")
}

pub(super) fn collect_extra(removed: &[(Location, String)]) -> Vec<Item> {
    let mut groups: BTreeMap<String, (usize, BTreeSet<String>)> = BTreeMap::new();
    for (loc, key) in removed {
        let path = display_path(loc);
        let depth = depth(loc);
        groups
            .entry(path)
            .or_insert((depth, BTreeSet::new()))
            .1
            .insert(key.clone());
    }
    items_from_groups(groups, Section::Extra, "")
}

fn items_from_groups(
    groups: BTreeMap<String, (usize, BTreeSet<String>)>,
    section: Section,
    keyword: &str,
) -> Vec<Item> {
    let mut items: Vec<Item> = groups
        .into_iter()
        .map(|(path, (depth, keys))| Item {
            section,
            props: keys.into_iter().collect(),
            sort: (depth, path.clone(), keyword.to_string(), String::new()),
            path,
        })
        .collect();
    items.sort_by(|a, b| a.sort.cmp(&b.sort));
    items
}

/// `Required` 以外のエラーを 1 件 1 行へ変換する
///
/// `AdditionalProperties` は通常 [`strip_extra_keys`] で消えるためここには
/// 残らない。除去できなかった場合だけ `[値]` 側に残して情報を落とさない。
pub(super) fn collect_value_errors(
    errors: &[ValidationError<'_>],
    opts: &FeedbackOptions,
) -> Vec<Item> {
    let mut items = Vec::new();
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    for error in errors {
        if matches!(error.kind(), ValidationErrorKind::Required { .. }) {
            continue;
        }
        let path = display_path(error.instance_path());
        let keyword = error.kind().keyword().to_string();
        let detail = describe_error(error, opts);
        if !seen.insert((path.clone(), keyword.clone(), detail.clone())) {
            continue;
        }
        items.push(Item {
            section: Section::Value,
            sort: (
                depth(error.instance_path()),
                path.clone(),
                keyword,
                detail.clone(),
            ),
            path,
            props: vec![detail],
        });
    }
    items.sort_by(|a, b| a.sort.cmp(&b.sort));
    items
}
