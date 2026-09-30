//! The protocol between `browse` and `jsd`, its JavaScript helper.
//!
//! Messages are JSON objects, one per line, with a `"t"` (type) field.
//! See `docs/JAVASCRIPT.md` for the vocabulary.

#![no_std]

extern crate alloc;

pub mod dom;
pub mod json;

#[cfg(test)]
mod tests;

pub use json::Json;
