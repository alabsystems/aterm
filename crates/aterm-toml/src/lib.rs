// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TOML 1.0.0 for aterm, first-party: a serde front end (`aterm.toml`, every
//! `Cargo.toml`, the art assets) and a comment-preserving document model with
//! byte spans on every node (the Preferences window's edits, config
//! diagnostics), both over one parser. It replaces `toml` and `toml_edit`.
//!
//! ```
//! # use serde::Deserialize;
//! #[derive(Deserialize)]
//! struct Config {
//!     font_px: f64,
//! }
//!
//! // The SEMANTIC half: what the file means.
//! let config: Config = aterm_toml::from_str("font_px = 12.0 # cozy").unwrap();
//! assert_eq!(config.font_px, 12.0);
//!
//! // The SYNTACTIC half: what the file says.
//! let mut document: aterm_toml::edit::DocumentMut =
//!     "font_px = 12.0 # cozy\n".parse().unwrap();
//! document["font_px"] = aterm_toml::edit::Item::Value(18.0.into());
//! ```
//!
//! # What is enforced
//!
//! Duplicate keys and every redefinition rule in the spec are errors, not
//! last-write-wins. A config parser that quietly accepts a key twice makes the
//! file an operator reads and the file the program obeys two different
//! documents; which one wins then depends on parser internals. See
//! [`edit`] for the full list.
//!
//! Nesting is bounded ([`edit`]'s `ParseLimits`), so hostile input gets an
//! error rather than a stack overflow.
//!
//! For a type whose `Serialize` and `Deserialize` are inverse (derived ones
//! are), what [`to_string`] writes [`from_str`] reads back as the same value,
//! or `to_string` refuses it. A table keyed by integers, `bool`s or newtypes
//! reads back, each key from the one spelling written for it, whether it is
//! read straight from the document or through serde's buffer (a flattened
//! map, an untagged or internally tagged enum). A `None` struct field is left
//! out, and any other `None` (an array element, a map value, a variant's
//! payload, `Some(None)`, a newtype over `None`) is an error, not a silently
//! shorter document. See [`ser`] for why.
//!
//! # What is NOT supported
//!
//! This crate targets TOML 1.0.0. TOML 1.1 additions — the `\e` and `\x`
//! escapes, newlines and trailing commas in inline tables, and times without
//! seconds — are not supported.

#![forbid(unsafe_code)]

mod datetime;
pub mod de;
pub mod edit;
mod error;
pub mod ser;
mod value;

pub use datetime::{Date, Datetime, DatetimeParseError, Offset, Time};
pub use de::from_str;
pub use error::{Error, Result};
pub use ser::to_string;
pub use value::{Index, Map, Table, Value};
