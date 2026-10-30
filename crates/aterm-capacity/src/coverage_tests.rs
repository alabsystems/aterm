// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether a held claim covers a process's builds ([`crate::covered_memory`], [`crate::covering`]),
//! on fixed processes and a scratch ledger: no test reads the live machine or this user's ledger.

use crate::reading::{Proc, Procs};
use crate::{
    Admit, COVER_ENV, Fixed, Held, Ledger, NO_COVER, Need, Pace, Reading, Record, admit_with,
    covered_memory, covering,
};

const GIB: u64 = 1 << 30;

fn record(pid: u32, need: Need, held: bool) -> Record {
    Record {
        pid,
        start_ms: 1,
        seq: 0,
        checkout: "/ck".to_owned(),
        need,
        held: held.then_some(Held {
            admitted_ms: 1,
            cores: 0,
            alone: false,
        }),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    }
}

/// The line `build` (300) <- `targo` (200) <- `verify` (100) <- `shell` (50).
fn line() -> Procs {
    let p = |pid, ppid| Proc {
        pid,
        ppid,
        footprint: 0,
        written: 0,
    };
    Procs::of(vec![
        p(300, 200),
        p(200, 100),
        p(100, 50),
        p(50, 1),
        p(400, 1),
    ])
}

/// A held claim of memory that asked to cover its tree covers the builds of its holder and of
/// every process descended from it. NEGATIVE CONTROL: a VM's claim of memory (no `covers`), a
/// disk claim that asked (no memory), a covering claim that still waits, and one held by a
/// process outside the line cover nothing; and with no processes read, only the holder itself is
/// covered.
#[test]
fn a_held_covering_claim_covers_its_holders_line_and_nothing_else() {
    let procs = line();
    let stage = record(
        100,
        Need::new("verify test compile").mem(18 * GIB).covers(),
        true,
    );
    let covered = |r: &Record, pid| covered_memory(pid, &[r], Some(&procs));
    assert!(covered(&stage, 300), "a grandchild's build");
    assert!(covered(&stage, 100), "its holder's own");
    assert!(!covered(&stage, 50), "its holder's parent");
    assert!(!covered(&stage, 400), "a process outside the line");
    let vm = record(100, Need::new("lab vm boot").mem(8 * GIB).margin(GIB), true);
    assert!(!covered(&vm, 300), "a VM's memory lies outside the tree");
    let disk = record(100, Need::new("verify disk").disk(GIB, "/").covers(), true);
    assert!(!covered(&disk, 300), "a claim of no memory");
    let waiting = record(100, Need::new("verify lint").mem(12 * GIB).covers(), false);
    assert!(!covered(&waiting, 300), "a claim that waits");
    let elsewhere = record(400, Need::new("another run").mem(12 * GIB).covers(), true);
    assert!(!covered(&elsewhere, 300), "another line's claim");
    assert!(covered_memory(100, &[&stage], None));
    assert!(!covered_memory(300, &[&stage], None), "no ancestry read");
}

/// A record says that it covers, and reads back so; one that does not ask says nothing.
/// NEGATIVE CONTROL: the record without the flag reads back without it.
#[test]
fn a_covering_claims_record_reads_back_as_covering() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let m = Fixed::new(Reading::of_gib(24, 20, 14));
    let mut say = |_: &str| {};
    let claim = admit_with(
        &ledger,
        &m,
        Need::new("targo test").mem(2 * GIB).covers(),
        Admit::Wait,
        Pace::default(),
        &mut say,
    )
    .expect("admitted");
    let plain = admit_with(
        &ledger,
        &m,
        Need::new("lab vm").mem(GIB),
        Admit::Wait,
        Pace::default(),
        &mut say,
    )
    .expect("admitted");
    let listed = ledger.list().expect("listed");
    let covers: Vec<(String, bool)> = listed
        .iter()
        .map(|r| (r.need.what.clone(), r.need.covers))
        .collect();
    assert_eq!(
        covers,
        vec![
            ("targo test".to_owned(), true),
            ("lab vm".to_owned(), false)
        ]
    );
    assert_eq!(ledger.of_pid(std::process::id()).expect("read").len(), 2);
    assert!(
        ledger.of_pid(1).expect("read").is_empty(),
        "no claim of pid 1"
    );
    drop((claim, plain));
}

/// A process of the holder's line is covered with the marker, without it, and with one naming a
/// pid outside the line or none (the whole ledger answers those: what reads the holder's claims
/// alone is [`the_cover_marker_reads_its_holders_claims_alone`]). NEGATIVE CONTROL: a line the
/// holder is not in is covered by neither path, and once the claim ends nothing covers.
#[test]
fn the_cover_marker_reads_its_holders_claims_and_the_ledger_answers_without_it() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let me = std::process::id();
    let m = Fixed::new(Reading::of_gib(24, 20, 14));
    let p = |pid, ppid| Proc {
        pid,
        ppid,
        footprint: 0,
        written: 0,
    };
    let build = me.wrapping_add(7);
    m.set_procs(Some(Procs::of(vec![p(build, me), p(me, 1)])));
    let mut say = |_: &str| {};
    let claim = admit_with(
        &ledger,
        &m,
        Need::new("verify test compile").mem(4 * GIB).covers(),
        Admit::Wait,
        Pace::default(),
        &mut say,
    )
    .expect("admitted");
    let marker = claim.record().cover_marker();
    let by = |marker: Option<&str>| {
        covering(&ledger, &m, build, marker)
            .expect("read")
            .map(|r| r.pid)
    };
    assert_eq!(by(Some(&marker)), Some(me));
    assert_eq!(by(None), Some(me));
    let outside = format!("0000000000001-{}-0", me.wrapping_add(99));
    assert_eq!(by(Some(&outside)), Some(me), "a marker outside the line");
    assert_eq!(by(Some(&me.to_string())), Some(me), "a bare pid");
    assert_eq!(by(Some("not a claim")), Some(me));
    m.set_procs(Some(Procs::of(vec![p(build, 1), p(me, 1)])));
    assert_eq!(by(Some(&marker)), None, "the build is not of its line");
    assert_eq!(by(None), None);
    m.set_procs(Some(Procs::of(vec![p(build, me), p(me, 1)])));
    drop(claim);
    assert_eq!(by(Some(&marker)), None, "its claim ended");
    assert_eq!(COVER_ENV, "ATERM_CAPACITY_COVER");
}

