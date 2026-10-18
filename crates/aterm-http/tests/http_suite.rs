// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-http integration files as modules of ONE test binary: each file
//! stays at its own path and keeps its own `#[test]`s. Their loopback servers
//! bind port 0 and nothing touches the process environment.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.

mod loopback;
mod signature_methods;
mod tls_stream;
