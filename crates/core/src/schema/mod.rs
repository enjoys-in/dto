//! Schema module — model, error types, and transformation engine.
//!
//! This module contains:
//! - [`model::DtoSchema`] / [`model::FieldSchema`] — the schema types.
//! - [`error::SchemaError`] — schema-level errors.
//! - [`transform::DtoTransformer`] / [`transform::TransformError`] — the
//!   transformation engine and its error type.

pub mod error;
pub mod model;
pub mod transform;

// Re-exports for convenience.
pub use error::SchemaError;
pub use model::{DtoSchema, FieldSchema};
pub use transform::{DtoTransformer, TransformError};
