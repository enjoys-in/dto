//! Converter — transforms a Recipe into the internal DtoSchema representation
//! so that both Recipe and JSON Schema inputs feed the same transformation engine.

use std::collections::HashMap;

use serde_json::Value;

use super::model::{Recipe, FieldMapping, RecipeError};
use crate::schema::model::{DtoSchema, FieldSchema};

/// Convert a `Recipe` into a `DtoSchema` so we can use the same transformation
/// engine regardless of input format.
pub fn recipe_to_schema(recipe: &Recipe) -> Result<DtoSchema, RecipeError> {
    let mut properties = HashMap::new();

    for mapping in &recipe.mappings {
        let field_schema = mapping_to_field_schema(mapping)?;
        properties.insert(mapping.target.clone(), field_schema);
    }

    Ok(DtoSchema {
        title: Some(recipe.name.clone()),
        schema_type: "object".to_string(),
        properties,
        items: None,
        required: recipe.required.clone(),
    })
}

/// Convert a single `FieldMapping` into a `FieldSchema`.
fn mapping_to_field_schema(mapping: &FieldMapping) -> Result<FieldSchema, RecipeError> {
    let (clean_source, inline_transform) = if mapping.literal {
        (mapping.source.clone(), None)
    } else {
        parse_source_and_transforms(&mapping.source)
    };

    let field_type = mapping.field_type.clone().unwrap_or_else(|| {
        if mapping.nested.is_some() {
            // A nested recipe always produces an object; without this the
            // nested mappings were silently ignored unless `"type": "object"` was set.
            "object".to_string()
        } else {
            infer_type_from_source(&clean_source)
        }
    });

    let from = if mapping.literal {
        None // Literal values don't use path resolution
    } else {
        Some(clean_source.clone())
    };

    let default = if mapping.literal {
        // For literals, the source value IS the default/literal value.
        Some(Value::String(mapping.source.clone()))
    } else {
        mapping.default.clone()
    };

    let transform = match (&mapping.transform, inline_transform) {
        (Some(explicit), Some(inline)) => Some(format!("{} | {}", inline, explicit)),
        (Some(explicit), None) => Some(explicit.clone()),
        (None, Some(inline)) => Some(inline),
        (None, None) => None,
    };

    // Handle nested recipes by recursing.
    let (nested_props, nested_required) = if let Some(ref nested_recipe) = mapping.nested {
        let nested_schema = recipe_to_schema(nested_recipe)?;
        (nested_schema.properties, nested_schema.required)
    } else {
        (HashMap::new(), Vec::new())
    };

    Ok(FieldSchema {
        field_type,
        from,
        transform,
        default,
        properties: nested_props,
        items: None,
        required: nested_required,
    })
}

/// Splits an expression like `"user.name | trim | lowercase"` into path `"user.name"`
/// and transform chain `"trim | lowercase"`.
pub fn parse_source_and_transforms(source: &str) -> (String, Option<String>) {
    match first_unquoted_pipe(source) {
        Some(idx) => {
            let path = source[..idx].trim().to_string();
            let tr: Vec<&str> = source[idx + 1..]
                .split('|')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            let transforms = if tr.is_empty() {
                None
            } else {
                Some(tr.join(" | "))
            };
            (path, transforms)
        }
        None => (source.to_string(), None),
    }
}

/// Byte index of the first `|` that is not inside a quoted key such as `["a|b"]`.
fn first_unquoted_pipe(source: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in source.char_indices() {
        match quote {
            Some(_) if escaped => escaped = false,
            Some(_) if c == '\\' => escaped = true,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '|' => return Some(i),
            None => {}
        }
    }
    None
}

/// Heuristic: infer a likely output type from the source path expression.
/// - Paths ending with `[*]` → "array"
/// - Everything else → "string" (safe default)
fn infer_type_from_source(source: &str) -> String {
    if source.ends_with("[*]") {
        "array".to_string()
    } else {
        "string".to_string()
    }
}

/// Convert a Recipe JSON string into a `DtoSchema` in one step.
pub fn recipe_json_to_schema(json: &str) -> Result<DtoSchema, RecipeError> {
    let recipe = Recipe::from_json(json)?;
    recipe.validate()?;
    recipe_to_schema(&recipe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_simple_recipe() {
        let json = r#"{
            "name": "UserSummary",
            "mappings": [
                { "source": "profile.name", "target": "name" },
                { "source": "profile.address.city", "target": "city" },
                { "source": "contact.emails[0]", "target": "email" }
            ],
            "required": ["name"]
        }"#;
        let schema = recipe_json_to_schema(json).unwrap();
        assert_eq!(schema.title.as_deref(), Some("UserSummary"));
        assert_eq!(schema.properties.len(), 3);
        assert_eq!(
            schema.properties["name"].from.as_deref(),
            Some("profile.name")
        );
        assert_eq!(
            schema.properties["city"].from.as_deref(),
            Some("profile.address.city")
        );
        assert_eq!(schema.required, vec!["name"]);
    }

    #[test]
    fn convert_recipe_with_transform() {
        let json = r#"{
            "name": "T",
            "mappings": [
                { "source": "user.name", "target": "name", "transform": "trim|uppercase" }
            ]
        }"#;
        let schema = recipe_json_to_schema(json).unwrap();
        assert_eq!(
            schema.properties["name"].transform.as_deref(),
            Some("trim|uppercase")
        );
    }

    #[test]
    fn convert_recipe_with_defaults() {
        let json = r#"{
            "name": "T",
            "mappings": [
                { "source": "x.y", "target": "val", "default": 42 }
            ]
        }"#;
        let schema = recipe_json_to_schema(json).unwrap();
        assert_eq!(
            schema.properties["val"].default,
            Some(serde_json::json!(42))
        );
    }

    #[test]
    fn convert_literal_mapping() {
        let json = r#"{
            "name": "T",
            "mappings": [
                { "source": "v2.0", "target": "api_version", "literal": true }
            ]
        }"#;
        let schema = recipe_json_to_schema(json).unwrap();
        // Literal: `from` is None, default is the literal string.
        assert!(schema.properties["api_version"].from.is_none());
        assert_eq!(
            schema.properties["api_version"].default,
            Some(serde_json::json!("v2.0"))
        );
    }

    #[test]
    fn convert_recipe_with_inline_pipe_transforms() {
        let json = r#"{
            "name": "Pipeline",
            "mappings": [
                { "source": "user.bio | trim | lowercase", "target": "clean_bio" },
                { "source": "user.age | to_string", "target": "age_str" }
            ]
        }"#;
        let schema = recipe_json_to_schema(json).unwrap();
        assert_eq!(schema.properties["clean_bio"].from.as_deref(), Some("user.bio"));
        assert_eq!(schema.properties["clean_bio"].transform.as_deref(), Some("trim | lowercase"));
        assert_eq!(schema.properties["age_str"].from.as_deref(), Some("user.age"));
        assert_eq!(schema.properties["age_str"].transform.as_deref(), Some("to_string"));
    }

    #[test]
    fn convert_wildcard_infers_array_type() {
        let json = r#"{
            "name": "T",
            "mappings": [
                { "source": "items[*]", "target": "all_items" }
            ]
        }"#;
        let schema = recipe_json_to_schema(json).unwrap();
        assert_eq!(schema.properties["all_items"].field_type, "array");
    }
}
