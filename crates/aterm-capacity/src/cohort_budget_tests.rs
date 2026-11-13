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

/// `--admit now` ON A COHORT-AWARE LEDGER WAITS ITS TURN FOR A CORE (TM-1 review): an override
/// bypasses room, never the physical core bound, on either kind of ledger. Beside a holder of
/// every core, with room to spare, a forced look admits nothing and looks again in its turn
/// ([`crate::Forced::Turn`], short of cores), never [`Error::Refused`], which its callers start
/// unclaimed past that bound. NEGATIVE CONTROLS: once the holder lets go, the same forced look
/// admits it; (by hand: the cohort look taking only the ledger's lock as its turn, as before)
/// `Refused`.
#[test]
fn a_forced_look_short_of_cores_on_a_cohort_ledger_waits_its_turn() {
    let fixture = CohortFixture::new();
    let machine = machine();
    let cores = crate::Machine::read(&machine, None).cores;
    let held = Waiter::enter_cohort(
        &fixture.ledger,
        &machine,
        Need::new("every core").mem_gib(1).cores(cores),
    )
    .unwrap();
    let held = match held.poll(&machine) {
        Polled::Admitted(claim) => claim,
        other => panic!("the holder: {other:?}"),
    };
    let need = Need::new("now").mem_gib(1).cores(1);
    let forced = Waiter::enter_cohort(&fixture.ledger, &machine, need.clone())
        .unwrap()
        .force(&machine);
    match forced {
        Ok(crate::Forced::Turn(waiter, short)) => {
            assert!(short.shorts.contains(&Short::Cores), "{short:?}");
            drop(waiter);
        }
        other => panic!("a forced look beside every core held: {other:?}"),
    }
    drop(held);
    match Waiter::enter_cohort(&fixture.ledger, &machine, need)
        .unwrap()
        .force(&machine)
    {
        Ok(crate::Forced::Admitted(claim, _)) => drop(claim),
        other => panic!("negative control: the cores freed: {other:?}"),
    }
}

/// `--admit now` ON A COHORT-AWARE LEDGER SHORT OF CORES AND OF ROOM WAITS ITS TURN (TM-1 review,
/// at the merge of main): the holder branch looks again for any look short of a core, whatever
/// else it is short of, and so does the cohort branch, never [`Error::Refused`], which the lab
/// boots unclaimed past the core bound. Beside a holder of every core and 30 of the 40 GiB free,
/// a forced look for a core and 20 GiB is short of both and is [`crate::Forced::Turn`].
/// NEGATIVE CONTROLS: once the holder lets go, the same forced look admits it; (by hand: the
/// cohort look waiting only for a shortfall made wholly of those) `Refused`.
#[test]
fn a_forced_look_short_of_cores_and_room_on_a_cohort_ledger_waits_its_turn() {
    let fixture = CohortFixture::new();
    let machine = machine();
    let cores = crate::Machine::read(&machine, None).cores;
    let held = Waiter::enter_cohort(
        &fixture.ledger,
        &machine,
        Need::new("every core").mem_gib(30).cores(cores),
    )
    .unwrap();
    let held = match held.poll(&machine) {
        Polled::Admitted(claim) => claim,
        other => panic!("the holder: {other:?}"),
    };
    let need = Need::new("now").mem_gib(20).cores(1);
    match Waiter::enter_cohort(&fixture.ledger, &machine, need.clone())
        .unwrap()
        .force(&machine)
    {
        Ok(crate::Forced::Turn(waiter, short)) => {
            assert!(short.shorts.contains(&Short::Cores), "{short:?}");
            assert!(
                short
                    .shorts
                    .iter()
                    .any(|short| matches!(short, Short::Memory { .. })),
                "{short:?}"
            );
            drop(waiter);
        }
        other => panic!("a forced look short of cores and room: {other:?}"),
    }
    drop(held);
    match Waiter::enter_cohort(&fixture.ledger, &machine, need)
        .unwrap()
        .force(&machine)
    {
        Ok(crate::Forced::Admitted(claim, _)) => drop(claim),
        other => panic!("negative control: the holder let go: {other:?}"),
    }
}

