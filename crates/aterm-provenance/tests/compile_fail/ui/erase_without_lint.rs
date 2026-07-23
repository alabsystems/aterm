// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compile-fail (lint-level): `Provenance::into_inner_erased` is marked
//! `#[deprecated]` so every call site emits a warning. Under the crate's
//! `-D deprecated` policy (enforced at the workspace level in CI), using
//! this method without a `// PROVENANCE-ERASE: <reason>` audit comment
//! is a compile error. `aterm audit policy` counts erasure sites
//! to make sure the budget outside `security::` modules stays at 0.
//!
//! This fixture exercises the deprecation by calling `into_inner_erased`
//! without the audit marker. When compiled with `-D deprecated` it fails;
//! under default lints it warns. The audit script is the authoritative
//! check — see `aterm audit policy`.

#![deny(deprecated)]

use aterm_provenance::{Provenance, Pty};

fn main() {
    let pty = Provenance::<_, Pty>::from_pty(42u32);
    // ERROR (under -D deprecated): use of deprecated associated function
    // `aterm_provenance::Provenance::<T, Pty>::into_inner_erased`.
    let _bypassed = pty.into_inner_erased();
}
