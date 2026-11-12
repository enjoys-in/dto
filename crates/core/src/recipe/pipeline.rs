//! Medallion Pipeline Engine
//!
//! Executes multi-target ELT pipelines with support for:
//! - Multi-table projection (fan-out from 1 source to multiple target silver tables)
//! - `unpack: "array_explode"` (unrolling array items with parent field broadcasting)
//! - `grain: "header"` vs `grain: "line"`
//! - Deduplication by `business_key` with `dedup_strategy: "latest"`
//! - Type casts (`decimal`, `date`, `timestamp`, `string`, `json`, `boolean`)

use std::collections::{HashMap, HashSet};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::model::{FieldMapping, RecipeError};
use crate::functions;
use crate::path::{tokenizer, resolver};

/// Full multi-target pipeline specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineSpec {
    #[serde(default)]
    pub sources: Vec<SourceSpec>,

    pub targets: Vec<TargetSpec>,

    #[serde(default)]
    pub table_first: bool,
}

/// Source specification (e.g. bronze staging table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSpec {
    pub id: String,
    #[serde(default)]
    pub bronze_table: Option<String>,
    #[serde(default)]
    pub filters: HashMap<String, Value>,
}

/// Target specification (e.g. silver table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetSpec {
    pub table: String,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub grain: Option<String>,
    #[serde(default)]
    pub business_key: Vec<String>,
    #[serde(default)]
    pub dedup_strategy: Option<String>,
    pub mappings: Vec<FieldMapping>,
    #[serde(default)]
    pub contract: Option<ContractSpec>,
}

/// Quality & contract specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractSpec {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub nullable: Vec<String>,
}

#[derive(Debug, Clone)]
struct PrecompiledExplodeMapping {
    target: String,
    transform: Option<String>,
    default: Option<Value>,
    is_exploded_field: bool,
    is_whole_element: bool,
    parsed_path: Option<crate::path::ast::DtoPath>,
}

