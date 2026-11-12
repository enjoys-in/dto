//! DTO transformation engine.
//!
//! Given a [`DtoSchema`] and an input JSON [`Value`], the [`DtoTransformer`]
//! produces a new JSON value whose shape matches the schema, with field values
//! extracted from the input using `x-from` path expressions and optionally
//! post-processed by `x-transform` functions.

use serde_json::{Map, Value};

use super::error::SchemaError;
use super::model::{DtoSchema, FieldSchema};
use crate::path::tokenizer::PathError;
use crate::path::{resolver, tokenizer};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during transformation.
#[derive(Debug, thiserror::Error)]
pub enum TransformError {
    /// The schema itself is invalid.
    #[error("schema error: {0}")]
    Schema(String),

    /// A path expression in `x-from` could not be parsed.
    #[error("path parse error: {0}")]
    PathParse(#[from] PathError),

    /// A required field resolved to `None` and no `default` was provided.
    #[error("required field '{field}' is missing and has no default")]
    MissingRequired {
        /// The field name that was missing.
        field: String,
    },

    /// The resolved value's JSON type does not match what the schema declared.
    #[error("type mismatch for field '{field}': expected {expected}, got {actual}")]
    TypeMismatch {
        /// The field name.
        field: String,
        /// The type declared in the schema.
        expected: String,
        /// The actual JSON type of the resolved value.
        actual: String,
    },

    /// An error occurred during a transform function execution.
    #[error("transform function error on field '{field}': {message}")]
    TransformFunction {
        /// The field name.
        field: String,
        /// The error message.
        message: String,
    },

    /// Output failed validation constraints.
    #[error("validation error on item {index}: {errors:?}")]
    Validation {
        /// Index of the item that failed.
        index: usize,
        /// List of validation error messages.
        errors: Vec<String>,
    },
}

impl From<SchemaError> for TransformError {
    fn from(err: SchemaError) -> Self {
        TransformError::Schema(err.to_string())
    }
}

// ---------------------------------------------------------------------------
// Transformer
// ---------------------------------------------------------------------------

/// The main transformation engine.
///
/// Holds a parsed [`DtoSchema`] and can transform one or many input documents
/// into the target DTO shape.
///
/// # Example
///
/// ```rust,ignore
/// let transformer = DtoTransformer::from_json(schema_json)?;
/// let output = transformer.transform(&input)?;
/// ```
pub struct DtoTransformer {
    schema: DtoSchema,
}

impl DtoTransformer {
    /// Create a new transformer from an already-parsed schema.
    pub fn new(schema: DtoSchema) -> Self {
        Self { schema }
    }

    /// Create a new transformer by parsing a JSON schema string.
    ///
    /// # Errors
    ///
    /// Propagates [`SchemaError`] wrapped in [`TransformError::Schema`].
    pub fn from_json(schema_json: &str) -> Result<Self, TransformError> {
        let schema = DtoSchema::from_json(schema_json)?;
        Ok(Self::new(schema))
    }

    /// Transform a single JSON value.
    ///
    /// - If the root schema type is `"object"` and the input is an object,
    ///   transforms it directly.
    /// - If the input is a [`Value::Array`], each element is transformed
    ///   individually and the results are collected into an output array.
    /// - If the root schema type is `"array"`, the input is treated according
    ///   to the `items` sub-schema.
    ///
    /// # Errors
    ///
    /// Returns [`TransformError`] on path-parse failures, missing required
    /// fields, or type mismatches.
    pub fn transform(&self, input: &Value) -> Result<Value, TransformError> {
        match input {
            Value::Array(arr) => {
                let results: Result<Vec<Value>, _> = arr
                    .iter()
                    .map(|item| self.transform_object(item))
                    .collect();
                Ok(Value::Array(results?))
            }
            _ => self.transform_object(input),
        }
    }

    /// Transform a single object according to the schema.
    fn transform_object(&self, input: &Value) -> Result<Value, TransformError> {
        let mut output = Map::new();

        for (field_name, field_schema) in &self.schema.properties {
            let is_required = self.schema.required.contains(field_name);
            match self.transform_field(field_name, field_schema, input)? {
                Some(val) => {
                    output.insert(field_name.clone(), val);
                }
                None if is_required => {
                    return Err(TransformError::MissingRequired {
                        field: field_name.clone(),
                    });
                }
                None => { /* optional, omit */ }
            }
        }

        Ok(Value::Object(output))
    }

