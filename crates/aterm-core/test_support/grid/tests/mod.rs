// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Grid tests — residual modules after Batch 2 migration to aterm-grid (#6556).
//!
//! All zero-coupling, scrollback-coupled, and performance tests moved to
//! `aterm-grid` in Batches 1–2. Remaining here: facade contract test and
//! `take_response_preserves_capacity` (requires `crate::terminal::Terminal`).

mod extras_performance;
mod facade_completeness;
