//! Unified API — the main entry point that accepts a Recipe, a JSON Schema or a
//! Medallion pipeline and provides a single, ergonomic transformation interface.

use std::collections::HashMap;

use rayon::prelude::*;
use serde_json::Value;

use super::compiled::CompiledObject;
use super::memory::{estimate_heap_bytes, AutoChunker, StreamStats};
use super::validator::{validate_output, ValidationError};
use crate::recipe::converter::recipe_to_schema;
use crate::recipe::model::{Constraint, Recipe};
use crate::recipe::parser::{detect_and_parse, SchemaInput};
use crate::recipe::pipeline::{CompiledPipeline, PipelineSpec};
use crate::schema::model::DtoSchema;
use crate::schema::transform::{json_type_name, TransformError};

const PIPELINE_NOT_CHUNKABLE: &str = "chunked/streaming transforms are not supported for \
    pipelines: deduplication needs the whole input, so call transform() on the full input";

/// The unified DTO transformation engine.
///
/// Accepts a Recipe, a JSON Schema or a multi-target pipeline, compiles it once
/// (paths, transform chains, required fields — configuration errors surface
/// here), and transforms data accordingly. Arrays are processed in parallel
/// with Rayon.
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
    mode: Mode,
    /// Original recipe constraints (if input was a Recipe).
    constraints: HashMap<String, Constraint>,
    /// Whether to validate output against constraints.
    validate: bool,
}

enum Mode {
    /// Recipe or JSON Schema, normalized to a schema and compiled.
    Object { schema: DtoSchema, plan: CompiledObject },
    /// Multi-target Medallion pipeline.
    Pipeline(CompiledPipeline),
}

/// Result of a batch transformation, containing successes and per-item errors.
#[derive(Debug)]
pub struct BatchResult {
    /// Successfully transformed items in original order.
    pub items: Vec<Value>,
    /// Original input index of each entry in `items` (they differ once an item fails).
    pub indices: Vec<usize>,
    /// Items that failed transformation: (original_index, error).
    pub errors: Vec<(usize, TransformError)>,
    /// Validation errors for successfully transformed items: (original_index, errors).
    /// These items are still present in `items`.
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
        recipe.validate().map_err(|e| TransformError::Schema(e.to_string()))?;
        let schema = recipe_to_schema(&recipe)
            .map_err(|e| TransformError::Schema(e.to_string()))?;
        let plan = CompiledObject::from_schema(&schema)?;

