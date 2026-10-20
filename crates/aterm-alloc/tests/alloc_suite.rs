// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-alloc integration files as modules of ONE test binary: each file
//! stays at its own path and keeps its own `#[test]`s. A panic check
//! silences the panic hook while it runs, as it did in its own binary; the
//! drop counters are thread-local.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.

mod arrayvec_differential;
mod panic_drop_parity;
