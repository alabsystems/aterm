// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LAB'S OWN TABLES (docs/DESIGN-lab-tracker-2026-11-14.md): typed Commons table families
//! the whole lab reads and writes, beside the registry (`crate::alab::registry`).
//!
//! - [`tracker`]: `lab/tracker`, the goals, lanes and packages the lab runs on, with every
//!   change saying who, why and on what evidence.

pub mod tracker;
