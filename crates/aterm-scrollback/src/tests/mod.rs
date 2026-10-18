// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for the scrollback module.

use super::*;

mod basic;
mod decompression;
mod generated;
mod image_horizon;
mod line_limit;
mod memory_budget;
mod repaired_trimmed;
mod truncation;

#[path = "../performance_tests.rs"]
mod performance_tests;
