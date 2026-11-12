//! # Recipe Module
//!
//! Provides the **Recipe** format — a simplified, user-friendly alternative to
//! JSON Schema for defining DTO transformations. Think of it like a cooking
//! recipe: you define ingredients (source fields), steps (mappings + transforms),
//! and constraints (validation rules).
//!
//! ## Recipe JSON Example
//!
//! ```json
//! {
//!   "name": "UserSummary",
//!   "version": "1.0.0",
//!   "mappings": [
//!     { "source": "profile.full_name",       "target": "name" },
//!     { "source": "profile.address.city",    "target": "city" },
//!     { "source": "contact.emails[0]",       "target": "primary_email" },
//!     { "source": "roles[*].name",           "target": "role_names" },
//!     { "source": "profile.age",             "target": "age",  "cast": "decimal" }
//!   ],
//!   "constraints": {
//!     "name":  { "type": "string", "max_length": 255 },
//!     "age":   { "type": "number", "min": 0, "max": 150 }
//!   },
//!   "required": ["name", "primary_email"]
//! }
//! ```

pub mod model;
pub mod parser;
pub mod converter;
pub mod pipeline;

pub use model::*;
pub use parser::*;
pub use converter::*;
pub use pipeline::*;
