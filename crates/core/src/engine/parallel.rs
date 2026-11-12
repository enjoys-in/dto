//! Parallel batch transformation engine using Rayon.
//!
//! When transforming an array of objects, each element is processed on a
//! separate Rayon thread-pool worker — fully non-blocking and memory-efficient.

use rayon::prelude::*;
use serde_json::Value;

use crate::path::{tokenizer, resolver};
use crate::functions;
use crate::schema::model::{DtoSchema, FieldSchema};
use crate::schema::transform::TransformError;

/// Transform a batch of JSON objects in parallel.
///
/// Each item in `items` is transformed independently using `schema`.
/// Errors are collected per-item; the first error short-circuits that item only,
/// not the entire batch.
///
/// Returns `(successes, errors)`:
/// - `successes`: Vec of (original_index, transformed_value)
/// - `errors`: Vec of (original_index, error)
pub fn transform_batch_parallel(
    schema: &DtoSchema,
    items: &[Value],
) -> (Vec<(usize, Value)>, Vec<(usize, TransformError)>) {
    let results: Vec<(usize, Result<Value, TransformError>)> = items
        .par_iter()
        .enumerate()
        .map(|(idx, item)| {
            let result = transform_single(schema, item);
            (idx, result)
        })
        .collect();

    let mut successes = Vec::with_capacity(results.len());
    let mut errors = Vec::new();

    for (idx, result) in results {
        match result {
            Ok(val) => successes.push((idx, val)),
            Err(err) => errors.push((idx, err)),
        }
    }

    // Sort successes by index to preserve original order.
    successes.sort_by_key(|(idx, _)| *idx);

    (successes, errors)
}

/// Transform a batch, returning only the successful results in original order.
/// Returns an error if ALL items fail.
pub fn transform_batch_parallel_values(
    schema: &DtoSchema,
    items: &[Value],
) -> Result<Vec<Value>, Vec<(usize, TransformError)>> {
    let (successes, errors) = transform_batch_parallel(schema, items);

    if successes.is_empty() && !errors.is_empty() {
        return Err(errors);
    }

    Ok(successes.into_iter().map(|(_, v)| v).collect())
}

/// Transform a single JSON object using a schema.
/// This is the non-blocking, per-item transform called by the parallel engine.
fn transform_single(
    schema: &DtoSchema,
    input: &Value,
) -> Result<Value, TransformError> {
    let mut output = serde_json::Map::with_capacity(schema.properties.len());

    for (field_name, field_schema) in &schema.properties {
        match transform_field(field_name, field_schema, input)? {
            Some(value) => {
                output.insert(field_name.clone(), value);
            }
            None => {
                // Check if required
                if schema.required.contains(field_name) {
                    return Err(TransformError::MissingRequired {
                        field: field_name.clone(),
                    });
                }
            }
        }
    }

    Ok(Value::Object(output))
}

