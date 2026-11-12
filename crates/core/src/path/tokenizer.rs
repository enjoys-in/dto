//! Tokenizer / parser for dot-notation path expressions.
//!
//! Converts a string like `"users[0].profile.name"` into a [`DtoPath`].
//!
//! # Supported Syntax
//!
//! | Expression           | Parsed segments                                        |
//! |----------------------|--------------------------------------------------------|
//! | `user.profile.name`  | `[Field("user"), Field("profile"), Field("name")]`     |
//! | `orders[0].id`       | `[Field("orders"), Index(0), Field("id")]`             |
//! | `items[-1].sku`      | `[Field("items"), Index(-1), Field("sku")]`            |
//! | `items[*].name`      | `[Field("items"), Wildcard, Field("name")]`            |
//! | `name`               | `[Field("name")]`                                      |
//! | `headers.content-type` | `[Field("headers"), Field("content-type")]`          |
//! | `user["first name"]` | `[Field("user"), Field("first name")]`                 |
//! | `meta['@type']`      | `[Field("meta"), Field("@type")]`                      |
//!
//! Unquoted field names may contain letters, digits, `_` and `-`. Any other key
//! can be quoted with `"` or `'` inside brackets; `\` escapes a quote or backslash.
//!
//! # Errors
//!
//! Returns [`PathError`] for malformed input (empty paths, double dots,
//! trailing dots, unclosed brackets, invalid bracket content, etc.).

use super::ast::{DtoPath, Segment};

/// Errors that can occur while parsing a dot-notation path expression.
#[derive(Debug, thiserror::Error)]
pub enum PathError {
    /// The input string is empty or contains only whitespace.
    #[error("empty path expression")]
    EmptyPath,

    /// An unexpected character was encountered at the given position.
    #[error("unexpected character '{0}' at position {1}")]
    UnexpectedChar(char, usize),

    /// A `[` was found without a matching `]`.
    #[error("unclosed bracket at position {0}")]
    UnclosedBracket(usize),

    /// A bracket pair `[]` contains nothing.
    #[error("empty bracket at position {0}")]
    EmptyBracket(usize),

    /// The content inside brackets is not a valid integer or `*`.
    #[error("invalid index '{0}' in bracket")]
    InvalidIndex(String),

    /// Two consecutive dots or a trailing dot produced an empty segment.
    #[error("empty segment (double dot) at position {0}")]
    EmptySegment(usize),

    /// A quoted key (`["..."]`) was not closed.
    #[error("unclosed quoted key starting at position {0}")]
    UnclosedQuote(usize),
}

