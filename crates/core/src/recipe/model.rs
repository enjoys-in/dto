//! Recipe data model — the user-facing "cooking recipe" for data transformation.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A complete transformation recipe.
///
/// This is the user-friendly alternative to raw JSON Schema.
/// Users define `mappings` (source → target) with optional transforms,
/// `constraints` for validation, and `required` fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    /// Human-readable name for this recipe (e.g., "UserSummaryDTO").
    pub name: String,

    /// Semantic version string (e.g., "1.0.0").
    #[serde(default = "default_version")]
    pub version: String,

    /// Optional description of what this recipe produces.
    #[serde(default)]
    pub description: Option<String>,

    /// The field mappings: each maps a source path to a target output key.
    pub mappings: Vec<FieldMapping>,

    /// Optional validation constraints keyed by target field name.
    #[serde(default)]
    pub constraints: HashMap<String, Constraint>,

    /// List of target field names that must be present in the output (non-null).
    #[serde(default)]
    pub required: Vec<String>,

    /// Optional metadata / tags for categorization.
    #[serde(default)]
    pub tags: Vec<String>,
}

fn default_version() -> String {
    "1.0.0".to_string()
}

/// A single source→target field mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldMapping {
    /// Source path using dot notation (e.g., `"profile.address.city"`,
    /// `"orders[0].total"`, `"roles[*].name"`).
    pub source: String,

    /// Target key name in the output object (e.g., `"part_number"`).
    /// Accepts both `target` and `to`.
    #[serde(alias = "to")]
    pub target: String,

    /// Optional transform function or cast to apply after extraction.
    /// Can be a single name (`"trim"`), cast (`"decimal"`), or pipe-separated chain.
    /// Accepts both `transform` and `cast`.
    #[serde(alias = "cast")]
    #[serde(default)]
    pub transform: Option<String>,

    /// Optional unpack strategy (e.g. `"array_explode"`).
    #[serde(default)]
    pub unpack: Option<String>,

    /// Mapping classification: "column", "extension", etc.
    #[serde(default)]
    pub kind: Option<String>,

    /// Default value if the source path resolves to nothing.
    #[serde(default)]
    pub default: Option<serde_json::Value>,

    /// Optional type hint for the output field.
    #[serde(rename = "type")]
    #[serde(default)]
    pub field_type: Option<String>,

    /// If true, source is treated as a literal value, not a path expression.
    #[serde(default)]
    pub literal: bool,

    /// Optional nested recipe for transforming extracted sub-objects.
    #[serde(default)]
    pub nested: Option<Box<Recipe>>,
}

/// Validation constraint for an output field.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Constraint {
    /// Expected type: "string", "number", "boolean", "array", "object", "null"
    #[serde(rename = "type")]
    #[serde(default)]
    pub value_type: Option<String>,

    /// Minimum numeric value (inclusive).
    #[serde(default)]
    pub min: Option<f64>,

    /// Maximum numeric value (inclusive).
    #[serde(default)]
    pub max: Option<f64>,

    /// Minimum string / array length.
    #[serde(default)]
    pub min_length: Option<usize>,

    /// Maximum string / array length.
    #[serde(default)]
    pub max_length: Option<usize>,

    /// Regex pattern for string values. Like JSON Schema it is unanchored (it may
    /// match anywhere), so use `^...$` to match the whole value. Non-string values
    /// are not checked (use `type: "string"` to require a string). Syntax is
    /// `regex-lite`: `\d`, `\w`, `\s` are ASCII-only and Unicode classes such as
    /// `\p{L}` are not supported.
    #[serde(default)]
    pub pattern: Option<String>,

    /// Allowed values (enum).
    #[serde(default)]
    pub allowed: Option<Vec<serde_json::Value>>,

    /// Custom error message when constraint is violated.
    #[serde(default)]
    pub message: Option<String>,

    /// `pattern` compiled on first use, shared across threads.
    #[serde(skip)]
    pub(crate) compiled_pattern: std::sync::OnceLock<Option<regex_lite::Regex>>,
}

/// Constraint `type` values understood by the validator.
pub const CONSTRAINT_TYPES: &[&str] =
    &["string", "number", "integer", "boolean", "bool", "array", "object", "null"];