impl PipelineSpec {
    /// Parse a pipeline specification from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, RecipeError> {
        serde_json::from_str(json).map_err(RecipeError::Parse)
    }

    /// Parse a pipeline specification from a `serde_json::Value`.
    pub fn from_value(value: Value) -> Result<Self, RecipeError> {
        serde_json::from_value(value).map_err(RecipeError::Parse)
    }

    /// Execute the entire pipeline on input data (single object or array of objects).
    /// Returns a map of `table_name -> array of transformed rows`.
    pub fn execute(&self, input: &Value) -> Result<HashMap<String, Vec<Value>>, String> {
        // Process each document (either single object or items in an array)
        let items: Vec<&Value> = match input {
            Value::Array(arr) => arr.iter().collect(),
            Value::Object(_) => vec![input],
            _ => return Err("pipeline input must be a JSON object or array of objects".to_string()),
        };

        // Process all target tables in parallel using Rayon
        let target_outputs: Result<Vec<(String, Vec<Value>)>, String> = self
            .targets
            .par_iter()
            .map(|target| {
                let mut table_rows = Vec::new();
                for item in &items {
                    let rows = self.execute_target_on_item(target, item)?;
                    table_rows.extend(rows);
                }

                // Deduplicate if requested
                if let Some(ref strategy) = target.dedup_strategy {
                    if strategy == "latest" && !target.business_key.is_empty() {
                        table_rows = deduplicate_latest(table_rows, &target.business_key);
                    }
                }

                Ok((target.table.clone(), table_rows))
            })
            .collect();

        let mut results = HashMap::with_capacity(self.targets.len());
        for (table, rows) in target_outputs? {
            results.insert(table, rows);
        }

        Ok(results)
    }

    fn execute_target_on_item(&self, target: &TargetSpec, item: &Value) -> Result<Vec<Value>, String> {
        // Detect if any mapping requests array explode or if grain is line
        let has_explode = target.mappings.iter().any(|m| {
            m.unpack.as_deref() == Some("array_explode") || m.source.contains("[*]")
        });

        let is_line_grain = target.grain.as_deref() == Some("line") || has_explode;

        if !is_line_grain {
            // Header grain: exactly 1 row from this input item
            let row = self.transform_single_row(&target.mappings, item, item)?;
            Ok(vec![Value::Object(row)])
        } else {
            // Line grain: find the array to explode
            let explode_path = self.find_explode_path(target);
            if let Some(ref path_prefix) = explode_path {
                let array_path = tokenizer::parse(path_prefix).map_err(|e| e.to_string())?;
                let resolved_array = resolver::resolve(item, &array_path);

                match resolved_array {
                    Some(Value::Array(elements)) => {
                        let explode_pattern = format!("{}[*]", path_prefix);
                        let explode_prefix_dot = format!("{}[*].", path_prefix);

                        let precompiled: Vec<PrecompiledExplodeMapping> = target
                            .mappings
                            .iter()
                            .map(|m| {
                                let is_exploded_field = m.unpack.as_deref() == Some("array_explode")
                                    || m.source.starts_with(&explode_pattern);
                                let (is_whole, path_str) = if is_exploded_field {
                                    if m.source == explode_pattern {
                                        (true, None)
                                    } else if m.source.starts_with(&explode_prefix_dot) {
                                        (false, Some(&m.source[explode_prefix_dot.len()..]))
                                    } else {
                                        (false, Some(m.source.as_str()))
                                    }
                                } else {
                                    (false, Some(m.source.as_str()))
                                };

                                let parsed_path = path_str.and_then(|p| tokenizer::parse(p).ok());

                                PrecompiledExplodeMapping {
                                    target: m.target.clone(),
                                    transform: m.transform.clone(),
                                    default: m.default.clone(),
                                    is_exploded_field,
                                    is_whole_element: is_whole,
                                    parsed_path,
                                }
                            })
                            .collect();

                        let rows: Result<Vec<Value>, String> = elements
                            .par_iter()
                            .map(|elem| {
                                let mut row = serde_json::Map::with_capacity(precompiled.len());
                                for m in &precompiled {
                                    let extracted = if m.is_exploded_field {
                                        if m.is_whole_element {
                                            Some(elem.clone())
                                        } else if let Some(ref path) = m.parsed_path {
                                            resolver::resolve(elem, path)
                                        } else {
                                            None
                                        }
                                    } else if let Some(ref path) = m.parsed_path {
                                        resolver::resolve(item, path)
                                    } else {
                                        None
                                    };

                                    let val = match extracted {
                                        Some(v) if !v.is_null() => Some(v),
                                        _ => m.default.clone(),
                                    };

                                    if let Some(mut v) = val {
                                        if let Some(ref tr) = m.transform {
                                            v = functions::apply_transform_chain(tr, v)
                                                .map_err(|e| format!("transform error on '{}': {e}", m.target))?;
                                        }
                                        row.insert(m.target.clone(), v);
                                    }
                                }
                                Ok(Value::Object(row))
                            })
                            .collect();

                        rows
                    }
                    _ => Ok(Vec::new()), // No array elements to explode
                }
            } else {
                // Fallback to single row if no array path found
                let row = self.transform_single_row(&target.mappings, item, item)?;
                Ok(vec![Value::Object(row)])
            }
        }
    }

    fn find_explode_path(&self, target: &TargetSpec) -> Option<String> {
        for m in &target.mappings {
            if m.unpack.as_deref() == Some("array_explode") || m.source.contains("[*]") {
                if let Some(idx) = m.source.find("[*]") {
                    let prefix = m.source[..idx].trim_end_matches('.');
                    if !prefix.is_empty() {
                        return Some(prefix.to_string());
                    }
                }
            }
        }
        None
    }

    fn transform_single_row(
        &self,
        mappings: &[FieldMapping],
        root: &Value,
        current: &Value,
    ) -> Result<serde_json::Map<String, Value>, String> {
        let mut row = serde_json::Map::with_capacity(mappings.len());

        for m in mappings {
            let val = self.extract_and_transform(m, root, current)?;
            if let Some(v) = val {
                row.insert(m.target.clone(), v);
            }
        }

        Ok(row)
    }

    #[allow(dead_code)]
    fn transform_exploded_row(
        &self,
        mappings: &[FieldMapping],
        explode_prefix: &str,
        root: &Value,
        line_item: &Value,
    ) -> Result<serde_json::Map<String, Value>, String> {
        let mut row = serde_json::Map::with_capacity(mappings.len());
        let explode_pattern = format!("{}[*]", explode_prefix);

        for m in mappings {
            let is_exploded_field = m.unpack.as_deref() == Some("array_explode") || m.source.starts_with(&explode_pattern);

            let extracted = if is_exploded_field {
                if m.source == explode_pattern {
                    // Whole transaction/element
                    Some(line_item.clone())
                } else if m.source.starts_with(&format!("{}.", explode_pattern)) {
                    let subpath = &m.source[explode_pattern.len() + 1..];
                    let parsed = tokenizer::parse(subpath).map_err(|e| e.to_string())?;
                    resolver::resolve(line_item, &parsed)
                } else {
                    // Relative path from line_item
                    let parsed = tokenizer::parse(&m.source).map_err(|e| e.to_string())?;
                    resolver::resolve(line_item, &parsed)
                }
            } else {
                // Broadcast from root/parent document!
                let parsed = tokenizer::parse(&m.source).map_err(|e| e.to_string())?;
                resolver::resolve(root, &parsed)
            };

            let val = match extracted {
                Some(v) if !v.is_null() => Some(v),
                _ => m.default.clone(),
            };

            if let Some(mut v) = val {
                if let Some(ref transform_chain) = m.transform {
                    v = functions::apply_transform_chain(transform_chain, v)
                        .map_err(|e| format!("transform error on '{}': {e}", m.target))?;
                }
                row.insert(m.target.clone(), v);
            }
        }

        Ok(row)
    }

    fn extract_and_transform(
        &self,
        mapping: &FieldMapping,
        root: &Value,
        _current: &Value,
    ) -> Result<Option<Value>, String> {
        let (path_str, inline_tr) = super::converter::parse_source_and_transforms(&mapping.source);

        let extracted = if mapping.literal {
            Some(Value::String(path_str))
        } else if !path_str.is_empty() {
            let parsed = tokenizer::parse(&path_str).map_err(|e| e.to_string())?;
            resolver::resolve(root, &parsed)
        } else {
            None
        };

        let val = match extracted {
            Some(v) if !v.is_null() => Some(v),
            _ => mapping.default.clone(),
        };

        let mut val = match val {
            Some(v) => v,
            None => return Ok(None),
        };

        let combined_transform = match (&mapping.transform, inline_tr) {
            (Some(explicit), Some(inline)) => Some(format!("{} | {}", inline, explicit)),
            (Some(explicit), None) => Some(explicit.clone()),
            (None, Some(inline)) => Some(inline),
            (None, None) => None,
        };

        if let Some(ref tr) = combined_transform {
            val = functions::apply_transform_chain(tr, val)
                .map_err(|e| format!("transform error on '{}': {e}", mapping.target))?;
        }

        Ok(Some(val))
    }
}

