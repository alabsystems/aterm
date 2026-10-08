// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Unsafe path boundary tests (exercised under Miri): the `get_unchecked*`
//! paths in `write_char_styled` and `write_wide_char` at edge indices.

use super::super::*;
use super::make_row;

mod boundaries;
mod memory_safety;
mod overwrite;
mod unchecked;
