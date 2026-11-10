// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{Accounting, Fixed, Ledger, Proc, Procs, Reading, ledger::CohortFixture};

fn machine() -> Fixed {
    let machine = Fixed::new(Reading::of_gib(64, 40, 8));
    machine.set_procs(Some(Procs::of(vec![Proc {
        pid: std::process::id(),
        ppid: 1,
        charged: 30 * crate::floor::GIB,
        footprint: 30 * crate::floor::GIB,
        written: 0,
        cpu_ns: 0,
        runnable_ns: 0,
    }])));
    machine
}

#[test]
fn independent_same_custodian_budgets_reserve_before_arm_and_reopen() {
    let fixture = CohortFixture::new();
    let machine = machine();
    let first =
        Waiter::enter_cohort(&fixture.ledger, &machine, Need::new("first").mem_gib(30)).unwrap();
    let mut first = match first.poll(&machine) {
        Polled::Admitted(claim) => claim,
        other => panic!("first: {other:?}"),
    };
    assert_eq!(first.record().accounting, Accounting::CohortOwned);
    let second =
        Waiter::enter_cohort(&fixture.ledger, &machine, Need::new("second").mem_gib(20)).unwrap();
    let second = match second.poll(&machine) {
        Polled::Waiting(waiter, _) => waiter,
        other => panic!("same creator footprint must not supply credit: {other:?}"),
    };
    let reopened = Ledger::at(fixture.ledger.dir());
    let rows = reopened.list().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.accounting == Accounting::CohortOwned));
    assert_eq!(
        crate::reserved(
            &rows,
            Some(
                &Procs::of(vec![Proc {
                    pid: std::process::id(),
                    ppid: 1,
                    charged: 30 * crate::floor::GIB,
                    footprint: 30 * crate::floor::GIB,
                    written: 0,
                    cpu_ns: 0,
                    runnable_ns: 0,
                }])
                .trees(&[std::process::id()])
            ),
            &[]
        )
        .iter()
        .map(|r| r.mem)
        .sum::<u64>(),
        50 * crate::floor::GIB
    );
    let attempt = first.arm_cohort().unwrap();
    assert_eq!(first.record().accounting, Accounting::CohortOwned);
    first.cancel_cohort(attempt.cancel().unwrap()).unwrap();
    assert_eq!(first.record().accounting, Accounting::CohortOwned);
    assert!(first.credit(&[99]).is_err());
    assert_eq!(reopened.list().unwrap().len(), 2);
    drop(second);
    drop(first);
    assert!(
        reopened.list().unwrap().is_empty(),
        "unused cancellation/drop has no execution custody"
    );
}

#[test]
fn ordinary_description_keeps_holder_credit_and_legacy_owned_entry_refuses() {
    let fixture = CohortFixture::new();
    let machine = machine();
    let first = match Waiter::enter(&fixture.ledger, &machine, Need::new("ordinary").mem_gib(30))
        .unwrap()
        .poll(&machine)
    {
        Polled::Admitted(claim) => claim,
        other => panic!("ordinary: {other:?}"),
    };
    assert_eq!(first.record().accounting, Accounting::Holder);
    let second = Waiter::enter(
        &fixture.ledger,
        &machine,
        Need::new("ordinary2").mem_gib(20),
    )
    .unwrap()
    .poll(&machine);
    assert!(
        matches!(second, Polled::Admitted(_)),
        "ordinary measured creator credit is unchanged"
    );
    let absent = fixture.ledger.dir().join("absent-legacy");
    assert!(Waiter::enter_cohort(&Ledger::at(&absent), &machine, Need::new("refused")).is_err());
    assert!(!absent.exists(), "refusal creates no legacy claim");
}

#[test]
fn owned_journal_rejects_unknown_or_downgraded_law_and_has_no_lifetime_change() {
    let fixture = CohortFixture::new();
    let machine = machine();
    let waiter =
        Waiter::enter_cohort(&fixture.ledger, &machine, Need::new("owned").mem_gib(1)).unwrap();
    let text = std::fs::read_to_string(waiter.laid.path()).unwrap();
    let namespace = fixture.namespace.nonce();
    let (record, journal) =
        crate::cohort_journal::Journal::decode(&text, false, namespace).unwrap();
    assert_eq!(record.accounting, Accounting::CohortOwned);
    assert!(journal.description());
    for altered in [
        text.replace("cohort_owned", "unknown"),
        text.replacen("\"v\":3", "\"v\":2", 1),
        text.replace("\"reservation_law\":\"cohort_owned\",", ""),
    ] {
        assert!(crate::cohort_journal::Journal::decode(&altered, false, namespace).is_err());
    }
}
