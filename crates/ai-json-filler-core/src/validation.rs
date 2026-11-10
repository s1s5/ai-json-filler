use jsonschema::Validator;
use serde_json::Value;

use crate::schema::display_path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    pub path: String,
    pub keyword: String,
    pub message: String,
}

pub fn validate(validator: &Validator, value: &Value) -> Vec<ValidationIssue> {
    validator
        .iter_errors(value)
        .map(|e| ValidationIssue {
            path: display_path(e.instance_path()),
            keyword: e.kind().keyword().to_string(),
            message: e.to_string(),
        })
        .collect()
}

pub fn validate_seed(validator: &Validator, value: &Value) -> Result<(), Vec<ValidationIssue>> {
    let issues = validate(validator, value);
    if issues.is_empty() || issues.iter().all(|i| i.keyword == "required") {
        Ok(())
    } else {
        Err(issues)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::FillSchema;
    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::json;

    fn schema(value: Value) -> FillSchema {
        FillSchema::from_value(value).expect("schema must be valid")
    }

    #[test]
    fn seed_allows_missing_required() {
        let schema = schema(json!({
            "type": "object",
            "properties": {
                "title": {"type": "string"},
                "count": {"type": "number"}
            },
            "required": ["title", "count"],
            "additionalProperties": false
        }));
        assert!(schema.validate_seed(&json!({"title": "x"})).is_ok());
    }

    #[test]
    fn seed_rejects_type_error() {
        let schema = schema(json!({
            "type": "object",
            "properties": {"count": {"type": "number"}},
            "required": ["count"]
        }));
        let issues = schema.validate_seed(&json!({"count": "x"})).unwrap_err();
        assert!(issues.iter().any(|i| i.keyword == "type"), "{issues:?}");
        assert_eq!(issues[0].path, "count");
    }

    #[test]
    fn seed_rejects_extra_key() {
        let schema = schema(json!({
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "additionalProperties": false
        }));
        let issues = schema
            .validate_seed(&json!({"title": "x", "extra": 1}))
            .unwrap_err();
        assert!(
            issues.iter().any(|i| i.keyword == "additionalProperties"),
            "{issues:?}"
        );
    }

    #[test]
    fn full_validation_accepts_complete() {
        let schema = schema(json!({
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"],
            "additionalProperties": false
        }));
        assert!(schema.validate(&json!({"title": "x"})).is_ok());
    }

    #[test]
    fn full_validation_rejects_missing() {
        let schema = schema(json!({
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"]
        }));
        let issues = schema.validate(&json!({})).unwrap_err();
        assert!(issues.iter().any(|i| i.keyword == "required"), "{issues:?}");
    }

    #[test]
    fn for_type_closes_objects() {
        #[derive(Debug, Deserialize, JsonSchema)]
        struct S {
            a: String,
        }

        let parsed: S = serde_json::from_value(json!({"a": "x"})).expect("value");
        assert_eq!(parsed.a, "x");

        let schema = FillSchema::for_type::<S>().expect("schema");
        assert!(schema.validate(&json!({"a": "x"})).is_ok());
        let issues = schema.validate(&json!({"a": "x", "b": 1})).unwrap_err();
        assert!(
            issues.iter().any(|i| i.keyword == "additionalProperties"),
            "{issues:?}"
        );
    }
}