impl Constraint {
    /// Reject constraint definitions the validator would otherwise ignore silently.
    pub fn check_definition(&self, field: &str) -> Result<(), String> {
        if let Some(ref t) = self.value_type {
            if !CONSTRAINT_TYPES.contains(&t.as_str()) {
                return Err(format!(
                    "constraint on '{field}': unknown type '{t}' (expected one of: {})",
                    CONSTRAINT_TYPES.join(", ")
                ));
            }
        }
        if let Some(ref pattern) = self.pattern {
            if let Err(e) = regex_lite::Regex::new(pattern) {
                return Err(format!("constraint on '{field}': invalid pattern '{pattern}': {e}"));
            }
        }
        if let (Some(min), Some(max)) = (self.min, self.max) {
            if min > max {
                return Err(format!("constraint on '{field}': min {min} is greater than max {max}"));
            }
        }
        if let (Some(min), Some(max)) = (self.min_length, self.max_length) {
            if min > max {
                return Err(format!(
                    "constraint on '{field}': min_length {min} is greater than max_length {max}"
                ));
            }
        }
        Ok(())
    }
}

impl Recipe {
    /// Parse a Recipe from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, RecipeError> {
        serde_json::from_str(json).map_err(RecipeError::Parse)
    }

    /// Parse a Recipe from a `serde_json::Value`.
    pub fn from_value(value: serde_json::Value) -> Result<Self, RecipeError> {
        serde_json::from_value(value).map_err(RecipeError::Parse)
    }

    /// Validate this recipe for internal consistency.
    pub fn validate(&self) -> Result<(), RecipeError> {
        if self.name.is_empty() {
            return Err(RecipeError::Invalid("recipe name cannot be empty".into()));
        }
        if self.mappings.is_empty() {
            return Err(RecipeError::Invalid("recipe must have at least one mapping".into()));
        }

        // Check that required fields reference existing mapping targets.
        let targets: std::collections::HashSet<&str> =
            self.mappings.iter().map(|m| m.target.as_str()).collect();
        for req in &self.required {
            if !targets.contains(req.as_str()) {
                return Err(RecipeError::Invalid(format!(
                    "required field '{}' does not match any mapping target",
                    req
                )));
            }
        }

        // Two mappings with the same target would silently overwrite each other.
        if targets.len() != self.mappings.len() {
            let mut seen = std::collections::HashSet::new();
            if let Some(dup) = self.mappings.iter().find(|m| !seen.insert(m.target.as_str())) {
                return Err(RecipeError::Invalid(format!(
                    "duplicate mapping target '{}'",
                    dup.target
                )));
            }
        }

        // Check that constraint keys reference existing mapping targets.
        for (key, constraint) in &self.constraints {
            if !targets.contains(key.as_str()) {
                return Err(RecipeError::Invalid(format!(
                    "constraint key '{}' does not match any mapping target",
                    key
                )));
            }
            constraint.check_definition(key).map_err(RecipeError::Invalid)?;
        }

        // Nested recipes must be valid themselves and must produce an object.
        for mapping in &self.mappings {
            if let Some(ref nested) = mapping.nested {
                if let Some(ref t) = mapping.field_type {
                    if t != "object" {
                        return Err(RecipeError::Invalid(format!(
                            "mapping '{}' has a nested recipe but type '{t}'; nested recipes produce an object",
                            mapping.target
                        )));
                    }
                }
                nested.validate()?;
                if !nested.constraints.is_empty() {
                    return Err(RecipeError::Invalid(format!(
                        "mapping '{}': constraints are only supported on the top-level recipe",
                        mapping.target
                    )));
                }
            }
        }

        // Validate each mapping's source is non-empty.
        for (i, mapping) in self.mappings.iter().enumerate() {
            if !mapping.literal && mapping.source.is_empty() {
                return Err(RecipeError::Invalid(format!(
                    "mapping[{}] has empty source path",
                    i
                )));
            }
            if mapping.target.is_empty() {
                return Err(RecipeError::Invalid(format!(
                    "mapping[{}] has empty target field",
                    i
                )));
            }
        }

        Ok(())
    }
}

