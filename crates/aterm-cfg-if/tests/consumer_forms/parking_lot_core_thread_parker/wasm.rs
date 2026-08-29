// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Stand-in for `parking_lot_core-0.9.12/src/thread_parker/wasm.rs`, so the
//! verbatim `#[path = "wasm.rs"] mod imp;` arm has a real file to point at.

/// Which backend file the chain actually selected, observable at run time.
pub const PARKER: &str = "wasm.rs";
