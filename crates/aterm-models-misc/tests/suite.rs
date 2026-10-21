// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tier-0 suite of this crate's models: ONE test binary for every file below, instead of
//! one linked and started per file. Each file keeps its own `#[test]`s, named `<file>::<test>`
//! here; none sets an environment variable, installs a hook or re-executes itself.
//!
//! `autotests = false`: a `tests/*.rs` file that is not a `mod` line here NEVER COMPILES. Run one
//! file's tests with `targo --unverified test -p aterm-models-misc --test suite -- <file>::`.

mod derived_vitals;
