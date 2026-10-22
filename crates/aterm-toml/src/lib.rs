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
//! Duplicate keys and every other redefinition rule in the spec are errors,
//! not last-write-wins. Nesting depth is bounded, so hostile input is an error
//! rather than a stack overflow. TOML 1.1 drafts (the `\e` and `\x` escapes,
//! newlines in inline tables, seconds-optional times) are not supported.

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