/// The marker's holder's claims are read ALONE ([`Ledger::of_pid`]): with another pid's claim
/// file unreadable, which fails a read of the whole ledger, a marked build is covered all the
/// same. NEGATIVE CONTROL: the same build with no marker reads the whole ledger, and fails.
#[cfg(unix)]
#[test]
fn the_cover_marker_reads_its_holders_claims_alone() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let me = std::process::id();
    let m = Fixed::new(Reading::of_gib(24, 20, 14));
    let build = me.wrapping_add(7);
    m.set_procs(Some(Procs::of(vec![
        Proc {
            pid: build,
            ppid: me,
            footprint: 0,
            written: 0,
        },
        Proc {
            pid: me,
            ppid: 1,
            footprint: 0,
            written: 0,
        },
    ])));
    let claim = admit_with(
        &ledger,
        &m,
        Need::new("verify test compile").mem(4 * GIB).covers(),
        Admit::Wait,
        Pace::default(),
        &mut |_: &str| {},
    )
    .expect("admitted");
    let other = dir
        .path()
        .join("ledger")
        .join(format!("0000000000001-{}-0.claim", me.wrapping_add(500)));
    std::fs::write(&other, "").expect("another pid's claim file");
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o000)).expect("unreadable");
    let marker = claim.record().cover_marker();
    let marked = covering(&ledger, &m, build, Some(&marker)).map(|r| r.map(|r| r.pid));
    let unmarked = covering(&ledger, &m, build, None).map(|r| r.map(|r| r.pid));
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o600)).expect("readable");
    assert_eq!(marked.expect("read alone"), Some(me));
    assert!(unmarked.is_err(), "{unmarked:?}");
    drop(claim);
}

/// COVERAGE IS A CLAIM'S, NOT ITS HOLDER'S: one process holding two covering claims (verify's
/// stages) covers a build marked with either by that claim alone, and one marked [`NO_COVER`]
/// (a stage that claimed nothing) by none; once a marked claim ends, its build is covered by
/// nothing though its holder's other claim is held. NEGATIVE CONTROL: unmarked, the whole ledger
/// answers, and the other claim covers.
#[test]
fn a_marker_names_one_claim_of_its_holder() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let me = std::process::id();
    let m = Fixed::new(Reading::of_gib(48, 40, 14));
    let build = me.wrapping_add(7);
    m.set_procs(Some(Procs::of(vec![
        Proc {
            pid: build,
            ppid: me,
            footprint: 0,
            written: 0,
        },
        Proc {
            pid: me,
            ppid: 1,
            footprint: 0,
            written: 0,
        },
    ])));
    let take = |what: &str| {
        admit_with(
            &ledger,
            &m,
            Need::new(what).mem(4 * GIB).covers(),
            Admit::Wait,
            Pace::default(),
            &mut |_: &str| {},
        )
        .expect("admitted")
    };
    let lint = take("verify tippy lint");
    let compile = take("verify test compile");
    let by = |marker: Option<&str>| {
        covering(&ledger, &m, build, marker)
            .expect("read")
            .map(|r| r.need.what)
    };
    let (lints, compiles) = (
        lint.record().cover_marker(),
        compile.record().cover_marker(),
    );
    assert_ne!(lints, compiles);
    assert_eq!(by(Some(&lints)).as_deref(), Some("verify tippy lint"));
    assert_eq!(by(Some(&compiles)).as_deref(), Some("verify test compile"));
    assert_eq!(by(Some(NO_COVER)), None, "a stage that claimed nothing");
    drop(lint);
    assert_eq!(by(Some(&lints)), None, "its own claim ended");
    assert_eq!(by(None).as_deref(), Some("verify test compile"));
    assert_eq!(Record::marker_holder(&compiles), Some(me));
    assert_eq!(Record::marker_holder(&me.to_string()), None);
    drop(compile);
}

/// A ledger that cannot be read is an error, which a build admission fails open on: a file
/// stands where the directory should. NEGATIVE CONTROL: a ledger not made yet holds no claim.
#[test]
fn an_unreadable_ledger_is_an_error_and_an_absent_one_covers_nothing() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let file = dir.path().join("ledger");
    std::fs::write(&file, b"not a directory").expect("a file");
    let m = Fixed::new(Reading::of_gib(24, 20, 14));
    assert!(covering(&Ledger::at(&file), &m, 1, None).is_err());
    let absent = Ledger::at(dir.path().join("absent"));
    assert_eq!(covering(&absent, &m, 1, None).expect("read"), None);
}
