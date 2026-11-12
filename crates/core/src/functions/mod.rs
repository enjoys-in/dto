//! Built-in transform functions for the DTO transformation engine.
//!
//! Each function takes a [`serde_json::Value`] and returns a transformed value,
//! or a [`FunctionError`] if the input type is incompatible.

use serde_json::Value;
use thiserror::Error;

/// Errors that can occur when applying a transform function.
#[derive(Debug, Error)]
pub enum FunctionError {
    /// The requested function name is not recognized.
    #[error("unknown transform function: '{0}'")]
    UnknownFunction(String),

    /// The function cannot operate on the given value type.
    #[error("function '{function}' cannot be applied to {value_type}")]
    InvalidInput {
        /// Name of the function that was called.
        function: String,
        /// Human-readable description of the value type that was passed.
        value_type: String,
    },

    /// The value has the right type but its content could not be parsed
    /// (e.g. `"not a date"` passed to `date`).
    #[error("function '{function}' cannot parse {value}: expected {expected}")]
    InvalidValue {
        /// Name of the function that was called.
        function: String,
        /// The offending value (truncated for display).
        value: String,
        /// Description of the accepted formats.
        expected: String,
    },
}

/// Signature shared by every built-in transform function.
pub type TransformFn = fn(Value) -> Result<Value, FunctionError>;

/// Look up a built-in transform function (or one of its aliases) by name.
pub fn lookup(name: &str) -> Option<TransformFn> {
    let f: TransformFn = match name {
        "trim" => fn_trim,
        "lowercase" | "lower" => fn_lowercase,
        "uppercase" | "upper" => fn_uppercase,
        "to_string" | "string" => fn_to_string,
        "to_number" | "number" | "decimal" | "float" | "int" | "integer" => fn_to_number,
        "to_bool" | "bool" | "boolean" => fn_to_bool,
        "json" => fn_identity,
        "date" => fn_to_date,
        "timestamp" => fn_to_timestamp,
        "flatten" => fn_flatten,
        "length" | "len" => fn_length,
        "keys" => fn_keys,
        "values" => fn_values,
        "first" => fn_first,
        "last" => fn_last,
        "reverse" => fn_reverse,
        "unique" | "uniq" => fn_unique,
        "sort" => fn_sort,
        "compact" => fn_compact,
        _ => return None,
    };
    Some(f)
}

/// Apply a named transform function to a JSON value.
///
/// Returns the transformed value, or an error if the function name is unknown
/// or the input type is not supported by the function.
///
/// # Examples
///
/// ```
/// use serde_json::json;
/// use dto_core::functions::apply_transform;
///
/// let result = apply_transform("trim", json!("  hello  ")).unwrap();
/// assert_eq!(result, json!("hello"));
/// ```
pub fn apply_transform(name: &str, value: Value) -> Result<Value, FunctionError> {
    let f = lookup(name).ok_or_else(|| FunctionError::UnknownFunction(name.to_string()))?;
    f(value)
}

/// Parse a chain such as `"trim | lowercase"` into function pointers once, so it
/// can be applied to many values without re-parsing. Unknown names are an error.
pub fn compile_chain(chain: &str) -> Result<Vec<TransformFn>, FunctionError> {
    chain
        .replace("->", "|")
        .split(['|', ','])
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| lookup(name).ok_or_else(|| FunctionError::UnknownFunction(name.to_string())))
        .collect()
}

/// Apply a chain produced by [`compile_chain`], left to right.
pub fn apply_compiled_chain(chain: &[TransformFn], mut value: Value) -> Result<Value, FunctionError> {
    for f in chain {
        value = f(value)?;
    }
    Ok(value)
}

/// Apply a chain of transform functions separated by `|`, `,`, or `->`.
///
/// Functions are executed sequentially from left to right.
/// For example: `"trim | lowercase | to_string"` or `"to_string | trim"`
///
/// # Examples
///
/// ```
/// use serde_json::json;
/// use dto_core::functions::apply_transform_chain;
///
/// let result = apply_transform_chain("trim | lowercase", json!("  HELLO WORLD  ")).unwrap();
/// assert_eq!(result, json!("hello world"));
///
/// let num_as_str = apply_transform_chain("to_string | trim", json!(12345)).unwrap();
/// assert_eq!(num_as_str, json!("12345"));
/// ```
pub fn apply_transform_chain(chain: &str, value: Value) -> Result<Value, FunctionError> {
    apply_compiled_chain(&compile_chain(chain)?, value)
}

/// Returns a list of all available transform function names (including aliases).
pub fn available_functions() -> Vec<&'static str> {
    vec![
        "trim",
        "lowercase",
        "lower",
        "uppercase",
        "upper",
        "to_string",
        "string",
        "to_number",
        "number",
        "decimal",
        "float",
        "int",
        "integer",
        "to_bool",
        "bool",
        "boolean",
        "json",
        "date",
        "timestamp",
        "flatten",
        "length",
        "len",
        "keys",
        "values",
        "first",
        "last",
        "reverse",
        "unique",
        "uniq",
        "sort",
        "compact",
    ]
}

// ─── Helper ────────────────────────────────────────────────────────────────

/// Return a human-readable type name for a [`Value`].
fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Convenience to build an [`FunctionError::InvalidInput`].
fn invalid(function: &str, value: &Value) -> FunctionError {
    FunctionError::InvalidInput {
        function: function.to_string(),
        value_type: value_type_name(value).to_string(),
    }
}

// ─── Function implementations ──────────────────────────────────────────────

