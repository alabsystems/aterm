// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE toolchain resolution, `aterm_toolchain::discovery`, under the path atpkg's
//! callers (`aterm help rust`, the release cutter) always named it, and rustup's
//! home and binary beside it ([`rustup`]).

pub use aterm_toolchain::discovery::*;
pub use aterm_toolchain::rustup;