    /// Transform a single field according to its [`FieldSchema`].
    ///
    /// Returns `Ok(Some(value))` when a value was produced, `Ok(None)` when the
    /// field should be omitted, or `Err` on failure.
    fn transform_field(
        &self,
        field_name: &str,
        field_schema: &FieldSchema,
        input: &Value,
    ) -> Result<Option<Value>, TransformError> {
        // --- Nested object: recurse into sub-properties ---
        if field_schema.field_type == "object" && !field_schema.properties.is_empty() {
            // Determine the sub-input: if `x-from` is given, resolve it first;
            // otherwise use the full input so nested paths resolve from root.
            let sub_input = if let Some(ref from_expr) = field_schema.from {
                let path = tokenizer::parse(from_expr)?;
                resolver::resolve(input, &path).unwrap_or(Value::Null)
            } else {
                input.clone()
            };

            let mut nested_output = Map::new();
            for (sub_name, sub_schema) in &field_schema.properties {
                let is_required = field_schema.required.contains(sub_name);
                match self.transform_field(sub_name, sub_schema, &sub_input)? {
                    Some(v) => {
                        nested_output.insert(sub_name.clone(), v);
                    }
                    None if is_required => {
                        return Err(TransformError::MissingRequired {
                            field: format!("{field_name}.{sub_name}"),
                        });
                    }
                    None => {}
                }
            }
            return Ok(Some(Value::Object(nested_output)));
        }

        // --- Resolve `x-from` path (and support inline pipe chaining) ---
        let (path_str, inline_transforms) = if let Some(ref from_expr) = field_schema.from {
            if from_expr.contains('|') {
                let mut parts = from_expr.split('|');
                let path = parts.next().unwrap_or("").trim().to_string();
                let tr: Vec<&str> = parts.map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
                (path, if tr.is_empty() { None } else { Some(tr.join(" | ")) })
            } else {
                (from_expr.clone(), None)
            }
        } else {
            (String::new(), None)
        };

        let resolved = if !path_str.is_empty() {
            let path = tokenizer::parse(&path_str)?;
            resolver::resolve(input, &path)
        } else {
            None
        };

        // --- Apply default if needed ---
        let value = match resolved {
            Some(v) if !v.is_null() => v,
            _ => {
                if let Some(ref default_val) = field_schema.default {
                    default_val.clone()
                } else {
                    return Ok(None);
                }
            }
        };

        // --- Combine inline transforms with explicit field_schema.transform ---
        let transform_chain = match (&field_schema.transform, inline_transforms) {
            (Some(explicit), Some(inline)) => Some(format!("{} | {}", inline, explicit)),
            (Some(explicit), None) => Some(explicit.clone()),
            (None, Some(inline)) => Some(inline),
            (None, None) => None,
        };

        // --- Apply transform chain if specified ---
        let value = if let Some(ref chain) = transform_chain {
            crate::functions::apply_transform_chain(chain, value)
                .map_err(|e| TransformError::TransformFunction {
                    field: field_name.to_string(),
                    message: e.to_string(),
                })?
        } else {
            value
        };

        Ok(Some(value))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return the JSON "type" label for a [`Value`].
#[allow(dead_code)]
fn json_type_label(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -- helpers --

    fn make_transformer(schema_json: &str) -> DtoTransformer {
        DtoTransformer::from_json(schema_json).expect("schema should parse")
    }

    // 1. Basic flat object transformation
    #[test]
    fn flat_object_transformation() {
        let schema = r#"{
            "type": "object",
            "properties": {
                "name": { "type": "string", "x-from": "user_name" },
                "age":  { "type": "number", "x-from": "user_age" }
            },
            "required": ["name"]
        }"#;

        let input = json!({
            "user_name": "Alice",
            "user_age": 30
        });

        let t = make_transformer(schema);
        let out = t.transform(&input).unwrap();

        assert_eq!(out["name"], "Alice");
        assert_eq!(out["age"], 30);
    }

    // 2. Nested dot-notation paths
    #[test]
    fn nested_dot_notation_paths() {
        let schema = r#"{
            "type": "object",
            "properties": {
                "city": { "type": "string", "x-from": "address.city" },
                "zip":  { "type": "string", "x-from": "address.zip" }
            }
        }"#;

        let input = json!({
            "address": {
                "city": "Portland",
                "zip": "97201"
            }
        });

        let t = make_transformer(schema);
        let out = t.transform(&input).unwrap();

        assert_eq!(out["city"], "Portland");
        assert_eq!(out["zip"], "97201");
    }

    // 3. Array input — batch transformation
    #[test]
    fn array_input_batch_transformation() {
        let schema = r#"{
            "type": "object",
            "properties": {
                "id": { "type": "number", "x-from": "id" }
            }
        }"#;

        let input = json!([
            { "id": 1 },
            { "id": 2 },
            { "id": 3 }
        ]);

        let t = make_transformer(schema);
        let out = t.transform(&input).unwrap();

