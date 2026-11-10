use jsonschema::Validator;
use jsonschema::error::{ValidationError, ValidationErrorKind};
use jsonschema::paths::{Location, LocationSegment};
use serde_json::Value;

use crate::error::FillError;

pub struct FillSchema {
    full: Value,
    partial: Value,
    validator: Validator,
}

impl std::fmt::Debug for FillSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FillSchema")
            .field("full", &self.full)
            .field("partial", &self.partial)
            .finish()
    }
}

impl FillSchema {
    pub fn for_type<T>() -> Result<Self, FillError>
    where
        T: serde::de::DeserializeOwned + schemars::JsonSchema,
    {
        let schema = schemars::schema_for!(T);
        let mut full = schema.as_value().clone();
        close_objects(&mut full);
        Self::from_value(full)
    }

    pub fn from_value(full: Value) -> Result<Self, FillError> {
        let is_object = full.get("type").and_then(Value::as_str) == Some("object");
        let has_properties = full
            .get("properties")
            .map(Value::is_object)
            .unwrap_or(false);
        if !is_object || !has_properties {
            return Err(FillError::UnsupportedRootSchema);
        }
        let validator =
            jsonschema::validator_for(&full).map_err(|e| FillError::Schema(e.to_string()))?;
        let partial = to_partial_schema(&full);
        Ok(Self {
            full,
            partial,
            validator,
        })
    }

    pub fn full(&self) -> &Value {
        &self.full
    }
    pub fn partial(&self) -> &Value {
        &self.partial
    }
    pub(crate) fn validator(&self) -> &Validator {
        &self.validator
    }

    pub fn validate(&self, value: &Value) -> Result<(), Vec<crate::ValidationIssue>> {
        let issues = crate::validation::validate(&self.validator, value);
        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
    }

    pub fn validate_seed(&self, value: &Value) -> Result<(), Vec<crate::ValidationIssue>> {
        crate::validation::validate_seed(&self.validator, value)
    }
}

pub(crate) fn to_partial_schema(output_schema: &Value) -> Value {
    let mut schema = output_schema.clone();
    partial_recursive(&mut schema);
    schema
}

fn partial_recursive(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    obj.remove("required");

    if let Some(props) = obj.get_mut("properties").and_then(|p| p.as_object_mut()) {
        for (_, prop) in props.iter_mut() {
            partial_recursive(prop);
        }
    }
    if let Some(items) = obj.get_mut("items") {
        partial_recursive(items);
    }
    for key in ["$defs", "definitions"] {
        if let Some(defs) = obj.get_mut(key).and_then(|d| d.as_object_mut()) {
            for (_, def) in defs.iter_mut() {
                partial_recursive(def);
            }
        }
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(branches) = obj.get_mut(key).and_then(|b| b.as_array_mut()) {
            for branch in branches.iter_mut() {
                partial_recursive(branch);
            }
        }
    }

    if !matches!(obj.get("additionalProperties"), Some(v) if !v.is_boolean()) {
        obj.insert("additionalProperties".to_string(), Value::Bool(true));
    }
    if !matches!(obj.get("unevaluatedProperties"), Some(v) if !v.is_boolean()) {
        obj.insert("unevaluatedProperties".to_string(), Value::Bool(true));
    }
}

/// `close_objects` adds `additionalProperties: false`. It must not be used
/// by `from_value` (which preserves the caller's schema).
pub(crate) fn close_objects(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    let has_all_of = obj.get("allOf").and_then(Value::as_array).is_some();
    let has_properties = obj.get("properties").and_then(Value::as_object).is_some();

    if has_properties && !has_all_of {
        match obj.get("additionalProperties") {
            Some(Value::Bool(_)) | None => {
                obj.insert("additionalProperties".to_string(), Value::Bool(false));
            }
            Some(_) => {}
        }
    }
    if has_all_of && !obj.contains_key("unevaluatedProperties") {
        obj.insert("unevaluatedProperties".to_string(), Value::Bool(false));
    }

    if let Some(props) = obj.get_mut("properties").and_then(|p| p.as_object_mut()) {
        for (_, prop) in props.iter_mut() {
            close_objects(prop);
        }
    }
    if let Some(items) = obj.get_mut("items") {
        close_objects(items);
    }
    for key in ["$defs", "definitions"] {
        if let Some(defs) = obj.get_mut(key).and_then(|d| d.as_object_mut()) {
            for (_, def) in defs.iter_mut() {
                close_objects(def);
            }
        }
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(branches) = obj.get_mut(key).and_then(|b| b.as_array_mut()) {
            for branch in branches.iter_mut() {
                close_objects(branch);
            }
        }
    }
}

