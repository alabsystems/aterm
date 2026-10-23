// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The crate's one integration-test binary, one directory per area: `shared/`, `artifact/`,
//! `channel/` and `lane/`.

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

mod artifact;
mod channel;
mod lane;
mod shared;
