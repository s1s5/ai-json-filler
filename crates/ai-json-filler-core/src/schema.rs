use jsonschema::Validator;
use jsonschema::error::{ValidationError, ValidationErrorKind};
use jsonschema::paths::{Location, LocationSegment};
use serde_json::Value;

use crate::error::FillError;

/// Which top-level shape [`FillSchema`] operates on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKind {
    /// Route is a flat object (`type: object` with `properties`).
    Object,
    /// Route is an array of flat objects (`type: array`, `items` is a flat object).
    Array,
}

/// Limits applied when rendering the field list with
/// [`FillSchema::guide_fields`].
///
/// Both limits default to "unlimited": the description is the only hint the
/// model gets about what to put in a field, so it is truncated only when the
/// caller asks for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuideOptions {
    /// Maximum number of characters kept from a `description`.
    ///
    /// `0` (the default) keeps the whole description. When greater than zero,
    /// the text is cut at the last `。` inside the limit if there is one, or
    /// at the limit itself, and `…` is appended.
    pub max_description_chars: usize,
    /// Maximum number of *optional* fields listed.
    ///
    /// `None` (the default) lists every field. Required fields are always
    /// listed, whatever this limit is. When optional fields are dropped, a
    /// trailing `…他 N 件（任意フィールド）` line reports how many.
    pub max_fields: Option<usize>,
}

pub struct FillSchema {
    full: Value,
    partial: Value,
    validator: Validator,
    kind: RootKind,
}

