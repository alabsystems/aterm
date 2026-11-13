// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
