// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Property-based tests for aterm-core.
//!
//! Physical files relocated to test_support/proptest/ (Part of #6814).
//! These tests still exercise crate-private seams and remain owned by aterm-core.

#[path = "../../../test_support/proptest/grapheme.rs"]
mod grapheme;

// SCR-1 absolute-content pinning: live output while scrolled back keeps the same
// absolute row at the viewport top (the engine invariant the renderer's scroll-restore
// relies on). Generalizes the single processing.rs unit case over random volumes.
#[path = "../../../test_support/proptest/scroll_pin.rs"]
mod scroll_pin;

// Sixel decoder crash-safety + image-invariant proptests. Gated on the same
// off-by-default `sixel` feature that compiles the decoder: with the feature
// off there is no `crate::sixel` to exercise, so the module stays compiled out
// (mirroring the consume-only build). Run with `--features sixel`.
#[cfg(feature = "sixel")]
#[path = "../../../test_support/proptest/sixel.rs"]
mod sixel;

/// The checked-in seed corpus for one of the modules above, named OUTRIGHT.
///
/// proptest's default persistence (`SourceParallel`) derives the seed file from
/// `file!()`: it walks up from the source until it finds a directory holding a
/// `lib.rs`, then re-roots the remaining suffix under a sibling
/// `proptest-regressions/`. Every module above is mounted with
/// `#[path = "../../../test_support/…"]` (#6814), so `file!()` carries `..`
/// components — the walk stops at this crate's `src/`, and the suffix it
/// re-appends still leads with `..`. The corpus therefore resolves to
/// `src/tests/test_support/proptest/<name>.txt`: an untracked path inside
/// `src/`, which nobody checks in, and which a seed checked in where proptest
/// documents it would never be read from.
///
/// Measured, not inferred (2026-09-16 on `scrollback.rs`, and again 2026-09-27
/// on `grapheme.rs`): a forced failure prints `Saving this and future failures
/// in …/src/tests/proptest/../proptest-regressions/../test_support/proptest/
/// grapheme.txt` and writes it there.
///
/// An absolute path off `CARGO_MANIFEST_DIR` is immune to both the `#[path]`
/// spelling and the working directory, so the corpus is read and written where
/// it is checked in.
fn seed_corpus(path: &'static str) -> proptest::prelude::ProptestConfig {
    proptest::prelude::ProptestConfig {
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(path),
        )),
        ..proptest::prelude::ProptestConfig::default()
    }
}