/// Trim whitespace from a string value (or array of strings). Non-string values are returned as-is.
fn fn_trim(value: Value) -> Result<Value, FunctionError> {
    match value {
        Value::String(s) => Ok(Value::String(s.trim().to_string())),
        Value::Array(arr) => {
            let mapped: Result<Vec<_>, _> = arr.into_iter().map(fn_trim).collect();
            Ok(Value::Array(mapped?))
        }
        other => Ok(other),
    }
}

/// Convert a string (or array of strings) to lowercase. Errors on non-string values.
fn fn_lowercase(value: Value) -> Result<Value, FunctionError> {
    match value {
        Value::String(s) => Ok(Value::String(s.to_lowercase())),
        Value::Array(arr) => {
            let mapped: Result<Vec<_>, _> = arr.into_iter().map(fn_lowercase).collect();
            Ok(Value::Array(mapped?))
        }
        other => Err(invalid("lowercase", &other)),
    }
}

/// Convert a string (or array of strings) to uppercase. Errors on non-string values.
fn fn_uppercase(value: Value) -> Result<Value, FunctionError> {
    match value {
        Value::String(s) => Ok(Value::String(s.to_uppercase())),
        Value::Array(arr) => {
            let mapped: Result<Vec<_>, _> = arr.into_iter().map(fn_uppercase).collect();
            Ok(Value::Array(mapped?))
        }
        other => Err(invalid("uppercase", &other)),
    }
}

/// Convert any value to its string representation.
///
/// - Strings are returned as-is.
/// - Numbers, bools, and null produce their literal text.
/// - Objects and arrays produce compact JSON.
fn fn_to_string(value: Value) -> Result<Value, FunctionError> {
    let s = match &value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        // Arrays and objects → compact JSON
        _ => serde_json::to_string(&value).unwrap_or_default(),
    };
    Ok(Value::String(s))
}

/// Parse a value to a number.
///
/// - Numbers are returned as-is.
/// - Strings are parsed as f64.
/// - Bools: `true` → 1, `false` → 0.
fn fn_to_number(value: Value) -> Result<Value, FunctionError> {
    match &value {
        Value::Number(_) => Ok(value),
        Value::String(s) => {
            let n: f64 = s
                .trim()
                .parse()
                .map_err(|_| invalid("to_number", &value))?;
            Ok(serde_json::Number::from_f64(n)
                .map(Value::Number)
                .unwrap_or(Value::Null))
        }
        Value::Bool(b) => {
            let n = if *b { 1 } else { 0 };
            Ok(Value::Number(serde_json::Number::from(n)))
        }
        _ => Err(invalid("to_number", &value)),
    }
}

/// Convert a value to a boolean.
///
/// - Bool → returned as-is.
/// - Number → `0` is `false`, everything else is `true`.
/// - String → `"true"` / `"1"` → `true`; `"false"` / `"0"` / `""` → `false`.
/// - Null → `false`.
/// - Otherwise → error.
fn fn_to_bool(value: Value) -> Result<Value, FunctionError> {
    match &value {
        Value::Bool(_) => Ok(value),
        Value::Number(n) => {
            let is_zero = n.as_f64().map(|f| f == 0.0).unwrap_or(false);
            Ok(Value::Bool(!is_zero))
        }
        Value::String(s) => match s.as_str() {
            "true" | "1" => Ok(Value::Bool(true)),
            "false" | "0" | "" => Ok(Value::Bool(false)),
            _ => Err(invalid("to_bool", &value)),
        },
        Value::Null => Ok(Value::Bool(false)),
        _ => Err(invalid("to_bool", &value)),
    }
}

/// `json` cast: pass the value through unchanged.
fn fn_identity(value: Value) -> Result<Value, FunctionError> {
    Ok(value)
}

const DATE_FORMATS: &str =
    "an ISO-8601 date (YYYY-MM-DD), an ISO-8601 date-time, or Unix epoch seconds/milliseconds";

/// Build an [`FunctionError::InvalidValue`] for an unparsable date/time value.
fn unparsable(function: &str, value: &Value) -> FunctionError {
    let mut shown = value.to_string();
    if shown.chars().count() > 60 {
        shown = shown.chars().take(57).collect::<String>() + "...";
    }
    FunctionError::InvalidValue {
        function: function.to_string(),
        value: shown,
        expected: DATE_FORMATS.to_string(),
    }
}

/// Convert a value to a calendar date string (`YYYY-MM-DD`).
///
/// - Strings must be an ISO-8601 date or date-time (`T` or space separator,
///   optional seconds, fraction and `Z`/`±HH:MM` offset). The calendar date is
///   taken as written; no time-zone conversion is applied.
/// - Numbers are Unix epoch time in UTC: seconds, or milliseconds when the
///   magnitude is ≥ 1e11 (i.e. any date after 1973-03-03 given in ms).
/// - Anything else (including invalid dates like `2024-02-30`) is an error.
fn fn_to_date(value: Value) -> Result<Value, FunctionError> {
    match &value {
        Value::String(s) => match parse_iso_datetime(s.trim()) {
            Some(dt) => Ok(Value::String(format!("{:04}-{:02}-{:02}", dt.year, dt.month, dt.day))),
            None => Err(unparsable("date", &value)),
        },
        Value::Number(n) => match epoch_millis(n) {
            Some(ms) => {
                let (y, m, d) = civil_from_days(ms.div_euclid(MS_PER_DAY));
                Ok(Value::String(format!("{y:04}-{m:02}-{d:02}")))
            }
            None => Err(unparsable("date", &value)),
        },
        _ => Err(invalid("date", &value)),
    }
}

