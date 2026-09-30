//! A `no_std` CSS engine for the RustOS browser: CSS Syntax 3 tokenizer
//! and parser, Selectors 4, Media Queries 4 and `@supports`, the cascade
//! (origins, `!important`, `@layer`, specificity, `@import`, nesting),
//! custom properties and computed styles for the properties the layout
//! and paint code use.
//!
//! Typical use:
//! ```ignore
//! let mut set = StyleSet::with_user_agent(Device::default());
//! set.add(author_css, Origin::Author);
//! let style = set.compute(&element, Some(&parent_style), inline, &hints, None);
//! ```

#![no_std]

extern crate alloc;

pub mod cascade;
pub mod colors;
pub mod hints;
pub mod math;
pub mod media;
pub mod parser;
pub mod selector;
pub mod style;
pub mod tokenizer;
pub mod values;

#[cfg(test)]
mod tests;

pub use cascade::{FontFace, Import, Origin, StyleSet};
pub use media::{Device, MediaType, Pointer};
pub use selector::{Element, PseudoElement, SelectorList, State};
pub use style::ComputedStyle;
pub use values::{Color, LengthPercentage, Rgba};

/// The HTML user-agent stylesheet.
pub const UA_CSS: &str = include_str!("ua.css");
