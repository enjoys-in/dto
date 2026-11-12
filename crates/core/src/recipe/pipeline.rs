//! Medallion Pipeline Engine
//!
//! Executes multi-target ELT pipelines with support for:
//! - Multi-table projection (fan-out from 1 source to multiple target silver tables)
//! - `unpack: "array_explode"` (unrolling array items with parent field broadcasting)
//! - `grain: "header"` vs `grain: "line"`
//! - Deduplication by `business_key` with `dedup_strategy: "latest"` (optionally
//!   ordered by a `sequence_by` column)
//! - Type casts (`decimal`, `date`, `timestamp`, `string`, `json`, `boolean`)
//!
//! A [`PipelineSpec`] is compiled into a [`CompiledPipeline`] once; configuration
//! mistakes (unknown grain / strategy / function, bad paths, duplicate tables,
//! dedup keys that no mapping produces) are reported at that point.

use std::cmp::Ordering;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::converter::parse_source_and_transforms;
use super::model::{FieldMapping, RecipeError};
use crate::functions::{self, TransformFn};
use crate::path::{ast::DtoPath, resolver, tokenizer};
use crate::schema::transform::json_type_name;

/// Arrays with at least this many elements are exploded in parallel.
const PARALLEL_EXPLODE_MIN: usize = 256;

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
    /// Output column that orders duplicates for `dedup_strategy: "latest"`: the
    /// row with the greatest value wins (numbers numerically, strings
    /// lexicographically — ISO-8601 timestamps in one offset sort correctly;
    /// missing/null sort lowest; ties go to the later row). Without it, the last
    /// row in input order wins.
    #[serde(default, alias = "order_by")]
    pub sequence_by: Option<String>,
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

impl PipelineSpec {
    /// Parse a pipeline specification from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, RecipeError> {
        serde_json::from_str(json).map_err(RecipeError::Parse)
    }

    /// Parse a pipeline specification from a `serde_json::Value`.
    pub fn from_value(value: Value) -> Result<Self, RecipeError> {
        serde_json::from_value(value).map_err(RecipeError::Parse)
    }

    /// Validate the specification and compile it for execution.
    pub fn compile(&self) -> Result<CompiledPipeline, String> {
        if self.targets.is_empty() {
            return Err("pipeline must define at least one target".into());
        }
        let mut tables = HashSet::new();
        for target in &self.targets {
            if !tables.insert(target.table.as_str()) {
                return Err(format!(
                    "duplicate target table '{}': each table may appear only once",
                    target.table
                ));
            }
        }
        let targets = self
            .targets
            .iter()
            .map(compile_target)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CompiledPipeline { targets })
    }

    /// Execute the entire pipeline on input data (single object or array of objects).
    /// Returns a map of `table_name -> array of transformed rows`.
    pub fn execute(&self, input: &Value) -> Result<HashMap<String, Vec<Value>>, String> {
        self.compile()?.execute(input)
    }
}

/// A validated, ready-to-run pipeline.
#[derive(Debug, Clone)]
pub struct CompiledPipeline {
    targets: Vec<CompiledTarget>,
}

#[derive(Debug, Clone)]
struct CompiledTarget {
    table: String,
    grain: Grain,
    mappings: Vec<PipeMapping>,
    dedup: Option<Dedup>,
}

#[derive(Debug, Clone)]
enum Grain {
    /// One row per input document.
    Header,
    /// One row per element of the array at `array`.
    Line { array: DtoPath, array_name: String },
}

#[derive(Debug, Clone)]
struct PipeMapping {
    target: String,
    source: PipeSource,
    chain: Vec<TransformFn>,
    default: Option<Value>,
}

/// Where a mapping's raw value comes from.
#[derive(Debug, Clone)]
enum PipeSource {
    Literal(Value),
    /// Path resolved against the whole input document (broadcast to every line row).
    Document(DtoPath),
    /// Path resolved against the current exploded element (`None` = the whole element).
    Element(Option<DtoPath>),
}

#[derive(Debug, Clone)]
struct Dedup {
    business_key: Vec<String>,
    sequence_by: Option<String>,
}

impl CompiledPipeline {
    /// Execute on a single object or an array of objects.
    pub fn execute(&self, input: &Value) -> Result<HashMap<String, Vec<Value>>, String> {
        match input {
            Value::Array(items) => self.execute_items(items, true),
            Value::Object(_) => self.execute_items(std::slice::from_ref(input), false),
            other => Err(format!(
                "pipeline input must be a JSON object or array of objects, got {}",
                json_type_name(other)
            )),
        }
    }

