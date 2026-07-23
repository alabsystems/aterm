// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Scroll region and unscroll-from-scrollback tests.
//!
//! Split into focused submodules to keep each test group discoverable:
//! - `region_ops`: IL/DL/SU/SD behavior within scroll regions
//! - `hyperlink_regressions`: CellExtras/hyperlink regression coverage
//! - `unscroll`: tiered scrollback unscroll behavior and attribute restoration
//! - `unscroll_region_scroll`: unscroll metadata/damage verification for cache reuse

use super::super::*;

mod hyperlink_regressions;
mod region_ops;
mod unscroll;
mod unscroll_region_scroll;