/// Transform a single field: resolve path → apply transform → return value.
fn transform_field(
    field_name: &str,
    field_schema: &FieldSchema,
    input: &Value,
) -> Result<Option<Value>, TransformError> {
    // Step 1: Resolve the source path (and support inline pipe chaining)
    let (from_path, inline_transforms) = if let Some(ref from_str) = field_schema.from {
        if from_str.contains('|') {
            let mut parts = from_str.split('|');
            let path = parts.next().unwrap_or("").trim().to_string();
            let tr: Vec<&str> = parts.map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
            (path, if tr.is_empty() { None } else { Some(tr.join(" | ")) })
        } else {
            (from_str.clone(), None)
        }
    } else {
        (String::new(), None)
    };

    let resolved = if !from_path.is_empty() {
        let path = tokenizer::parse(&from_path)?;
        resolver::resolve(input, &path)
    } else {
        None
    };

    // Step 2: Apply default if needed.
    let value = match resolved {
        Some(v) if !v.is_null() => Some(v),
        _ => field_schema.default.clone(),
    };

    // Step 3: If still None, return None (caller handles required check).
    let mut value = match value {
        Some(v) => v,
        None => return Ok(None),
    };

    // Step 4: Apply transform chain (combining inline and explicit).
    let transform_chain = match (&field_schema.transform, inline_transforms) {
        (Some(explicit), Some(inline)) => Some(format!("{} | {}", inline, explicit)),
        (Some(explicit), None) => Some(explicit.clone()),
        (None, Some(inline)) => Some(inline),
        (None, None) => None,
    };

    if let Some(ref chain) = transform_chain {
        value = functions::apply_transform_chain(chain, value)
            .map_err(|e| TransformError::TransformFunction {
                field: field_name.to_string(),
                message: e.to_string(),
            })?;
    }

    // Step 5: Handle nested object schemas recursively.
    if field_schema.field_type == "object" && !field_schema.properties.is_empty() {
        if let Value::Object(_) = &value {
            let nested_schema = DtoSchema {
                title: None,
                schema_type: "object".to_string(),
                properties: field_schema.properties.clone(),
                items: None,
                required: field_schema.required.clone(),
            };
            value = transform_single(&nested_schema, &value)?;
        }
    }

    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn make_schema(props: Vec<(&str, FieldSchema)>, required: Vec<&str>) -> DtoSchema {
        DtoSchema {
            title: None,
            schema_type: "object".into(),
            properties: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            items: None,
            required: required.into_iter().map(String::from).collect(),
        }
    }

    fn field(from: &str) -> FieldSchema {
        FieldSchema {
            field_type: "string".into(),
            from: Some(from.into()),
            transform: None,
            default: None,
            properties: HashMap::new(),
            items: None,
            required: vec![],
        }
    }

    fn field_with_default(from: &str, default: Value) -> FieldSchema {
        FieldSchema {
            default: Some(default),
            ..field(from)
        }
    }

    fn field_with_transform(from: &str, transform: &str) -> FieldSchema {
        FieldSchema {
            transform: Some(transform.into()),
            ..field(from)
        }
    }

    #[test]
    fn parallel_batch_simple() {
        let schema = make_schema(
            vec![("name", field("user.name"))],
            vec![],
        );

        let items: Vec<Value> = (0..100)
            .map(|i| json!({ "user": { "name": format!("User{i}") } }))
            .collect();

        let (successes, errors) = transform_batch_parallel(&schema, &items);
        assert_eq!(successes.len(), 100);
        assert!(errors.is_empty());

        // Verify order is preserved
        for (idx, val) in &successes {
            assert_eq!(val["name"], format!("User{idx}"));
        }
    }

    #[test]
    fn parallel_batch_with_errors() {
        let schema = make_schema(
            vec![("name", field("user.name"))],
            vec!["name"],
        );

        let items = vec![
            json!({ "user": { "name": "Alice" } }),
            json!({ "other": "data" }),  // Missing user.name, will fail required
            json!({ "user": { "name": "Charlie" } }),
        ];

        let (successes, errors) = transform_batch_parallel(&schema, &items);
        assert_eq!(successes.len(), 2);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, 1); // index 1 failed
    }

    #[test]
    fn parallel_with_transform_chain() {
        let schema = make_schema(
            vec![("name", field_with_transform("user.name", "trim|uppercase"))],
            vec![],
        );

        let items = vec![
            json!({ "user": { "name": "  alice  " } }),
            json!({ "user": { "name": "  bob  " } }),
        ];

        let results = transform_batch_parallel_values(&schema, &items).unwrap();
        assert_eq!(results[0]["name"], "ALICE");
        assert_eq!(results[1]["name"], "BOB");
    }

    #[test]
    fn parallel_with_defaults() {
        let schema = make_schema(
            vec![("city", field_with_default("address.city", json!("Unknown")))],
            vec![],
        );

        let items = vec![
            json!({ "address": { "city": "NYC" } }),
            json!({ "no_address": true }),
        ];

        let results = transform_batch_parallel_values(&schema, &items).unwrap();
        assert_eq!(results[0]["city"], "NYC");
        assert_eq!(results[1]["city"], "Unknown");
    }

    #[test]
    fn single_transform_with_wildcard() {
        let schema = make_schema(
            vec![("emails", field("contact.emails[*]"))],
            vec![],
        );

        let input = json!({
            "contact": {
                "emails": ["a@b.com", "c@d.com", "e@f.com"]
            }
        });

        let result = transform_single(&schema, &input).unwrap();
        assert_eq!(result["emails"], json!(["a@b.com", "c@d.com", "e@f.com"]));
    }

    #[test]
    fn empty_batch_returns_empty() {
        let schema = make_schema(vec![("x", field("y"))], vec![]);
        let results = transform_batch_parallel_values(&schema, &[]).unwrap();
        assert!(results.is_empty());
    }
}
