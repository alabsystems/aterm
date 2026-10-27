// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Combined `aterm-font` integration test binary (`autotests = false` in `Cargo.toml`,
//! so every `tests/*.rs` file must be a `mod` here or a standalone `[[test]]`).

#[path = "../../aterm-render/tests/font_oracle/corpus.rs"]
#[allow(dead_code)]
mod corpus;

#[cfg(any(feature = "hint", debug_assertions))]
use corpus::Fnv;
#[cfg(any(feature = "hint", debug_assertions))]
#[path = "../../aterm-render/tests/font_oracle/hint_raster.rs"]
mod hint_raster;
#[cfg(any(feature = "hint", debug_assertions))]
#[path = "../../aterm-render/src/raster.rs"]
#[allow(dead_code)]
mod raster;

mod bounded_work;
mod gpos_shaping;
mod gvar_tuples;
#[cfg(any(feature = "hint", debug_assertions))]
mod hint_extremes;
#[cfg(any(feature = "hint", debug_assertions))]
mod hint_oracle;
mod never_panic;
mod reader_oracle;
mod sinks;
mod synthetic_tables;