        assert!(out.is_array());
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["id"], 1);
        assert_eq!(arr[2]["id"], 3);
    }

    // 4. Wildcard extraction (e.g. `roles[*].name`)
    #[test]
    fn wildcard_extraction() {
        let schema = r#"{
            "type": "object",
            "properties": {
                "role_names": { "type": "array", "x-from": "roles[*].name" }
            }
        }"#;

        let input = json!({
            "roles": [
                { "name": "admin" },
                { "name": "editor" },
                { "name": "viewer" }
            ]
        });

        let t = make_transformer(schema);
        let out = t.transform(&input).unwrap();

        let names = out["role_names"].as_array().unwrap();
        assert_eq!(names, &vec![json!("admin"), json!("editor"), json!("viewer")]);
    }

    // 5. Default values
    #[test]
    fn default_values() {
        let schema = r#"{
            "type": "object",
            "properties": {
                "status": { "type": "string", "x-from": "status", "default": "active" }
            }
        }"#;

        // Input has no "status" key.
        let input = json!({});

        let t = make_transformer(schema);
        let out = t.transform(&input).unwrap();

        assert_eq!(out["status"], "active");
    }

    // 6. Missing required field → error
    #[test]
    fn missing_required_field_error() {
        let schema = r#"{
            "type": "object",
            "properties": {
                "id": { "type": "number", "x-from": "id" }
            },
            "required": ["id"]
        }"#;

        let input = json!({});

        let t = make_transformer(schema);
        let err = t.transform(&input).unwrap_err();

        match err {
            TransformError::MissingRequired { ref field } => assert_eq!(field, "id"),
            other => panic!("expected MissingRequired, got: {other}"),
        }
    }

    // 7. Nested object schemas
    #[test]
    fn nested_object_schemas() {
        let schema = r#"{
            "type": "object",
            "properties": {
                "profile": {
                    "type": "object",
                    "x-from": "data",
                    "properties": {
                        "first": { "type": "string", "x-from": "first_name" },
                        "last":  { "type": "string", "x-from": "last_name" }
                    },
                    "required": ["first"]
                }
            }
        }"#;

        let input = json!({
            "data": {
                "first_name": "Bob",
                "last_name": "Smith"
            }
        });

        let t = make_transformer(schema);
        let out = t.transform(&input).unwrap();

        assert_eq!(out["profile"]["first"], "Bob");
        assert_eq!(out["profile"]["last"], "Smith");
    }

    // 8. x-transform application
    #[test]
    fn x_transform_application() {
        let schema = r#"{
            "type": "object",
            "properties": {
                "email": {
                    "type": "string",
                    "x-from": "email",
                    "x-transform": "lowercase"
                },
                "name": {
                    "type": "string",
                    "x-from": "name",
                    "x-transform": "trim"
                }
            }
        }"#;

        let input = json!({
            "email": "ALICE@EXAMPLE.COM",
            "name": "  Bob  "
        });

        let t = make_transformer(schema);
        let out = t.transform(&input).unwrap();

        assert_eq!(out["email"], "alice@example.com");
        assert_eq!(out["name"], "Bob");
    }

    // 9. Optional field omitted from output
    #[test]
    fn optional_field_omitted_when_missing() {
        let schema = r#"{
            "type": "object",
            "properties": {
                "name":   { "type": "string", "x-from": "name" },
                "middle": { "type": "string", "x-from": "middle_name" }
            },
            "required": ["name"]
        }"#;

        let input = json!({ "name": "Eve" });

        let t = make_transformer(schema);
        let out = t.transform(&input).unwrap();

        assert_eq!(out["name"], "Eve");
        assert!(out.get("middle").is_none());
    }

    // 10. Default on null
    #[test]
    fn default_on_null_value() {
        let schema = r#"{
            "type": "object",
            "properties": {
                "level": { "type": "number", "x-from": "level", "default": 1 }
            }
        }"#;

        let input = json!({ "level": null });

        let t = make_transformer(schema);
        let out = t.transform(&input).unwrap();

        assert_eq!(out["level"], 1);
    }

    #[test]
    fn x_transform_chaining() {
        let schema = r#"{
            "type": "object",
            "properties": {
                "clean_text": { "type": "string", "x-from": "raw", "x-transform": "trim | lowercase" }
            }
        }"#;

        let input = json!({ "raw": "   HELLO WORLD   " });
        let t = make_transformer(schema);
        let out = t.transform(&input).unwrap();
        assert_eq!(out["clean_text"], "hello world");
    }

    #[test]
    fn x_from_inline_chaining_with_number_to_string() {
        let schema = r#"{
            "type": "object",
            "properties": {
                "score_str": { "type": "string", "x-from": "user.score | to_string | trim" }
            }
        }"#;

        let input = json!({ "user": { "score": 99.5 } });
        let t = make_transformer(schema);
        let out = t.transform(&input).unwrap();
        assert_eq!(out["score_str"], "99.5");
    }
}
