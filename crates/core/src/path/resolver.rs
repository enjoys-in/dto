//! Resolver for dot-notation paths against [`serde_json::Value`] trees.
//!
//! Given a parsed [`DtoPath`] and a JSON value, the resolver walks the value
//! tree according to the path segments and returns the matched sub-value.
//!
//! # Wildcard Projections
//!
//! When a [`Segment::Wildcard`] is encountered on a JSON array, the remaining
//! path is applied to **every** element. The results are collected into a new
//! [`Value::Array`], skipping elements where the remaining path does not match.

use serde_json::Value;

use super::ast::{DtoPath, Segment};

/// Resolves a parsed path against a JSON value.
///
/// Returns [`None`] if any segment doesn't match the data shape (e.g., a
/// [`Segment::Field`] applied to an array, or an out-of-bounds index).
///
/// For [`Segment::Wildcard`] segments, the remaining path is applied to each
/// element of the array and results are collected into a [`Value::Array`].
/// Elements where the remaining path yields no match are silently skipped.
///
/// # Examples
///
/// ```
/// use serde_json::json;
/// use dto_core::path::{tokenizer::parse, resolver::resolve};
///
/// let data = json!({
///     "users": [
///         { "name": "Alice" },
///         { "name": "Bob" },
///     ]
/// });
///
/// let path = parse("users[*].name").unwrap();
/// let result = resolve(&data, &path);
/// assert_eq!(result, Some(json!(["Alice", "Bob"])));
/// ```
pub fn resolve(value: &Value, path: &DtoPath) -> Option<Value> {
    resolve_segments(value, &path.segments)
}

