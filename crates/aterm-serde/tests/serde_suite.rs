// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Ported upstream `serde` test suite for `aterm-serde` and `aterm-serde-derive`.

#![allow(
    clippy::cast_lossless,
    clippy::decimal_literal_representation,
    clippy::derive_partial_eq_without_eq,
    clippy::empty_enums,
    clippy::manual_assert,
    clippy::needless_pass_by_value,
    clippy::uninlined_format_args,
    clippy::unreadable_literal,
    reason = "upstream's test_suite and serde_test are kept as written (casts, literals, \
              fixtures, the harness's panics and by-value signatures), so each file still \
              diffs cleanly against serde 1.0.228 and serde_test 1.0.177"
)]

extern crate aterm_serde as serde;
extern crate aterm_serde_derive as serde_derive;

#[macro_use]
mod macros;

mod support;

mod annotations;
mod attrs;
mod de;
mod de_error;
mod derive_input;
mod enum_internally_tagged;
mod enum_untagged;
mod ignored_any;
mod ser;
mod text_keys;
mod untrusted;
mod value;
