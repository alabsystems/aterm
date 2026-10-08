// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! JSON for aterm: a serde `Deserializer`/`Serializer` for typed models, and
//! an untyped [`Value`] tree with `json!`, over one parser and one writer.
//! Replaces `serde_json`.
//!
//! ```
//! # use serde::Deserialize;
//! #[derive(Deserialize)]
//! struct Release { tag_name: String }
//!
//! // Typed.
//! let release: Release = aterm_json::from_str(r#"{"tag_name":"v1.2.3"}"#).unwrap();
//! assert_eq!(release.tag_name, "v1.2.3");
//!
//! // Untyped.
//! let body = aterm_json::json!({"model": "qwen", "options": {"temperature": 0}});
//! assert_eq!(aterm_json::to_string(&body).unwrap(),
//!            r#"{"model":"qwen","options":{"temperature":0}}"#);
//! ```
//!
//! # What is enforced
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
//!   request body, so "equivalent" would not have been enough. `tests/json.rs`
//!   pins it.
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
    // Through the text form on purpose: it is lossless for everything `Value`
    // holds, so a second, `Value`-building serializer would only be a second
    // chance to disagree (non-finite floats, integer width, key coercion).
    //
    // The exception is an integer past 64 bits, which `Value::Number` cannot
    // hold. The strict reader refuses it ("number out of range"), as
    // `serde_json::to_value` does, rather than rounding it to an f64.
    // `from_slice` stays lenient: a wide integer on the wire is a float there.
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
