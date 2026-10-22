// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-aos integration suite: every integration test file is a `mod` here.
//!
//! `toy` is the task every test hosts (an accumulator with an intentionally introduced
//! demonstration fault); the other modules each test one module of the crate.

mod toy;

mod checkpoint_codec;
mod conformance_publication;
mod effect_frontier;
mod ident_digest;
mod log_codec;
mod publish_boundary;
mod publish_faults;
mod replay_fold;
mod wire_codec;
