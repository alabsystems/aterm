// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn asked() -> Asked {
    Asked {
        what: "targo test aterm-gui".to_owned(),
        checkout: Some("/Users//me/secret-project".to_owned()),
        at_ms: 1_000,
    }
}

fn facts() -> Digest {
    Digest::of("test", &[b"facts"])
}

#[test]
fn an_admitted_claim_writes_its_own_wait_and_no_words() {
    let row = row(
        &asked(),
        &Decision::Admitted(Fig::Known(4_250)),
        1_000,
        "m14",
        facts(),
    );
    assert_eq!(row.kind, Kind::Admission);
    assert_eq!(
        (row.wait_ms.clone(), row.wall_ms.clone()),
        (Fig::Known(4_250), Fig::Known(4_250))
    );
    assert_eq!(
        (row.cpu_ms.clone(), row.peak_bytes.clone()),
        (Fig::Na, Fig::Na)
    );
    assert_eq!(
        (row.outcome, row.stop, row.meter),
        (Outcome::Ok, Stop::Exit, Meter::Exact)
    );
    assert_eq!(row.label, "targo");
    let line = row.line();
    assert!(
        !line.contains("aterm-gui") && !line.contains("secret-project"),
        "{line}"
    );
    assert_eq!(
        row.key,
        Some(Digest::key(
            &Kind::Admission,
            "targo test aterm-gui",
            Some("/Users//me/secret-project")
        ))
    );
}

#[test]
fn a_claim_not_admitted_says_why_by_its_outcome_and_stop() {
    let at = |e: &Error| {
        let r = row(
            &asked(),
            &Decision::Not(e, Fig::Known(9)),
            1,
            "m14",
            facts(),
        );
        (r.outcome, r.stop, r.sig, r.meter)
    };
    assert_eq!(
        at(&Error::Never("too big".to_owned())),
        (Outcome::Refused, Stop::Exit, None, Meter::Stamped)
    );
    assert_eq!(
        at(&Error::Unmet {
            what: "verify".to_owned(),
            short: crate::Shortfall::default(),
            holders: None,
            relieved: true,
        }),
        (Outcome::Refused, Stop::Exit, None, Meter::Stamped)
    );
    assert_eq!(
        at(&Error::Stopped {
            signal: Some(2),
            short: None
        }),
        (Outcome::Fail, Stop::Signal, Some(2), Meter::Stamped)
    );
    assert_eq!(
        at(&Error::Ledger(std::io::Error::other("gone"))),
        (Outcome::Fail, Stop::Unknown, None, Meter::Stamped)
    );
    assert_eq!(
        at(&Error::Refused(crate::Shortfall::default())),
        (Outcome::Refused, Stop::Exit, None, Meter::Stamped)
    );
}

fn host() -> aterm_telemetry::HostFacts {
    aterm_telemetry::HostFacts {
        host: "m14".to_owned(),
        model: "Mac15,14".to_owned(),
        cpu: "Apple M3 Ultra".to_owned(),
        cores: 32,
        mem_mib: 98_304,
        os: "macOS 27.0.1".to_owned(),
        aterm: "0.155.0+gtest".to_owned(),
    }
}

fn refused() -> Result<Claim, Error> {
    Err(Error::Never("too big".to_owned()))
}

/// A CLAIM THAT WAITED ACROSS A SLEEP (TM-1 review): macOS's monotonic clock stops while the
/// Mac sleeps and the wall clock does not, so the claim's own [`Claim::waited`] says an hour
/// less than its record's stamps. Its admission row says the record's wait, by the wall
/// clock, which is what its build or test row reads from the same record ([`wait`]): one
/// wait from one clock. NEGATIVE CONTROL (by hand: the admitted row's wait taken from
/// `waited`, as before): the row says seconds where the build row says an hour.
#[test]
fn a_claim_that_waited_across_a_sleep_says_one_wait_in_both_rows() {
    const HOUR: u64 = 3_600_000;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let ledger = Ledger::at(tmp.path().join("ledger"));
    let m = crate::Fixed::new(crate::Reading::of_gib(48, 40, 14));
    let need = || crate::Need::new("targo test -p a").mem(crate::floor::GIB);
    let asked = Asked::of(&need());
    let mut claim = crate::admit_with(
        &ledger,
        &m,
        need(),
        crate::Admit::Wait,
        crate::Pace::default(),
        &mut |_| {},
    )
    .expect("held");
    claim.slept_while_waiting(HOUR);
    assert!(
        claim.waited() < std::time::Duration::from_secs(60),
        "this process's clock saw no sleep"
    );
    let record = claim.record().clone();
    let build_rows = wait(record.start_ms, record.held.map(|h| h.admitted_ms));
    assert!(
        build_rows.known().is_some_and(|w| w >= HOUR),
        "{build_rows:?}"
    );
    let mut rows = Vec::new();
    let facts = host();
    decided_in(
        &ledger,
        ledger.dir(),
        &facts.host,
        facts.digest(),
        &asked,
        Ok(&claim),
        |row| {
            rows.push(row.clone());
            true
        },
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].wait_ms, build_rows, "one wait in both rows");
    assert_eq!(rows[0].wall_ms, rows[0].wait_ms);
    assert_eq!(rows[0].meter, Meter::Exact);
}

