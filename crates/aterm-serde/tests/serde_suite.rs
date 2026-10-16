// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Upstream serde's own test suite (serde 1.0.228 `test_suite/tests`, on the
//! `serde_test` token harness), ported onto the surface this crate keeps
//! (docs/AUDIT-third-party-2026-10-15.md, SP.4).
//!
//! A test was ported when everything it touches is still here: the trait
//! defaults and their error text, the std impls aterm's models use, and the
//! `Content` buffering behind `tag`, `untagged` and `flatten`. A test of a
//! type this crate no longer implements (NonZero, Rc, net, time, ranges,
//! atomics, the ffi strings, sets), of an attribute aterm never writes
//! (`borrow`, `with`/`serialize_with`, `from`/`into`, `other`, `content`,
//! `expecting`, the one-direction `skip_*` and `rename(serialize = ..)`, and
//! `untagged`, `skip` or `alias` on a variant), or of a helper deserializer it
//! dropped stayed behind; a few were cut down to their in-set half, and say
//! so. The suite writes only the keys, positions and forms in
//! docs/measured/third-party-2026-10-15/serde-attrs.tsv: aterm-serde-derive
//! refuses every other one at compile time, so this suite compiling is the
//! check.
//!
//! The derives are aterm-serde-derive's (SP.5). Their output names the crate
//! `::serde`, and a crate cannot name itself that through Cargo (a self
//! dev-dependency's rename is dropped), so the two `extern crate` lines below
//! put `serde` and `serde_derive` in this crate's extern prelude: the derive
//! output resolves, and every ported file keeps upstream's
//! `use serde_derive::...`.

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
mod untrusted;
mod value;