    /// Execute on a slice of documents. `is_batch` controls whether error
    /// messages include the failing document's index.
    pub fn execute_items(
        &self,
        items: &[Value],
        is_batch: bool,
    ) -> Result<HashMap<String, Vec<Value>>, String> {
        let outputs: Vec<Result<Vec<Value>, String>> = self
            .targets
            .par_iter()
            .map(|target| target.execute(items, is_batch))
            .collect();

        let mut results = HashMap::with_capacity(self.targets.len());
        for (target, rows) in self.targets.iter().zip(outputs) {
            results.insert(target.table.clone(), rows?);
        }
        Ok(results)
    }
}

impl CompiledTarget {
    fn execute(&self, items: &[Value], is_batch: bool) -> Result<Vec<Value>, String> {
        let per_item: Vec<Result<Vec<Value>, String>> = items
            .par_iter()
            .enumerate()
            .map(|(i, item)| {
                self.rows_for(item).map_err(|e| {
                    if is_batch {
                        format!("target '{}', item {i}: {e}", self.table)
                    } else {
                        format!("target '{}': {e}", self.table)
                    }
                })
            })
            .collect();

        let mut rows = Vec::with_capacity(items.len());
        for item_rows in per_item {
            rows.extend(item_rows?);
        }

        if let Some(ref dedup) = self.dedup {
            rows = dedup.apply(rows);
        }
        Ok(rows)
    }

    fn rows_for(&self, doc: &Value) -> Result<Vec<Value>, String> {
        if !doc.is_object() {
            return Err(format!("expected a JSON object, got {}", json_type_name(doc)));
        }
        match &self.grain {
            Grain::Header => Ok(vec![self.row(doc, None)?]),
            Grain::Line { array, array_name } => match resolver::resolve(doc, array) {
                // No line items: no line rows.
                None | Some(Value::Null) => Ok(Vec::new()),
                Some(Value::Array(elements)) => {
                    if elements.len() >= PARALLEL_EXPLODE_MIN {
                        elements.par_iter().map(|e| self.row(doc, Some(e))).collect()
                    } else {
                        elements.iter().map(|e| self.row(doc, Some(e))).collect()
                    }
                }
                Some(other) => Err(format!(
                    "expected an array at '{array_name}' to explode, got {}",
                    json_type_name(&other)
                )),
            },
        }
    }

    fn row(&self, doc: &Value, element: Option<&Value>) -> Result<Value, String> {
        let mut row = Map::new();
        for m in &self.mappings {
            let raw = match &m.source {
                PipeSource::Literal(v) => Some(v.clone()),
                PipeSource::Document(path) => resolver::resolve(doc, path),
                PipeSource::Element(None) => element.cloned(),
                PipeSource::Element(Some(path)) => element.and_then(|e| resolver::resolve(e, path)),
            };
            let value = match raw {
                Some(v) if !v.is_null() => Some(v),
                _ => m.default.clone(),
            };
            if let Some(mut v) = value {
                if !m.chain.is_empty() {
                    v = functions::apply_compiled_chain(&m.chain, v)
                        .map_err(|e| format!("transform error on '{}': {e}", m.target))?;
                }
                row.insert(m.target.clone(), v);
            }
        }
        Ok(Value::Object(row))
    }
}

impl Dedup {
    /// Keep one row per business key. Rows whose key is incomplete (a key column
    /// missing or null) are never merged — previously they all collapsed into one.
    /// Surviving rows keep their original relative order.
    fn apply(&self, rows: Vec<Value>) -> Vec<Value> {
        let mut winner: HashMap<Vec<String>, usize> = HashMap::new();
        let mut keep = vec![false; rows.len()];

        for (i, row) in rows.iter().enumerate() {
            let Some(key) = self.key_of(row) else {
                keep[i] = true;
                continue;
            };
            match winner.entry(key) {
                Entry::Vacant(slot) => {
                    slot.insert(i);
                }
                Entry::Occupied(mut slot) => {
                    let replace = match &self.sequence_by {
                        // Not `Less` → equal values go to the later row.
                        Some(col) => {
                            compare_sequence(row.get(col), rows[*slot.get()].get(col)) != Ordering::Less
                        }
                        None => true,
                    };
                    if replace {
                        slot.insert(i);
                    }
                }
            }
        }
        for &i in winner.values() {
            keep[i] = true;
        }

        rows.into_iter()
            .zip(keep)
            .filter_map(|(row, kept)| kept.then_some(row))
            .collect()
    }

