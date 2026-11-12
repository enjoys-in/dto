//! Unified API — the main entry point that accepts either Recipe or JSON Schema
//! and provides a single, ergonomic transformation interface.

use std::collections::HashMap;

use serde_json::Value;

use crate::recipe::model::Recipe;
use crate::recipe::parser::{detect_and_parse, SchemaInput};
use crate::recipe::converter::recipe_to_schema;
use crate::schema::model::DtoSchema;
use crate::schema::transform::TransformError;
use super::parallel::{transform_batch_parallel, transform_batch_parallel_values};
use super::validator::{validate_output, ValidationError};

/// The unified DTO transformation engine.
///
/// Accepts either a Recipe or a JSON Schema and transforms data accordingly.
/// Supports single-object and batch (array) processing with Rayon parallelism.
///
/// # Example (Recipe format)
/// ```rust
/// use dto_core::engine::DtoEngine;
/// use serde_json::json;
///
/// let recipe = r#"{
///     "name": "UserSummary",
///     "version": "1.0.0",
///     "mappings": [
///         { "source": "profile.name", "target": "name" },
///         { "source": "profile.address.city", "target": "city" }
///     ],
///     "required": ["name"]
/// }"#;
///
/// let engine = DtoEngine::from_json(recipe).unwrap();
///
/// let input = json!({
///     "profile": {
///         "name": "Jane",
///         "address": { "city": "NYC" }
///     }
/// });
///
/// let output = engine.transform(&input).unwrap();
/// assert_eq!(output["name"], "Jane");
/// assert_eq!(output["city"], "NYC");
/// ```
pub struct DtoEngine {
    /// The compiled schema (both Recipe and JSON Schema are normalized to this).
    schema: Option<DtoSchema>,
    /// Multi-target Medallion Pipeline (for sources / targets schemas).
    pipeline: Option<crate::recipe::pipeline::PipelineSpec>,
    /// Original recipe constraints (if input was a Recipe).
    constraints: HashMap<String, crate::recipe::model::Constraint>,
    /// Whether to validate output against constraints.
    validate: bool,
}

/// Result of a batch transformation, containing successes and per-item errors.
#[derive(Debug)]
pub struct BatchResult {
    /// Successfully transformed items in original order.
    pub items: Vec<Value>,
    /// Items that failed transformation: (original_index, error).
    pub errors: Vec<(usize, TransformError)>,
    /// Validation errors for successfully transformed items (if constraints enabled).
    pub validation_errors: Vec<(usize, Vec<ValidationError>)>,
}

impl DtoEngine {
    /// Create an engine from a JSON string (auto-detects Pipeline vs Recipe vs JSON Schema).
    pub fn from_json(json: &str) -> Result<Self, TransformError> {
        let input = detect_and_parse(json)
            .map_err(|e| TransformError::Schema(e.to_string()))?;

        match input {
            SchemaInput::Recipe(recipe) => Self::from_recipe(recipe),
            SchemaInput::Pipeline(pipeline) => Self::from_pipeline(pipeline),
            SchemaInput::JsonSchema(value) => Self::from_json_schema_value(&value),
        }
    }

    /// Create an engine from a `Recipe`.
    pub fn from_recipe(recipe: Recipe) -> Result<Self, TransformError> {
        let constraints = recipe.constraints.clone();
        let schema = recipe_to_schema(&recipe)
            .map_err(|e| TransformError::Schema(e.to_string()))?;

        Ok(Self {
            schema: Some(schema),
            pipeline: None,
            constraints,
            validate: true,
        })
    }

    /// Create an engine from a multi-target `PipelineSpec`.
    pub fn from_pipeline(pipeline: crate::recipe::pipeline::PipelineSpec) -> Result<Self, TransformError> {
        Ok(Self {
            schema: None,
            pipeline: Some(pipeline),
            constraints: HashMap::new(),
            validate: false,
        })
    }

    /// Create an engine from a JSON Schema `serde_json::Value`.
    pub fn from_json_schema_value(value: &Value) -> Result<Self, TransformError> {
        let schema: DtoSchema = serde_json::from_value(value.clone())
            .map_err(|e| TransformError::Schema(e.to_string()))?;

        Ok(Self {
            schema: Some(schema),
            pipeline: None,
            constraints: HashMap::new(),
            validate: false,
        })
    }

    /// Enable or disable constraint validation on output.
    pub fn set_validation(&mut self, enabled: bool) {
        self.validate = enabled;
    }

