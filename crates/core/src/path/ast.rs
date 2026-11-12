//! Abstract syntax tree for dot-notation path expressions.
//!
//! A path expression like `users[0].profile.name` is parsed into a [`DtoPath`]
//! containing a sequence of [`Segment`]s that describe how to traverse a JSON
//! value tree.

use std::fmt;

/// A single segment in a dot-notation path.
///
/// Each segment describes one step of traversal into a nested JSON structure.
///
/// # Examples
///
/// | Syntax   | Segment           |
/// |----------|-------------------|
/// | `user`   | `Field("user")`   |
/// | `[0]`    | `Index(0)`        |
/// | `[-1]`   | `Index(-1)`       |
/// | `[*]`    | `Wildcard`        |
#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    /// A named field: `user`, `profile`, `name`.
    Field(String),
    /// A numeric array index: `[0]`, `[2]`, `[-1]` (negative = from end).
    Index(i64),
    /// A wildcard projection: `[*]` — iterates all elements of an array.
    Wildcard,
}

/// A parsed dot-notation path (sequence of segments).
///
/// Constructed via [`crate::path::tokenizer::parse`] and consumed by
/// [`crate::path::resolver::resolve`].
///
/// # Display
///
/// Implements [`fmt::Display`] to reconstruct a canonical path string:
///
/// ```
/// use dto_core::path::ast::{DtoPath, Segment};
///
/// let path = DtoPath {
///     segments: vec![
///         Segment::Field("items".into()),
///         Segment::Wildcard,
///         Segment::Field("sku".into()),
///     ],
/// };
/// assert_eq!(path.to_string(), "items[*].sku");
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct DtoPath {
    /// The ordered list of segments that make up this path.
    pub segments: Vec<Segment>,
}

impl fmt::Display for DtoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for seg in &self.segments {
            match seg {
                Segment::Field(name)
                    if !name.is_empty()
                        && name.chars().all(crate::path::tokenizer::is_bare_key_char) =>
                {
                    if !first {
                        write!(f, ".")?;
                    }
                    write!(f, "{name}")?;
                }
                Segment::Field(name) => {
                    // Keys with other characters round-trip as `["..."]`.
                    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
                    write!(f, "[\"{escaped}\"]")?;
                }
                Segment::Index(i) => {
                    write!(f, "[{i}]")?;
                }
                Segment::Wildcard => {
                    write!(f, "[*]")?;
                }
            }
            first = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_simple_fields() {
        let path = DtoPath {
            segments: vec![
                Segment::Field("user".into()),
                Segment::Field("profile".into()),
                Segment::Field("name".into()),
            ],
        };
        assert_eq!(path.to_string(), "user.profile.name");
    }

    #[test]
    fn display_field_with_index() {
        let path = DtoPath {
            segments: vec![
                Segment::Field("orders".into()),
                Segment::Index(0),
                Segment::Field("id".into()),
            ],
        };
        assert_eq!(path.to_string(), "orders[0].id");
    }

    #[test]
    fn display_negative_index() {
        let path = DtoPath {
            segments: vec![
                Segment::Field("items".into()),
                Segment::Index(-1),
                Segment::Field("sku".into()),
            ],
        };
        assert_eq!(path.to_string(), "items[-1].sku");
    }

    #[test]
    fn display_wildcard() {
        let path = DtoPath {
            segments: vec![
                Segment::Field("items".into()),
                Segment::Wildcard,
                Segment::Field("name".into()),
            ],
        };
        assert_eq!(path.to_string(), "items[*].name");
    }

    #[test]
    fn display_single_field() {
        let path = DtoPath {
            segments: vec![Segment::Field("name".into())],
        };
        assert_eq!(path.to_string(), "name");
    }

    #[test]
    fn display_only_index() {
        let path = DtoPath {
            segments: vec![Segment::Index(3)],
        };
        assert_eq!(path.to_string(), "[3]");
    }

    #[test]
    fn display_consecutive_brackets() {
        let path = DtoPath {
            segments: vec![
                Segment::Field("matrix".into()),
                Segment::Index(0),
                Segment::Index(1),
            ],
        };
        assert_eq!(path.to_string(), "matrix[0][1]");
    }
}
