use std::marker::PhantomData;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{BoxFuture, FillError, Tool, ToolOutput, ToolSpec};

pub struct TypedTool<A, H> {
    name: String,
    description: String,
    parameters: Value,
    handler: H,
    _marker: PhantomData<fn() -> A>,
}

pub fn typed_tool<A, H>(
    name: impl Into<String>,
    description: impl Into<String>,
    handler: H,
) -> TypedTool<A, H>
where
    A: DeserializeOwned + schemars::JsonSchema + Send + 'static,
    H: Fn(A) -> BoxFuture<'static, Result<ToolOutput, FillError>> + Send + Sync,
{
    let schema = schemars::schema_for!(A);
    TypedTool {
        name: name.into(),
        description: description.into(),
        parameters: schema.as_value().clone(),
        handler,
        _marker: PhantomData,
    }
}

impl<A, H> Tool for TypedTool<A, H>
where
    A: DeserializeOwned + Send + 'static,
    H: Fn(A) -> BoxFuture<'static, Result<ToolOutput, FillError>> + Send + Sync,
{
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }

    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<ToolOutput, FillError>> {
        let parsed = match serde_json::from_value::<A>(args) {
            Ok(parsed) => parsed,
            Err(e) => {
                let name = self.name.clone();
                return Box::pin(async move {
                    Err(FillError::Tool {
                        name,
                        message: e.to_string(),
                    })
                });
            }
        };
        (self.handler)(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolSet;
    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::json;
    use std::sync::Arc;

    #[derive(Deserialize, JsonSchema)]
    struct Foo {
        _a: String,
    }

    #[test]
    fn typed_tool_schema() {
        let tool = typed_tool::<Foo, _>("foo", "a test tool", |_: Foo| {
            Box::pin(async { Ok(ToolOutput::success("ok")) })
        });
        assert_eq!(tool.spec().name, "foo");
        assert_eq!(tool.spec().description, "a test tool");
        assert_eq!(tool.spec().parameters["type"], json!("object"));
    }

    #[tokio::test]
    async fn typed_tool_rejects_bad_args() {
        let tool = typed_tool::<Foo, _>("foo", "a test tool", |_: Foo| {
            Box::pin(async { Ok(ToolOutput::success("ok")) })
        });
        let result = tool.call(json!({"_a": 1})).await;
        match result.unwrap_err() {
            FillError::Tool { name, message: _ } => assert_eq!(name, "foo"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    struct FixedTool;
    impl Tool for FixedTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "fixed".to_string(),
                description: "fixed tool".to_string(),
                parameters: json!({"type": "object"}),
            }
        }
        fn prompt_hint(&self) -> Option<&str> {
            Some("use me")
        }
        fn call<'a>(&'a self, _args: Value) -> BoxFuture<'a, Result<ToolOutput, FillError>> {
            Box::pin(async move { Ok(ToolOutput::success("fixed")) })
        }
    }

    #[tokio::test]
    async fn toolset_unknown() {
        let set = ToolSet::new();
        match set.call("nope", json!({ "x": 1 })).await.unwrap_err() {
            FillError::UnknownTool { name } => assert_eq!(name, "nope"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn toolset_dispatches() {
        let mut set = ToolSet::new();
        set.insert(Arc::new(FixedTool));
        let out = set.call("fixed", json!({ "x": 1 })).await.unwrap();
        assert_eq!(out.content, "fixed");
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn toolset_contains() {
        let mut set = ToolSet::new();
        set.insert(Arc::new(FixedTool));
        assert!(set.contains("fixed"));
        assert!(!set.contains("missing"));
    }
}
