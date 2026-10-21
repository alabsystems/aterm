// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The producer's one integration-test binary: `shared/` (the shared types), `artifact/`,
//! `channel/` and `lane/`, one directory per area of the crate.
//!
//! The host needs the `zstd` CLI on `PATH`, as a publishing host does: every real pack
//! is compressed by it (`artifact::tarwrite::COMPRESSOR`), so without it the tests that
//! pack a tree (tarwrite, pack, bundle and the end-to-end ones) fail with "did not
//! start". It is not a stock macOS tool and aterm provides none (S12's Compressor port
//! removes the tests' need), so without it these tests end COULD NOT RUN.

mod artifact;
mod channel;
mod lane;
mod shared;
