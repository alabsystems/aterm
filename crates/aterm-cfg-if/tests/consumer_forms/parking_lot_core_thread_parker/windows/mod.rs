// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Stand-in for `parking_lot_core-0.9.12/src/thread_parker/windows/mod.rs`.

/// Which backend file the chain actually selected, observable at run time.
pub const PARKER: &str = "windows/mod.rs";
