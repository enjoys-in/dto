//! Constraint validation for transformed output fields.

use serde_json::Value;

use crate::recipe::model::Constraint;

/// Errors during constraint validation.
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("field '{field}': type mismatch — expected {expected}, got {actual}")]
    TypeMismatch {
        field: String,
        expected: String,
        actual: String,
    },

    #[error("field '{field}': value {value} is below minimum {min}")]
    BelowMin {
        field: String,
        value: f64,
        min: f64,
    },

    #[error("field '{field}': value {value} exceeds maximum {max}")]
    AboveMax {
        field: String,
        value: f64,
        max: f64,
    },

    #[error("field '{field}': length {length} is below minimum {min_length}")]
    TooShort {
        field: String,
        length: usize,
        min_length: usize,
    },

    #[error("field '{field}': length {length} exceeds maximum {max_length}")]
    TooLong {
        field: String,
        length: usize,
        max_length: usize,
    },

    #[error("field '{field}': value not in allowed set")]
    NotAllowed { field: String },

    #[error("field '{field}': value does not match pattern '{pattern}'")]
    PatternMismatch { field: String, pattern: String },

    #[error("{0}")]
    Custom(String),
}

/// Validate a single field value against its constraint.
///
/// When the constraint defines a custom `message`, it replaces the built-in
/// error text.
pub fn validate_field(
    field_name: &str,
    value: &Value,
    constraint: &Constraint,
) -> Result<(), ValidationError> {
    check_field(field_name, value, constraint).map_err(|err| match constraint.message {
        Some(ref message) => ValidationError::Custom(format!("field '{field_name}': {message}")),
        None => err,
    })
}

fn check_field(
    field_name: &str,
    value: &Value,
    constraint: &Constraint,
) -> Result<(), ValidationError> {
    // Type check
    if let Some(ref expected_type) = constraint.value_type {
        let actual_type = json_type_name(value);
        if !type_matches(expected_type, value) {
            return Err(ValidationError::TypeMismatch {
                field: field_name.to_string(),
                expected: expected_type.clone(),
                actual: actual_type.to_string(),
            });
        }
    }

    // Numeric range check
    if let Some(n) = value.as_f64() {
        if let Some(min) = constraint.min {
            if n < min {
                return Err(ValidationError::BelowMin {
                    field: field_name.to_string(),
                    value: n,
                    min,
                });
            }
        }
        if let Some(max) = constraint.max {
            if n > max {
                return Err(ValidationError::AboveMax {
                    field: field_name.to_string(),
                    value: n,
                    max,
                });
            }
        }
    }

    // Length check (strings count characters, not UTF-8 bytes; arrays count items)
    let length = match value {
        Value::String(s) => Some(s.chars().count()),
        Value::Array(a) => Some(a.len()),
        _ => None,
    };
    if let Some(len) = length {
        if let Some(min_len) = constraint.min_length {
            if len < min_len {
                return Err(ValidationError::TooShort {
                    field: field_name.to_string(),
                    length: len,
                    min_length: min_len,
                });
            }
        }
        if let Some(max_len) = constraint.max_length {
            if len > max_len {
                return Err(ValidationError::TooLong {
                    field: field_name.to_string(),
                    length: len,
                    max_length: max_len,
                });
            }
        }
    }

    // Regex pattern (strings only, unanchored like JSON Schema)
    if let (Some(pattern), Value::String(s)) = (&constraint.pattern, value) {
        let regex = constraint
            .compiled_pattern
            .get_or_init(|| regex_lite::Regex::new(pattern).ok());
        let matched = regex.as_ref().is_some_and(|re| re.is_match(s));
        if !matched {
            return Err(ValidationError::PatternMismatch {
                field: field_name.to_string(),
                pattern: pattern.clone(),
            });
        }
    }

    // Allowed values (enum)
    if let Some(ref allowed) = constraint.allowed {
        if !allowed.contains(value) {
            return Err(ValidationError::NotAllowed {
                field: field_name.to_string(),
            });
        }
    }

    Ok(())
}