/// Convert a value to a normalized ISO-8601 timestamp
/// (`YYYY-MM-DDTHH:MM:SS[.fraction][Z|±HH:MM]`).
///
/// - Strings use the same grammar as [`fn_to_date`]. A date-only string becomes
///   midnight; a missing offset stays missing (no time zone is assumed); an
///   offset is kept as written (normalized to `±HH:MM`).
/// - Numbers are Unix epoch time (seconds, or ms when ≥ 1e11) rendered in UTC with `Z`.
fn fn_to_timestamp(value: Value) -> Result<Value, FunctionError> {
    match &value {
        Value::String(s) => match parse_iso_datetime(s.trim()) {
            Some(dt) => {
                let (h, mi, sec, frac) = dt.time.unwrap_or((0, 0, 0, String::new()));
                let mut out = format!(
                    "{:04}-{:02}-{:02}T{h:02}:{mi:02}:{sec:02}",
                    dt.year, dt.month, dt.day
                );
                if !frac.is_empty() {
                    out.push('.');
                    out.push_str(&frac);
                }
                if let Some(offset) = dt.offset {
                    out.push_str(&offset);
                }
                Ok(Value::String(out))
            }
            None => Err(unparsable("timestamp", &value)),
        },
        Value::Number(n) => match epoch_millis(n) {
            Some(ms) => {
                let (y, m, d) = civil_from_days(ms.div_euclid(MS_PER_DAY));
                let ms_of_day = ms.rem_euclid(MS_PER_DAY);
                let (h, mi, sec, milli) = (
                    ms_of_day / 3_600_000,
                    ms_of_day / 60_000 % 60,
                    ms_of_day / 1000 % 60,
                    ms_of_day % 1000,
                );
                let mut out = format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{sec:02}");
                if milli != 0 {
                    out.push_str(&format!(".{milli:03}"));
                }
                out.push('Z');
                Ok(Value::String(out))
            }
            None => Err(unparsable("timestamp", &value)),
        },
        _ => Err(invalid("timestamp", &value)),
    }
}

// ─── Date/time parsing helpers (no external dependencies) ──────────────────

const MS_PER_DAY: i64 = 86_400_000;
/// 0000-01-01T00:00:00Z and 9999-12-31T23:59:59.999Z in epoch milliseconds.
const MIN_EPOCH_MS: i64 = -62_167_219_200_000;
const MAX_EPOCH_MS: i64 = 253_402_300_799_999;

/// Pieces of a parsed ISO-8601 date / date-time.
struct DateTimeParts {
    year: i64,
    month: u32,
    day: u32,
    /// (hour, minute, second, fractional-second digits)
    time: Option<(u32, u32, u32, String)>,
    /// `"Z"` or `"±HH:MM"`.
    offset: Option<String>,
}

/// Parse `YYYY-MM-DD[(T|t| )HH:MM[:SS[(.|,)fraction]][Z|z|±HH[:]MM|±HH]]`,
/// validating every calendar and clock field.
fn parse_iso_datetime(s: &str) -> Option<DateTimeParts> {
    let b = s.as_bytes();
    let mut i = 0usize;

    let digits = |i: &mut usize, n: usize| -> Option<u32> {
        let chunk = b.get(*i..*i + n)?;
        if !chunk.iter().all(u8::is_ascii_digit) {
            return None;
        }
        *i += n;
        Some(chunk.iter().fold(0u32, |acc, d| acc * 10 + u32::from(d - b'0')))
    };
    let expect = |i: &mut usize, c: u8| -> Option<()> {
        (b.get(*i) == Some(&c)).then(|| *i += 1)
    };

    let year = i64::from(digits(&mut i, 4)?);
    expect(&mut i, b'-')?;
    let month = digits(&mut i, 2)?;
    expect(&mut i, b'-')?;
    let day = digits(&mut i, 2)?;
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    if i == b.len() {
        return Some(DateTimeParts { year, month, day, time: None, offset: None });
    }

    if !matches!(b[i], b'T' | b't' | b' ') {
        return None;
    }
    i += 1;
    let hour = digits(&mut i, 2)?;
    expect(&mut i, b':')?;
    let minute = digits(&mut i, 2)?;
    let mut second = 0;
    let mut fraction = String::new();
    if b.get(i) == Some(&b':') {
        i += 1;
        second = digits(&mut i, 2)?;
        if matches!(b.get(i), Some(b'.') | Some(b',')) {
            i += 1;
            let start = i;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            if i == start {
                return None;
            }
            fraction = s[start..i].to_string();
        }
    }
    // Allow a leap second (60); reject everything else out of range.
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let offset = match b.get(i) {
        None => None,
        Some(b'Z') | Some(b'z') => {
            i += 1;
            Some("Z".to_string())
        }
        Some(&sign @ (b'+' | b'-')) => {
            i += 1;
            let oh = digits(&mut i, 2)?;
            let om = if i == b.len() {
                0
            } else {
                if b[i] == b':' {
                    i += 1;
                }
                digits(&mut i, 2)?
            };
            if oh > 23 || om > 59 {
                return None;
            }
            Some(format!("{}{oh:02}:{om:02}", sign as char))
        }
        Some(_) => return None,
    };
    if i != b.len() {
        return None;
    }

    Some(DateTimeParts {
        year,
        month,
        day,
        time: Some((hour, minute, second, fraction)),
        offset,
    })
}

fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Interpret a JSON number as Unix epoch milliseconds (seconds when |n| < 1e11).
fn epoch_millis(n: &serde_json::Number) -> Option<i64> {
    let f = n.as_f64()?;
    if !f.is_finite() {
        return None;
    }
    let ms = (if f.abs() >= 1e11 { f } else { f * 1000.0 }).round();
    if ms < MIN_EPOCH_MS as f64 || ms > MAX_EPOCH_MS as f64 {
        return None;
    }
    Some(ms as i64)
}

/// Days since 1970-01-01 → (year, month, day) in the proleptic Gregorian calendar.
/// (Howard Hinnant's `civil_from_days` algorithm.)
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Flatten one level of nested arrays. Non-array values are returned as-is.
fn fn_flatten(value: Value) -> Result<Value, FunctionError> {
    match value {
        Value::Array(arr) => {
            let mut flat = Vec::new();
            for item in arr {
                match item {
                    Value::Array(inner) => flat.extend(inner),
                    other => flat.push(other),
                }
            }
            Ok(Value::Array(flat))
        }
        other => Ok(other),
    }
}

/// Return the length of a string, array, or object.
fn fn_length(value: Value) -> Result<Value, FunctionError> {
    let len = match &value {
        Value::String(s) => s.len(),
        Value::Array(a) => a.len(),
        Value::Object(o) => o.len(),
        _ => return Err(invalid("length", &value)),
    };
    Ok(Value::Number(serde_json::Number::from(len)))
}

/// Return the keys of an object as an array of strings.
fn fn_keys(value: Value) -> Result<Value, FunctionError> {
    match value {
        Value::Object(map) => {
            let keys: Vec<Value> = map.keys().map(|k| Value::String(k.clone())).collect();
            Ok(Value::Array(keys))
        }
        other => Err(invalid("keys", &other)),
    }
}

/// Return the values of an object as an array.
fn fn_values(value: Value) -> Result<Value, FunctionError> {
    match value {
        Value::Object(map) => {
            let vals: Vec<Value> = map.into_iter().map(|(_, v)| v).collect();
            Ok(Value::Array(vals))
        }
        other => Err(invalid("values", &other)),
    }
}

/// Return the first element of an array, or `null` if empty.
fn fn_first(value: Value) -> Result<Value, FunctionError> {
    match value {
        Value::Array(mut arr) => {
            if arr.is_empty() {
                Ok(Value::Null)
            } else {
                Ok(arr.swap_remove(0))
            }
        }
        other => Err(invalid("first", &other)),
    }
}

/// Return the last element of an array, or `null` if empty.
fn fn_last(value: Value) -> Result<Value, FunctionError> {
    match value {
        Value::Array(mut arr) => Ok(arr.pop().unwrap_or(Value::Null)),
        other => Err(invalid("last", &other)),
    }
}

/// Reverse an array or a string.
fn fn_reverse(value: Value) -> Result<Value, FunctionError> {
    match value {
        Value::Array(mut arr) => {
            arr.reverse();
            Ok(Value::Array(arr))
        }
        Value::String(s) => Ok(Value::String(s.chars().rev().collect())),
        other => Err(invalid("reverse", &other)),
    }
}

/// Remove duplicate values from an array, preserving insertion order.
///
/// Equality is determined by comparing the JSON serialization of each element.
fn fn_unique(value: Value) -> Result<Value, FunctionError> {
    match value {
        Value::Array(arr) => {
            let mut seen = std::collections::HashSet::new();
            let mut result = Vec::new();
            for item in arr {
                // Use compact JSON as the dedup key
                let key = serde_json::to_string(&item).unwrap_or_default();
                if seen.insert(key) {
                    result.push(item);
                }
            }
            Ok(Value::Array(result))
        }
        other => Err(invalid("unique", &other)),
    }
}

/// Sort an array of strings or an array of numbers.
///
/// Mixed-type arrays produce an error.
fn fn_sort(value: Value) -> Result<Value, FunctionError> {
    match value {
        Value::Array(arr) => {
            if arr.is_empty() {
                return Ok(Value::Array(arr));
            }

            // Determine element types
            let all_strings = arr.iter().all(|v| v.is_string());
            let all_numbers = arr.iter().all(|v| v.is_number());

            if all_strings {
                let mut strings: Vec<String> = arr
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                strings.sort();
                Ok(Value::Array(
                    strings.into_iter().map(Value::String).collect(),
                ))
            } else if all_numbers {
                let mut nums: Vec<(f64, Value)> = arr
                    .into_iter()
                    .map(|v| {
                        let f = v.as_f64().unwrap_or(0.0);
                        (f, v)
                    })
                    .collect();
                nums.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                Ok(Value::Array(nums.into_iter().map(|(_, v)| v).collect()))
            } else {
                Err(FunctionError::InvalidInput {
                    function: "sort".to_string(),
                    value_type: "mixed-type array".to_string(),
                })
            }
        }
        other => Err(invalid("sort", &other)),
    }
}