    fn key_of(&self, row: &Value) -> Option<Vec<String>> {
        let obj = row.as_object()?;
        self.business_key
            .iter()
            .map(|col| match obj.get(col) {
                None | Some(Value::Null) => None,
                Some(v) => Some(v.to_string()),
            })
            .collect()
    }
}

/// Total order for `sequence_by` values: missing/null < bool < number < string < other.
fn compare_sequence(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    fn rank(v: Option<&Value>) -> u8 {
        match v {
            None | Some(Value::Null) => 0,
            Some(Value::Bool(_)) => 1,
            Some(Value::Number(_)) => 2,
            Some(Value::String(_)) => 3,
            Some(_) => 4,
        }
    }
    match (a, b) {
        (Some(Value::Number(x)), Some(Value::Number(y))) => {
            x.as_f64().partial_cmp(&y.as_f64()).unwrap_or(Ordering::Equal)
        }
        (Some(Value::String(x)), Some(Value::String(y))) => x.cmp(y),
        (Some(Value::Bool(x)), Some(Value::Bool(y))) => x.cmp(y),
        _ => rank(a).cmp(&rank(b)),
    }
}

fn is_explode(m: &FieldMapping) -> bool {
    m.unpack.as_deref() == Some("array_explode")
}

/// The array to explode: the part before `[*]` of the first `unpack` mapping,
/// falling back to the first mapping that contains `[*]` at all.
fn find_explode_prefix(mappings: &[FieldMapping]) -> Option<String> {
    let prefix_of = |m: &FieldMapping| -> Option<String> {
        if m.literal {
            return None;
        }
        let (path, _) = parse_source_and_transforms(&m.source);
        let idx = path.find("[*]")?;
        let prefix = path[..idx].trim_end_matches('.');
        (!prefix.is_empty()).then(|| prefix.to_string())
    };
    mappings
        .iter()
        .filter(|m| is_explode(m))
        .find_map(prefix_of)
        .or_else(|| mappings.iter().find_map(prefix_of))
}

