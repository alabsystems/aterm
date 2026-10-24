// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_test 1.0.177 src/lib.rs (github.com/serde-rs/test) and
// modified by the aterm project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! Format-independent `serde_test` token harness (`Serializer` / `Deserializer` token assertions),
//! without the upstream `Configure` readable/compact wrapper.

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
