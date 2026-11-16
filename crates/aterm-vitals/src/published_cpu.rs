// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The temporal meaning of native cumulative CPU-counter observations.
//!
//! A complete ownership census does not make CPU publication synchronous. A counter read
//! can omit unpublished execution and a later read can publish execution from before the
//! earlier read. Read timestamps bound observations, not the accounting epochs behind them.

/// Which quantity a CPU rate describes, independently of ownership/read coverage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CpuBasis {
    /// Growth published by cumulative counters, divided by the frontend read separation.
    #[default]
    PublishedCounters,
}

/// Whether the producer establishes CPU execution within the frontend read interval.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CpuExecutionWindow {
    /// No bound on unpublished counter tails or their publication epochs is available.
    #[default]
    Unavailable,
}

/// CPU accounting semantics, separate from missing reads and ownership coverage.
///
/// There is no producer-supported execution-window assurance in this type. In particular,
/// complete coverage cannot turn reported growth into an actual-window floor, share or
/// machine remainder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuAccounting {
    /// The quantity represented by a rate.
    pub basis: CpuBasis,
    /// The independently established temporal assurance.
    pub execution_window: CpuExecutionWindow,
}

impl CpuAccounting {
    /// Native cumulative counters, with unknown execution epochs.
    pub const PUBLISHED_COUNTERS: Self = Self {
        basis: CpuBasis::PublishedCounters,
        execution_window: CpuExecutionWindow::Unavailable,
    };
}