/// Errors related to recipe parsing and validation.
#[derive(Debug, thiserror::Error)]
pub enum RecipeError {
    #[error("failed to parse recipe: {0}")]
    Parse(#[from] serde_json::Error),

    #[error("invalid recipe: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_minimal_recipe() {
        let json = r#"{
            "name": "Test",
            "mappings": [
                { "source": "a.b", "target": "c" }
            ]
        }"#;
        let recipe = Recipe::from_json(json).unwrap();
        assert_eq!(recipe.name, "Test");
        assert_eq!(recipe.version, "1.0.0");
        assert_eq!(recipe.mappings.len(), 1);
        assert_eq!(recipe.mappings[0].source, "a.b");
        assert_eq!(recipe.mappings[0].target, "c");
    }

    #[test]
    fn parse_target_and_cast_syntax() {
        let json = r#"{
            "name": "MedallionTest",
            "mappings": [
                {
                    "source": "reference.price",
                    "target": "standard_cost",
                    "cast": "decimal",
                    "kind": "column"
                },
                {
                    "source": "transactions[*].qty",
                    "target": "quantity",
                    "cast": "decimal",
                    "unpack": "array_explode"
                }
            ]
        }"#;
        let recipe = Recipe::from_json(json).unwrap();
        assert_eq!(recipe.mappings[0].target, "standard_cost");
        assert_eq!(recipe.mappings[0].transform.as_deref(), Some("decimal"));
        assert_eq!(recipe.mappings[0].kind.as_deref(), Some("column"));
        assert_eq!(recipe.mappings[1].target, "quantity");
        assert_eq!(recipe.mappings[1].unpack.as_deref(), Some("array_explode"));
    }

    #[test]
    fn parse_full_recipe() {
        let val = json!({
            "name": "UserSummary",
            "version": "2.0.0",
            "description": "Maps raw user to summary",
            "mappings": [
                { "source": "profile.name", "target": "name" },
                { "source": "profile.age",  "target": "age", "transform": "to_number" },
                { "source": "roles[*].name", "target": "roles" }
            ],
            "constraints": {
                "name": { "type": "string", "max_length": 100 },
                "age":  { "type": "number", "min": 0, "max": 200 }
            },
            "required": ["name"],
            "tags": ["user", "summary"]
        });
        let recipe = Recipe::from_value(val).unwrap();
        assert_eq!(recipe.name, "UserSummary");
        assert_eq!(recipe.version, "2.0.0");
        assert_eq!(recipe.mappings.len(), 3);
        assert!(recipe.constraints.contains_key("name"));
        assert_eq!(recipe.required, vec!["name"]);
        assert_eq!(recipe.tags, vec!["user", "summary"]);
    }

    #[test]
    fn validate_empty_name_fails() {
        let recipe = Recipe {
            name: String::new(),
            version: "1.0.0".into(),
            description: None,
            mappings: vec![FieldMapping {
                source: "a".into(),
                target: "b".into(),
                transform: None,
                unpack: None,
                kind: None,
                default: None,
                field_type: None,
                literal: false,
                nested: None,
            }],
            constraints: Default::default(),
            required: vec![],
            tags: vec![],
        };
        assert!(recipe.validate().is_err());
    }

    #[test]
    fn validate_empty_mappings_fails() {
        let recipe = Recipe {
            name: "Test".into(),
            version: "1.0.0".into(),
            description: None,
            mappings: vec![],
            constraints: Default::default(),
            required: vec![],
            tags: vec![],
        };
        assert!(recipe.validate().is_err());
    }

    #[test]
    fn validate_required_not_in_targets_fails() {
        let recipe = Recipe {
            name: "Test".into(),
            version: "1.0.0".into(),
            description: None,
            mappings: vec![FieldMapping {
                source: "a".into(),
                target: "b".into(),
                transform: None,
                unpack: None,
                kind: None,
                default: None,
                field_type: None,
                literal: false,
                nested: None,
            }],
            constraints: Default::default(),
            required: vec!["nonexistent".into()],
            tags: vec![],
        };
        assert!(recipe.validate().is_err());
    }

    #[test]
    fn parse_mapping_with_default_and_transform() {
        let json = r#"{
            "name": "T",
            "mappings": [
                {
                    "source": "user.name",
                    "target": "displayName",
                    "transform": "trim|uppercase",
                    "default": "ANONYMOUS"
                }
            ]
        }"#;
        let recipe = Recipe::from_json(json).unwrap();
        let m = &recipe.mappings[0];
        assert_eq!(m.transform.as_deref(), Some("trim|uppercase"));
        assert_eq!(m.default, Some(json!("ANONYMOUS")));
    }

    #[test]
    fn parse_literal_mapping() {
        let json = r#"{
            "name": "T",
            "mappings": [
                { "source": "1.0.0", "target": "version", "literal": true }
            ]
        }"#;
        let recipe = Recipe::from_json(json).unwrap();
        assert!(recipe.mappings[0].literal);
    }
}