fn compile_target(t: &TargetSpec) -> Result<CompiledTarget, String> {
    if t.table.is_empty() {
        return Err("target table name cannot be empty".into());
    }
    let err = |msg: String| format!("target '{}': {msg}", t.table);

    if t.mappings.is_empty() {
        return Err(err("must have at least one mapping".into()));
    }
    let mut produced = HashSet::new();
    for m in &t.mappings {
        if m.target.is_empty() {
            return Err(err("a mapping has an empty target".into()));
        }
        if !produced.insert(m.target.as_str()) {
            return Err(err(format!("duplicate mapping target '{}'", m.target)));
        }
        if !m.literal && m.source.trim().is_empty() {
            return Err(err(format!("mapping '{}' has an empty source", m.target)));
        }
        if let Some(unpack) = m.unpack.as_deref() {
            if unpack != "array_explode" {
                return Err(err(format!(
                    "mapping '{}': unknown unpack '{unpack}' (expected 'array_explode')",
                    m.target
                )));
            }
        }
        if m.nested.is_some() {
            return Err(err(format!(
                "mapping '{}': nested recipes are not supported in pipelines",
                m.target
            )));
        }
    }

    let line = match t.grain.as_deref() {
        Some("header") => {
            if let Some(m) = t.mappings.iter().find(|m| is_explode(m)) {
                return Err(err(format!(
                    "mapping '{}' uses unpack 'array_explode', which requires grain 'line'",
                    m.target
                )));
            }
            false
        }
        Some("line") => true,
        // Unspecified: explode when anything asks for it (original behaviour).
        None => t
            .mappings
            .iter()
            .any(|m| is_explode(m) || (!m.literal && m.source.contains("[*]"))),
        Some(other) => {
            return Err(err(format!("unknown grain '{other}' (expected 'header' or 'line')")))
        }
    };

    let grain = if line {
        let array_name = find_explode_prefix(&t.mappings).ok_or_else(|| {
            err("line grain needs a mapping whose source contains '<array>[*]' to choose the array to explode".into())
        })?;
        let array = tokenizer::parse(&array_name)
            .map_err(|e| err(format!("invalid array path '{array_name}': {e}")))?;
        Grain::Line { array, array_name }
    } else {
        Grain::Header
    };

    let explode_pattern = match &grain {
        Grain::Line { array_name, .. } => Some(format!("{array_name}[*]")),
        Grain::Header => None,
    };
    let mappings = t
        .mappings
        .iter()
        .map(|m| {
            compile_mapping(m, explode_pattern.as_deref())
                .map_err(|e| err(format!("mapping '{}': {e}", m.target)))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let dedup = match t.dedup_strategy.as_deref() {
        None | Some("none") => {
            if let Some(ref col) = t.sequence_by {
                return Err(err(format!(
                    "sequence_by '{col}' has no effect without dedup_strategy 'latest'"
                )));
            }
            None
        }
        Some("latest") => {
            if t.business_key.is_empty() {
                return Err(err("dedup_strategy 'latest' requires a non-empty business_key".into()));
            }
            for col in t.business_key.iter().chain(t.sequence_by.iter()) {
                if !produced.contains(col.as_str()) {
                    return Err(err(format!(
                        "dedup column '{col}' is not produced by any mapping target"
                    )));
                }
            }
            Some(Dedup { business_key: t.business_key.clone(), sequence_by: t.sequence_by.clone() })
        }
        Some(other) => {
            return Err(err(format!(
                "unknown dedup_strategy '{other}' (expected 'latest' or 'none')"
            )))
        }
    };

    Ok(CompiledTarget { table: t.table.clone(), grain, mappings, dedup })
}

fn compile_mapping(m: &FieldMapping, explode_pattern: Option<&str>) -> Result<PipeMapping, String> {
    let parse = |p: &str| tokenizer::parse(p).map_err(|e| format!("invalid source path '{p}': {e}"));

    let (source, inline) = if m.literal {
        (PipeSource::Literal(Value::String(m.source.clone())), None)
    } else {
        // Inline pipes (`a.b | trim`) are split off before the path is parsed;
        // previously exploded fields kept the pipes and silently resolved to nothing.
        let (path, inline) = parse_source_and_transforms(&m.source);
        let source = match explode_pattern {
            Some(pattern) if is_explode(m) || path.starts_with(pattern) => {
                match path.strip_prefix(pattern) {
                    Some(rest) => {
                        let rest = rest.strip_prefix('.').unwrap_or(rest);
                        PipeSource::Element(if rest.is_empty() { None } else { Some(parse(rest)?) })
                    }
                    // `unpack` with a path relative to the exploded element.
                    None => PipeSource::Element(Some(parse(&path)?)),
                }
            }
            _ => PipeSource::Document(parse(&path)?),
        };
        (source, inline)
    };

    let chain = match (inline, m.transform.as_deref()) {
        (Some(i), Some(e)) => Some(format!("{i} | {e}")),
        (Some(i), None) => Some(i),
        (None, Some(e)) => Some(e.to_string()),
        (None, None) => None,
    };
    let chain = match chain {
        Some(c) => functions::compile_chain(&c).map_err(|e| e.to_string())?,
        None => Vec::new(),
    };

    Ok(PipeMapping { target: m.target.clone(), source, chain, default: m.default.clone() })
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

    // ── Regression tests ────────────────────────────────────────────────

    fn run(spec: Value, input: Value) -> Result<HashMap<String, Vec<Value>>, String> {
        PipelineSpec::from_value(spec).unwrap().execute(&input)
    }

    fn compile_err(spec: Value) -> String {
        PipelineSpec::from_value(spec).unwrap().compile().unwrap_err()
    }

    #[test]
    fn exploded_fields_honour_inline_pipes() {
        let out = run(
            json!({"targets":[{"table":"t","grain":"line","mappings":[
                {"source":"id","target":"id"},
                {"source":"tx[*].qty | to_number","target":"qty","unpack":"array_explode"}]}]}),
            json!({"id":"A","tx":[{"qty":"5"},{"qty":"7"}]}),
        )
        .unwrap();
        assert_eq!(out["t"], vec![json!({"id":"A","qty":5.0}), json!({"id":"A","qty":7.0})]);
    }

    #[test]
    fn explicit_header_grain_is_respected_with_wildcards() {
        let out = run(
            json!({"targets":[{"table":"t","grain":"header","mappings":[
                {"source":"id","target":"id"},{"source":"roles[*].name","target":"roles"}]}]}),
            json!({"id":"A","roles":[{"name":"x"},{"name":"y"}]}),
        )
        .unwrap();
        assert_eq!(out["t"], vec![json!({"id":"A","roles":["x","y"]})]);
    }

    #[test]
    fn literals_work_in_line_grain() {
        let out = run(
            json!({"targets":[{"table":"t","grain":"line","mappings":[
                {"source":"api","target":"src","literal":true},
                {"source":"tx[*].id","target":"id","unpack":"array_explode"}]}]}),
            json!({"tx":[{"id":1}]}),
        )
        .unwrap();
        assert_eq!(out["t"], vec![json!({"src":"api","id":1})]);
    }

    #[test]
    fn dedup_latest_with_sequence_by_keeps_the_newest_row() {
        let spec = json!({"targets":[{"table":"t","business_key":["k"],"dedup_strategy":"latest",
            "sequence_by":"ts","mappings":[
            {"source":"k","target":"k"},{"source":"v","target":"v"},{"source":"ts","target":"ts"}]}]});
        let out = run(
            spec,
            json!([{"k":"A","v":"new","ts":"2024-12-01"},{"k":"A","v":"old","ts":"2020-01-01"},{"k":"B","v":"b"}]),
        )
        .unwrap();
        assert_eq!(out["t"], vec![json!({"k":"A","v":"new","ts":"2024-12-01"}), json!({"k":"B","v":"b"})]);
    }

    #[test]
    fn dedup_without_sequence_by_keeps_the_last_row_in_input_order() {
        let out = run(
            json!({"targets":[{"table":"t","business_key":["k"],"dedup_strategy":"latest","mappings":[
                {"source":"k","target":"k"},{"source":"v","target":"v"}]}]}),
            json!([{"k":1,"v":"a"},{"k":2,"v":"b"},{"k":1,"v":"c"}]),
        )
        .unwrap();
        assert_eq!(out["t"], vec![json!({"k":2,"v":"b"}), json!({"k":1,"v":"c"})]);
    }

    #[test]
    fn rows_missing_the_business_key_are_not_collapsed() {
        let out = run(
            json!({"targets":[{"table":"t","business_key":["k"],"dedup_strategy":"latest","mappings":[
                {"source":"k","target":"k"},{"source":"v","target":"v"}]}]}),
            json!([{"v":1},{"v":2},{"k":null,"v":3}]),
        )
        .unwrap();
        assert_eq!(out["t"].len(), 3);
    }

    #[test]
    fn bad_records_report_target_and_item() {
        let err = run(
            json!({"targets":[{"table":"t","mappings":[{"source":"d","target":"d","cast":"date"}]}]}),
            json!([{"d":"2024-01-01"},{"d":"garbage"}]),
        )
        .unwrap_err();
        assert!(err.starts_with("target 't', item 1: transform error on 'd'"), "got {err}");

        let err = run(json!({"targets":[{"table":"t","mappings":[{"source":"a","target":"a"}]}]}), json!([1]))
            .unwrap_err();
        assert!(err.contains("item 0: expected a JSON object"), "got {err}");
    }

    #[test]
    fn pipeline_configuration_mistakes_fail_at_compile_time() {
        let m = json!([{"source":"k","target":"k"}]);
        assert!(compile_err(json!({"targets":[{"table":"t","mappings":m},{"table":"t","mappings":m}]}))
            .contains("duplicate target table 't'"));
        assert!(compile_err(json!({"targets":[{"table":"t","dedup_strategy":"lastest","business_key":["k"],"mappings":m}]}))
            .contains("unknown dedup_strategy 'lastest'"));
        assert!(compile_err(json!({"targets":[{"table":"t","dedup_strategy":"latest","mappings":m}]}))
            .contains("requires a non-empty business_key"));
        assert!(compile_err(json!({"targets":[{"table":"t","dedup_strategy":"latest","business_key":["kk"],"mappings":m}]}))
            .contains("'kk' is not produced"));
        assert!(compile_err(json!({"targets":[{"table":"t","grain":"lines","mappings":m}]}))
            .contains("unknown grain 'lines'"));
        assert!(compile_err(json!({"targets":[{"table":"t","grain":"header","mappings":[
            {"source":"tx[*].a","target":"a","unpack":"array_explode"}]}]}))
            .contains("requires grain 'line'"));
        assert!(compile_err(json!({"targets":[{"table":"t","mappings":[
            {"source":"k","target":"k","cast":"nope"}]}]}))
            .contains("nope"));
    }
}