/// Recursively merges `update` into `base`. Objects merge key-by-key;
/// scalars/arrays replace.
pub(crate) fn deep_merge(base: &mut Value, update: &Value) {
    match (base, update) {
        (Value::Object(base_obj), Value::Object(update_obj)) => {
            for (key, update_val) in update_obj {
                match base_obj.get_mut(key) {
                    Some(base_val) => deep_merge(base_val, update_val),
                    None => {
                        base_obj.insert(key.clone(), update_val.clone());
                    }
                }
            }
        }
        (base, update) => *base = update.clone(),
    }
}

/// Collect `AdditionalProperties` / `UnevaluatedProperties` errors into
/// `(path, keys)`.
pub(crate) fn collect_extra_keys(errors: &[ValidationError<'_>]) -> Vec<(Location, Vec<String>)> {
    use std::collections::{BTreeMap, BTreeSet};

    let mut out: BTreeMap<String, (Location, BTreeSet<String>)> = BTreeMap::new();
    for error in errors {
        let unexpected = match error.kind() {
            ValidationErrorKind::AdditionalProperties { unexpected }
            | ValidationErrorKind::UnevaluatedProperties { unexpected } => unexpected,
            _ => continue,
        };
        let path = display_path(error.instance_path());
        let entry = out
            .entry(path)
            .or_insert_with(|| (error.instance_path().clone(), BTreeSet::new()));
        entry.1.extend(unexpected.iter().cloned());
    }
    out.into_iter()
        .map(|(_, (loc, keys))| (loc, keys.into_iter().collect()))
        .collect()
}

/// Remove the collected extra keys from `instance`. Returns the removed
/// `(loc, key)` pairs.
pub(crate) fn strip_extra_keys(
    instance: &mut Value,
    extras: &[(Location, Vec<String>)],
) -> Vec<(Location, String)> {
    let mut removed = Vec::new();
    for (loc, keys) in extras {
        let Some(target) = resolve_mut(instance, loc) else {
            continue;
        };
        let Some(obj) = target.as_object_mut() else {
            continue;
        };
        for key in keys {
            if obj.remove(key).is_some() {
                removed.push((loc.clone(), key.clone()));
            }
        }
    }
    removed
}

/// Resolve a [Location] into a mutable reference in `root`.
fn resolve_mut<'a>(root: &'a mut Value, loc: &Location) -> Option<&'a mut Value> {
    let mut current = root;
    for segment in loc.iter() {
        current = match segment {
            LocationSegment::Property(name) => current.as_object_mut()?.get_mut(name.as_ref())?,
            LocationSegment::Index(index) => current.as_array_mut()?.get_mut(index)?,
        };
    }
    Some(current)
}

