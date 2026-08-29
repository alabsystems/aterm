// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! JSON for aterm — the serde front end and the untyped document model,
//! first-party.
//!
//! # Why this crate exists
//!
//! aterm reads and writes JSON in two registers, and until now that cost two
//! third-party packages — `serde_json` and the float formatter `zmij` behind
//! it, 24,936 lines measured by `forge survey`:
//!
//! | job | what it needs | what it used |
//! |-----|---------------|--------------|
//! | the GitHub Releases reply, the install progress file, the operator wire, the checkpoint meta | a `serde::Deserializer` / `Serializer` | `serde_json` |
//! | an LLM response, a recording index, a metrics reply an assertion pokes at | an untyped tree + `json!` | `serde_json::Value` |
//!
//! Both are here, over one parser and one writer.
//!
//! ```
//! # use serde::Deserialize;
//! #[derive(Deserialize)]
//! struct Release { tag_name: String }
//!
//! // The TYPED half: 44 call sites in this tree are `from_str::<T>` against a
//! // `#[derive(Deserialize)]` model, so the replacement has to BE a serde
//! // Deserializer — a parser returning a tree would mean rewriting the models.
//! let release: Release = aterm_json::from_str(r#"{"tag_name":"v1.2.3"}"#).unwrap();
//! assert_eq!(release.tag_name, "v1.2.3");
//!
//! // The UNTYPED half.
//! let body = aterm_json::json!({"model": "qwen", "options": {"temperature": 0}});
//! assert_eq!(aterm_json::to_string(&body).unwrap(),
//!            r#"{"model":"qwen","options":{"temperature":0}}"#);
//! ```
//!
//! # What is enforced
//!
//! JSON is small but it has sharp edges, and each of these is a decision:
//!
//! * **trailing data is an error.** [`from_str`] parses one value and requires
//!   end-of-input.
//! * **nesting is bounded** ([`de::RECURSION_LIMIT`], 128 — `serde_json`'s
//!   default). Ten thousand `[` from a hostile peer is an error, not a stack
//!   overflow.
//! * **numbers keep their type.** A `u64` that arrives as an integer leaves as
//!   the same integer; only a fraction, an exponent, or a magnitude past 64
//!   bits becomes an `f64`.
//! * **`\u` escapes must pair.** A lone surrogate half is refused rather than
//!   silently replaced, because it is not a character.
//! * **unescaped control characters are refused** in strings, per RFC 8259.
//! * **duplicate keys are last-wins**, which is `serde_json`'s rule and what
//!   `#[derive(Deserialize)]` does. This is deliberately the OPPOSITE of
//!   [`aterm_toml`](https://docs.rs/aterm-toml), which rejects them: a config
//!   file a person edits and a wire format a server emits are different
//!   problems.
//! * **output is byte-identical to `serde_json`'s** — compact, object keys
//!   sorted, only ASCII controls escaped, non-finite floats as `null`. Some of
//!   this JSON is hashed into a checkpoint fingerprint and some of it is a
//!   request body, so "equivalent" would not have been enough.
//!   `tests/oracle.rs` holds it to that against `serde_json` itself.
//!
//! # What is NOT here
//!
//! No pretty-printing, no `Deserializer::from_reader`, no arbitrary-precision
//! numbers, no `RawValue`, no comment or trailing-comma tolerance. Nothing in
//! this tree asked for them, and each would be a new way for two readers to
//! disagree about one document.

#![deny(missing_docs)]

pub mod de;
pub mod error;
mod macros;
pub mod ser;
mod value;

pub use de::{Deserializer, from_slice, from_str};
pub use error::{Error, Result};
pub use ser::{to_string, to_vec};
pub use value::{Map, Number, Value};

use serde::Deserialize as _;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Convert any `Serialize` value into a [`Value`].
///
/// # Errors
/// Whatever the `Serialize` impl reports — a map with a non-string key, most
/// commonly.
pub fn to_value<T: Serialize>(value: T) -> Result<Value> {
    // Through the text form on purpose. A dedicated `Value`-building serializer
    // would be another two hundred lines with its own edge cases (non-finite
    // floats, integer width, key coercion) that would then have to be kept
    // agreeing with the text serializer — and JSON's text form is lossless for
    // everything `Value` can hold, so the two paths would only ever be two
    // chances to disagree. Both call sites convert one small struct.
    //
    // The ONE thing the text form is not lossless for is a 128-bit integer past
    // 64 bits: this crate serializes `u128::MAX` exactly, but `Value::Number`
    // cannot hold it, so reading it back would round it to an f64 and hand back
    // a `Value` that is not the value. `serde_json::to_value` FAILS CLOSED
    // there ("number out of range"), so this reader is the strict one, which
    // fails closed the same way. `from_slice` stays lenient: a wide integer in
    // a document a server sent is a float to `serde_json` too.
    let text = to_vec(&value)?;
    let mut de = de::Deserializer::exact(&text);
    let parsed = Value::deserialize(&mut de)?;
    de.end()?;
    Ok(parsed)
}

/// Interpret a [`Value`] as any `Deserialize` type.
///
/// # Errors
/// Whatever the `Deserialize` impl reports — a missing field or a type
/// mismatch.
pub fn from_value<T: DeserializeOwned>(value: Value) -> Result<T> {
    // Same reasoning as [`to_value`].
    let text = to_vec(&value)?;
    from_slice(&text)
}
