//! Recipe parser — detects input format (Recipe vs JSON Schema) and parses accordingly.

use serde_json::Value;

use super::model::{Recipe, RecipeError};

/// The supported input formats for defining DTO transformations.
#[derive(Debug, Clone)]
pub enum SchemaInput {
    /// A user-friendly Recipe definition.
    Recipe(Recipe),
    /// A Medallion multi-target Pipeline definition.
    Pipeline(super::pipeline::PipelineSpec),
    /// A standard JSON Schema with `x-from` extensions.
    JsonSchema(Value),
}

/// Auto-detect the input format from a JSON string and parse it.
///
/// Detection heuristic:
/// - If the JSON object has a `"targets"` key → parse as multi-target Pipeline.
/// - If the JSON object has a `"mappings"` key → parse as Recipe.
/// - If the JSON object has a `"properties"` or `"$schema"` key → treat as JSON Schema.
/// - Otherwise → try Recipe first, fall back to JSON Schema.
pub fn detect_and_parse(json: &str) -> Result<SchemaInput, RecipeError> {
    let value: Value = serde_json::from_str(json).map_err(RecipeError::Parse)?;
    detect_and_parse_value(value)
}

/// Auto-detect the input format from a `serde_json::Value`.
pub fn detect_and_parse_value(value: Value) -> Result<SchemaInput, RecipeError> {
    if let Value::Object(ref map) = value {
        // Multi-target pipeline format: has "targets"
        if map.contains_key("targets") {
            let pipeline = super::pipeline::PipelineSpec::from_value(value)?;
            return Ok(SchemaInput::Pipeline(pipeline));
        }

        // Recipe format: must have "mappings"
        if map.contains_key("mappings") {
            let recipe = Recipe::from_value(value)?;
            recipe.validate()?;
            return Ok(SchemaInput::Recipe(recipe));
        }

        // JSON Schema format: has "properties" or "$schema"
        if map.contains_key("properties") || map.contains_key("$schema") {
            return Ok(SchemaInput::JsonSchema(value));
        }

        // Ambiguous: try Recipe, then fall back to JSON Schema.
        if let Ok(recipe) = Recipe::from_value(value.clone()) {
            if recipe.validate().is_ok() {
                return Ok(SchemaInput::Recipe(recipe));
            }
        }
        Ok(SchemaInput::JsonSchema(value))
    } else {
        Err(RecipeError::Invalid(
            "schema input must be a JSON object".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_recipe_format() {
        let json = r#"{
            "name": "Test",
            "mappings": [{ "source": "a", "target": "b" }]
        }"#;
        let result = detect_and_parse(json).unwrap();
        assert!(matches!(result, SchemaInput::Recipe(_)));
    }

    #[test]
    fn detect_json_schema_format() {
        let json = r#"{
            "type": "object",
            "properties": {
                "name": { "type": "string", "x-from": "user.name" }
            }
        }"#;
        let result = detect_and_parse(json).unwrap();
        assert!(matches!(result, SchemaInput::JsonSchema(_)));
    }

    #[test]
    fn detect_schema_with_dollar_schema() {
        let json = r#"{
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {}
        }"#;
        let result = detect_and_parse(json).unwrap();
        assert!(matches!(result, SchemaInput::JsonSchema(_)));
    }

    #[test]
    fn detect_invalid_input_not_object() {
        let json = r#"[1, 2, 3]"#;
        assert!(detect_and_parse(json).is_err());
    }
}
