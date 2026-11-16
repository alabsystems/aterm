// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Plain process observations shared by native readers and their upper judgments.
//! No host reads, timing judgments, cleanup eligibility or restart authority live here.

#![forbid(unsafe_code)]

pub mod left;
pub mod program_check;
pub mod services;
