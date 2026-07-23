// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Text shaping configuration for terminal rendering.
//!
//! Core types are defined in `aterm-types::text_shaping` and re-exported here
//! for backward compatibility.

// Re-export core types from aterm-types (Part of #2584).
pub(crate) use aterm_types::text_shaping::TextShapingConfig;
// AmbiguousWidth and LigatureMode are consumed only by test code within aterm-core;
// aterm-gpu imports them directly from aterm-types. The string→`FontFeature`
// parser (`parse_font_features`) is the canonical one in
// `aterm_types::text_shaping`; the GUI config loader calls it directly.
#[cfg(test)]
pub use aterm_types::text_shaping::{AmbiguousWidth, LigatureMode};

// FFI-safe text shaping types (AtermLigatureMode, AtermAmbiguousWidth,
// AtermFontFeature, AtermFontFeatureSet, AtermTextShapingConfig) now live
// in aterm-gpu/src/ffi/hybrid/text_shaping_ffi.rs. cbindgen picks them up
// via parse.include. See #2777.

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
#[path = "../test_support/text_shaping_config_tests.rs"]
mod tests;
