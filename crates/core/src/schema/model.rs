//! Schema model types representing a DTO transformation schema.
//!
//! The schema is modelled after JSON Schema with custom `x-from` and
//! `x-transform` extension annotations that describe how to map fields from a
//! source document into the target DTO shape.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::error::SchemaError;

/// Represents a DTO transformation schema.
///
/// Modeled after JSON Schema with `x-from` extension for source-path mapping
/// and `x-transform` for optional post-extraction transforms.
///
/// # Example (JSON)
///
/// ```json
/// {
///   "title": "UserDTO",
///   "type": "object",
///   "properties": {
///     "name": { "type": "string", "x-from": "user.profile.name" },
///     "age":  { "type": "number", "x-from": "user.age", "default": 0 }
///   },
///   "required": ["name"]
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DtoSchema {
    /// Schema title (optional, informational).
    #[serde(default)]
    pub title: Option<String>,

    /// The type of the root schema node: `"object"`, `"array"`, `"string"`,
    /// `"number"`, `"boolean"`, or `"null"`.
    #[serde(rename = "type")]
    pub schema_type: String,

    /// Properties for object types — maps each output field name to its
    /// [`FieldSchema`].
    #[serde(default)]
    pub properties: HashMap<String, FieldSchema>,

    /// For array types, the schema of each item.
    #[serde(default)]
    pub items: Option<Box<FieldSchema>>,

    /// Required field names.
    #[serde(default)]
    pub required: Vec<String>,
}

impl DtoSchema {
    /// Parse a [`DtoSchema`] from a JSON string.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::ParseError`] if `json` is not valid JSON or does
    /// not match the expected schema shape.
    pub fn from_json(json: &str) -> Result<Self, SchemaError> {
        let schema: DtoSchema = serde_json::from_str(json)?;
        schema.validate()?;
        Ok(schema)
    }

    /// Parse a [`DtoSchema`] from an already-parsed [`serde_json::Value`].
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::ParseError`] if the value does not match the
    /// expected schema shape.
    pub fn from_value(value: &serde_json::Value) -> Result<Self, SchemaError> {
        let schema: DtoSchema = serde_json::from_value(value.clone())?;
        schema.validate()?;
        Ok(schema)
    }

    /// Basic semantic validation.
    fn validate(&self) -> Result<(), SchemaError> {
        let valid_types = ["object", "array", "string", "number", "boolean", "null"];
        if !valid_types.contains(&self.schema_type.as_str()) {
            return Err(SchemaError::Invalid(format!(
                "unsupported root schema type: '{}'",
                self.schema_type
            )));
        }

        if self.schema_type == "object" && self.properties.is_empty() && self.required.is_empty() {
            // Allow empty-object schemas — they just produce `{}`.
        }

        if self.schema_type == "array" && self.items.is_none() {
            return Err(SchemaError::Invalid(
                "array schema must have an 'items' definition".into(),
            ));
        }

        Ok(())
    }
}

/// Schema for a single field in the output DTO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldSchema {
    /// The JSON Schema type (`"string"`, `"number"`, `"object"`, …).
    #[serde(rename = "type")]
    pub field_type: String,

    /// Source path expression using dot notation (e.g., `"user.profile.name"`).
    ///
    /// Parsed at transform-time by [`crate::path::tokenizer::parse`] and
    /// resolved by [`crate::path::resolver::resolve`].
    #[serde(rename = "x-from", default)]
    pub from: Option<String>,

    /// Optional transform function name to apply after extraction
    /// (e.g., `"trim"`, `"lowercase"`, `"to_string"`).
    #[serde(rename = "x-transform", default)]
    pub transform: Option<String>,

    /// Default value if source path resolves to `None`.
    #[serde(default)]
    pub default: Option<serde_json::Value>,

    /// Nested properties (for nested object types).
    #[serde(default)]
    pub properties: HashMap<String, FieldSchema>,

    /// For array field types, schema of each item.
    #[serde(default)]
    pub items: Option<Box<FieldSchema>>,

    /// Required fields (for nested objects).
    #[serde(default)]
    pub required: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_object_schema() {
        let json = r#"{
            "title": "UserDTO",
            "type": "object",
            "properties": {
                "name": { "type": "string", "x-from": "user.name" },
                "age":  { "type": "number", "x-from": "user.age", "default": 0 }
            },
            "required": ["name"]
        }"#;

        let schema = DtoSchema::from_json(json).unwrap();
        assert_eq!(schema.title.as_deref(), Some("UserDTO"));
        assert_eq!(schema.schema_type, "object");
        assert_eq!(schema.properties.len(), 2);
        assert_eq!(schema.required, vec!["name"]);

        let name_field = &schema.properties["name"];
        assert_eq!(name_field.field_type, "string");
        assert_eq!(name_field.from.as_deref(), Some("user.name"));
        assert!(name_field.transform.is_none());

        let age_field = &schema.properties["age"];
        assert_eq!(age_field.default, Some(serde_json::json!(0)));
    }

    #[test]
    fn parse_from_value() {
        let val = serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "number", "x-from": "id" }
            }
        });
        let schema = DtoSchema::from_value(&val).unwrap();
        assert_eq!(schema.schema_type, "object");
    }

    #[test]
    fn reject_invalid_root_type() {
        let json = r#"{ "type": "foobar" }"#;
        let err = DtoSchema::from_json(json).unwrap_err();
        assert!(err.to_string().contains("unsupported root schema type"));
    }

    #[test]
    fn reject_array_without_items() {
        let json = r#"{ "type": "array" }"#;
        let err = DtoSchema::from_json(json).unwrap_err();
        assert!(err.to_string().contains("items"));
    }

    #[test]
    fn parse_nested_object_field() {
        let json = r#"{
            "type": "object",
            "properties": {
                "address": {
                    "type": "object",
                    "properties": {
                        "city": { "type": "string", "x-from": "location.city" }
                    },
                    "required": ["city"]
                }
            }
        }"#;

        let schema = DtoSchema::from_json(json).unwrap();
        let addr = &schema.properties["address"];
        assert_eq!(addr.field_type, "object");
        assert_eq!(addr.properties.len(), 1);
        assert_eq!(addr.required, vec!["city"]);
    }

    #[test]
    fn parse_array_items_schema() {
        let json = r#"{
            "type": "object",
            "properties": {
                "tags": {
                    "type": "array",
                    "x-from": "metadata.tags",
                    "items": { "type": "string" }
                }
            }
        }"#;

        let schema = DtoSchema::from_json(json).unwrap();
        let tags = &schema.properties["tags"];
        assert_eq!(tags.field_type, "array");
        assert!(tags.items.is_some());
    }

    #[test]
    fn parse_transform_annotation() {
        let json = r#"{
            "type": "object",
            "properties": {
                "email": {
                    "type": "string",
                    "x-from": "user.email",
                    "x-transform": "lowercase"
                }
            }
        }"#;

        let schema = DtoSchema::from_json(json).unwrap();
        let email = &schema.properties["email"];
        assert_eq!(email.transform.as_deref(), Some("lowercase"));
    }
}