/// Deduplicate rows keeping the latest occurrence based on a composite business key.
fn deduplicate_latest(rows: Vec<Value>, business_key: &[String]) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::with_capacity(rows.len());

    // Iterate in reverse to keep latest
    for row in rows.into_iter().rev() {
        if let Value::Object(ref obj) = row {
            let key: Vec<String> = business_key
                .iter()
                .map(|k| obj.get(k).map(|v| v.to_string()).unwrap_or_default())
                .collect();

            if seen.insert(key) {
                deduped.push(row);
            }
        } else {
            deduped.push(row);
        }
    }

    deduped.reverse();
    deduped
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_medallion_pipeline_explode() {
        let pipeline_json = r#"{
            "sources": [
                { "id": "item", "bronze_table": "stg_upload" }
            ],
            "targets": [
                {
                    "table": "silver_part",
                    "grain": "header",
                    "mappings": [
                        { "source": "item_id", "target": "part_number" },
                        { "source": "reference.price", "target": "standard_cost", "cast": "decimal" }
                    ]
                },
                {
                    "table": "silver_purchase_order_line",
                    "grain": "line",
                    "business_key": ["po_id", "extra_po_line_key"],
                    "dedup_strategy": "latest",
                    "mappings": [
                        { "source": "item_id", "target": "part_number" },
                        { "source": "transactions[*].purchaseOrderId", "target": "po_id", "unpack": "array_explode" },
                        { "source": "transactions[*].productCartId", "target": "extra_po_line_key", "unpack": "array_explode" },
                        { "source": "transactions[*].quantity", "target": "quantity", "cast": "decimal", "unpack": "array_explode" },
                        { "source": "transactions[*].price", "target": "unit_price", "cast": "decimal", "unpack": "array_explode" },
                        { "source": "transactions[*]", "target": "flex_attributes", "cast": "json", "unpack": "array_explode" }
                    ]
                }
            ]
        }"#;

        let input_data = json!({
            "item_id": "ITM-990",
            "reference": {
                "price": "45.50"
            },
            "transactions": [
                {
                    "purchaseOrderId": "PO-100",
                    "productCartId": "CART-1",
                    "quantity": "5",
                    "price": "40.00"
                },
                {
                    "purchaseOrderId": "PO-101",
                    "productCartId": "CART-2",
                    "quantity": "10",
                    "price": "38.50"
                }
            ]
        });

        let pipeline = PipelineSpec::from_json(pipeline_json).unwrap();
        let result = pipeline.execute(&input_data).unwrap();

        // 1. Verify silver_part has 1 header record
        let parts = &result["silver_part"];
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["part_number"], "ITM-990");
        assert_eq!(parts[0]["standard_cost"], 45.50);

        // 2. Verify silver_purchase_order_line has 2 exploded records with broadcast part_number
        let lines = &result["silver_purchase_order_line"];
        assert_eq!(lines.len(), 2);

        assert_eq!(lines[0]["part_number"], "ITM-990");
        assert_eq!(lines[0]["po_id"], "PO-100");
        assert_eq!(lines[0]["quantity"], 5.0);
        assert_eq!(lines[0]["unit_price"], 40.0);
        assert!(lines[0]["flex_attributes"].is_object());

        assert_eq!(lines[1]["part_number"], "ITM-990");
        assert_eq!(lines[1]["po_id"], "PO-101");
        assert_eq!(lines[1]["quantity"], 10.0);
        assert_eq!(lines[1]["unit_price"], 38.5);
    }
}
