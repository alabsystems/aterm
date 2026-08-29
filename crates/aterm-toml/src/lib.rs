// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TOML 1.0.0 for aterm — the serde front end and the comment-preserving
//! document model, first-party.
//!
//! # Why this crate exists
//!
//! aterm reads and writes TOML in three registers, and until now that cost four
//! third-party packages:
//!
//! | job | what it needs | what it used |
//! |-----|---------------|--------------|
//! | load `aterm.toml`, every `Cargo.toml`, the art assets | a `serde::Deserializer` | `toml` |
//! | save the Preferences window's edits without touching a comment | a format-preserving DOM | `toml_edit` |
//! | underline a config diagnostic in the editor | byte spans on every node | both |
//!
//! `toml` + `toml_edit` brought SIX packages and 64,783 lines of third-party
//! source for that, measured by `forge survey` on every cell before and after
//! the swap: `toml`, `toml_edit`, `toml_write`, `winnow`, and `serde_spanned` +
//! `toml_datetime`, which are reached only through `toml` itself. One of the
//! six, `winnow`, aterm had to FORK to fix an `offset_from` divide-by-zero,
//! which meant carrying a vendored parser-combinator library and a
//! `[patch.crates-io]` entry in order to read config files. That fork went with
//! them.
//!
//! This crate is both registers over one parser, in a fraction of the code,
//! with the spec's rules enforced where they are load-bearing.
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
//! Nothing in TOML 1.0.0 is left out. The deliberate omissions are all TOML
//! 1.1 *drafts* — the `\e` escape, unquoted trailing newlines in inline
//! tables, seconds-optional times, and `\x` escapes — because 1.1 is not a
//! released spec and the crates this replaces do not implement them either.

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