impl std::fmt::Debug for FillSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FillSchema")
            .field("kind", &self.kind)
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
        let kind = determine_root_kind(&full)?;
        check_flat(&full, &full, "", true, true, &mut Vec::new())?;
        let validator =
            jsonschema::validator_for(&full).map_err(|e| FillError::Schema(e.to_string()))?;
        let partial = to_partial_schema(&full);
        Ok(Self {
            full,
            partial,
            validator,
            kind,
        })
    }

    /// The top-level shape this schema operates on.
    pub fn root_kind(&self) -> RootKind {
        self.kind
    }

    /// Whether the route is an array of flat objects.
    ///
    /// Inferred from the schema: a `Vec<Row>` becomes Array mode without an
    /// explicit flag.
    pub fn is_array(&self) -> bool {
        matches!(self.kind, RootKind::Array)
    }

    pub fn full(&self) -> &Value {
        &self.full
    }
    pub fn partial(&self) -> &Value {
        &self.partial
    }

    /// Render the field list the model should fill, as bullet lines.
    ///
    /// Only the list itself is returned (`- name: string  (必須)  会社名`);
    /// the heading and the preamble around it are built by
    /// `prompt::guide_section`. An empty string means "nothing to render",
    /// so the caller does not leave an empty heading behind.
    ///
    /// Odd schemas never panic here: what can be written is written, and an
    /// unusable schema yields an empty string.
    pub fn guide_fields(&self, opts: &GuideOptions) -> String {
        let Some(node) = self.guide_node() else {
            return String::new();
        };
        let Some(properties) = node.get("properties").and_then(Value::as_object) else {
            return String::new();
        };
        if properties.is_empty() {
            return String::new();
        }

        let required = required_names(node);
        let mut lines: Vec<String> = Vec::new();
        let mut optional_written = 0usize;
        let mut optional_skipped = 0usize;
        for (name, prop) in properties {
            let is_required = required.iter().any(|required| *required == name);
            if !is_required {
                if let Some(max) = opts.max_fields
                    && optional_written >= max
                {
                    optional_skipped += 1;
                    continue;
                }
                optional_written += 1;
            }
            lines.push(render_field(name, prop, &self.full, is_required, opts));
        }
        if optional_skipped > 0 {
            lines.push(format!("…他 {optional_skipped} 件（任意フィールド）"));
        }
        lines.join("\n")
    }

    /// The schema node that carries the fields to render.
    ///
    /// Object mode uses the root; array mode uses the element object behind
    /// `items` (`$ref` resolved, as schemars emits `#/$defs/Row` there).
    fn guide_node(&self) -> Option<&Value> {
        match self.kind {
            RootKind::Object => Some(&self.full),
            RootKind::Array => {
                let items = self.full.get("items")?;
                match items.get("$ref").and_then(Value::as_str) {
                    Some(reference) => resolve_local_ref(&self.full, reference),
                    None => Some(items),
                }
            }
        }
    }

    /// The property names the object at `loc` may hold.
    ///
    /// `loc` is an instance path taken from a validation error: empty (the
    /// root object) or `[i]` (an array element). Local `$ref`s are resolved.
    /// `None` means the location cannot be resolved, so callers must not guess.
    pub(crate) fn properties_at(&self, loc: &Location) -> Option<&serde_json::Map<String, Value>> {
        let mut node = &self.full;
        for segment in loc.iter() {
            let obj = resolve_schema_node(&self.full, node)?;
            node = match segment {
                LocationSegment::Property(name) => {
                    obj.get("properties")?.as_object()?.get(name.as_ref())?
                }
                LocationSegment::Index(_) => obj.get("items")?,
            };
        }
        resolve_schema_node(&self.full, node)?
            .get("properties")?
            .as_object()
    }

    /// The property name that `key` was most likely meant to be.
    ///
    /// A property matching `key` with its trailing `.` removed wins.
    /// Otherwise the property within edit distance 2 is returned (ties go to
    /// the shorter name, then to the lexicographically smaller one). `None`
    /// means there is nothing close enough, so the caller should stay silent.
    pub(crate) fn suggest_property(&self, loc: &Location, key: &str) -> Option<String> {
        let properties = self.properties_at(loc)?;
        let trimmed = key.trim_end_matches('.');
        if trimmed != key && properties.contains_key(trimmed) {
            return Some(trimmed.to_string());
        }

        let mut best: Option<(usize, &str)> = None;
        for name in properties.keys() {
            let distance = edit_distance(key, name);
            if distance > 2 {
                continue;
            }
            let closer = match best {
                None => true,
                Some((best_distance, best_name)) => {
                    (distance, name.chars().count(), name.as_str())
                        < (best_distance, best_name.chars().count(), best_name)
                }
            };
            if closer {
                best = Some((distance, name.as_str()));
            }
        }
        best.map(|(_, name)| name.to_string())
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

/// Decide the root kind from the top-level schema.
fn determine_root_kind(root: &Value) -> Result<RootKind, FillError> {
    let Some(obj) = root.as_object() else {
        return Err(FillError::UnsupportedRootSchema);
    };
    match obj.get("type").and_then(Value::as_str) {
        Some("object") => {
            if obj.get("properties").map(Value::is_object).unwrap_or(false) {
                Ok(RootKind::Object)
            } else {
                Err(FillError::UnsupportedRootSchema)
            }
        }
        Some("array") => Ok(RootKind::Array),
        _ => Err(FillError::UnsupportedRootSchema),
    }
}

/// Recursively check that the schema only uses shapes the filler supports.
///
/// * `root` — the top-level schema, used to resolve local `$ref`s.
/// * `node` — the schema node currently inspected.
/// * `path` — the JSON path of `node` for error messages.
/// * `allow_object` — whether an object may appear at this position.
///   `true` only for the root and for array items.
/// * `allow_array` — whether an array may appear at this position.
///   `true` for the root and for object properties; `false` for array items.
/// * `stack` — `$ref` strings on the current recursion chain, for cycles.
fn check_flat(
    root: &Value,
    node: &Value,
    path: &str,
    allow_object: bool,
    allow_array: bool,
    stack: &mut Vec<String>,
) -> Result<(), FillError> {
    let Some(obj) = node.as_object() else {
        return Err(FillError::UnsupportedSchemaShape {
            path: display_path_for_schema(path),
            reason: "schema must be an object".to_string(),
        });
    };

    // A schema-valued `additionalProperties` is a map type, not a flat record.
    if let Some(additional) = obj.get("additionalProperties")
        && !additional.is_boolean()
    {
        return Err(FillError::UnsupportedSchemaShape {
            path: display_path_for_schema(path),
            reason: "map type (schema-valued additionalProperties) is unsupported".to_string(),
        });
    }

    if obj.get("prefixItems").is_some() {
        return Err(FillError::UnsupportedSchemaShape {
            path: display_path_for_schema(path),
            reason: "prefixItems (tuple) is unsupported".to_string(),
        });
    }

    // Resolve local `$ref`s. Siblings next to `$ref` are ignored; schemars
    // only emits descriptions there.
    if let Some(r) = obj.get("$ref").and_then(Value::as_str) {
        if stack.iter().any(|s| s == r) {
            return Err(FillError::UnsupportedSchemaShape {
                path: display_path_for_schema(path),
                reason: format!("$ref cycle at {r}"),
            });
        }
        let target = resolve_local_ref_checked(root, r, path)?;
        stack.push(r.to_string());
        let result = check_flat(root, target, path, allow_object, allow_array, stack);
        stack.pop();
        return result;
    }

    // `allOf`: every branch must satisfy the same position rules.
    if let Some(branches) = obj.get("allOf").and_then(Value::as_array) {
        for (i, branch) in branches.iter().enumerate() {
            let child = format!("{path}.allOf[{i}]");
            check_flat(root, branch, &child, allow_object, allow_array, stack)?;
        }
    }

    // `anyOf` / `oneOf`: only scalar unions are allowed, and only at field
    // positions. Objects, arrays, and unions at array-item positions are
    // rejected.
    if let Some(branches) = obj
        .get("anyOf")
        .or_else(|| obj.get("oneOf"))
        .and_then(Value::as_array)
    {
        if branches.is_empty() {
            return Err(FillError::UnsupportedSchemaShape {
                path: display_path_for_schema(path),
                reason: "empty anyOf/oneOf".to_string(),
            });
        }
        if allow_object {
            return Err(FillError::UnsupportedSchemaShape {
                path: display_path_for_schema(path),
                reason: "anyOf/oneOf at an object position is unsupported".to_string(),
            });
        }
        for (i, branch) in branches.iter().enumerate() {
            let child = format!("{path}.anyOf[{i}]");
            check_flat(root, branch, &child, false, false, stack)?;
        }
    }

    let has_scalar_union = obj.get("anyOf").or_else(|| obj.get("oneOf")).is_some() && !allow_object;
    let declared = type_names(obj);
    let has_properties = obj.get("properties").and_then(Value::as_object).is_some();
    let has_items = obj.get("items").is_some();

    // `type` may be a string or an array of strings.
    let (is_object, is_array, has_scalar) = match declared.as_deref() {
        Some(types) => {
            let is_object = types.contains(&"object");
            let is_array = types.contains(&"array");
            let has_scalar = types.iter().any(|t| is_scalar_type(t));
            let invalid: Vec<&str> = types
                .iter()
                .copied()
                .filter(|t| !is_scalar_type(t) && *t != "object" && *t != "array")
                .collect();
            if !invalid.is_empty() {
                return Err(FillError::UnsupportedSchemaShape {
                    path: display_path_for_schema(path),
                    reason: format!("unknown type '{}'", invalid.join(", ")),
                });
            }
            (is_object, is_array, has_scalar)
        }
        None => (has_properties, has_items, false),
    };

    if is_object && is_array {
        return Err(FillError::UnsupportedSchemaShape {
            path: display_path_for_schema(path),
            reason: "object/array union is unsupported".to_string(),
        });
    }

    if is_object
        && declared
            .as_deref()
            .map(|types| types.len() != 1)
            .unwrap_or(false)
    {
        return Err(FillError::UnsupportedSchemaShape {
            path: display_path_for_schema(path),
            reason: "object union is unsupported".to_string(),
        });
    }

    if is_array
        && declared
            .as_deref()
            .map(|types| {
                types
                    .iter()
                    .any(|name| is_scalar_type(name) && *name != "null")
            })
            .unwrap_or(false)
    {
        return Err(FillError::UnsupportedSchemaShape {
            path: display_path_for_schema(path),
            reason: "array/scalar union is unsupported".to_string(),
        });
    }

    if is_object {
        if !allow_object {
            return Err(FillError::UnsupportedSchemaShape {
                path: display_path_for_schema(path),
                reason: "nested object is unsupported".to_string(),
            });
        }
        if !has_properties {
            return Err(FillError::UnsupportedSchemaShape {
                path: display_path_for_schema(path),
                reason: "object without properties is unsupported".to_string(),
            });
        }
        if let Some(props) = obj.get("properties").and_then(Value::as_object) {
            for (name, prop) in props {
                let child = if path.is_empty() {
                    format!("properties.{name}")
                } else {
                    format!("{path}.properties.{name}")
                };
                check_flat(root, prop, &child, false, true, stack)?;
            }
        }
    } else if is_array {
        if !allow_array {
            return Err(FillError::UnsupportedSchemaShape {
                path: display_path_for_schema(path),
                reason: "nested array is unsupported".to_string(),
            });
        }
        let Some(items) = obj.get("items") else {
            return Err(FillError::UnsupportedSchemaShape {
                path: display_path_for_schema(path),
                reason: "array without items is unsupported".to_string(),
            });
        };
        if items.is_array() {
            return Err(FillError::UnsupportedSchemaShape {
                path: format!("{}.items", display_path_for_schema(path)),
                reason: "tuple array is unsupported".to_string(),
            });
        }
        // At the root, array items may be objects. Inside an object property,
        // arrays may only contain scalars. Array items are never arrays.
        let child = format!("{path}.items");
        check_flat(root, items, &child, allow_object, false, stack)?;
    } else if has_scalar {
        // Scalar or scalar union. At an object position this is invalid.
        if allow_object {
            return Err(FillError::UnsupportedSchemaShape {
                path: display_path_for_schema(path),
                reason: "scalar schema at an object position".to_string(),
            });
        }
    } else if has_scalar_union && !has_properties && !has_items {
        // A scalar `anyOf` / `oneOf` without `type`; branches were checked
        // above.
    } else {
        return Err(FillError::UnsupportedSchemaShape {
            path: display_path_for_schema(path),
            reason: "schema has no type (would allow objects)".to_string(),
        });
    }

    Ok(())
}

/// The `type` keyword as a list of strings, if present.
fn type_names(obj: &serde_json::Map<String, Value>) -> Option<Vec<&str>> {
    match obj.get("type") {
        Some(Value::String(s)) => Some(vec![s.as_str()]),
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<&str>>(),
        ),
        _ => None,
    }
}

