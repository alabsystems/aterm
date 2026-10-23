// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unicode lookup tables.
//!
//! [`width`] and [`gcb`] were derived from the Unicode 16.0 Character Database by a generator
//! this repository never carried and are maintained by hand; [`props`] is generated and
//! sealed (`examples/unicode_tables.rs`), so it changes only by rerunning that example.
//!
//! Modules:
//! - [`width`] — East Asian Width / zero-width classification (UAX #11).
//! - [`gcb`] — Grapheme_Cluster_Break property + UAX #29 break matrix.
//! - [`props`] — `Script` and `General_Category` run tables
//!   (`examples/unicode_tables.rs props`, read through [`crate::script`] and
//!   [`crate::general_category`]).
//!
//! Only `width`'s API is exported from the crate; `gcb` is `pub(crate)` because
//! the segmentation state machine lives in [`crate::grapheme_iter`] and
//! consumes the GCB tables directly.

pub(crate) mod gcb;
pub(crate) mod props;
pub(crate) mod width;

// Re-export commonly used functions
pub use width::{char_width, char_width_cjk, is_ambiguous_width, str_width};