/// Remove `null` values from an array. Non-array values are returned as-is.
fn fn_compact(value: Value) -> Result<Value, FunctionError> {
    match value {
        Value::Array(arr) => {
            let filtered: Vec<Value> = arr.into_iter().filter(|v| !v.is_null()).collect();
            Ok(Value::Array(filtered))
        }
        other => Ok(other),
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── trim ────────────────────────────────────────────────────────────

    #[test]
    fn trim_string_with_whitespace() {
        assert_eq!(apply_transform("trim", json!("  hello  ")).unwrap(), json!("hello"));
    }

    #[test]
    fn trim_string_no_whitespace() {
        assert_eq!(apply_transform("trim", json!("hello")).unwrap(), json!("hello"));
    }

    #[test]
    fn trim_empty_string() {
        assert_eq!(apply_transform("trim", json!("   ")).unwrap(), json!(""));
    }

    #[test]
    fn trim_non_string_passes_through() {
        assert_eq!(apply_transform("trim", json!(42)).unwrap(), json!(42));
        assert_eq!(apply_transform("trim", json!(null)).unwrap(), json!(null));
        assert_eq!(apply_transform("trim", json!(true)).unwrap(), json!(true));
    }

    // ── lowercase ───────────────────────────────────────────────────────

    #[test]
    fn lowercase_string() {
        assert_eq!(apply_transform("lowercase", json!("HELLO")).unwrap(), json!("hello"));
        assert_eq!(apply_transform("lower", json!("HELLO")).unwrap(), json!("hello"));
    }

    #[test]
    fn lowercase_mixed_case() {
        assert_eq!(apply_transform("lowercase", json!("HeLLo WoRLd")).unwrap(), json!("hello world"));
    }

    #[test]
    fn lowercase_non_string_errors() {
        assert!(apply_transform("lowercase", json!(42)).is_err());
    }

    // ── uppercase ───────────────────────────────────────────────────────

    #[test]
    fn uppercase_string() {
        assert_eq!(apply_transform("uppercase", json!("hello")).unwrap(), json!("HELLO"));
        assert_eq!(apply_transform("upper", json!("hello")).unwrap(), json!("HELLO"));
    }

    #[test]
    fn uppercase_non_string_errors() {
        assert!(apply_transform("uppercase", json!(true)).is_err());
    }

    // ── to_string ───────────────────────────────────────────────────────

    #[test]
    fn to_string_from_number() {
        assert_eq!(apply_transform("to_string", json!(123)).unwrap(), json!("123"));
        assert_eq!(apply_transform("string", json!(1.5)).unwrap(), json!("1.5"));
    }

    #[test]
    fn to_string_from_bool() {
        assert_eq!(apply_transform("to_string", json!(true)).unwrap(), json!("true"));
        assert_eq!(apply_transform("to_string", json!(false)).unwrap(), json!("false"));
    }

    #[test]
    fn to_string_from_null() {
        assert_eq!(apply_transform("to_string", json!(null)).unwrap(), json!("null"));
    }

    #[test]
    fn to_string_from_string_passthrough() {
        assert_eq!(apply_transform("to_string", json!("hello")).unwrap(), json!("hello"));
    }

    #[test]
    fn to_string_from_array() {
        let result = apply_transform("to_string", json!([1, 2, 3])).unwrap();
        assert_eq!(result, json!("[1,2,3]"));
    }

    #[test]
    fn to_string_from_object() {
        let result = apply_transform("to_string", json!({"a": 1})).unwrap();
        assert_eq!(result, json!(r#"{"a":1}"#));
    }

    // ── to_number ───────────────────────────────────────────────────────

    #[test]
    fn to_number_from_string() {
        assert_eq!(apply_transform("to_number", json!("42")).unwrap(), json!(42.0));
        assert_eq!(apply_transform("number", json!("3.14")).unwrap(), json!(3.14));
    }

    #[test]
    fn to_number_from_string_with_whitespace() {
        assert_eq!(apply_transform("to_number", json!("  99  ")).unwrap(), json!(99.0));
    }

    #[test]
    fn to_number_from_number_passthrough() {
        assert_eq!(apply_transform("to_number", json!(7)).unwrap(), json!(7));
    }

    #[test]
    fn to_number_from_bool() {
        assert_eq!(apply_transform("to_number", json!(true)).unwrap(), json!(1));
        assert_eq!(apply_transform("to_number", json!(false)).unwrap(), json!(0));
    }

    #[test]
    fn to_number_from_unparseable_string_errors() {
        assert!(apply_transform("to_number", json!("abc")).is_err());
    }

    #[test]
    fn to_number_from_null_errors() {
        assert!(apply_transform("to_number", json!(null)).is_err());
    }

    #[test]
    fn to_number_from_array_errors() {
        assert!(apply_transform("to_number", json!([1])).is_err());
    }

    // ── to_bool ─────────────────────────────────────────────────────────

    #[test]
    fn to_bool_from_bool() {
        assert_eq!(apply_transform("to_bool", json!(true)).unwrap(), json!(true));
        assert_eq!(apply_transform("bool", json!(false)).unwrap(), json!(false));
    }

    #[test]
    fn to_bool_from_number() {
        assert_eq!(apply_transform("to_bool", json!(0)).unwrap(), json!(false));
        assert_eq!(apply_transform("to_bool", json!(1)).unwrap(), json!(true));
        assert_eq!(apply_transform("to_bool", json!(-1)).unwrap(), json!(true));
        assert_eq!(apply_transform("to_bool", json!(0.0)).unwrap(), json!(false));
    }

    #[test]
    fn to_bool_from_string() {
        assert_eq!(apply_transform("to_bool", json!("true")).unwrap(), json!(true));
        assert_eq!(apply_transform("to_bool", json!("1")).unwrap(), json!(true));
        assert_eq!(apply_transform("to_bool", json!("false")).unwrap(), json!(false));
        assert_eq!(apply_transform("to_bool", json!("0")).unwrap(), json!(false));
        assert_eq!(apply_transform("to_bool", json!("")).unwrap(), json!(false));
    }

    #[test]
    fn to_bool_from_unrecognized_string_errors() {
        assert!(apply_transform("to_bool", json!("yes")).is_err());
    }

    #[test]
    fn to_bool_from_null() {
        assert_eq!(apply_transform("to_bool", json!(null)).unwrap(), json!(false));
    }

    #[test]
    fn to_bool_from_array_errors() {
        assert!(apply_transform("to_bool", json!([1, 2])).is_err());
    }

    #[test]
    fn to_bool_from_object_errors() {
        assert!(apply_transform("to_bool", json!({"a": 1})).is_err());
    }

    // ── flatten ─────────────────────────────────────────────────────────

    #[test]
    fn flatten_nested_arrays() {
        assert_eq!(
            apply_transform("flatten", json!([[1, 2], [3, 4], [5]])).unwrap(),
            json!([1, 2, 3, 4, 5])
        );
    }

    #[test]
    fn flatten_mixed() {
        assert_eq!(
            apply_transform("flatten", json!([1, [2, 3], 4])).unwrap(),
            json!([1, 2, 3, 4])
        );
    }

    #[test]
    fn flatten_already_flat() {
        assert_eq!(
            apply_transform("flatten", json!([1, 2, 3])).unwrap(),
            json!([1, 2, 3])
        );
    }

    #[test]
    fn flatten_non_array_passthrough() {
        assert_eq!(apply_transform("flatten", json!("hello")).unwrap(), json!("hello"));
        assert_eq!(apply_transform("flatten", json!(42)).unwrap(), json!(42));
    }

    #[test]
    fn flatten_empty_array() {
        assert_eq!(apply_transform("flatten", json!([])).unwrap(), json!([]));
    }

    // ── length ──────────────────────────────────────────────────────────

    #[test]
    fn length_of_string() {
        assert_eq!(apply_transform("length", json!("hello")).unwrap(), json!(5));
        assert_eq!(apply_transform("len", json!("")).unwrap(), json!(0));
    }

    #[test]
    fn length_of_array() {
        assert_eq!(apply_transform("length", json!([1, 2, 3])).unwrap(), json!(3));
    }

    #[test]
    fn length_of_object() {
        assert_eq!(apply_transform("length", json!({"a": 1, "b": 2})).unwrap(), json!(2));
    }

    #[test]
    fn length_of_number_errors() {
        assert!(apply_transform("length", json!(42)).is_err());
    }

    #[test]
    fn length_of_null_errors() {
        assert!(apply_transform("length", json!(null)).is_err());
    }

    // ── keys ────────────────────────────────────────────────────────────

    #[test]
    fn keys_of_object() {
        let result = apply_transform("keys", json!({"b": 2, "a": 1})).unwrap();
        let arr = result.as_array().unwrap();
        // serde_json preserves insertion order by default — just check contents
        let mut keys: Vec<&str> = arr.iter().map(|v| v.as_str().unwrap()).collect();
        keys.sort();
        assert_eq!(keys, vec!["a", "b"]);
    }

    #[test]
    fn keys_of_empty_object() {
        assert_eq!(apply_transform("keys", json!({})).unwrap(), json!([]));
    }

    #[test]
    fn keys_of_non_object_errors() {
        assert!(apply_transform("keys", json!([1])).is_err());
        assert!(apply_transform("keys", json!("hello")).is_err());
    }

    // ── values ──────────────────────────────────────────────────────────

    #[test]
    fn values_of_object() {
        let result = apply_transform("values", json!({"a": 1, "b": 2})).unwrap();
        let arr = result.as_array().unwrap();
        let mut nums: Vec<i64> = arr.iter().map(|v| v.as_i64().unwrap()).collect();
        nums.sort();
        assert_eq!(nums, vec![1, 2]);
    }

    #[test]
    fn values_of_non_object_errors() {
        assert!(apply_transform("values", json!("hello")).is_err());
    }

    // ── first ───────────────────────────────────────────────────────────

    #[test]
    fn first_of_array() {
        assert_eq!(apply_transform("first", json!([10, 20, 30])).unwrap(), json!(10));
    }

    #[test]
    fn first_of_empty_array() {
        assert_eq!(apply_transform("first", json!([])).unwrap(), json!(null));
    }

    #[test]
    fn first_of_non_array_errors() {
        assert!(apply_transform("first", json!("hello")).is_err());
    }

    // ── last ────────────────────────────────────────────────────────────

    #[test]
    fn last_of_array() {
        assert_eq!(apply_transform("last", json!([10, 20, 30])).unwrap(), json!(30));
    }

    #[test]
    fn last_of_empty_array() {
        assert_eq!(apply_transform("last", json!([])).unwrap(), json!(null));
    }

    #[test]
    fn last_of_non_array_errors() {
        assert!(apply_transform("last", json!(42)).is_err());
    }

    // ── reverse ─────────────────────────────────────────────────────────

    #[test]
    fn reverse_array() {
        assert_eq!(
            apply_transform("reverse", json!([1, 2, 3])).unwrap(),
            json!([3, 2, 1])
        );
    }

    #[test]
    fn reverse_string() {
        assert_eq!(apply_transform("reverse", json!("abcd")).unwrap(), json!("dcba"));
    }

    #[test]
    fn reverse_empty_array() {
        assert_eq!(apply_transform("reverse", json!([])).unwrap(), json!([]));
    }

    #[test]
    fn reverse_empty_string() {
        assert_eq!(apply_transform("reverse", json!("")).unwrap(), json!(""));
    }

    #[test]
    fn reverse_non_array_non_string_errors() {
        assert!(apply_transform("reverse", json!(42)).is_err());
    }

    // ── unique ──────────────────────────────────────────────────────────

    #[test]
    fn unique_array_numbers() {
        assert_eq!(
            apply_transform("unique", json!([1, 2, 2, 3, 1])).unwrap(),
            json!([1, 2, 3])
        );
    }

    #[test]
    fn unique_array_strings() {
        assert_eq!(
            apply_transform("uniq", json!(["a", "b", "a", "c"])).unwrap(),
            json!(["a", "b", "c"])
        );
    }

    #[test]
    fn unique_preserves_order() {
        assert_eq!(
            apply_transform("unique", json!([3, 1, 2, 1, 3])).unwrap(),
            json!([3, 1, 2])
        );
    }

    #[test]
    fn unique_empty() {
        assert_eq!(apply_transform("unique", json!([])).unwrap(), json!([]));
    }

    #[test]
    fn unique_non_array_errors() {
        assert!(apply_transform("unique", json!("hello")).is_err());
    }

    // ── sort ────────────────────────────────────────────────────────────

    #[test]
    fn sort_strings() {
        assert_eq!(
            apply_transform("sort", json!(["c", "a", "b"])).unwrap(),
            json!(["a", "b", "c"])
        );
    }

    #[test]
    fn sort_numbers() {
        assert_eq!(
            apply_transform("sort", json!([3, 1, 2])).unwrap(),
            json!([1, 2, 3])
        );
    }

    #[test]
    fn sort_empty_array() {
        assert_eq!(apply_transform("sort", json!([])).unwrap(), json!([]));
    }

    #[test]
    fn sort_mixed_type_errors() {
        assert!(apply_transform("sort", json!([1, "a"])).is_err());
    }

    #[test]
    fn sort_non_array_errors() {
        assert!(apply_transform("sort", json!("hello")).is_err());
    }

    #[test]
    fn sort_negative_numbers() {
        assert_eq!(
            apply_transform("sort", json!([-3, 0, 2, -1])).unwrap(),
            json!([-3, -1, 0, 2])
        );
    }

    // ── compact ─────────────────────────────────────────────────────────

    #[test]
    fn compact_removes_nulls() {
        assert_eq!(
            apply_transform("compact", json!([1, null, 2, null, 3])).unwrap(),
            json!([1, 2, 3])
        );
    }

    #[test]
    fn compact_no_nulls() {
        assert_eq!(
            apply_transform("compact", json!([1, 2, 3])).unwrap(),
            json!([1, 2, 3])
        );
    }

    #[test]
    fn compact_all_nulls() {
        assert_eq!(
            apply_transform("compact", json!([null, null])).unwrap(),
            json!([])
        );
    }

    #[test]
    fn compact_non_array_passthrough() {
        assert_eq!(apply_transform("compact", json!("hello")).unwrap(), json!("hello"));
        assert_eq!(apply_transform("compact", json!(42)).unwrap(), json!(42));
    }

    // ── unknown function ────────────────────────────────────────────────

    #[test]
    fn unknown_function_errors() {
        let err = apply_transform("nonexistent", json!(42));
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("nonexistent"));
    }

    // ── available_functions ─────────────────────────────────────────────

    #[test]
    fn available_functions_includes_all() {
        let fns = available_functions();
        assert!(fns.contains(&"trim"));
        assert!(fns.contains(&"lowercase"));
        assert!(fns.contains(&"lower"));
        assert!(fns.contains(&"uppercase"));
        assert!(fns.contains(&"upper"));
        assert!(fns.contains(&"to_string"));
        assert!(fns.contains(&"string"));
        assert!(fns.contains(&"to_number"));
        assert!(fns.contains(&"number"));
        assert!(fns.contains(&"to_bool"));
        assert!(fns.contains(&"bool"));
        assert!(fns.contains(&"flatten"));
        assert!(fns.contains(&"length"));
        assert!(fns.contains(&"len"));
        assert!(fns.contains(&"keys"));
        assert!(fns.contains(&"values"));
        assert!(fns.contains(&"first"));
        assert!(fns.contains(&"last"));
        assert!(fns.contains(&"reverse"));
        assert!(fns.contains(&"unique"));
        assert!(fns.contains(&"uniq"));
        assert!(fns.contains(&"sort"));
        assert!(fns.contains(&"compact"));
    }

    // ── edge cases ──────────────────────────────────────────────────────

    #[test]
    fn trim_tabs_and_newlines() {
        assert_eq!(apply_transform("trim", json!("\t hello \n")).unwrap(), json!("hello"));
    }

    #[test]
    fn sort_floats() {
        assert_eq!(
            apply_transform("sort", json!([2.5, 1.1, 3.3])).unwrap(),
            json!([1.1, 2.5, 3.3])
        );
    }

    #[test]
    fn unique_with_objects() {
        assert_eq!(
            apply_transform("unique", json!([{"a":1}, {"a":1}, {"b":2}])).unwrap(),
            json!([{"a":1}, {"b":2}])
        );
    }

    #[test]
    fn flatten_deeply_nested_only_one_level() {
        assert_eq!(
            apply_transform("flatten", json!([[1, [2, 3]], [4]])).unwrap(),
            json!([1, [2, 3], 4])
        );
    }

    #[test]
    fn reverse_unicode_string() {
        assert_eq!(apply_transform("reverse", json!("café")).unwrap(), json!("éfac"));
    }

    #[test]
    fn length_unicode_string() {
        // "café" is 5 bytes in UTF-8 — length returns byte length
        let result = apply_transform("length", json!("café")).unwrap();
        assert_eq!(result, json!(5));
    }

    #[test]
    fn sort_single_element() {
        assert_eq!(apply_transform("sort", json!([42])).unwrap(), json!([42]));
    }

    #[test]
    fn compact_with_false_and_zero_preserved() {
        assert_eq!(
            apply_transform("compact", json!([0, false, null, "", 1])).unwrap(),
            json!([0, false, "", 1])
        );
    }

    #[test]
    fn first_single_element() {
        assert_eq!(apply_transform("first", json!([99])).unwrap(), json!(99));
    }

    #[test]
    fn last_single_element() {
        assert_eq!(apply_transform("last", json!([99])).unwrap(), json!(99));
    }

    #[test]
    fn chain_trim_and_lowercase() {
        let result = apply_transform_chain("trim | lowercase", json!("   ALICE IN WONDERLAND   ")).unwrap();
        assert_eq!(result, json!("alice in wonderland"));
    }

    #[test]
    fn chain_number_to_string_and_trim() {
        let result = apply_transform_chain("to_string | trim", json!(9876)).unwrap();
        assert_eq!(result, json!("9876"));
    }

    #[test]
    fn chain_string_to_number() {
        let result = apply_transform_chain("trim | to_number", json!("  42.5  ")).unwrap();
        assert_eq!(result, json!(42.5));
    }

    #[test]
    fn chain_comma_separated() {
        let result = apply_transform_chain("trim, uppercase", json!("  hello  ")).unwrap();
        assert_eq!(result, json!("HELLO"));
    }

    #[test]
    fn chain_with_arrow_syntax() {
        let result = apply_transform_chain("trim -> lowercase", json!("  HELLO  ")).unwrap();
        assert_eq!(result, json!("hello"));
    }

    // ── date / timestamp ────────────────────────────────────────────────

    #[test]
    fn date_accepts_iso_dates_and_datetimes() {
        assert_eq!(apply_transform("date", json!("2024-03-15")).unwrap(), json!("2024-03-15"));
        assert_eq!(apply_transform("date", json!(" 2024-03-15T10:20:30Z ")).unwrap(), json!("2024-03-15"));
        assert_eq!(apply_transform("date", json!("2026-11-12T14:30:00+05:30")).unwrap(), json!("2026-11-12"));
        assert_eq!(apply_transform("date", json!("2024-03-15 10:20")).unwrap(), json!("2024-03-15"));
        assert_eq!(apply_transform("date", json!("2024-02-29")).unwrap(), json!("2024-02-29"));
    }

    #[test]
    fn date_rejects_garbage_instead_of_truncating() {
        for bad in [
            "not a date", "Thursday", "15/03/2024", "2024-02-30", "2023-02-29",
            "2024-13-01", "2024-03-15Tgarbage", "2024-03-15T25:00", "", "2024-3-5",
        ] {
            let err = apply_transform("date", json!(bad)).unwrap_err();
            assert!(
                matches!(err, FunctionError::InvalidValue { .. }),
                "{bad:?} should be rejected, got {err}"
            );
        }
        assert!(apply_transform("date", json!(true)).is_err());
    }

    #[test]
    fn date_from_epoch_seconds_and_millis() {
        assert_eq!(apply_transform("date", json!(0)).unwrap(), json!("1970-01-01"));
        assert_eq!(apply_transform("date", json!(1710497430)).unwrap(), json!("2024-03-15"));
        assert_eq!(apply_transform("date", json!(1710497430000_i64)).unwrap(), json!("2024-03-15"));
        assert_eq!(apply_transform("date", json!(-86400)).unwrap(), json!("1969-12-31"));
        assert_eq!(apply_transform("date", json!(951782400)).unwrap(), json!("2000-02-29"));
        assert!(apply_transform("date", json!(1e20)).is_err());
    }

    #[test]
    fn timestamp_normalizes_and_validates() {
        let ts = |v: Value| apply_transform("timestamp", v);
        assert_eq!(ts(json!("2026-11-12T14:30:00+05:30")).unwrap(), json!("2026-11-12T14:30:00+05:30"));
        assert_eq!(ts(json!("2024-03-15 10:20:30")).unwrap(), json!("2024-03-15T10:20:30"));
        assert_eq!(ts(json!("2024-03-15t10:20z")).unwrap(), json!("2024-03-15T10:20:00Z"));
        assert_eq!(
            ts(json!("2024-03-15T10:20:30.123456-0400")).unwrap(),
            json!("2024-03-15T10:20:30.123456-04:00")
        );
        assert_eq!(ts(json!("2024-03-15T10:20:30+01")).unwrap(), json!("2024-03-15T10:20:30+01:00"));
        assert_eq!(ts(json!("2024-03-15")).unwrap(), json!("2024-03-15T00:00:00"));
        assert_eq!(ts(json!(1710497430)).unwrap(), json!("2024-03-15T10:10:30Z"));
        assert_eq!(ts(json!(1710497430123_i64)).unwrap(), json!("2024-03-15T10:10:30.123Z"));
        for bad in ["yesterday", "2024-03-15T10", "2024-03-15T10:20:30+25:00", "2024-03-15T10:20:30.", "10:20:30"] {
            assert!(ts(json!(bad)).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn compile_chain_rejects_unknown_names_up_front() {
        let err = compile_chain("trim | uppercsae").unwrap_err();
        assert!(err.to_string().contains("uppercsae"));
        let chain = compile_chain("trim -> lowercase, uppercase").unwrap();
        assert_eq!(chain.len(), 3);
        assert_eq!(apply_compiled_chain(&chain, json!("  Ab ")).unwrap(), json!("AB"));
    }

    #[test]
    fn every_listed_function_resolves() {
        for name in available_functions() {
            assert!(lookup(name).is_some(), "available_functions lists '{name}' but lookup fails");
        }
    }
}
