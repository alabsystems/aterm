// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A test the HOST cannot run ends typed, never as a pass: a GPU, a platform
//! font or a privilege the machine lacks decides nothing about the code.
//!
//! Defined at the bottom of the graph, in aterm-uds (which this crate depends
//! on), so aterm-uds's own tests reach it without a dependency cycle.

pub use aterm_uds::host::{COULD_NOT_RUN, could_not_run};
