// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TOML 1.0.0 for aterm — the serde front end and the comment-preserving
//! document model, first-party.
//!
//! # What it serves
//!
//! | job | what it needs |
//! |-----|---------------|
//! | load `aterm.toml`, every `Cargo.toml`, the art assets | a `serde::Deserializer` |
//! | save the Preferences window's edits without touching a comment | a format-preserving DOM |
//! | underline a config diagnostic in the editor | byte spans on every node |
//!
//! All three run over one parser. It replaces `toml` and `toml_edit` (six
//! packages, 64,783 lines of third-party source, including a forked `winnow`).
//!
//! # The two halves
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
