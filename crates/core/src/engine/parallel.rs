//! Parallel batch transformation using Rayon.
//!
//! Each element of a batch is transformed on the Rayon thread pool against a
//! [`CompiledObject`] plan, so paths and transform chains are parsed once per
//! batch rather than once per field per record.

use rayon::prelude::*;
use serde_json::Value;

use super::compiled::CompiledObject;
use crate::schema::model::DtoSchema;
use crate::schema::transform::TransformError;

/// Transform a batch of JSON objects in parallel.
///
/// Each item in `items` is transformed independently using `schema`.
/// Errors are collected per item; one failing item does not stop the others.
///
/// Returns `(successes, errors)`:
/// - `successes`: Vec of (original_index, transformed_value), in input order
/// - `errors`: Vec of (original_index, error), in input order
///
/// If the schema itself is invalid, `successes` is empty and `errors` holds a
/// single entry at index 0 describing the schema problem.
pub fn transform_batch_parallel(
    schema: &DtoSchema,
    items: &[Value],
) -> (Vec<(usize, Value)>, Vec<(usize, TransformError)>) {
    match CompiledObject::from_schema(schema) {
        Ok(plan) => partition(transform_each(&plan, items)),
        Err(e) => (Vec::new(), vec![(0, e)]),
    }
}

/// Transform a batch, returning every result in original order.
///
/// Returns `Err` with all per-item errors if **any** item fails. (Previously
/// failing items were silently dropped unless every item failed.)
pub fn transform_batch_parallel_values(
    schema: &DtoSchema,
    items: &[Value],
) -> Result<Vec<Value>, Vec<(usize, TransformError)>> {
    let (successes, errors) = transform_batch_parallel(schema, items);
    if errors.is_empty() {
        Ok(successes.into_iter().map(|(_, v)| v).collect())
    } else {
        Err(errors)
    }
}

/// Transform every item in parallel, keeping one result per input in order.
pub(crate) fn transform_each(
    plan: &CompiledObject,
    items: &[Value],
) -> Vec<Result<Value, TransformError>> {
    items.par_iter().map(|item| plan.transform(item)).collect()
}

fn partition(
    results: Vec<Result<Value, TransformError>>,
) -> (Vec<(usize, Value)>, Vec<(usize, TransformError)>) {
    let mut successes = Vec::with_capacity(results.len());
    let mut errors = Vec::new();
    for (idx, result) in results.into_iter().enumerate() {
        match result {
            Ok(v) => successes.push((idx, v)),
            Err(e) => errors.push((idx, e)),
        }
    }
    (successes, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use crate::schema::model::FieldSchema;

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

        let plan = CompiledObject::from_schema(&schema).unwrap();
        let result = plan.transform(&input).unwrap();
        assert_eq!(result["emails"], json!(["a@b.com", "c@d.com", "e@f.com"]));
    }

    #[test]
    fn empty_batch_returns_empty() {
        let schema = make_schema(vec![("x", field("y"))], vec![]);
        let results = transform_batch_parallel_values(&schema, &[]).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn values_variant_fails_when_any_item_fails() {
        // Regression: one bad record among good ones used to be dropped silently.
        let schema = make_schema(vec![("name", field("user.name"))], vec!["name"]);
        let items = vec![
            json!({ "user": { "name": "Alice" } }),
            json!({ "other": "data" }),
            json!({ "user": { "name": "Charlie" } }),
        ];
        let errors = transform_batch_parallel_values(&schema, &items).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, 1);
    }

    #[test]
    fn invalid_schema_is_reported_not_skipped() {
        let schema = make_schema(vec![("x", field_with_transform("y", "nope"))], vec![]);
        let (ok, errors) = transform_batch_parallel(&schema, &[json!({ "y": 1 })]);
        assert!(ok.is_empty());
        assert!(errors[0].1.to_string().contains("nope"));
    }
}