    /// Transform a single JSON value.
    ///
    /// - If `input` is an object: transforms it into a single output object (or multi-table object).
    /// - If `input` is an array: transforms each element in parallel.
    pub fn transform(&self, input: &Value) -> Result<Value, TransformError> {
        // Multi-target pipeline path
        if let Some(ref pipeline) = self.pipeline {
            let res = pipeline.execute(input)
                .map_err(TransformError::Schema)?;
            let mut map = serde_json::Map::new();
            for (table, rows) in res {
                map.insert(table, Value::Array(rows));
            }
            return Ok(Value::Object(map));
        }

        let schema = self.schema.as_ref().ok_or_else(|| {
            TransformError::Schema("engine has neither schema nor pipeline configured".to_string())
        })?;

        match input {
            Value::Array(items) => {
                let results = transform_batch_parallel_values(schema, items)
                    .map_err(|errs| errs.into_iter().next().unwrap().1)?;

                // Validate if constraints exist
                if self.validate && !self.constraints.is_empty() {
                    for (idx, item) in results.iter().enumerate() {
                        if let Err(validation_errs) = validate_output(item, &self.constraints) {
                            return Err(TransformError::Validation {
                                index: idx,
                                errors: validation_errs.into_iter().map(|e| e.to_string()).collect(),
                            });
                        }
                    }
                }

                Ok(Value::Array(results))
            }
            Value::Object(_) => {
                let items = [input.clone()];
                let results = transform_batch_parallel_values(schema, &items)
                    .map_err(|errs| errs.into_iter().next().unwrap().1)?;

                let result = results.into_iter().next().unwrap_or(Value::Null);

                if self.validate && !self.constraints.is_empty() {
                    if let Err(validation_errs) = validate_output(&result, &self.constraints) {
                        return Err(TransformError::Validation {
                            index: 0,
                            errors: validation_errs.into_iter().map(|e| e.to_string()).collect(),
                        });
                    }
                }

                Ok(result)
            }
            _ => Err(TransformError::Schema(
                "input must be a JSON object or array of objects".into(),
            )),
        }
    }

    /// Transform a batch with detailed results (successes + per-item errors).
    pub fn transform_batch(&self, items: &[Value]) -> BatchResult {
        if let Some(ref pipeline) = self.pipeline {
            let batch_val = Value::Array(items.to_vec());
            match pipeline.execute(&batch_val) {
                Ok(res) => {
                    let mut map = serde_json::Map::new();
                    for (table, rows) in res {
                        map.insert(table, Value::Array(rows));
                    }
                    return BatchResult {
                        items: vec![Value::Object(map)],
                        errors: Vec::new(),
                        validation_errors: Vec::new(),
                    };
                }
                Err(e) => {
                    return BatchResult {
                        items: Vec::new(),
                        errors: vec![(0, TransformError::Schema(e))],
                        validation_errors: Vec::new(),
                    };
                }
            }
        }

        let schema = match self.schema.as_ref() {
            Some(s) => s,
            None => {
                return BatchResult {
                    items: Vec::new(),
                    errors: vec![(0, TransformError::Schema("no schema configured".into()))],
                    validation_errors: Vec::new(),
                };
            }
        };

        let (successes, errors) = transform_batch_parallel(schema, items);

        let mut validation_errors = Vec::new();

        if self.validate && !self.constraints.is_empty() {
            for (idx, item) in &successes {
                if let Err(errs) = validate_output(item, &self.constraints) {
                    validation_errors.push((*idx, errs));
                }
            }
        }

        BatchResult {
            items: successes.into_iter().map(|(_, v)| v).collect(),
            errors,
            validation_errors,
        }
    }

    /// Get a reference to the compiled schema (if in single-schema mode).
    pub fn schema(&self) -> Option<&DtoSchema> {
        self.schema.as_ref()
    }

    /// Transform an array in micro-batches with bounded peak memory usage.
    ///
    /// When transforming 100k or millions of records, processing in chunks of `chunk_size`
    /// (e.g. 2,000 to 5,000) keeps peak RSS memory bounded while retaining
    /// maximum Rayon multi-threaded throughput.
    pub fn transform_chunks(
        &self,
        items: &[Value],
        chunk_size: usize,
    ) -> Result<Vec<Value>, TransformError> {
        let chunk_size = chunk_size.max(1);
        let mut all_results = Vec::with_capacity(items.len());

        for chunk in items.chunks(chunk_size) {
            let chunk_val = Value::Array(chunk.to_vec());
            let transformed = self.transform(&chunk_val)?;
            if let Value::Array(mut arr) = transformed {
                all_results.append(&mut arr);
            }
        }

        Ok(all_results)
    }