/// A COHORT-AWARE LEDGER ADMITS NO BACKGROUND NEED, SO NO FORCED LOOK THERE MEETS A RELEASE STOP
/// (TM-1 review, at the merge of main): a Release claim ends repeatable Background work
/// ([`Short::Release`]), which no override passes. [`Waiter::force`]'s holder branch looks again
/// on it; its cohort branch names only the ledger's lock and cores, because a Background need is
/// refused at either entry to a cohort-aware ledger. NEGATIVE CONTROL: an Ordinary need's forced
/// look beside the Release admits at once, short of nothing. (By hand: with the entry's guard
/// gone, the Background need enters.)
#[test]
fn a_cohort_ledger_admits_no_background_need_so_no_forced_look_there_meets_a_release() {
    let fixture = CohortFixture::new();
    let machine = machine();
    let release = Waiter::enter_cohort(
        &fixture.ledger,
        &machine,
        Need::new("release").rank(crate::Rank::Release),
    )
    .unwrap();
    let release = match release.poll(&machine) {
        Polled::Admitted(claim) => claim,
        other => panic!("the Release: {other:?}"),
    };
    let background = Need::new("background now")
        .mem_gib(1)
        .rank(crate::Rank::Background);
    for entered in [
        Waiter::enter_cohort(&fixture.ledger, &machine, background.clone()),
        Waiter::enter(&fixture.ledger, &machine, background),
    ] {
        let refused = entered.expect_err("a Background need on a cohort-aware ledger");
        assert!(refused.to_string().contains("holder ledger"), "{refused}");
    }
    match Waiter::enter_cohort(
        &fixture.ledger,
        &machine,
        Need::new("ordinary now").mem_gib(1),
    )
    .unwrap()
    .force(&machine)
    {
        Ok(crate::Forced::Admitted(claim, None)) => drop(claim),
        other => panic!("negative control: an Ordinary look beside the Release: {other:?}"),
    }
    drop(release);
}

/// `--admit now` ON A COHORT-AWARE LEDGER SHORT OF ROOM IS REFUSED (TM-1 review): its forced look
/// only polls, so one that finds no room beside a held claim admits nothing and says what it was
/// short of ([`Error::Refused`], never the ledger's failure), and its `kind=admission` row says
/// `outcome=refused`. NEGATIVE CONTROL: once the holder lets go, the same forced look admits it.
#[test]
fn a_forced_look_short_of_room_on_a_cohort_ledger_is_refused() {
    let fixture = CohortFixture::new();
    let machine = machine();
    let held =
        Waiter::enter_cohort(&fixture.ledger, &machine, Need::new("first").mem_gib(30)).unwrap();
    let held = match held.poll(&machine) {
        Polled::Admitted(claim) => claim,
        other => panic!("the holder: {other:?}"),
    };
    let need = Need::new("now").mem_gib(20);
    let forced = Waiter::enter_cohort(&fixture.ledger, &machine, need.clone())
        .unwrap()
        .force(&machine);
    let refused = match forced {
        Err(e @ Error::Refused(_)) => e,
        other => panic!("a forced look beside the holder: {other:?}"),
    };
    let mut written = Vec::new();
    crate::telemetry::Asked::of(&need).decided_in(
        &fixture.ledger,
        fixture.ledger.dir(),
        "m14",
        aterm_telemetry::Digest::dir("/facts"),
        &Err(refused),
        |row| {
            written.push((row.outcome, row.stop));
            true
        },
    );
    assert_eq!(
        written,
        [(
            aterm_telemetry::Outcome::Refused,
            aterm_telemetry::Stop::Exit
        )]
    );
    drop(held);
    match Waiter::enter_cohort(&fixture.ledger, &machine, need)
        .unwrap()
        .force(&machine)
    {
        Ok(crate::Forced::Admitted(claim, _)) => drop(claim),
        other => panic!("negative control: room freed: {other:?}"),
    }
}
