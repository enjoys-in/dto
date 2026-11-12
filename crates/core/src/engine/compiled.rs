//! Compiled transformation plans.
//!
//! A [`DtoSchema`] is compiled once — source paths parsed, transform chains
//! resolved to function pointers, required flags attached — so per-record work
//! is only path resolution and function calls. Configuration mistakes (bad
//! paths, unknown functions, `required` names with no mapping) are reported when
//! the engine is created instead of being skipped silently at transform time.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::functions::{self, TransformFn};
use crate::path::{ast::DtoPath, resolver, tokenizer};
use crate::recipe::converter::parse_source_and_transforms;
use crate::schema::model::{DtoSchema, FieldSchema};
use crate::schema::transform::{json_type_name, TransformError};

/// A compiled object mapping (the root schema, or a nested object field).
#[derive(Debug, Clone)]
pub(crate) struct CompiledObject {
    fields: Vec<CompiledField>,
}

#[derive(Debug, Clone)]
struct CompiledField {
    /// Output key.
    name: String,
    /// Dotted name used in error messages (`address.city` for nested fields).
    full_name: String,
    /// Whether the schema declared a source (`x-from` / recipe `source`).
    has_source: bool,
    /// Parsed source path (`None` when there is no source or it is empty).
    path: Option<DtoPath>,
    /// Inline (`a.b | trim`) transforms followed by explicit ones.
    chain: Vec<TransformFn>,
    default: Option<Value>,
    required: bool,
    /// Sub-mapping for `type: "object"` fields with `properties`.
    nested: Option<CompiledObject>,
}

impl CompiledObject {
    /// Compile a schema's root properties.
    pub(crate) fn from_schema(schema: &DtoSchema) -> Result<Self, TransformError> {
        Self::compile(&schema.properties, &schema.required, "")
    }

    fn compile(
        properties: &HashMap<String, FieldSchema>,
        required: &[String],
        prefix: &str,
    ) -> Result<Self, TransformError> {
        for name in required {
            if !properties.contains_key(name) {
                return Err(TransformError::Schema(format!(
                    "required field '{prefix}{name}' has no matching property/mapping"
                )));
            }
        }

        // Sorted for deterministic evaluation (and therefore deterministic errors).
        let mut names: Vec<&String> = properties.keys().collect();
        names.sort();

        let fields = names
            .into_iter()
            .map(|name| {
                let fs = &properties[name];
                let full_name = format!("{prefix}{name}");

                let (path_str, inline) = match fs.from.as_deref() {
                    Some(from) => {
                        let (p, t) = parse_source_and_transforms(from);
                        (Some(p), t)
                    }
                    None => (None, None),
                };

                let path = match path_str.as_deref() {
                    Some(p) if !p.is_empty() => Some(tokenizer::parse(p).map_err(|e| {
                        TransformError::Schema(format!(
                            "field '{full_name}': invalid source path '{p}': {e}"
                        ))
                    })?),
                    _ => None,
                };

                let chain_str = match (inline, fs.transform.as_deref()) {
                    (Some(i), Some(e)) => Some(format!("{i} | {e}")),
                    (Some(i), None) => Some(i),
                    (None, Some(e)) => Some(e.to_string()),
                    (None, None) => None,
                };
                let chain = match chain_str {
                    Some(c) => functions::compile_chain(&c).map_err(|e| {
                        TransformError::Schema(format!("field '{full_name}': {e}"))
                    })?,
                    None => Vec::new(),
                };

                let nested = if fs.field_type == "object" && !fs.properties.is_empty() {
                    Some(Self::compile(&fs.properties, &fs.required, &format!("{full_name}."))?)
                } else {
                    None
                };

                Ok(CompiledField {
                    name: name.clone(),
                    full_name,
                    has_source: fs.from.is_some(),
                    path,
                    chain,
                    default: fs.default.clone(),
                    required: required.contains(name),
                    nested,
                })
            })
            .collect::<Result<Vec<_>, TransformError>>()?;

        Ok(Self { fields })
    }

    /// Transform one input record. The input must be a JSON object.
    pub(crate) fn transform(&self, input: &Value) -> Result<Value, TransformError> {
        if !input.is_object() {
            return Err(TransformError::NotAnObject(json_type_name(input)));
        }
        self.build(input).map(Value::Object)
    }

    fn build(&self, input: &Value) -> Result<Map<String, Value>, TransformError> {
        let mut out = Map::new();
        for field in &self.fields {
            match field.produce(input)? {
                Some(v) => {
                    out.insert(field.name.clone(), v);
                }
                None if field.required => {
                    return Err(TransformError::MissingRequired { field: field.full_name.clone() });
                }
                None => {}
            }
        }
        Ok(out)
    }
}

impl CompiledField {
    fn produce(&self, input: &Value) -> Result<Option<Value>, TransformError> {
        // A nested object without a source of its own is assembled from the
        // current input (previously it was dropped from the output entirely).
        if let (Some(nested), false) = (&self.nested, self.has_source) {
            let value = Value::Object(nested.build(input)?);
            return self.apply_chain(value).map(Some);
        }

        let resolved = self.path.as_ref().and_then(|p| resolver::resolve(input, p));
        let value = match resolved {
            Some(v) if !v.is_null() => v,
            _ => match &self.default {
                Some(d) => d.clone(),
                None => return Ok(None),
            },
        };

        let mut value = self.apply_chain(value)?;
        if let Some(nested) = &self.nested {
            if value.is_object() {
                value = Value::Object(nested.build(&value)?);
            }
        }
        Ok(Some(value))
    }

    fn apply_chain(&self, value: Value) -> Result<Value, TransformError> {
        if self.chain.is_empty() {
            return Ok(value);
        }
        functions::apply_compiled_chain(&self.chain, value).map_err(|e| {
            TransformError::TransformFunction { field: self.full_name.clone(), message: e.to_string() }
        })
    }
}