fn is_scalar_type(name: &str) -> bool {
    matches!(name, "string" | "number" | "integer" | "boolean" | "null")
}

/// Resolve a local `$ref` (`#`, `#/...`) against `root`.
///
/// Returns `None` for external references and for fragments that do not
/// exist, so callers that only render text can degrade instead of failing.
fn resolve_local_ref<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let fragment = reference.strip_prefix('#')?;
    root.pointer(fragment)
}

/// Resolve a local `$ref` for schema validation, reporting why it failed.
fn resolve_local_ref_checked<'a>(
    root: &'a Value,
    r: &str,
    path: &str,
) -> Result<&'a Value, FillError> {
    if !r.starts_with('#') {
        return Err(FillError::UnsupportedSchemaShape {
            path: display_path_for_schema(path),
            reason: "external $ref is unsupported".to_string(),
        });
    }
    resolve_local_ref(root, r).ok_or_else(|| FillError::UnsupportedSchemaShape {
        path: display_path_for_schema(path),
        reason: format!("$ref target '{r}' not found"),
    })
}

/// Follow local `$ref`s until a concrete node is reached, so callers can
/// read `properties` / `items` without caring where the object is defined.
fn resolve_schema_node<'a>(root: &'a Value, node: &'a Value) -> Option<&'a Value> {
    let mut current = node;
    for _ in 0..8 {
        match current.get("$ref").and_then(Value::as_str) {
            Some(reference) => current = resolve_local_ref(root, reference)?,
            None => return Some(current),
        }
    }
    None
}

