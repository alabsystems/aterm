// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-core's panic-freedom and correctness fuzz group: one test binary for
//! the deterministic-LCG fuzzers. They are the crate's CPU long poles, kept out
//! of `core_suite` so that suite finishes on its own clock. See `core_suite.rs`
//! for the `autotests = false` rule.
//!
//! Run one fuzzer with
//! `targo --unverified test -p aterm-core --test core_fuzz -- <file>::<test> --exact`
//! (`fuzz_process_never_panics`'s fresh-terminal properties print their own
//! replay line).

mod fuzz_alt_screen;
mod fuzz_process_never_panics;