        Ok(Self {
            mode: Mode::Object { schema, plan },
            constraints: recipe.constraints,
            validate: true,
        })
    }

    /// Create an engine from a multi-target `PipelineSpec`.
    pub fn from_pipeline(pipeline: PipelineSpec) -> Result<Self, TransformError> {
        let compiled = pipeline.compile().map_err(TransformError::Schema)?;
        Ok(Self {
            mode: Mode::Pipeline(compiled),
            constraints: HashMap::new(),
            validate: false,
        })
    }

    /// Create an engine from a JSON Schema `serde_json::Value`.
    pub fn from_json_schema_value(value: &Value) -> Result<Self, TransformError> {
        let schema = DtoSchema::from_value(value)?;
        let plan = CompiledObject::from_schema(&schema)?;

        Ok(Self {
            mode: Mode::Object { schema, plan },
            constraints: HashMap::new(),
            validate: false,
        })
    }

    /// Enable or disable constraint validation on output.
    pub fn set_validation(&mut self, enabled: bool) {
        self.validate = enabled;
    }

    /// Whether this engine runs a multi-target pipeline (output is `{table: rows}`).
    pub fn is_pipeline(&self) -> bool {
        matches!(self.mode, Mode::Pipeline(_))
    }

    /// Transform a single JSON value.
    ///
    /// - Object input: transformed into a single output object (or `{table: rows}` for pipelines).
    /// - Array input: every element is transformed in parallel. If **any** element
    ///   fails (missing required field, bad cast, constraint violation, non-object
    ///   element) the whole call fails with an error naming that element's index.
    ///   Use [`DtoEngine::transform_batch`] to keep the successes instead.
    pub fn transform(&self, input: &Value) -> Result<Value, TransformError> {
        let plan = match &self.mode {
            Mode::Pipeline(pipeline) => {
                let tables = pipeline.execute(input).map_err(TransformError::Pipeline)?;
                return Ok(tables_to_value(tables));
            }
            Mode::Object { plan, .. } => plan,
        };

        match input {
            Value::Array(items) => self.transform_items(items, 0).map(Value::Array),
            Value::Object(_) => {
                let output = plan.transform(input)?;
                self.check_constraints(&output, 0)?;
                Ok(output)
            }
            other => Err(TransformError::Schema(format!(
                "input must be a JSON object or array of objects, got {}",
                json_type_name(other)
            ))),
        }
    }

    /// Transform a slice of records in parallel, failing on the first bad record.
    ///
    /// `base_index` is added to item positions in error messages, so callers that
    /// feed a large input in pieces can report positions in the full input.
    /// Not available for pipelines (see [`DtoEngine::is_pipeline`]).
    pub fn transform_items(
        &self,
        items: &[Value],
        base_index: usize,
    ) -> Result<Vec<Value>, TransformError> {
        let plan = self.object_plan()?;
        let results: Vec<Result<Value, TransformError>> = items
            .par_iter()
            .enumerate()
            .map(|(i, item)| {
                let index = base_index + i;
                let output = plan.transform(item).map_err(|e| e.at_item(index))?;
                self.check_constraints(&output, index)?;
                Ok(output)
            })
            .collect();
        // Collecting in order returns the error of the lowest failing index.
        results.into_iter().collect()
    }

    /// Transform a batch with detailed results (successes + per-item errors).
    pub fn transform_batch(&self, items: &[Value]) -> BatchResult {
        let plan = match &self.mode {
            Mode::Pipeline(pipeline) => {
                return match pipeline.execute_items(items, true) {
                    Ok(tables) => BatchResult {
                        items: vec![tables_to_value(tables)],
                        indices: vec![0],
                        errors: Vec::new(),
                        validation_errors: Vec::new(),
                    },
                    Err(e) => BatchResult {
                        items: Vec::new(),
                        indices: Vec::new(),
                        errors: vec![(0, TransformError::Pipeline(e))],
                        validation_errors: Vec::new(),
                    },
                };
            }
            Mode::Object { plan, .. } => plan,
        };

        /// Transformed record plus its constraint violations (if any).
        type Outcome = Result<(Value, Option<Vec<ValidationError>>), TransformError>;

        let check = self.validate && !self.constraints.is_empty();
        let results: Vec<Outcome> = items
            .par_iter()
            .map(|item| {
                let output = plan.transform(item)?;
                let invalid = if check {
                    validate_output(&output, &self.constraints).err()
                } else {
                    None
                };
                Ok((output, invalid))
            })
            .collect();

        let mut batch = BatchResult {
            items: Vec::with_capacity(items.len()),
            indices: Vec::with_capacity(items.len()),
            errors: Vec::new(),
            validation_errors: Vec::new(),
        };
        for (idx, result) in results.into_iter().enumerate() {
            match result {
                Ok((output, invalid)) => {
                    if let Some(errs) = invalid {
                        batch.validation_errors.push((idx, errs));
                    }
                    batch.items.push(output);
                    batch.indices.push(idx);
                }
                Err(e) => batch.errors.push((idx, e)),
            }
        }
        batch
    }

    /// Get a reference to the compiled schema (if in single-schema mode).
    pub fn schema(&self) -> Option<&DtoSchema> {
        match &self.mode {
            Mode::Object { schema, .. } => Some(schema),
            Mode::Pipeline(_) => None,
        }
    }

    /// Transform an array `chunk_size` records at a time.
    ///
    /// All output is accumulated and returned, so this does **not** bound memory;
    /// use [`DtoEngine::transform_stream`] for that. Error positions refer to
    /// `items`. Not available for pipelines.
    pub fn transform_chunks(
        &self,
        items: &[Value],
        chunk_size: usize,
    ) -> Result<Vec<Value>, TransformError> {
        self.object_plan()?;
        let chunk_size = chunk_size.max(1);
        let mut all_results = Vec::with_capacity(items.len());

        for (n, chunk) in items.chunks(chunk_size).enumerate() {
            all_results.extend(self.transform_items(chunk, n * chunk_size)?);
        }

        Ok(all_results)
    }

    /// Stream transform an iterator of items through a chunk callback.
    ///
    /// At most `chunk_size` input records and their outputs are held at once:
    /// each chunk is transformed on the Rayon pool and handed to `on_chunk`
    /// (e.g. written to stdout/file) before the next one is read. Error positions
    /// refer to the whole stream. Not available for pipelines.
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
        self.object_plan()?;
        let chunk_size = chunk_size.max(1);
        let mut buffer = Vec::with_capacity(chunk_size);
        let mut total_processed = 0;

        for item in items {
            buffer.push(item);
            if buffer.len() >= chunk_size {
                let transformed = self.transform_items(&buffer, total_processed)?;
                total_processed += buffer.len();
                buffer.clear();
                on_chunk(transformed)?;
            }
        }

        if !buffer.is_empty() {
            let transformed = self.transform_items(&buffer, total_processed)?;
            total_processed += buffer.len();
            on_chunk(transformed)?;
        }

        Ok(total_processed)
    }

    /// Stream transform with **memory-capped (auto) batching**.
    ///
    /// Instead of a fixed record count, each record's in-memory size is
    /// estimated as it arrives ([`estimate_heap_bytes`]) and a batch is
    /// transformed as soon as its inputs plus their (learned) output size would
    /// reach `max_batch_bytes`. Narrow rows give large batches, wide rows
    /// small ones, and the memory held for records stays near the cap either
    /// way. A single record larger than the cap is processed on its own.
    ///
    /// Returns statistics about the batches used. Not available for pipelines.
    pub fn transform_stream_auto<I, F>(
        &self,
        items: I,
        max_batch_bytes: usize,
        mut on_chunk: F,
    ) -> Result<StreamStats, TransformError>
    where
        I: IntoIterator<Item = Value>,
        F: FnMut(Vec<Value>) -> Result<(), TransformError>,
    {
        self.object_plan()?;
        let mut chunker = AutoChunker::new(max_batch_bytes);
        let mut buffer = Vec::new();
        let mut processed = 0;

        for item in items {
            let bytes = estimate_heap_bytes(&item);
            buffer.push(item);
            if chunker.push(bytes) {
                processed = self.run_auto_batch(&mut buffer, processed, &mut chunker, &mut on_chunk)?;
            }
        }
        if !buffer.is_empty() {
            self.run_auto_batch(&mut buffer, processed, &mut chunker, &mut on_chunk)?;
        }
        Ok(chunker.stats().clone())
    }

    /// Transform one auto-sized batch, teach the chunker its output size, and
    /// hand the outputs on. Inputs are dropped before `on_chunk` runs.
    /// Returns the number of records processed so far.
    pub fn run_auto_batch<F>(
        &self,
        buffer: &mut Vec<Value>,
        processed: usize,
        chunker: &mut AutoChunker,
        on_chunk: &mut F,
    ) -> Result<usize, TransformError>
    where
        F: FnMut(Vec<Value>) -> Result<(), TransformError>,
    {
        let outputs = self.transform_items(buffer, processed)?;
        let count = buffer.len();
        buffer.clear();
        chunker.finish_batch(outputs.iter().map(estimate_heap_bytes).sum());
        on_chunk(outputs)?;
        Ok(processed + count)
    }

    fn object_plan(&self) -> Result<&CompiledObject, TransformError> {
        match &self.mode {
            Mode::Object { plan, .. } => Ok(plan),
            Mode::Pipeline(_) => Err(TransformError::Schema(PIPELINE_NOT_CHUNKABLE.into())),
        }
    }

    fn check_constraints(&self, output: &Value, index: usize) -> Result<(), TransformError> {
        if !self.validate || self.constraints.is_empty() {
            return Ok(());
        }
        validate_output(output, &self.constraints).map_err(|errs| {
            let mut errors: Vec<String> = errs.iter().map(ToString::to_string).collect();
            errors.sort();
            TransformError::Validation { index, errors }
        })
    }
}