/// Validate all constrained fields in a transformed output object.
pub fn validate_output(
    output: &Value,
    constraints: &std::collections::HashMap<String, Constraint>,
) -> Result<(), Vec<ValidationError>> {
    if constraints.is_empty() {
        return Ok(());
    }

    let obj = match output.as_object() {
        Some(o) => o,
        None => return Ok(()), // Only validate objects
    };

    let errors: Vec<ValidationError> = constraints
        .iter()
        .filter_map(|(field, constraint)| {
            if let Some(value) = obj.get(field) {
                validate_field(field, value, constraint).err()
            } else {
                None // Missing field is handled by required checks, not constraints
            }
        })
        .collect();

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_matches(expected: &str, value: &Value) -> bool {
    match expected {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => {
            value.is_i64()
                || value.is_u64()
                || value.as_f64().is_some_and(|f| f.is_finite() && f.fract() == 0.0)
        }
        "boolean" | "bool" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true, // Unknown type → permissive
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn constraint(
        vtype: Option<&str>,
        min: Option<f64>,
        max: Option<f64>,
        min_len: Option<usize>,
        max_len: Option<usize>,
    ) -> Constraint {
        Constraint {
            value_type: vtype.map(String::from),
            min,
            max,
            min_length: min_len,
            max_length: max_len,
            pattern: None,
            allowed: None,
            message: None,
            ..Default::default()
        }
    }

    #[test]
    fn valid_string() {
        let c = constraint(Some("string"), None, None, None, Some(10));
        assert!(validate_field("name", &json!("hello"), &c).is_ok());
    }

    #[test]
    fn type_mismatch() {
        let c = constraint(Some("string"), None, None, None, None);
        assert!(validate_field("name", &json!(42), &c).is_err());
    }

    #[test]
    fn number_below_min() {
        let c = constraint(Some("number"), Some(0.0), Some(150.0), None, None);
        assert!(validate_field("age", &json!(-5), &c).is_err());
    }

    #[test]
    fn number_above_max() {
        let c = constraint(Some("number"), Some(0.0), Some(150.0), None, None);
        assert!(validate_field("age", &json!(200), &c).is_err());
    }

    #[test]
    fn number_in_range() {
        let c = constraint(Some("number"), Some(0.0), Some(150.0), None, None);
        assert!(validate_field("age", &json!(30), &c).is_ok());
    }

    #[test]
    fn string_too_long() {
        let c = constraint(Some("string"), None, None, None, Some(5));
        assert!(validate_field("code", &json!("abcdef"), &c).is_err());
    }

    #[test]
    fn string_too_short() {
        let c = constraint(Some("string"), None, None, Some(3), None);
        assert!(validate_field("code", &json!("ab"), &c).is_err());
    }

    #[test]
    fn allowed_values() {
        let c = Constraint {
            allowed: Some(vec![json!("admin"), json!("user")]),
            ..constraint(None, None, None, None, None)
        };
        assert!(validate_field("role", &json!("admin"), &c).is_ok());
        assert!(validate_field("role", &json!("guest"), &c).is_err());
    }

    #[test]
    fn integer_type_rejects_fractions() {
        let c = constraint(Some("integer"), None, None, None, None);
        assert!(validate_field("n", &json!(3), &c).is_ok());
        assert!(validate_field("n", &json!(3.0), &c).is_ok());
        assert!(validate_field("n", &json!(2.5), &c).is_err());
        assert!(validate_field("n", &json!("3"), &c).is_err());
    }

    #[test]
    fn string_length_counts_characters_not_bytes() {
        let c = constraint(Some("string"), None, None, None, Some(4));
        // "José" is 4 characters but 5 UTF-8 bytes.
        assert!(validate_field("name", &json!("José"), &c).is_ok());
        assert!(validate_field("name", &json!("Josée"), &c).is_err());
    }

    #[test]
    fn custom_message_replaces_default_text() {
        let c = Constraint {
            message: Some("must be an adult".into()),
            ..constraint(Some("number"), Some(18.0), None, None, None)
        };
        let err = validate_field("age", &json!(12), &c).unwrap_err().to_string();
        assert_eq!(err, "field 'age': must be an adult");
    }

    #[test]
    fn pattern_is_unanchored_and_string_only() {
        let c = Constraint { pattern: Some(r"\d{3}".into()), ..Default::default() };
        assert!(validate_field("code", &json!("ab123cd"), &c).is_ok());
        assert!(validate_field("code", &json!("ab12cd"), &c).is_err());
        // Non-strings are not pattern-checked (use `type` for that).
        assert!(validate_field("code", &json!(12), &c).is_ok());

        let anchored = Constraint { pattern: Some(r"^[A-Z]{3}$".into()), ..Default::default() };
        assert!(validate_field("ccy", &json!("USD"), &anchored).is_ok());
        assert!(validate_field("ccy", &json!("USDX"), &anchored).is_err());
    }
}