    /// Stream transform an iterator of items through a chunk callback.
    ///
    /// This allows zero-accumulation streaming: each chunk is transformed on the Rayon
    /// thread pool and immediately passed to `on_chunk` (e.g., written to stdout/file)
    /// before dropping the chunk from memory.
    pub fn transform_stream<I, F>(
        &self,
        items: I,
        chunk_size: usize,
        mut on_chunk: F,
    ) -> Result<usize, TransformError>
    where
        I: IntoIterator<Item = Value>,
        F: FnMut(Vec<Value>) -> Result<(), TransformError>,
    {
        let chunk_size = chunk_size.max(1);
        let mut buffer = Vec::with_capacity(chunk_size);
        let mut total_processed = 0;

        for item in items {
            buffer.push(item);
            if buffer.len() >= chunk_size {
                let chunk_items = std::mem::replace(&mut buffer, Vec::with_capacity(chunk_size));
                let chunk_val = Value::Array(chunk_items);
                let transformed = self.transform(&chunk_val)?;
                if let Value::Array(arr) = transformed {
                    total_processed += arr.len();
                    on_chunk(arr)?;
                }
            }
        }

        if !buffer.is_empty() {
            let chunk_val = Value::Array(buffer);
            let transformed = self.transform(&chunk_val)?;
            if let Value::Array(arr) = transformed {
                total_processed += arr.len();
                on_chunk(arr)?;
            }
        }

        Ok(total_processed)
    }
}

// Implement Send + Sync so engine can be shared across threads safely
// (DtoSchema is composed of String, HashMap, Vec — all Send+Sync)
unsafe impl Send for DtoEngine {}
unsafe impl Sync for DtoEngine {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn engine_from_recipe_json() {
        let recipe = r#"{
            "name": "UserSummary",
            "version": "1.0.0",
            "mappings": [
                { "source": "profile.name", "target": "name" },
                { "source": "profile.address.city", "target": "city" }
            ],
            "required": ["name"]
        }"#;

        let engine = DtoEngine::from_json(recipe).unwrap();
        let input = json!({
            "profile": {
                "name": "Alice",
                "address": { "city": "NYC" }
            }
        });

