// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-font integration files as modules of ONE test binary: each file
//! stays at its own path and keeps its own `#[test]`s.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-font --test font_suite -- <file>::`.

// The font oracle's std-only half (corpus format, digest, face table), shared
// with aterm-render's recorder and replay.
#[path = "../../aterm-render/tests/font_oracle/corpus.rs"]
#[allow(dead_code)]
mod corpus;

// The hinting oracle's shared half (the answer format and aterm-render's
// raster paths), over the corpus digest and aterm-render's rasterizer.
#[cfg(feature = "hint")]
use corpus::Fnv;
#[cfg(feature = "hint")]
#[path = "../../aterm-render/tests/font_oracle/hint_raster.rs"]
mod hint_raster;
#[cfg(feature = "hint")]
#[path = "../../aterm-render/src/raster.rs"]
#[allow(dead_code)]
mod raster;

mod bounded_work;
mod gvar_tuples;
#[cfg(feature = "hint")]
mod hint_oracle;
mod never_panic;
mod reader_oracle;
mod sinks;
mod synthetic_tables;
