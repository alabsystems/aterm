// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Joined real SDK/native Source protocol qualification. Only an explicit
//! debug feature candidate runs, in a fresh native owner world with IP denied.

#[cfg(not(debug_assertions))]
pub fn cmd(_: &[String]) -> crate::Res {
    Err(crate::Fail::die(
        "codex-source-proof requires a debug lab artifact",
    ))
}

#[cfg(debug_assertions)]
#[path = "codex_source_proof_driver.rs"]
mod driver;
#[cfg(debug_assertions)]
pub use driver::cmd;
