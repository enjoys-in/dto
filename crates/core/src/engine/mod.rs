//! # Engine Module
//!
//! High-performance, multi-threaded transformation engine that accepts
//! either a **Recipe** or **JSON Schema** and processes data in parallel
//! using Rayon. Zero-copy where possible, non-blocking batch processing.

mod compiled;
mod memory;
mod parallel;
mod validator;
mod unified;

pub use memory::*;
pub use parallel::*;
pub use validator::*;
pub use unified::*;