/// Render a [Location] as `market.region`, `news[0]`, or `(root)`.
pub(crate) fn display_path(loc: &Location) -> String {
    let mut out = String::new();
    for segment in loc.iter() {
        match segment {
            LocationSegment::Property(name) => {
                if is_identifier(&name) {
                    if !out.is_empty() {
                        out.push('.');
                    }
                    out.push_str(&name);
                } else {
                    out.push_str(&format!("[\"{}\"]", escape_key(&name)));
                }
            }
            LocationSegment::Index(index) => out.push_str(&format!("[{index}]")),
        }
    }
    if out.is_empty() {
        "(root)".to_string()
    } else {
        out
    }
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn escape_key(key: &str) -> String {
    key.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::json;

    #[test]
    fn partial_removes_nested_required() {
        let schema = json!(
            {
                "type": "object",
                "properties": {
                    "nested": {
                        "type": "object",
                        "properties": {"x": {"type": "string"}},
                        "required": ["x"]
                    }
                },
                "required": ["nested"]
            }
        );
        let partial = to_partial_schema(&schema);
        assert!(partial.get("required").is_none());
        let nested = partial["properties"]["nested"].as_object().unwrap();
        assert!(nested.get("required").is_none());
    }

    #[test]
    fn partial_removes_defs_required() {
        let schema = json!(
            {
                "type": "object",
                "properties": {"a": {"type": "string"}},
                "$defs": {
                    "Inner": {
                        "type": "object",
                        "properties": {"x": {"type": "string"}},
                        "required": ["x"]
                    }
                }
            }
        );
        let partial = to_partial_schema(&schema);
        let inner = partial["$defs"]["Inner"].as_object().unwrap();
        assert!(inner.get("required").is_none());
    }

    #[test]
    fn partial_keeps_object_additional_properties() {
        let schema = json!(
            {
                "type": "object",
                "properties": {"a": {"type": "string"}},
                "additionalProperties": {"type": "string"}
            }
        );
        let partial = to_partial_schema(&schema);
        assert_eq!(partial["additionalProperties"], json!({"type": "string"}));
    }

    #[test]
    fn close_objects_adds_false() {
        let mut schema = json!(
            {
                "type": "object",
                "properties": {"a": {"type": "string"}}
            }
        );
        close_objects(&mut schema);
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn close_objects_skips_maps_and_value() {
        // map-like / map-less: a property that is itself a map type should not
        // be forced additionalProperties:false
        let mut schema = json!(
            {
                "type": "object",
                "properties": {
                    "a": {"type": "string"},
                    "meta": {
                        "type": "object",
                        "additionalProperties": {"type": "any"}
                    }
                }
            }
        );
        // this schema's additionalProperties is a schema (object), which is a
        // boolean-like sentinel? The reference ensures an object-typed
        // additionalProperties that is not a boolean stays as-is is handled by
        // the `Some(_)` arm below.
        close_objects(&mut schema);
        // top-level properties => false
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[derive(Deserialize, JsonSchema, PartialEq, Eq)]
    #[serde(untagged)]
    enum Tagged {
        A { a: String },
        B { b: u32 },
    }

    impl std::fmt::Debug for Tagged {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Tagged::A { a } => write!(f, "A {{ a: {a} }}"),
                Tagged::B { b } => write!(f, "B {{ b: {b} }}"),
            }
        }
    }

    #[derive(Deserialize, JsonSchema, PartialEq, Eq)]
    struct Wrapper {
        nested: Tagged,
    }

    impl std::fmt::Debug for Wrapper {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "Wrapper {{ nested: {:?} }}", self.nested)
        }
    }

    fn wrapper_schema() -> Value {
        schemars::schema_for!(Wrapper).as_value().clone()
    }

    #[test]
    fn partial_recurses_anyof() {
        let partial = to_partial_schema(&wrapper_schema());
        let branches = partial["$defs"]["Tagged"]["anyOf"].as_array().unwrap();
        assert_eq!(branches.len(), 2);
        for branch in branches {
            assert!(branch.get("required").is_none());
        }
    }

    #[test]
    fn close_objects_recurses_anyof() {
        let mut schema = wrapper_schema();
        close_objects(&mut schema);
        for branch in schema["$defs"]["Tagged"]["anyOf"].as_array().unwrap() {
            assert_eq!(branch["additionalProperties"], json!(false));
        }
    }

    #[test]
    fn deep_merge_replaces_arrays() {
        let mut base = json!({"a": [1, 2, 3]});
        deep_merge(&mut base, &json!({"a": [4, 5]}));
        assert_eq!(base["a"], json!([4, 5]));
    }

    #[test]
    fn deep_merge_recurses_objects() {
        let mut base = json!({"a": {"x": 1, "y": 2}});
        deep_merge(&mut base, &json!({"a": {"y": 3, "z": 4}}));
        assert_eq!(base["a"]["x"], json!(1));
        assert_eq!(base["a"]["y"], json!(3));
        assert_eq!(base["a"]["z"], json!(4));
    }

    #[test]
    fn display_path_formats() {
        let mut loc = Location::new();
        loc = loc.join(LocationSegment::Property("market".into()));
        loc = loc.join(LocationSegment::Property("region".into()));
        assert_eq!(display_path(&loc), "market.region");

        let mut idx = Location::new();
        idx = idx.join(LocationSegment::Index(0));
        idx = idx.join(LocationSegment::Property("title".into()));
        assert_eq!(display_path(&idx), "[0].title");

        let mut weird = Location::new();
        weird = weird.join(LocationSegment::Property("a b".into()));
        assert_eq!(display_path(&weird), r#"["a b"]"#);

        assert_eq!(display_path(&Location::new()), "(root)");
    }

    #[test]
    fn from_value_rejects_non_object_root() {
        let err = FillSchema::from_value(json!([1, 2, 3])).unwrap_err();
        assert!(matches!(err, FillError::UnsupportedRootSchema));
    }

    #[test]
    fn strip_extra_keys_removes_nested() {
        let mut instance = json!(
            {
                "market": {
                    "region": "JP",
                    "extra": "x"
                }
            }
        );
        let extras = vec![(loc(), vec!["extra".to_string()])];
        let removed = strip_extra_keys(&mut instance, &extras);
        assert_eq!(removed.len(), 1);
        assert_eq!(instance, json!({"market": {"region": "JP"}}));
    }

    fn loc() -> Location {
        let mut l = Location::new();
        l = l.join(LocationSegment::Property("market".into()));
        l
    }
}
