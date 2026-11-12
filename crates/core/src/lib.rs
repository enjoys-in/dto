//! # dto_core
//!
//! A high-performance, multi-threaded DTO (Data Transfer Object) transformation engine.
//!
//! Transform JSON data structures using dot-notation path expressions and either
//! **JSON Schema** (with `x-from` annotations) or **Recipes** (declarative source→target mappings
//! with constraints and validation).
//!
//! ## Quick Example (Recipe format)
//!
//! ```rust
//! use dto_core::DtoEngine;
//! use serde_json::json;
//!
//! let recipe = r#"{
//!     "name": "UserSummary",
//!     "version": "1.0.0",
//!     "mappings": [
//!         { "source": "user.profile.full_name", "target": "name" },
//!         { "source": "user.contact.emails[0]", "target": "email" },
//!         { "source": "user.roles[*].name",     "target": "roles" }
//!     ],
//!     "required": ["name", "email"]
//! }"#;
//!
//! let engine = DtoEngine::from_json(recipe).unwrap();
//!
//! let input = json!({
//!     "user": {
//!         "profile": { "full_name": "Jane Doe" },
//!         "contact": { "emails": ["jane@example.com", "j.doe@work.com"] },
//!         "roles": [{ "name": "Admin" }, { "name": "Editor" }]
//!     }
//! });
//!
//! let output = engine.transform(&input).unwrap();
//! assert_eq!(output["name"], "Jane Doe");
//! assert_eq!(output["email"], "jane@example.com");
//! assert_eq!(output["roles"], json!(["Admin", "Editor"]));
//! ```

pub mod functions;
pub mod path;
pub mod schema;
pub mod recipe;
pub mod engine;

// Re-export key types at crate root for convenience
pub use engine::{estimate_heap_bytes, parse_size, AutoChunker, BatchResult, DtoEngine, StreamStats};
pub use recipe::{Recipe, FieldMapping, Constraint};
pub use schema::model::DtoSchema;
pub use schema::transform::{DtoTransformer, TransformError};
pub use path::tokenizer::parse as parse_path;
pub use path::resolver::resolve as resolve_path;
pub use functions::{apply_transform, apply_transform_chain};
