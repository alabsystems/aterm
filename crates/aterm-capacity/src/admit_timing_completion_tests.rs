// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn protection_expiring_during_work_cannot_qualify_its_result() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path().join("ledger"));
    let machine = crate::tests::TimingMachine::new(&[(0, 0), (1, 99), (2, 198), (3, 297)]);
    let mut claim = crate::tests::timed_in(&ledger, &machine);
    claim
        .measurement_entry_on(&machine, Pace::default(), &mut |_| false)
        .unwrap();
    claim.record.need.quiet_policy = crate::QuietPolicy::Unknown;
    assert_eq!(
        claim.timing_admission(),
        Err(crate::TimingIssue::UnknownAdmission)
    );
    claim.record.need.quiet_policy = crate::QuietPolicy::Timing;
    {
        // Inject completion after the EXISTING hold, without waiting or changing its duration.
        let admitted = now_ms().saturating_sub(millis(claim.record.need.patience.unwrap().hold));
        claim.record.held.as_mut().unwrap().admitted_ms = admitted;
        claim.record.timing.admitted_ms = Some(admitted);
    }
    assert_eq!(claim.timing_admission(), Err(crate::TimingIssue::Expired));
}
