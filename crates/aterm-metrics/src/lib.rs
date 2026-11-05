// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![forbid(unsafe_code)]

//! aterm-metrics: each Mac's own metric rows on `metrics.log`, and the merge every fleet view
//! computes from its own copy (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.14, step Q3).
//!
//! Each Mac already holds its accounts' quota, its token counts and its other measurements.
//! It publishes them as its own rows, only when they change, plus one summary per closed
//! minute and hour. Every Mac holds every Mac's rows, and each fleet view (who has quota, who
//! is near a wall, tokens per hour) is an exact merge computed from its own copy. No Mac polls
//! another; a Mac that is away shows its last rows with their age.
//!
//! - [`subject`]: the subject grammar, `/m/n-<id>/{local,shared}/…`, with compiled metric
//!   names.
//! - [`row`]: the row codecs (quota, agy, place, needs, unread, and the window record), words
//!   and numbers only.
//! - [`rules`]: the log's retention, astream's `Retention`.
//! - [`merge`]: the two merges (snapshots by highest sequence, counters by highest revision
//!   then a sum) and the view model with ages.
//! - [`view`]: the pages (Q5).
//!
//! This crate reads no clock, no file and no network: the node service's publisher (Q4) and
//! the fleet views (Q5) hand it their rows and their `now`.

pub mod merge;
pub mod row;
pub mod rules;
pub mod subject;
pub mod view;

#[cfg(test)]
mod tests;
