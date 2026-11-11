// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::symbols::SymbolDiff;

#[test]
fn findings_outrank_a_stop_and_a_stop_alone_could_not_run() {
    let mut report = Report::default();
    assert_eq!(report.exit_code(), EXIT_PASS);
    report.could_not_run = Some(CouldNotRun::bare("no trustc"));
    assert_eq!(report.exit_code(), EXIT_COULD_NOT_RUN);
    report
        .findings
        .push(Finding::Symbol(SymbolDiff::AbsentFromReference {
            name: "made_up".into(),
        }));
    assert_eq!(
        report.exit_code(),
        EXIT_FINDING,
        "a defect found before the machine failed is still a defect"
    );
}
