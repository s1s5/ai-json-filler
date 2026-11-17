//! 「埋まっていてほしい項目」= ゴール項目。
//!
//! `required` はスキーマ検証の担当で、`Option<T>` / `#[serde(default)]` の
//! フィールドは 1 つも入らなくても完了してしまう。ゴール項目はその上に
//! 「値が存在すること」だけを要求する。意味的な正しさはスキーマ検証に任せる。
//!
//! 記法は `field`（Object モード）と `[].field`（Array モード）だけ。
//! 存在しないフィールドをゴールにすると永久に完了しないセッションになるため、
//! 記法とフィールド名は [`FillSession::with_goal_fields`] の構築時に検査する。
//!
//! [`FillSession::with_goal_fields`]: crate::FillSession::with_goal_fields

use jsonschema::paths::{Location, LocationSegment};
use serde_json::Value;

use crate::{FillError, FillSchema, RootKind};

/// 1 つのゴール項目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalPath {
    /// トップレベルのフィールド（Object モードのみ）
    Field(String),
    /// 全要素のフィールド（Array モードのみ）
    EveryElement(String),
}

/// Array モードで要素 0 件を許すかどうか。
///
/// 件数の上下限は扱わない。`Fail` は「最低 1 件」だけを要求する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GoalEmpty {
    /// 要素 0 件でも充足（既定）
    #[default]
    Pass,
    /// 最低 1 件を要求する
    Fail,
}

impl GoalPath {
    /// 記法を検証してゴール項目にする。
    ///
    /// ルート種別と記法が食い違う場合（Object モードの `[].x`、Array モードの
    /// 素の `x`）も [`FillError::InvalidGoalPath`] にする。
    pub(crate) fn parse(spec: &str, kind: RootKind) -> Result<Self, FillError> {
        let invalid = || FillError::InvalidGoalPath {
            path: spec.to_string(),
        };
        match kind {
            RootKind::Object => {
                if spec.is_empty() || has_notation(spec) {
                    return Err(invalid());
                }
                Ok(GoalPath::Field(spec.to_string()))
            }
            RootKind::Array => {
                let Some(name) = spec.strip_prefix("[].") else {
                    return Err(invalid());
                };
                if name.is_empty() || has_notation(name) {
                    return Err(invalid());
                }
                Ok(GoalPath::EveryElement(name.to_string()))
            }
        }
    }

    /// フィールド名。
    pub fn field(&self) -> &str {
        match self {
            GoalPath::Field(name) | GoalPath::EveryElement(name) => name,
        }
    }

    /// 指定されたときの記法（エラー表示に使う）。
    pub(crate) fn spec(&self) -> String {
        match self {
            GoalPath::Field(name) => name.clone(),
            GoalPath::EveryElement(name) => format!("[].{name}"),
        }
    }
}

/// パス区切りや添字の記法が混ざっていないか。
fn has_notation(name: &str) -> bool {
    name.contains('.') || name.contains('[') || name.contains(']')
}

/// ゴール項目がスキーマに存在するかを確かめる。
///
/// `$ref` の解決は [`FillSchema::properties_at`] に任せる（Array モードの
/// 要素は `#/$defs/Row` になる）。
pub(crate) fn check_against_schema(path: &GoalPath, schema: &FillSchema) -> Result<(), FillError> {
    let loc = match path {
        GoalPath::Field(_) => Location::new(),
        GoalPath::EveryElement(_) => Location::new().join(LocationSegment::Index(0)),
    };
    let known = schema
        .properties_at(&loc)
        .is_some_and(|properties| properties.contains_key(path.field()));
    if known {
        Ok(())
    } else {
        Err(FillError::UnknownGoalField { path: path.spec() })
    }
}

/// 未充足の 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GoalMiss {
    /// トップレベルのフィールド
    Field(String),
    /// 配列要素のフィールド
    Element { index: usize, field: String },
}

impl GoalMiss {
    /// フィードバックの 1 行（`employees` / `[1].sales_share`）
    pub(crate) fn line(&self) -> String {
        match self {
            GoalMiss::Field(name) => name.clone(),
            GoalMiss::Element { index, field } => format!("[{index}].{field}"),
        }
    }
}

/// 未充足の一覧。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GoalReport {
    pub(crate) misses: Vec<GoalMiss>,
    /// `GoalEmpty::Fail` で要素が 0 件
    pub(crate) empty: bool,
}

impl GoalReport {
    pub(crate) fn is_satisfied(&self) -> bool {
        self.misses.is_empty() && !self.empty
    }
}

/// 現在の値に対して未充足のゴールを集める。
///
/// 充足 = 値が存在し、`null` でなく、`""` でない。`" "` は充足として扱う。
/// `EveryElement` は現在存在する全要素を見る（0 件なら何も検査しない）。
pub(crate) fn missing(goals: &[GoalPath], empty: GoalEmpty, value: &Value) -> GoalReport {
    let mut report = GoalReport::default();
    match value {
        Value::Object(object) => {
            for goal in goals {
                let GoalPath::Field(name) = goal else {
                    continue;
                };
                if !is_present(object.get(name)) {
                    report.misses.push(GoalMiss::Field(name.clone()));
                }
            }
        }
        Value::Array(items) => {
            if items.is_empty() {
                report.empty = empty == GoalEmpty::Fail;
                return report;
            }
            for goal in goals {
                let GoalPath::EveryElement(name) = goal else {
                    continue;
                };
                for (index, item) in items.iter().enumerate() {
                    let present = item
                        .as_object()
                        .is_some_and(|object| is_present(object.get(name)));
                    if !present {
                        report.misses.push(GoalMiss::Element {
                            index,
                            field: name.clone(),
                        });
                    }
                }
            }
        }
        _ => {}
    }
    report
}

fn is_present(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        // 空文字列は「まだ書かれていない」とみなす。空白だけの文字列は値として扱う。
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}