/// Internal recursive resolver that works on a slice of remaining segments.
fn resolve_segments(value: &Value, segments: &[Segment]) -> Option<Value> {
    if segments.is_empty() {
        return Some(value.clone());
    }

    let (current, rest) = segments.split_first()?;

    match current {
        Segment::Field(name) => {
            let obj = value.as_object()?;
            let child = obj.get(name.as_str())?;
            resolve_segments(child, rest)
        }
        Segment::Index(idx) => {
            let arr = value.as_array()?;
            let resolved_idx = if *idx < 0 {
                let positive = arr.len() as i64 + idx;
                if positive < 0 {
                    return None;
                }
                positive as usize
            } else {
                *idx as usize
            };
            let child = arr.get(resolved_idx)?;
            resolve_segments(child, rest)
        }
        Segment::Wildcard => {
            let arr = value.as_array()?;
            let collected: Vec<Value> = arr
                .iter()
                .filter_map(|element| resolve_segments(element, rest))
                .collect();
            Some(Value::Array(collected))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::tokenizer::parse;
    use serde_json::json;

    // ────────────────────────── Simple field access ──────────────────────────

    #[test]
    fn simple_field() {
        let data = json!({"name": "Alice"});
        let path = parse("name").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!("Alice")));
    }

    #[test]
    fn nested_field() {
        let data = json!({"user": {"profile": {"name": "Bob"}}});
        let path = parse("user.profile.name").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!("Bob")));
    }

    #[test]
    fn field_returns_object() {
        let data = json!({"user": {"age": 30}});
        let path = parse("user").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!({"age": 30})));
    }

    #[test]
    fn field_missing() {
        let data = json!({"name": "Alice"});
        let path = parse("age").unwrap();
        assert_eq!(resolve(&data, &path), None);
    }

    #[test]
    fn field_on_array_returns_none() {
        let data = json!([1, 2, 3]);
        let path = parse("name").unwrap();
        assert_eq!(resolve(&data, &path), None);
    }

    #[test]
    fn field_on_null_returns_none() {
        let data = json!(null);
        let path = parse("name").unwrap();
        assert_eq!(resolve(&data, &path), None);
    }

    #[test]
    fn field_on_scalar_returns_none() {
        let data = json!(42);
        let path = parse("name").unwrap();
        assert_eq!(resolve(&data, &path), None);
    }

    // ────────────────────────── Array indexing ──────────────────────────

    #[test]
    fn index_zero() {
        let data = json!({"items": [10, 20, 30]});
        let path = parse("items[0]").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!(10)));
    }

    #[test]
    fn index_last() {
        let data = json!({"items": [10, 20, 30]});
        let path = parse("items[2]").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!(30)));
    }

    #[test]
    fn index_negative_one() {
        let data = json!({"items": [10, 20, 30]});
        let path = parse("items[-1]").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!(30)));
    }

    #[test]
    fn index_negative_two() {
        let data = json!({"items": [10, 20, 30]});
        let path = parse("items[-2]").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!(20)));
    }

    #[test]
    fn index_negative_all() {
        let data = json!({"items": [10, 20, 30]});
        let path = parse("items[-3]").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!(10)));
    }

    #[test]
    fn index_out_of_bounds_positive() {
        let data = json!({"items": [10, 20, 30]});
        let path = parse("items[5]").unwrap();
        assert_eq!(resolve(&data, &path), None);
    }

    #[test]
    fn index_out_of_bounds_negative() {
        let data = json!({"items": [10, 20, 30]});
        let path = parse("items[-4]").unwrap();
        assert_eq!(resolve(&data, &path), None);
    }

    #[test]
    fn index_then_field() {
        let data = json!({"orders": [{"id": "a"}, {"id": "b"}]});
        let path = parse("orders[0].id").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!("a")));
    }

    #[test]
    fn index_on_object_returns_none() {
        let data = json!({"name": "Alice"});
        let path = parse("[0]").unwrap();
        assert_eq!(resolve(&data, &path), None);
    }

    #[test]
    fn index_on_empty_array() {
        let data = json!({"items": []});
        let path = parse("items[0]").unwrap();
        assert_eq!(resolve(&data, &path), None);
    }

    // ────────────────────────── Wildcard projections ──────────────────────────

    #[test]
    fn wildcard_field() {
        let data = json!({
            "users": [
                {"name": "Alice"},
                {"name": "Bob"},
                {"name": "Charlie"},
            ]
        });
        let path = parse("users[*].name").unwrap();
        assert_eq!(
            resolve(&data, &path),
            Some(json!(["Alice", "Bob", "Charlie"]))
        );
    }

    #[test]
    fn wildcard_skips_missing() {
        let data = json!({
            "users": [
                {"name": "Alice"},
                {"age": 30},
                {"name": "Charlie"},
            ]
        });
        let path = parse("users[*].name").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!(["Alice", "Charlie"])));
    }

    #[test]
    fn wildcard_returns_empty_array_when_no_matches() {
        let data = json!({
            "users": [
                {"age": 25},
                {"age": 30},
            ]
        });
        let path = parse("users[*].name").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!([])));
    }

    #[test]
    fn wildcard_on_empty_array() {
        let data = json!({"users": []});
        let path = parse("users[*].name").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!([])));
    }

    #[test]
    fn wildcard_without_remaining_path() {
        let data = json!({"items": [1, 2, 3]});
        let path = parse("items[*]").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!([1, 2, 3])));
    }

    #[test]
    fn wildcard_on_object_returns_none() {
        let data = json!({"name": "Alice"});
        let path = parse("[*]").unwrap();
        assert_eq!(resolve(&data, &path), None);
    }

    // ────────────────────────── Multi-level wildcards ──────────────────────────

    #[test]
    fn nested_wildcard() {
        let data = json!({
            "departments": [
                {
                    "members": [
                        {"name": "Alice"},
                        {"name": "Bob"},
                    ]
                },
                {
                    "members": [
                        {"name": "Charlie"},
                    ]
                },
            ]
        });
        let path = parse("departments[*].members[*].name").unwrap();
        let result = resolve(&data, &path);
        // First wildcard yields arrays per department, second wildcard is
        // applied within each department's members.
        assert_eq!(
            result,
            Some(json!([["Alice", "Bob"], ["Charlie"]]))
        );
    }

    #[test]
    fn wildcard_then_index() {
        let data = json!({
            "rows": [
                {"cells": [1, 2, 3]},
                {"cells": [4, 5, 6]},
            ]
        });
        let path = parse("rows[*].cells[0]").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!([1, 4])));
    }

    #[test]
    fn wildcard_then_negative_index() {
        let data = json!({
            "rows": [
                {"cells": [1, 2, 3]},
                {"cells": [4, 5, 6]},
            ]
        });
        let path = parse("rows[*].cells[-1]").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!([3, 6])));
    }

    // ────────────────────────── Edge cases ──────────────────────────

    #[test]
    fn resolve_null_value() {
        let data = json!({"key": null});
        let path = parse("key").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!(null)));
    }

    #[test]
    fn resolve_through_null() {
        let data = json!({"key": null});
        let path = parse("key.child").unwrap();
        assert_eq!(resolve(&data, &path), None);
    }

    #[test]
    fn resolve_empty_object() {
        let data = json!({});
        let path = parse("anything").unwrap();
        assert_eq!(resolve(&data, &path), None);
    }

    #[test]
    fn resolve_deeply_nested() {
        let data = json!({"a": {"b": {"c": {"d": {"e": 42}}}}});
        let path = parse("a.b.c.d.e").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!(42)));
    }

    #[test]
    fn resolve_returns_array_value() {
        let data = json!({"items": [1, 2, 3]});
        let path = parse("items").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!([1, 2, 3])));
    }

    #[test]
    fn resolve_boolean_value() {
        let data = json!({"active": true});
        let path = parse("active").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!(true)));
    }

    #[test]
    fn resolve_numeric_value() {
        let data = json!({"count": 7});
        let path = parse("count").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!(7)));
    }

    #[test]
    fn resolve_float_value() {
        let data = json!({"price": 9.99});
        let path = parse("price").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!(9.99)));
    }

    #[test]
    fn wildcard_mixed_types_in_array() {
        // Array contains objects, scalars, and nulls — wildcard should skip
        // elements where the sub-path doesn't resolve.
        let data = json!({
            "items": [
                {"name": "foo"},
                42,
                null,
                {"name": "bar"},
                [1, 2, 3],
            ]
        });
        let path = parse("items[*].name").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!(["foo", "bar"])));
    }

    #[test]
    fn index_on_nested_array() {
        let data = json!([[10, 20], [30, 40]]);
        let path = parse("[1][0]").unwrap();
        assert_eq!(resolve(&data, &path), Some(json!(30)));
    }
}