/// Characters allowed in an unquoted field name: letters, digits, `_` and `-`.
/// Any other key (spaces, dots, `@`, `$`, …) can be written as `["key"]`.
pub fn is_bare_key_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Parses a dot-notation path expression into a [`DtoPath`].
///
/// # Errors
///
/// Returns [`PathError`] if the input is empty, contains syntax errors such as
/// double dots, trailing dots, unclosed brackets, or invalid bracket content.
///
/// # Examples
///
/// ```
/// use dto_core::path::tokenizer::parse;
/// use dto_core::path::ast::Segment;
///
/// let path = parse("users[0].name").unwrap();
/// assert_eq!(path.segments, vec![
///     Segment::Field("users".into()),
///     Segment::Index(0),
///     Segment::Field("name".into()),
/// ]);
/// ```
pub fn parse(input: &str) -> Result<DtoPath, PathError> {
    if input.is_empty() {
        return Err(PathError::EmptyPath);
    }

    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut pos: usize = 0;
    let mut segments: Vec<Segment> = Vec::new();
    let mut expecting_field = false;

    // Leading dot is not allowed
    if chars[0] == '.' {
        return Err(PathError::UnexpectedChar('.', 0));
    }

    while pos < len {
        match chars[pos] {
            // Dot separator
            '.' => {
                if expecting_field {
                    // Two consecutive dots!
                    return Err(PathError::EmptySegment(pos));
                }
                pos += 1;
                if pos >= len {
                    // Trailing dot!
                    return Err(PathError::EmptySegment(pos));
                }
                expecting_field = true;
            }
            // Quoted key: ["content-type"] or ['first name'] (any characters).
            '[' if matches!(chars.get(pos + 1), Some('"') | Some('\'')) => {
                expecting_field = false;
                let bracket_start = pos;
                let quote = chars[pos + 1];
                pos += 2; // skip `[` and the opening quote

                let mut key = String::new();
                loop {
                    match chars.get(pos) {
                        None => return Err(PathError::UnclosedQuote(bracket_start)),
                        Some('\\') => {
                            // `\"`, `\'` and `\\` escape the next character.
                            match chars.get(pos + 1) {
                                Some(&c) => key.push(c),
                                None => return Err(PathError::UnclosedQuote(bracket_start)),
                            }
                            pos += 2;
                        }
                        Some(&c) if c == quote => {
                            pos += 1;
                            break;
                        }
                        Some(&c) => {
                            key.push(c);
                            pos += 1;
                        }
                    }
                }
                if chars.get(pos) != Some(&']') {
                    return Err(PathError::UnclosedBracket(bracket_start));
                }
                pos += 1; // skip ']'
                segments.push(Segment::Field(key));
            }
            // Bracket: index or wildcard.
            '[' => {
                expecting_field = false;
                let bracket_start = pos;
                pos += 1; // skip '['

                // Find the closing ']'.
                let content_start = pos;
                while pos < len && chars[pos] != ']' {
                    pos += 1;
                }
                if pos >= len {
                    return Err(PathError::UnclosedBracket(bracket_start));
                }

                let content: String = chars[content_start..pos].iter().collect();
                pos += 1; // skip ']'

                if content.is_empty() {
                    return Err(PathError::EmptyBracket(bracket_start));
                }

                if content == "*" {
                    segments.push(Segment::Wildcard);
                } else {
                    // Must be a valid integer (possibly negative).
                    match content.parse::<i64>() {
                        Ok(idx) => segments.push(Segment::Index(idx)),
                        Err(_) => return Err(PathError::InvalidIndex(content)),
                    }
                }
            }
            // Start of a bare field name.
            c if is_bare_key_char(c) => {
                expecting_field = false;
                let start = pos;
                while pos < len && is_bare_key_char(chars[pos]) {
                    pos += 1;
                }
                let field: String = chars[start..pos].iter().collect();
                segments.push(Segment::Field(field));
            }
            // Anything else is unexpected.
            c => {
                return Err(PathError::UnexpectedChar(c, pos));
            }
        }
    }

    if expecting_field || segments.is_empty() {
        return Err(PathError::EmptySegment(pos));
    }

    Ok(DtoPath { segments })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::ast::Segment;

    // ────────────────────────── Valid paths ──────────────────────────

    #[test]
    fn parse_single_field() {
        let p = parse("name").unwrap();
        assert_eq!(p.segments, vec![Segment::Field("name".into())]);
    }

    #[test]
    fn parse_dotted_fields() {
        let p = parse("user.profile.name").unwrap();
        assert_eq!(
            p.segments,
            vec![
                Segment::Field("user".into()),
                Segment::Field("profile".into()),
                Segment::Field("name".into()),
            ]
        );
    }

    #[test]
    fn parse_field_with_index() {
        let p = parse("orders[0].id").unwrap();
        assert_eq!(
            p.segments,
            vec![
                Segment::Field("orders".into()),
                Segment::Index(0),
                Segment::Field("id".into()),
            ]
        );
    }

    #[test]
    fn parse_negative_index() {
        let p = parse("items[-1].sku").unwrap();
        assert_eq!(
            p.segments,
            vec![
                Segment::Field("items".into()),
                Segment::Index(-1),
                Segment::Field("sku".into()),
            ]
        );
    }

    #[test]
    fn parse_wildcard() {
        let p = parse("items[*].name").unwrap();
        assert_eq!(
            p.segments,
            vec![
                Segment::Field("items".into()),
                Segment::Wildcard,
                Segment::Field("name".into()),
            ]
        );
    }

    #[test]
    fn parse_multiple_indices() {
        let p = parse("matrix[0][1]").unwrap();
        assert_eq!(
            p.segments,
            vec![
                Segment::Field("matrix".into()),
                Segment::Index(0),
                Segment::Index(1),
            ]
        );
    }

    #[test]
    fn parse_underscore_fields() {
        let p = parse("my_field.another_field").unwrap();
        assert_eq!(
            p.segments,
            vec![
                Segment::Field("my_field".into()),
                Segment::Field("another_field".into()),
            ]
        );
    }

    #[test]
    fn parse_large_index() {
        let p = parse("arr[999]").unwrap();
        assert_eq!(
            p.segments,
            vec![Segment::Field("arr".into()), Segment::Index(999)]
        );
    }

    #[test]
    fn parse_wildcard_only() {
        let p = parse("[*]").unwrap();
        assert_eq!(p.segments, vec![Segment::Wildcard]);
    }

    #[test]
    fn parse_index_only() {
        let p = parse("[0]").unwrap();
        assert_eq!(p.segments, vec![Segment::Index(0)]);
    }

    #[test]
    fn parse_wildcard_then_wildcard() {
        let p = parse("[*][*]").unwrap();
        assert_eq!(p.segments, vec![Segment::Wildcard, Segment::Wildcard]);
    }

    #[test]
    fn parse_field_wildcard_field_index() {
        let p = parse("data[*].values[0]").unwrap();
        assert_eq!(
            p.segments,
            vec![
                Segment::Field("data".into()),
                Segment::Wildcard,
                Segment::Field("values".into()),
                Segment::Index(0),
            ]
        );
    }

    // ────────────────────────── Invalid paths ──────────────────────────

    #[test]
    fn error_empty_path() {
        let err = parse("").unwrap_err();
        assert!(matches!(err, PathError::EmptyPath));
    }

    #[test]
    fn error_double_dot() {
        let err = parse("a..b").unwrap_err();
        assert!(matches!(err, PathError::EmptySegment(_)));
    }

    #[test]
    fn error_trailing_dot() {
        let err = parse("a.b.").unwrap_err();
        assert!(matches!(err, PathError::EmptySegment(_)));
    }

    #[test]
    fn error_leading_dot() {
        let err = parse(".a").unwrap_err();
        assert!(matches!(err, PathError::UnexpectedChar('.', 0)));
    }

    #[test]
    fn error_unclosed_bracket() {
        let err = parse("items[0").unwrap_err();
        assert!(matches!(err, PathError::UnclosedBracket(_)));
    }

    #[test]
    fn error_empty_bracket() {
        let err = parse("items[]").unwrap_err();
        assert!(matches!(err, PathError::EmptyBracket(_)));
    }

    #[test]
    fn error_invalid_index_alpha() {
        let err = parse("items[abc]").unwrap_err();
        assert!(matches!(err, PathError::InvalidIndex(_)));
    }

    #[test]
    fn error_invalid_index_mixed() {
        let err = parse("items[0a]").unwrap_err();
        assert!(matches!(err, PathError::InvalidIndex(_)));
    }

    #[test]
    fn error_unexpected_char_space() {
        let err = parse("a b").unwrap_err();
        assert!(matches!(err, PathError::UnexpectedChar(' ', _)));
    }

    #[test]
    fn error_unexpected_char_closing_bracket() {
        let err = parse("]foo").unwrap_err();
        assert!(matches!(err, PathError::UnexpectedChar(']', 0)));
    }

    // ────────────────────────── Display round-trip ──────────────────────────

    #[test]
    fn roundtrip_simple() {
        let input = "user.profile.name";
        let path = parse(input).unwrap();
        assert_eq!(path.to_string(), input);
    }

    #[test]
    fn roundtrip_with_index() {
        let input = "orders[0].id";
        let path = parse(input).unwrap();
        assert_eq!(path.to_string(), input);
    }

    #[test]
    fn roundtrip_with_wildcard() {
        let input = "items[*].sku";
        let path = parse(input).unwrap();
        assert_eq!(path.to_string(), input);
    }

    #[test]
    fn roundtrip_negative_index() {
        let input = "items[-1].value";
        let path = parse(input).unwrap();
        assert_eq!(path.to_string(), input);
    }

    #[test]
    fn hyphenated_bare_keys() {
        let path = parse("headers.content-type").unwrap();
        assert_eq!(
            path.segments,
            vec![Segment::Field("headers".into()), Segment::Field("content-type".into())]
        );
        assert_eq!(path.to_string(), "headers.content-type");
    }

    #[test]
    fn quoted_keys_allow_any_characters() {
        let path = parse(r#"user["first name"].tags['@type'][0]["a.b|c"]"#).unwrap();
        assert_eq!(
            path.segments,
            vec![
                Segment::Field("user".into()),
                Segment::Field("first name".into()),
                Segment::Field("tags".into()),
                Segment::Field("@type".into()),
                Segment::Index(0),
                Segment::Field("a.b|c".into()),
            ]
        );
        // Display round-trips through the parser.
        assert_eq!(parse(&path.to_string()).unwrap(), path);
    }

    #[test]
    fn quoted_key_escapes_and_errors() {
        let path = parse(r#"["say \"hi\""]"#).unwrap();
        assert_eq!(path.segments, vec![Segment::Field(r#"say "hi""#.into())]);
        assert!(matches!(parse(r#"a["oops"#), Err(PathError::UnclosedQuote(1))));
        assert!(matches!(parse(r#"a["x"y]"#), Err(PathError::UnclosedBracket(1))));
    }
}
