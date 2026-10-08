// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The VT suite: every engine-read conformance file in one test binary.
//!
//! These are pure feed-bytes → read-screen → assert cases (no environment,
//! global allocator, panic hook, timing bound or shared file path), so they
//! share one process and run concurrently. The files keep their paths, so
//! `tests/<file>.rs:<line>` citations resolve. Run one file's cases with
//! `targo --unverified test -p aterm-conformance --test vt -- <file>::`.
//!
//! The crate sets `autotests = false`, so a `tests/*.rs` file that is neither
//! a `mod` here nor a `[[test]]` in Cargo.toml never compiles. A file needing
//! its own process (a live binary, a timing window, a global) gets its own
//! `[[test]]` with a one-line reason.

mod conformance;
mod kitty_mouse;
mod osc8_schemes;
mod rect_ops;
mod responses;
mod sixel;
mod vt_categories;
mod vt_categories2;
mod vt_categories3;
mod xterm_alternate_finals;
mod xterm_differential_regressions;

// aterm-core's fresh-terminal escape-soup fuzz cases, mounted here so they
// also run against aterm-core with `sixel` (this crate's dependency) under
// `-p aterm-conformance` alone. They only read their seed and case-count
// overrides from the environment, so they share this process safely.
#[path = "../../aterm-core/tests/fuzz_process_never_panics/fresh.rs"]
mod fresh_terminal;
