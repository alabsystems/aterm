// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-spec's integration suite: ONE test binary for the engine's own
//! integration tests (the `ty_model!` macro's). The models'
//! Tier-0 tests live beside the models, in the `aterm-models-*` crates.
//!
//! Each file keeps its path and its own `#[test]`s; a test is named
//! `<file>::<test>` here.
//!
//! Sharing a process was checked, not assumed: no file below sets or removes an
//! environment variable, installs a hook or an allocator, or re-executes
//! itself.
//!
//! `autotests = false`: a `tests/*.rs` file that is neither a `mod` line here
//! nor a `[[test]]` of its own NEVER COMPILES. Run one file's tests with
//! `targo --unverified test -p aterm-spec --test spec_suite -- <file>::`.

mod macro_ty_model;
