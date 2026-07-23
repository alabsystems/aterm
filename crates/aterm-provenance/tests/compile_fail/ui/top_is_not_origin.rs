// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compile-fail: `Top` is a synthetic element used only in
//! `DynProvenance` diagnostics and TLA+ traces. It deliberately does
//! **not** implement the `Origin` trait, so it cannot appear as the
//! second type parameter of `Provenance<T, O>` — the lattice's `Top`
//! element is not a valid static type parameter (design §3.2).

use aterm_provenance::{Provenance, Top};

fn main() {
    // ERROR: the trait `Origin` is not implemented for `Top`.
    let _bad: Provenance<u8, Top>;
}
