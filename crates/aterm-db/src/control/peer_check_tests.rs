// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::client::CommonsLog;

/// The stage a log endpoint's check reaches after its acknowledged log classified to `log`.
fn after(log: CommonsLog) -> Stage {
    let read =
        acknowledged_reading(Ok(log)).map_or_else(|reading| reading.outcome, |_| Read::Clear);
    next(kernel::Custody::Log, Stage::Acknowledged, read, false)
}

/// PF4: an acknowledged log of another vault lineage, or of another root, refuses the peer at
/// a log endpoint, where a store with no log or no Commons epoch, which has nothing to judge
/// by, leaves the verdict to the records held ([`Stage::Held`]) and passes. Before PF4
/// `trusted_commons_log` answered `Ok(None)` for all four, so the check could not refuse the
/// first two without refusing the last two.
///
/// NEGATIVE CONTROL (in the test): read as one with no Commons epoch
/// ([`super::super::mutants::Mutant::LineageIsNoJudgement`], as the `Ok(None)` reading did),
/// a log of another lineage passes the peer.
#[test]
fn an_acknowledged_log_of_another_lineage_refuses_where_one_with_nothing_to_judge_passes() {
    use super::super::mutants::{Mutant, with};
    for (log, stage, why) in [
        (CommonsLog::NoLog, Stage::Pass, "no log"),
        (CommonsLog::NoCommonsEpochs, Stage::Pass, "no Commons epoch"),
        (CommonsLog::OtherLineage, Stage::Refuse, "another lineage"),
        (CommonsLog::OtherRoot, Stage::Refuse, "another root"),
    ] {
        let reached = after(log);
        assert!(reached == stage, "{why}: stage {}", reached as u8);
    }
    let reached = with(Mutant::LineageIsNoJudgement, || {
        after(CommonsLog::OtherLineage)
    });
    assert!(
        reached == Stage::Pass,
        "NEGATIVE CONTROL: another lineage read as nothing to judge passes the peer: stage {}",
        reached as u8
    );
}
