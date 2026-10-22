// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-http integration files as modules of ONE test binary: each file
//! stays at its own path and keeps its own `#[test]`s. Their loopback servers
//! bind port 0 and nothing touches the process environment.
//!
//! With `autotests = false` (`Cargo.toml`), a `tests/*.rs` file that is neither
//! a `mod` line here nor a `[[test]]` of its own never compiles.

mod golden_post;
mod loopback;
mod methods;
mod signature_methods;
mod streaming;
mod tls_stream;
