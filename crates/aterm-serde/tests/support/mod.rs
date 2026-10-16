// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_test 1.0.177 src/lib.rs (github.com/serde-rs/test) and
// modified by the aterm project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! The `serde_test` token harness: a `Serializer` that checks what it is fed
//! against a token list and a `Deserializer` that replays one, so a test
//! states a type's serde shape without a data format. The readable/compact
//! `Configure` wrapper is left out with the types that needed it.

mod assert;
mod de;
mod error;
mod ser;
mod token;

pub use self::assert::{
    assert_de_tokens, assert_de_tokens_error, assert_ser_tokens, assert_ser_tokens_error,
    assert_tokens,
};
pub use self::de::Deserializer;
pub use self::error::Error;
pub use self::token::Token;
