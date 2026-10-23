// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PROCESS SUITE: the aterm-objc integration files whose subject is the process itself, as
//! modules of ONE test binary that dispatches by test name.

#[cfg(target_os = "macos")]
#[path = "support/reexec.rs"]
mod reexec;

mod exception_containment;
mod fatal_paths;