        let output = engine.transform(&input).unwrap();
        assert_eq!(output["name"], "Alice");
        assert_eq!(output["city"], "NYC");
    }

    #[test]
    fn engine_from_json_schema() {
        let schema = r#"{
            "type": "object",
            "properties": {
                "fullName": { "type": "string", "x-from": "user.profile.name" },
                "email":    { "type": "string", "x-from": "user.emails[0]" }
            }
        }"#;

        let engine = DtoEngine::from_json(schema).unwrap();
        let input = json!({
            "user": {
                "profile": { "name": "Bob" },
                "emails": ["bob@example.com"]
            }
        });

        let output = engine.transform(&input).unwrap();
        assert_eq!(output["fullName"], "Bob");
        assert_eq!(output["email"], "bob@example.com");
    }

    #[test]
    fn engine_batch_array_input() {
        let recipe = r#"{
            "name": "Batch",
            "mappings": [
                { "source": "id", "target": "user_id" },
                { "source": "meta.tag", "target": "tag" }
            ]
        }"#;

        let engine = DtoEngine::from_json(recipe).unwrap();
        let input = json!([
            { "id": "u1", "meta": { "tag": "admin" } },
            { "id": "u2", "meta": { "tag": "user" } },
            { "id": "u3", "meta": { "tag": "mod" } }
        ]);

        let output = engine.transform(&input).unwrap();
        let arr = output.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["user_id"], "u1");
        assert_eq!(arr[1]["tag"], "user");
    }

    #[test]
    fn engine_with_constraints_validation() {
        let recipe = r#"{
            "name": "Validated",
            "mappings": [
                { "source": "name", "target": "name" },
                { "source": "age",  "target": "age" }
            ],
            "constraints": {
                "age": { "type": "number", "min": 0, "max": 150 }
            }
        }"#;

        let engine = DtoEngine::from_json(recipe).unwrap();

        // Valid input
        let valid = json!({ "name": "Test", "age": 25 });
        assert!(engine.transform(&valid).is_ok());

        // Invalid: age too high
        let invalid = json!({ "name": "Test", "age": 200 });
        assert!(engine.transform(&invalid).is_err());
    }

    #[test]
    fn engine_with_transform_chain() {
        let recipe = r#"{
            "name": "Transformed",
            "mappings": [
                { "source": "name", "target": "upper_name", "transform": "trim|uppercase" }
            ]
        }"#;

        let engine = DtoEngine::from_json(recipe).unwrap();
        let input = json!({ "name": "  hello world  " });
        let output = engine.transform(&input).unwrap();
        assert_eq!(output["upper_name"], "HELLO WORLD");
    }

    #[test]
    fn engine_with_inline_pipe_and_number_to_string() {
        let recipe = r#"{
            "name": "Pipeline",
            "mappings": [
                { "source": "user.name | trim | lowercase", "target": "clean_name" },
                { "source": "user.age | to_string | trim",  "target": "age_str" },
                { "source": "user.score", "target": "score_num", "transform": "trim | to_number" }
            ]
        }"#;

        let engine = DtoEngine::from_json(recipe).unwrap();
        let input = json!({
            "user": {
                "name": "  JOHN DOE  ",
                "age": 35,
                "score": "  99.9  "
            }
        });

        let output = engine.transform(&input).unwrap();
        assert_eq!(output["clean_name"], "john doe");
        assert_eq!(output["age_str"], "35");
        assert_eq!(output["score_num"], 99.9);
    }

    #[test]
    fn engine_missing_required_field() {
        let recipe = r#"{
            "name": "Strict",
            "mappings": [
                { "source": "id", "target": "user_id" }
            ],
            "required": ["user_id"]
        }"#;

        let engine = DtoEngine::from_json(recipe).unwrap();
        let input = json!({ "no_id_here": true });
        assert!(engine.transform(&input).is_err());
    }

    #[test]
    fn engine_batch_result_detailed() {
        let recipe = r#"{
            "name": "DetailedBatch",
            "mappings": [
                { "source": "val", "target": "out" }
            ],
            "required": ["out"]
        }"#;

        let engine = DtoEngine::from_json(recipe).unwrap();
        let items = vec![
            json!({ "val": "good" }),
            json!({ "nope": true }),
            json!({ "val": "also good" }),
        ];

        let result = engine.transform_batch(&items);
        assert_eq!(result.items.len(), 2);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].0, 1); // item at index 1 failed
    }

    #[test]
    fn engine_default_with_missing_source() {
        let recipe = r#"{
            "name": "Defaults",
            "mappings": [
                { "source": "missing.path", "target": "val", "default": "fallback" }
            ]
        }"#;

        let engine = DtoEngine::from_json(recipe).unwrap();
        let input = json!({ "other": "data" });
        let output = engine.transform(&input).unwrap();
        assert_eq!(output["val"], "fallback");
    }

    #[test]
    fn engine_literal_value() {
        let recipe = r#"{
            "name": "Literal",
            "mappings": [
                { "source": "v2.1.0", "target": "api_version", "literal": true },
                { "source": "name", "target": "name" }
            ]
        }"#;

        let engine = DtoEngine::from_json(recipe).unwrap();
        let input = json!({ "name": "Test" });
        let output = engine.transform(&input).unwrap();
        assert_eq!(output["api_version"], "v2.1.0");
        assert_eq!(output["name"], "Test");
    }

    #[test]
    fn engine_thread_safety() {
        // Verify DtoEngine can be shared across threads
        let recipe = r#"{
            "name": "ThreadSafe",
            "mappings": [{ "source": "x", "target": "y" }]
        }"#;

        let engine = std::sync::Arc::new(DtoEngine::from_json(recipe).unwrap());
        let mut handles = vec![];

        for i in 0..4 {
            let engine = engine.clone();
            handles.push(std::thread::spawn(move || {
                let input = json!({ "x": i });
                engine.transform(&input).unwrap()
            }));
        }

        for (i, handle) in handles.into_iter().enumerate() {
            let result = handle.join().unwrap();
            assert_eq!(result["y"], i);
        }
    }

    #[test]
    fn engine_transform_chunks() {
        let recipe = r#"{
            "name": "ChunkTest",
            "mappings": [{ "source": "n", "target": "val" }]
        }"#;

        let engine = DtoEngine::from_json(recipe).unwrap();
        let items: Vec<Value> = (0..10).map(|i| json!({ "n": i * 10 })).collect();

        let results = engine.transform_chunks(&items, 3).unwrap();
        assert_eq!(results.len(), 10);
        assert_eq!(results[0]["val"], 0);
        assert_eq!(results[9]["val"], 90);
    }

    #[test]
    fn engine_transform_stream() {
        let recipe = r#"{
            "name": "StreamTest",
            "mappings": [{ "source": "n", "target": "val" }]
        }"#;

        let engine = DtoEngine::from_json(recipe).unwrap();
        let items: Vec<Value> = (0..15).map(|i| json!({ "n": i + 1 })).collect();

        let mut collected_chunks = 0;
        let mut total_items = 0;

        let processed = engine
            .transform_stream(items, 4, |chunk| {
                collected_chunks += 1;
                total_items += chunk.len();
                Ok(())
            })
            .unwrap();

        assert_eq!(processed, 15);
        assert_eq!(total_items, 15);
        assert_eq!(collected_chunks, 4); // 4 + 4 + 4 + 3
    }
}
