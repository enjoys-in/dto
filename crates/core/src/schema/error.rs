//! Schema-level error types.

/// Errors that can occur when parsing or validating a [`DtoSchema`](super::model::DtoSchema).
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    /// The input string was not valid JSON or did not match the expected schema shape.
    #[error("failed to parse schema JSON: {0}")]
    ParseError(#[from] serde_json::Error),

    /// The schema was syntactically valid JSON but semantically invalid
    /// (e.g., missing `type` for a required node).
    #[error("invalid schema: {0}")]
    Invalid(String),
}
