// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::{ReportOutcome, outcome_of};
use crate::error::{Error, Kind, SideEffect};
use crate::write::{Deleted, Effect};

/// Every outcome reads back from its own slug, an effect's and a delete's slugs are the
/// outcomes they convert to, and only `created`, `deleted` and `unknown` say GitHub may
/// hold a change.
#[test]
fn each_report_outcome_reads_back_from_its_slug() {
    for outcome in ReportOutcome::ALL {
        assert_eq!(ReportOutcome::from_slug(outcome.slug()), Some(outcome));
    }
    assert_eq!(ReportOutcome::from_slug("done"), None);
    for effect in [Effect::Created, Effect::Converged, Effect::Unchanged] {
        assert_eq!(ReportOutcome::from(effect).slug(), effect.slug());
    }
    for deleted in [Deleted::Removed, Deleted::Gone] {
        assert_eq!(ReportOutcome::from(deleted).slug(), deleted.outcome_slug());
    }
    let changed: Vec<&str> = ReportOutcome::ALL
        .into_iter()
        .filter(|outcome| outcome.side_effect() != SideEffect::None)
        .map(ReportOutcome::slug)
        .collect();
    assert_eq!(changed, ["created", "deleted", "unknown"]);
}

/// Each row of the outcome rule: a settle that failed after a 2xx (could_not_run, side
/// effect applied) is `unknown`, a lost answer is `unknown`, a local stop before the send
/// is `not_sent`, and GitHub's or agh's refusal is `refused`.
#[test]
fn the_outcome_rule_row_by_row() {
    let after_a_2xx = Error::local("GitHub answered 201, and the journal could not")
        .with_side_effect(SideEffect::Applied);
    let rows = [
        (after_a_2xx, "unknown"),
        (
            Error::could_not_run("no answer").with_side_effect(SideEffect::Unknown),
            "unknown",
        ),
        (
            Error::new(Kind::OutcomeUnknown, "an open intent"),
            "unknown",
        ),
        (crate::write::unix_only(), "not_sent"),
        (Error::refused("a refusal"), "refused"),
        (Error::new(Kind::Conflict, "a guard"), "refused"),
        (Error::new(Kind::Invalid, "too long"), "refused"),
    ];
    for (error, word) in rows {
        assert_eq!(outcome_of(&error), word, "{error:?}");
    }
}
