// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual experimental restore outcomes in this process, observed by the private bench.
//! These counters and their control fields do not exist in normal release builds.

use std::sync::atomic::{AtomicU64, Ordering};

use super::wire::FieldSink;

/// What a requested restore experiment actually applied to its private copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RestoreExperiment {
    /// A copy was made and selected without damage.
    Copy,
    /// A copy was made and the requested damage was applied to it.
    Lookalike,
    /// The request could not be applied; no experimental copy was selected.
    Refused,
}

struct Counts {
    copies: AtomicU64,
    plants: AtomicU64,
    refused: AtomicU64,
}

impl Counts {
    const fn new() -> Self {
        Self {
            copies: AtomicU64::new(0),
            plants: AtomicU64::new(0),
            refused: AtomicU64::new(0),
        }
    }

    fn record(&self, outcome: RestoreExperiment) {
        match outcome {
            RestoreExperiment::Copy => {
                self.copies.fetch_add(1, Ordering::Relaxed);
            }
            RestoreExperiment::Lookalike => {
                self.copies.fetch_add(1, Ordering::Relaxed);
                self.plants.fetch_add(1, Ordering::Relaxed);
            }
            RestoreExperiment::Refused => {
                self.refused.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn emit(&self, sink: &mut dyn FieldSink) {
        sink.count(
            "restore_experiment_copies",
            self.copies.load(Ordering::Relaxed),
        );
        sink.count(
            "restore_experiment_plants",
            self.plants.load(Ordering::Relaxed),
        );
        sink.count(
            "restore_experiment_refused",
            self.refused.load(Ordering::Relaxed),
        );
    }
}

static COUNTS: Counts = Counts::new();

/// Record one completed request, never the requested arm alone.
pub(crate) fn record_restore_experiment(outcome: RestoreExperiment) {
    COUNTS.record(outcome);
}

pub(super) fn emit(sink: &mut dyn FieldSink) {
    COUNTS.emit(sink);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(counts: &Counts) -> String {
        crate::metrics::wire::render_text(|sink| counts.emit(sink))
    }

    #[test]
    fn only_successful_experiments_count_as_copies_or_plants() {
        let counts = Counts::new();
        counts.record(RestoreExperiment::Copy);
        assert_eq!(
            fields(&counts),
            " restore_experiment_copies=1 restore_experiment_plants=0 restore_experiment_refused=0"
        );
        counts.record(RestoreExperiment::Refused);
        assert_eq!(
            fields(&counts),
            " restore_experiment_copies=1 restore_experiment_plants=0 restore_experiment_refused=1"
        );
        counts.record(RestoreExperiment::Lookalike);
        assert_eq!(
            fields(&counts),
            " restore_experiment_copies=2 restore_experiment_plants=1 restore_experiment_refused=1"
        );
    }
}
