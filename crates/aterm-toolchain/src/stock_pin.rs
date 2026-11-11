// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`STOCK_PIN`], in a file of its own so a build script can mount it (`link_rules.rs`).

/// The one stock toolchain every STOCK EXCEPTION dev lane of the checkout compiles with
/// (`verify cells` and its windows-sys oracles, the web modules' lanes, the Linux lane's guest
/// agent, the libc oracle's cross cells), and whose lld a debug build links with on aarch64 Linux
/// (`link_rules.rs`). It is code, so the public snapshot, which carries no `publish/`, pins the
/// same release; [`crate::rustup::stock_pin`] holds it to the engine gate's wasm32 clause. No dev
/// lane falls back to rustup's floating `stable`, whose release is whatever a machine last
/// installed: one tree's verdict would then differ from one Mac to the next. (The release chain's
/// own slices, and the lanes that rehearse them, still build on `stable`.)
pub const STOCK_PIN: &str = "1.97.1";