/// Levenshtein distance, counted in characters.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitute = previous[j] + usize::from(ca != cb);
            current[j + 1] = substitute.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// The `required` names of an object node, empty when absent.
fn required_names(node: &Value) -> Vec<&str> {
    node.get("required")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// Render one bullet: `- name: string  (必須)  会社名`.
fn render_field(
    name: &str,
    prop: &Value,
    root: &Value,
    required: bool,
    opts: &GuideOptions,
) -> String {
    let mut line = format!("- {name}: {}", type_label(prop, root, 0));
    if required {
        line.push_str("  (必須)");
    }
    if let Some(description) = prop.get("description").and_then(Value::as_str) {
        let text = one_line(description, opts.max_description_chars);
        if !text.is_empty() {
            line.push_str("  ");
            line.push_str(&text);
        }
    }
    line
}

/// Render the type of a field, with `$ref` resolved and constraints appended.
fn type_label(schema: &Value, root: &Value, depth: usize) -> String {
    // Deep or cyclic references stop here instead of recursing forever.
    if depth > 4 {
        return "any".to_string();
    }
    let Some(obj) = schema.as_object() else {
        return "any".to_string();
    };

    if let Some(reference) = obj.get("$ref").and_then(Value::as_str) {
        return match resolve_local_ref(root, reference) {
            Some(target) => type_label(target, root, depth + 1),
            None => "any".to_string(),
        };
    }
    if let Some(value) = obj.get("const") {
        return value.to_string();
    }
    if let Some(values) = obj.get("enum").and_then(Value::as_array) {
        return enum_label(values, obj);
    }
    if let Some(branches) = obj
        .get("anyOf")
        .or_else(|| obj.get("oneOf"))
        .and_then(Value::as_array)
        && !branches.is_empty()
    {
        let mut labels: Vec<String> = Vec::new();
        for branch in branches {
            let label = type_label(branch, root, depth + 1);
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        return labels.join(" | ");
    }

    let declared = type_names(obj).unwrap_or_default();
    let scalars: Vec<&str> = declared
        .iter()
        .copied()
        .filter(|name| is_scalar_type(name))
        .collect();

    let mut base = if declared.contains(&"array") || obj.get("items").is_some() {
        let inner = obj
            .get("items")
            .map(|items| type_label(items, root, depth + 1))
            .filter(|label| label != "any")
            .unwrap_or_else(|| "object".to_string());
        format!("array<{inner}>")
    } else if declared.contains(&"object") {
        "object".to_string()
    } else if scalars.is_empty() {
        "any".to_string()
    } else {
        scalars.join(" | ")
    };
    // A nullable array keeps its `null` next to the element type.
    if base.starts_with("array<") && !scalars.is_empty() {
        base.push_str(" | ");
        base.push_str(&scalars.join(" | "));
    }

    match constraints_label(obj) {
        Some(constraints) => format!("{base}  ({constraints})"),
        None => base,
    }
}

/// Render up to five `enum` values as `one of "a" | "b"`.
fn enum_label(values: &[Value], obj: &serde_json::Map<String, Value>) -> String {
    let parts: Vec<String> = values
        .iter()
        .take(5)
        .map(|value| value.to_string())
        .collect();
    let mut label = format!("one of {}", parts.join(" | "));
    if values.len() > 5 {
        label.push_str(" | …");
    }
    match constraints_label(obj) {
        Some(constraints) => format!("{label}  ({constraints})"),
        None => label,
    }
}

/// `format` / `minimum` / `maximum` / `maxLength`, in that order.
fn constraints_label(obj: &serde_json::Map<String, Value>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(format) = obj.get("format").and_then(Value::as_str) {
        parts.push(format!("format: {format}"));
    }
    if let Some(minimum) = obj.get("minimum") {
        parts.push(format!("min: {minimum}"));
    }
    if let Some(maximum) = obj.get("maximum") {
        parts.push(format!("max: {maximum}"));
    }
    if let Some(max_length) = obj.get("maxLength") {
        parts.push(format!("maxLength: {max_length}"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

/// Collapse a description to one line and, when `max_chars` is not `0`, cut
/// it (at the last `。` inside the limit when possible) with a `…` marker.
fn one_line(text: &str, max_chars: usize) -> String {
    let mut flat = String::new();
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = !flat.is_empty();
            continue;
        }
        if pending_space {
            flat.push(' ');
            pending_space = false;
        }
        flat.push(ch);
    }

    if max_chars == 0 || flat.chars().count() <= max_chars {
        return flat;
    }
    let head: String = flat.chars().take(max_chars).collect();
    let cut = match head.rfind('。') {
        Some(index) => head[..index + '。'.len_utf8()].trim_end().to_string(),
        None => head.trim_end().to_string(),
    };
    format!("{cut}…")
}

fn display_path_for_schema(path: &str) -> String {
    let trimmed = path.trim_start_matches('.');
    if trimmed.is_empty() {
        "(root)".to_string()
    } else {
        trimmed.to_string()
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

    #[derive(Debug, Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct FlatRow {
        name: String,
        note: Option<String>,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct NestedRow {
        inner: FlatRow,
    }

    fn shape_error(value: Value) -> FillError {
        FillSchema::from_value(value).unwrap_err()
    }

    fn shape_path(err: FillError) -> String {
        match err {
            FillError::UnsupportedSchemaShape { path, .. } => path,
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn root_kind_object_for_flat_struct() {
        let schema = FillSchema::for_type::<FlatRow>().expect("schema");
        assert_eq!(schema.root_kind(), RootKind::Object);
        assert!(!schema.is_array());
    }

    #[test]
    fn root_kind_array_for_vec_struct() {
        let schema = FillSchema::for_type::<Vec<FlatRow>>().expect("schema");
        assert_eq!(schema.root_kind(), RootKind::Array);
        assert!(schema.is_array());
    }

    #[test]
    fn from_value_accepts_array_of_objects() {
        let schema = FillSchema::from_value(json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }
        }))
        .expect("schema");
        assert!(schema.is_array());
    }

    #[test]
    fn from_value_rejects_array_of_scalars() {
        let err = shape_error(json!({
            "type": "array",
            "items": { "type": "string" }
        }));
        assert_eq!(shape_path(err), "items");
    }

    #[test]
    fn from_value_rejects_array_without_items() {
        let err = shape_error(json!({ "type": "array" }));
        assert_eq!(shape_path(err), "(root)");
    }

    #[test]
    fn resolves_local_ref_for_array_items() {
        let schema = FillSchema::from_value(json!({
            "type": "array",
            "items": { "$ref": "#/$defs/Row" },
            "$defs": {
                "Row": {
                    "type": "object",
                    "properties": { "name": { "type": "string" } }
                }
            }
        }))
        .expect("schema");
        assert!(schema.is_array());
    }

    #[test]
    fn rejects_nested_object_property() {
        let err = shape_error(json!({
            "type": "object",
            "properties": {
                "inner": {
                    "type": "object",
                    "properties": { "x": { "type": "string" } }
                }
            }
        }));
        assert_eq!(shape_path(err), "properties.inner");
    }

    #[test]
    fn rejects_array_of_objects_property() {
        let err = shape_error(json!({
            "type": "object",
            "properties": {
                "rows": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": { "x": { "type": "string" } }
                    }
                }
            }
        }));
        assert_eq!(shape_path(err), "properties.rows.items");
    }

    #[test]
    fn rejects_nested_object_in_array_item() {
        let err = shape_error(json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "inner": {
                        "type": "object",
                        "properties": { "x": { "type": "string" } }
                    }
                }
            }
        }));
        assert_eq!(shape_path(err), "items.properties.inner");
    }

    #[test]
    fn rejects_map_property() {
        let err = shape_error(json!({
            "type": "object",
            "properties": {
                "meta": {
                    "type": "object",
                    "additionalProperties": { "type": "string" }
                }
            }
        }));
        assert_eq!(shape_path(err), "properties.meta");
    }

    #[test]
    fn rejects_untyped_property() {
        let err = shape_error(json!({
            "type": "object",
            "properties": { "x": {} }
        }));
        assert_eq!(shape_path(err), "properties.x");
    }

    #[test]
    fn allows_nullable_scalar_and_scalar_array() {
        let schema = FillSchema::from_value(json!({
            "type": "object",
            "properties": {
                "maybe": { "type": ["string", "null"] },
                "tags": { "type": "array", "items": { "type": "string" } },
                "maybe_tags": { "type": ["array", "null"], "items": { "type": "string" } }
            }
        }))
        .expect("schema");
        assert_eq!(schema.root_kind(), RootKind::Object);
    }

    #[test]
    fn allows_scalar_anyof_without_type() {
        let schema = FillSchema::from_value(json!({
            "type": "object",
            "properties": {
                "value": {
                    "anyOf": [
                        { "type": "string" },
                        { "type": "number" }
                    ]
                }
            }
        }))
        .expect("schema");
        assert_eq!(schema.root_kind(), RootKind::Object);
    }

    #[test]
    fn partial_schema_strips_required_in_array_items() {
        let schema = json!({
            "type": "array",
            "items": { "$ref": "#/$defs/Row" },
            "$defs": {
                "Row": {
                    "type": "object",
                    "properties": { "name": { "type": "string" } },
                    "required": ["name"]
                }
            }
        });
        let partial = to_partial_schema(&schema);
        assert!(partial["$defs"]["Row"].get("required").is_none());
    }

    #[test]
    fn for_type_rejects_nested_struct() {
        let err = FillSchema::for_type::<NestedRow>().unwrap_err();
        assert!(matches!(err, FillError::UnsupportedSchemaShape { .. }));
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct GuideCompany {
        /// 会社名
        name: String,
        /// 従業員数
        employees: Option<u32>,
        /// タグ
        tags: Vec<String>,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct GuideRow {
        /// 市場キー
        market_key: String,
        /// 売上シェア
        sales_share: Option<f64>,
    }

    fn guide(schema: &FillSchema) -> String {
        schema.guide_fields(&GuideOptions::default())
    }

    #[test]
    fn guide_fields_marks_required() {
        let schema = FillSchema::for_type::<GuideCompany>().expect("schema");
        let text = guide(&schema);
        assert!(text.contains("- name: string  (必須)"), "{text}");
        assert!(text.contains("- tags: array<string>  (必須)"), "{text}");
        assert!(
            !text.contains("- employees: integer | null  (必須)"),
            "{text}"
        );
    }

    #[test]
    fn guide_fields_renders_union_and_array() {
        let schema = FillSchema::for_type::<GuideCompany>().expect("schema");
        let text = guide(&schema);
        assert!(text.contains("- employees: integer | null"), "{text}");
        assert!(text.contains("- tags: array<string>"), "{text}");
    }

    #[test]
    fn guide_fields_includes_constraints() {
        let schema = FillSchema::from_value(json!({
            "type": "object",
            "properties": {
                "status": { "type": "string", "enum": ["active", "closed"] },
                "age": { "type": "integer", "minimum": 0, "maximum": 120 },
                "code": { "type": "string", "maxLength": 8 }
            },
            "required": ["status"]
        }))
        .expect("schema");
        let text = guide(&schema);
        assert!(text.contains(r#"one of "active" | "closed""#), "{text}");
        assert!(text.contains("min: 0"), "{text}");
        assert!(text.contains("max: 120"), "{text}");
        assert!(text.contains("maxLength: 8"), "{text}");
    }

    #[test]
    fn guide_fields_includes_doc_comment() {
        let schema = FillSchema::for_type::<GuideCompany>().expect("schema");
        let text = guide(&schema);
        let name_line = text
            .lines()
            .find(|line| line.starts_with("- name:"))
            .expect("name line");
        assert_eq!(name_line, "- name: string  (必須)  会社名");
    }

    #[test]
    fn guide_fields_resolves_ref_for_array_items() {
        let schema = FillSchema::for_type::<Vec<GuideRow>>().expect("schema");
        let text = guide(&schema);
        assert!(text.contains("- market_key: string  (必須)"), "{text}");
        assert!(text.contains("- sales_share: number | null"), "{text}");
    }

    #[test]
    fn guide_fields_truncates_description_only_when_configured() {
        let schema = FillSchema::from_value(json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "会社名です。とても長い説明がここに続きます。"
                }
            }
        }))
        .expect("schema");

        let full = guide(&schema);
        assert!(
            full.contains("会社名です。とても長い説明がここに続きます。"),
            "{full}"
        );

        let short = schema.guide_fields(&GuideOptions {
            max_description_chars: 8,
            max_fields: None,
        });
        assert!(short.contains("会社名です。…"), "{short}");
        assert!(!short.contains("とても"), "{short}");
    }

    #[test]
    fn guide_fields_keeps_required_when_max_fields_hits() {
        let schema = FillSchema::from_value(json!({
            "type": "object",
            "properties": {
                "a": { "type": "string" },
                "b": { "type": "string" },
                "c": { "type": "string" },
                "d": { "type": "string" }
            },
            "required": ["a", "b"]
        }))
        .expect("schema");
        let text = schema.guide_fields(&GuideOptions {
            max_description_chars: 0,
            max_fields: Some(1),
        });
        assert!(text.contains("- a:"), "{text}");
        assert!(text.contains("- b:"), "{text}");
        assert!(text.contains("- c:"), "{text}");
        assert!(!text.contains("- d:"), "{text}");
        assert!(text.contains("…他 1 件（任意フィールド）"), "{text}");
    }

    #[test]
    fn guide_fields_never_panics_on_odd_schema() {
        let empty = FillSchema::from_value(json!({
            "type": "object",
            "properties": {}
        }))
        .expect("schema");
        assert_eq!(guide(&empty), "");

        // Unresolvable references and non-object schemas degrade to `any`
        // instead of panicking.
        let root = json!({});
        assert_eq!(
            type_label(&json!({"$ref": "#/$defs/Missing"}), &root, 0),
            "any"
        );
        assert_eq!(type_label(&json!("nonsense"), &root, 0), "any");
        assert_eq!(type_label(&json!({}), &root, 0), "any");
    }

    fn loc() -> Location {
        let mut l = Location::new();
        l = l.join(LocationSegment::Property("market".into()));
        l
    }
}