/// A refused admission's wait runs from its ask's stamp to its decision's, by the same wall
/// clock: a clock set back between them is `?clock`, never a known 0, and so is an admitted
/// claim's whose grant reads before its ask. NEGATIVE CONTROL: stamps in order give theirs.
#[test]
fn a_wait_whose_stamps_run_backwards_is_no_known_wait() {
    assert_eq!(interval(5, 9), Fig::Unknown("clock".to_owned()));
    assert_eq!(interval(9, 5), Fig::Known(4));
    assert_eq!(wait(9, Some(5)), Fig::Unknown("clock".to_owned()));
    assert_eq!(wait(5, Some(9)), Fig::Known(4));
    assert_eq!(wait(5, Some(0)), Fig::Unknown("aterm-capacity".to_owned()));
    assert_eq!(wait(5, None), Fig::Unknown("aterm-capacity".to_owned()));
    let later = Asked {
        at_ms: u64::MAX,
        ..asked()
    };
    let mut rows = Vec::new();
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let ledger = Ledger::at(tmp.path().to_path_buf());
    let facts = host();
    decided_in(
        &ledger,
        ledger.dir(),
        &facts.host,
        facts.digest(),
        &later,
        refused().as_ref(),
        |row| {
            rows.push(row.clone());
            true
        },
    );
    assert_eq!(
        (rows[0].wait_ms.clone(), rows[0].meter),
        (Fig::Unknown("clock".to_owned()), Meter::Stamped)
    );
}

/// AN OWED ROW IS WRITTEN ONCE, AT ITS DECISION OR AS ITS WAIT IS GIVEN UP (TM-1 review): an
/// admission whose caller decides it across calls of its own ([`Owed`]) writes one row at its
/// decision, with the wait its record's stamps say, and none as it drops after; one given up
/// undecided writes one, `fail`, as it drops, or with the signal its caller names. One in
/// another ledger writes none. The capture takes `admit_with`'s row too, and one inside
/// another takes only its own. NEGATIVE CONTROLS (by hand): with `decided` writing nothing,
/// the first case has no row; with `Drop` writing whatever was decided, it has two.
#[test]
fn an_owed_row_is_written_once_at_its_decision_or_as_its_wait_is_given_up() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let ledger = Ledger::at(tmp.path().join("ledger"));
    let m = crate::Fixed::new(crate::Reading::of_gib(48, 40, 14));
    let need = || crate::Need::new("trust check a").mem(crate::floor::GIB);
    let words = |r: &Row| (r.kind.clone(), r.outcome, r.stop, r.sig, r.meter);
    let (claim, rows) = capture(ledger.clone(), || {
        let owed = Owed::of(&ledger, &need());
        let claim = match crate::Waiter::enter(&ledger, &m, need())
            .expect("entered")
            .poll(&m)
        {
            crate::Polled::Admitted(claim) => claim,
            other => panic!("not admitted: {other:?}"),
        };
        owed.decided(Ok(&claim));
        claim
    });
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        words(&rows[0]),
        (Kind::Admission, Outcome::Ok, Stop::Exit, None, Meter::Exact)
    );
    assert_eq!(
        rows[0].wait_ms,
        wait(claim.asked_ms(), claim.record().held.map(|h| h.admitted_ms))
    );
    let ((), rows) = capture(ledger.clone(), || drop(Owed::of(&ledger, &need())));
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        words(&rows[0]),
        (
            Kind::Admission,
            Outcome::Fail,
            Stop::Exit,
            None,
            Meter::Stamped
        )
    );
    let ((), rows) = capture(ledger.clone(), || {
        Owed::of(&ledger, &need()).stopped(Some(15));
    });
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        words(&rows[0]),
        (
            Kind::Admission,
            Outcome::Fail,
            Stop::Signal,
            Some(15),
            Meter::Stamped
        )
    );
    let other = Ledger::at(tmp.path().join("other"));
    let ((), rows) = capture(ledger.clone(), || drop(Owed::of(&other, &need())));
    assert!(rows.is_empty(), "another ledger: {rows:?}");
    let ((held, inner), outer) = capture(ledger.clone(), || {
        let inner = capture(ledger.clone(), || {
            crate::admit_with(
                &ledger,
                &m,
                need(),
                crate::Admit::Wait,
                crate::Pace::default(),
                &mut |_| {},
            )
            .expect("held")
        });
        drop(Owed::of(&ledger, &need()));
        inner
    });
    assert_eq!(
        (inner.len(), inner[0].outcome),
        (1, Outcome::Ok),
        "{inner:?}"
    );
    assert_eq!(
        (outer.len(), outer[0].outcome),
        (1, Outcome::Fail),
        "{outer:?}"
    );
    drop((claim, held));
}

#[test]
fn only_this_users_ledger_writes_a_row() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let users = tmp.path().join("ledger");
    std::fs::create_dir(&users).expect("ledger");
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir(&scratch).expect("scratch");
    let dir = tmp.path().join("telemetry");
    let lost = aterm_telemetry::direct::Lost::new();
    let at = |ledger: &Path| {
        let facts = host();
        decided_in(
            &Ledger::at(ledger.to_path_buf()),
            &users,
            &facts.host,
            facts.digest(),
            &asked(),
            refused().as_ref(),
            |row| aterm_telemetry::direct::write(dir.clone(), &facts, row, &lost),
        );
    };
    at(&scratch);
    assert!(!dir.exists(), "a scratch ledger wrote a row");
    at(&users);
    let read = aterm_telemetry::log::read(&dir, 4, None).expect("read");
    assert_eq!(read.rows.len(), 1);
    let r = &read.rows[0];
    assert_eq!(
        (&r.kind, r.outcome, r.label.as_str()),
        (&Kind::Admission, Outcome::Refused, "targo")
    );
}
