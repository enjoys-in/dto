//! Dot-notation path module for the DTO transformation engine.
//!
//! This module provides the ability to **parse** path expressions like
//! `user.profile.name`, `orders[0].id`, `items[*].sku`, and `items[-1].value`,
//! and then **resolve** them against [`serde_json::Value`] trees.
//!
//! # Submodules
//!
//! - [`ast`] — Path AST types ([`Segment`](ast::Segment), [`DtoPath`](ast::DtoPath)).
//! - [`tokenizer`] — Parser that converts a string into a `DtoPath`.
//! - [`resolver`] — Resolver that walks a JSON value according to a `DtoPath`.
//!
//! # Quick Start
//!
//! ```
//! use dto_core::path::{tokenizer::parse, resolver::resolve};
//! use serde_json::json;
//!
//! let data = json!({"users": [{"name": "Alice"}, {"name": "Bob"}]});
//! let path = parse("users[*].name").unwrap();
//! let result = resolve(&data, &path);
//! assert_eq!(result, Some(json!(["Alice", "Bob"])));
//! ```

pub mod ast;
pub mod resolver;
pub mod tokenizer;

// Re-export the most commonly used items for convenience.
pub use ast::{DtoPath, Segment};
pub use resolver::resolve;
pub use tokenizer::{parse, PathError};