fn tables_to_value(tables: HashMap<String, Vec<Value>>) -> Value {
    Value::Object(
        tables
            .into_iter()
            .map(|(table, rows)| (table, Value::Array(rows)))
            .collect(),
    )
}

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

    // ── Regression tests for previously silent failures ─────────────────

    fn engine(json: &str) -> DtoEngine {
        DtoEngine::from_json(json).unwrap()
    }

    fn load_error(json: &str) -> String {
        match DtoEngine::from_json(json) {
            Ok(_) => panic!("expected a load-time error for {json}"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn array_with_one_bad_record_fails_and_names_it() {
        let e = engine(r#"{"name":"T","mappings":[{"source":"id","target":"id"}],"required":["id"]}"#);
        let err = e.transform(&json!([{ "id": 1 }, { "nope": 2 }, { "id": 3 }])).unwrap_err();
        assert!(matches!(err, TransformError::Item { index: 1, .. }), "got {err}");
        assert!(err.to_string().starts_with("item 1: required field 'id'"));
    }

    #[test]
    fn validation_error_reports_original_index() {
        let e = engine(
            r#"{"name":"T","mappings":[{"source":"id","target":"id"},{"source":"age","target":"age"}],
                "constraints":{"age":{"type":"number","max":150}}}"#,
        );
        let err = e
            .transform(&json!([{ "id": 1, "age": 5 }, { "id": 2, "age": 7 }, { "id": 3, "age": 999 }]))
            .unwrap_err();
        assert!(matches!(err, TransformError::Validation { index: 2, .. }), "got {err}");
        assert_eq!(err.to_string(), "validation error on item 2: field 'age': value 999 exceeds maximum 150");
    }

    #[test]
    fn lowest_failing_index_is_reported_deterministically() {
        let e = engine(r#"{"name":"T","mappings":[{"source":"id","target":"id"}],"required":["id"]}"#);
        let mut items: Vec<Value> = (0..5000).map(|i| json!({ "id": i })).collect();
        items[4000] = json!({});
        items[1234] = json!({});
        for _ in 0..5 {
            let err = e.transform(&Value::Array(items.clone())).unwrap_err();
            assert!(matches!(err, TransformError::Item { index: 1234, .. }));
        }
    }

    #[test]
    fn non_object_records_are_rejected() {
        let e = engine(r#"{"name":"T","mappings":[{"source":"x","target":"y"}]}"#);
        let err = e.transform(&json!([{ "x": 1 }, 2])).unwrap_err();
        assert_eq!(err.to_string(), "item 1: expected a JSON object, got number");
        assert!(e.transform(&json!("str")).is_err());
    }

    #[test]
    fn chunked_and_streamed_errors_use_positions_in_the_full_input() {
        let e = engine(r#"{"name":"T","mappings":[{"source":"id","target":"id"}],"required":["id"]}"#);
        let mut items: Vec<Value> = (0..10).map(|i| json!({ "id": i })).collect();
        items[7] = json!({});

        let err = e.transform_chunks(&items, 3).unwrap_err();
        assert!(matches!(err, TransformError::Item { index: 7, .. }), "got {err}");

        let err = e.transform_stream(items, 3, |_| Ok(())).unwrap_err();
        assert!(matches!(err, TransformError::Item { index: 7, .. }), "got {err}");
    }

    #[test]
    fn batch_result_keeps_original_indices() {
        let e = engine(
            r#"{"name":"T","mappings":[{"source":"id","target":"id"},{"source":"age","target":"age"}],
                "required":["id"],"constraints":{"age":{"max":10}}}"#,
        );
        let r = e.transform_batch(&[json!({ "age": 1 }), json!({ "id": 2, "age": 99 })]);
        assert_eq!(r.indices, vec![1]);
        assert_eq!(r.errors[0].0, 0);
        assert_eq!(r.validation_errors[0].0, 1);
    }

    #[test]
    fn configuration_mistakes_fail_at_load_time() {
        assert!(load_error(r#"{"name":"T","mappings":[{"source":"x","target":"y","transform":"uppercsae"}]}"#)
            .contains("uppercsae"));
        assert!(load_error(r#"{"name":"T","mappings":[{"source":"a..b","target":"y"}]}"#).contains("a..b"));
        assert!(load_error(r#"{"name":"T","mappings":[{"source":"a","target":"y"},{"source":"b","target":"y"}]}"#)
            .contains("duplicate mapping target 'y'"));
        assert!(load_error(
            r#"{"type":"object","properties":{"a":{"type":"string","x-from":"a"}},"required":["zzz"]}"#
        )
        .contains("zzz"));
        assert!(load_error(
            r#"{"name":"T","mappings":[{"source":"a","target":"a"}],"constraints":{"a":{"type":"nubmer"}}}"#
        )
        .contains("unknown type 'nubmer'"));
        assert!(load_error(
            r#"{"name":"T","mappings":[{"source":"a","target":"a"}],"constraints":{"a":{"pattern":"^(x$"}}}"#
        )
        .contains("invalid pattern '^(x$'"));
    }

    #[test]
    fn pattern_constraint_is_enforced() {
        let e = engine(
            r#"{"name":"T","mappings":[{"source":"po","target":"po"}],
                "constraints":{"po":{"type":"string","pattern":"^PO-\\d{3}$"}}}"#,
        );
        assert_eq!(e.transform(&json!({ "po": "PO-123" })).unwrap(), json!({ "po": "PO-123" }));
        let err = e.transform(&json!([{ "po": "PO-123" }, { "po": "PO-12x" }])).unwrap_err();
        assert_eq!(
            err.to_string(),
            r"validation error on item 1: field 'po': value does not match pattern '^PO-\d{3}$'"
        );
    }

    #[test]
    fn quoted_and_hyphenated_keys_in_recipes() {
        let e = engine(
            r#"{"name":"T","mappings":[
                {"source":"headers.content-type | uppercase","target":"ct"},
                {"source":"user[\"first name\"] | trim","target":"first"},
                {"source":"meta['a|b']","target":"ab"}]}"#,
        );
        let out = e
            .transform(&json!({
                "headers": { "content-type": "json" },
                "user": { "first name": " Ada " },
                "meta": { "a|b": 1 }
            }))
            .unwrap();
        assert_eq!(out, json!({ "ct": "JSON", "first": "Ada", "ab": 1 }));
    }

    #[test]
    fn nested_recipe_without_explicit_type_is_applied() {
        let e = engine(
            r#"{"name":"T","mappings":[{"source":"user","target":"u",
                 "nested":{"name":"N","mappings":[{"source":"name","target":"n"}]}}]}"#,
        );
        assert_eq!(e.transform(&json!({ "user": { "name": "Al", "x": 1 } })).unwrap(), json!({ "u": { "n": "Al" } }));
    }

    #[test]
    fn json_schema_nested_object_without_x_from_reads_from_parent() {
        let e = engine(
            r#"{"type":"object","properties":{"addr":{"type":"object","required":["c"],
                 "properties":{"c":{"type":"string","x-from":"city"}}}}}"#,
        );
        assert_eq!(e.transform(&json!({ "city": "X" })).unwrap(), json!({ "addr": { "c": "X" } }));
        let err = e.transform(&json!({})).unwrap_err();
        assert!(err.to_string().contains("'addr.c'"), "got {err}");
    }

    #[test]
    fn date_cast_errors_instead_of_truncating() {
        let e = engine(r#"{"name":"T","mappings":[{"source":"d","target":"d","cast":"date"}]}"#);
        assert_eq!(e.transform(&json!({ "d": "2024-03-15T10:20:30Z" })).unwrap()["d"], "2024-03-15");
        assert!(e.transform(&json!({ "d": "not a date" })).is_err());
    }

    #[test]
    fn streaming_apis_reject_pipelines() {
        let e = engine(r#"{"targets":[{"table":"t","mappings":[{"source":"k","target":"k"}]}]}"#);
        assert!(e.is_pipeline());
        let err = e.transform_stream(vec![json!({ "k": 1 })], 1, |_| Ok(())).unwrap_err();
        assert!(err.to_string().contains("not supported for pipelines"));
        assert!(e.transform_chunks(&[json!({ "k": 1 })], 1).is_err());
    }

    #[test]
    fn engine_is_send_and_sync_without_unsafe_impls() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DtoEngine>();
    }

    #[test]
    fn auto_batching_adapts_to_row_width_and_keeps_order() {
        let e = engine(r#"{"name":"T","mappings":[{"source":"id","target":"id"}],"required":["id"]}"#);
        let budget = 1 << 20; // 1 MB
        let run = |pad: usize| {
            let items = (0..3000).map(move |i| json!({ "id": i, "pad": "x".repeat(pad) }));
            let mut out = Vec::new();
            let stats = e
                .transform_stream_auto(items, budget, |chunk| {
                    out.extend(chunk);
                    Ok(())
                })
                .unwrap();
            (stats, out)
        };

        let (narrow, out) = run(10);
        assert_eq!(out.len(), 3000);
        assert!(out.iter().enumerate().all(|(i, v)| v["id"] == i));
        let (wide, _) = run(2000);
        assert_eq!(narrow.records, 3000);
        assert_eq!(wide.records, 3000);
        assert!(wide.batches > narrow.batches, "wide rows need more, smaller batches");
        assert!(wide.max_batch < narrow.max_batch);
        // Every batch stays within the budget plus one record.
        assert!(narrow.max_batch_bytes <= budget + 4096, "{narrow:?}");
        assert!(wide.max_batch_bytes <= budget + 8192, "{wide:?}");
    }

    #[test]
    fn auto_batching_reports_positions_and_rejects_pipelines() {
        let e = engine(r#"{"name":"T","mappings":[{"source":"id","target":"id"}],"required":["id"]}"#);
        let items = (0..5000).map(|i| if i == 4321 { json!({}) } else { json!({ "id": i }) });
        let err = e.transform_stream_auto(items, 64 * 1024, |_| Ok(())).unwrap_err();
        assert!(matches!(err, TransformError::Item { index: 4321, .. }), "got {err}");

        let p = engine(r#"{"targets":[{"table":"t","mappings":[{"source":"k","target":"k"}]}]}"#);
        assert!(p.transform_stream_auto(vec![json!({})], 1 << 20, |_| Ok(())).is_err());
    }
}
