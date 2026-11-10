use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::{BoxFuture, FillError};

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn prompt_hint(&self) -> Option<&str> {
        None
    }
    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<ToolOutput, FillError>>;
}

pub struct ToolSet {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolSet {
    pub fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.spec().name;
        self.tools.insert(name, tool);
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    pub(crate) fn tools(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools.values()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn call<'a>(
        &'a self,
        name: &str,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, FillError>> {
        if let Some(tool) = self.tools.get(name) {
            return tool.call(args);
        }
        let name = name.to_string();
        Box::pin(async move { Err(FillError::UnknownTool { name }) })
    }
}

impl Default for ToolSet {
    fn default() -> Self {
        Self::new()
    }
}
