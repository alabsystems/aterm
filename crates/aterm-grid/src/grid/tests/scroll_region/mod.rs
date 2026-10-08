// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Scroll region and unscroll-from-scrollback tests:
//! - `band_moves`: the row-band record behind `coordinates_invalidated`
//! - `region_ops`: IL/DL/SU/SD within scroll regions
//! - `hyperlink_regressions`: CellExtras/hyperlink coverage
//! - `unscroll`: tiered scrollback unscroll and attribute restoration
//! - `unscroll_region_scroll`: unscroll metadata/damage for cache reuse

use super::super::*;

mod band_moves;
mod hyperlink_regressions;
mod region_ops;
mod unscroll;
mod unscroll_region_scroll;
