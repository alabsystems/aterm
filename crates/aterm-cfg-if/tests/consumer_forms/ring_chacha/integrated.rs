// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Stand-in for `ring-0.17.14/src/aead/chacha20_poly1305/integrated.rs`, the
//! module the if-only arm declares on aarch64-le and x86_64.

/// Marks that the if-only arm was selected on this cell.
pub const SELECTED: bool = true;
