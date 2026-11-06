// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The crate's one integration-test binary, one directory per area: `shared/`, `artifact/`,
//! `channel/` and `lane/`, and `adopt.rs`.

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

#[cfg(unix)]
mod adopt;
mod artifact;
mod channel;
#[cfg(unix)]
mod harness_local_guardian;
mod lane;
mod shared;
