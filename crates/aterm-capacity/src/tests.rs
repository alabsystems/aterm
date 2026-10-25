// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ledger's admissions on an injected machine ([`Fixed`]): no test reads the live one.

use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_spec::wait::{MUST_HAPPEN, until_within};

use crate::admit::{Other, decide};
use crate::floor::{self, GIB};
use crate::reading::{Proc, Procs, TreeUse};
use crate::{
    Admit, Error, Fixed, Ledger, Need, Pace, Polled, Reading, Record, Short, Volume, Waiter,
    admit_with,
};

/// A scratch ledger, removed when the test ends.
struct Scratch {
    ledger: Ledger,
    dir: aterm_tempfile::TempDir,
}

fn scratch(label: &str) -> Scratch {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("cap-{label}-"))
        .tempdir()
        .expect("a scratch ledger");
    Scratch {
        ledger: Ledger::at(dir.path().join("ledger")),
        dir,
    }
}

/// The acceptance machine: 48 GiB, 40 of it available, 14 idle cores.
fn mac48() -> Fixed {
    Fixed::new(Reading::of_gib(48, 40, 14))
}

/// A waiter for `need`, entered now.
fn enter(s: &Scratch, m: &Fixed, need: Need) -> Waiter {
    Waiter::enter(&s.ledger, m, need).expect("entered")
}

/// `line` names each of `figures`: what a person reads a wait line for, whatever its wording.
#[track_caller]
fn names(line: &str, figures: &[&str]) {
    for f in figures {
        assert!(line.contains(f), "{line:?} does not name {f:?}");
    }
}

/// Poll once and expect admission.
#[track_caller]
fn admitted(w: Waiter, m: &Fixed) -> crate::Claim {
    match w.poll(m) {
        Polled::Admitted(c) => c,
        Polled::Waiting(_, short) => panic!("expected admission, waits: {short}"),
    }
}

/// Poll once and expect a wait.
#[track_caller]
fn waits(w: Waiter, m: &Fixed) -> (Waiter, crate::Shortfall) {
    match w.poll(m) {
        Polled::Waiting(w, short) => (w, short),
        Polled::Admitted(c) => panic!("expected a wait, admitted: {:?}", c.record()),
    }
}

/// On a 48 GiB machine with 40 GiB available, a 30 GiB claim is admitted and a 20 GiB one
/// waits, short by 10 GiB, naming the 30 GiB holder.
#[test]
fn a_claim_that_fits_is_admitted_and_one_that_does_not_waits() {
    let s = scratch("fits");
    let m = mac48();
    let a = admitted(enter(&s, &m, Need::new("A").mem_gib(30)), &m);
    let (b, short) = waits(enter(&s, &m, Need::new("B").mem_gib(20)), &m);
    assert_eq!(
        short.shorts,
        vec![Short::Memory {
            need: 20 * GIB,
            available: 40 * GIB,
            reserved: 30 * GIB
        }]
    );
    assert_eq!(short.holders.len(), 1, "{short}");
    assert!(short.holders[0].held && short.holders[0].mem == 30 * GIB);
    names(
        &short.to_string(),
        &[
            "10 GiB",
            "20 GiB",
            "40 GiB",
            "30 GiB",
            &std::process::id().to_string(),
        ],
    );
    // The ledger lists both, the holder first.
    let listed = s.ledger.list().expect("listed");
    assert_eq!(
        listed
            .iter()
            .map(|r| (r.need.what.as_str(), r.held.is_some()))
            .collect::<Vec<_>>(),
        [("A", true), ("B", false)]
    );
    drop(a);
    let b = admitted(b, &m);
    assert_eq!(b.record().need.what, "B");
}

const STANDIN: &str = "ATERM_CAPACITY_TEST_STANDIN";
#[cfg(unix)]
const STANDIN_TEST: &str = "tests::standin_holds_a_claim_then_dies";

/// Run by [`a_dead_holder_releases_its_claim_on_the_next_poll`] in a process of its own:
/// holds a 30 GiB claim in the ledger under the directory [`STANDIN`] names, says so, and
/// once told to dies by `SIGKILL`, with the claim still held.
#[test]
fn standin_holds_a_claim_then_dies() {
    let Some(dir) = std::env::var_os(STANDIN).map(PathBuf::from) else {
        return;
    };
    let m = mac48();
    let claim = admit_with(
        &Ledger::at(dir.join("ledger")),
        &m,
        Need::new("stand-in A").mem_gib(30),
        Admit::Wait,
        Pace::default(),
        &mut |_| {},
    )
    .expect("the stand-in's claim");
    std::fs::write(dir.join("held"), std::process::id().to_string()).expect("said held");
    until_within(MUST_HAPPEN, "the test to tell the stand-in to die", || {
        dir.join("die").exists().then_some(())
    });
    let _kept = claim;
    aterm_uds::process::kill_self_now();
}

/// Start [`standin_holds_a_claim_then_dies`] on `s`'s ledger, a child of this process, and
/// wait until it holds its 30 GiB claim: the child and its pid.
#[cfg(unix)]
fn standin(s: &Scratch) -> (aterm_uds::spawnfd::Child, u32) {
    let child = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
        .args(["--exact", STANDIN_TEST, "--test-threads=1", "--nocapture"])
        .env(STANDIN, s.dir.path())
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the stand-in");
    let held = s.dir.path().join("held");
    let pid: u32 = until_within(MUST_HAPPEN, "the stand-in to hold its claim", || {
        std::fs::read_to_string(&held).ok()?.trim().parse().ok()
    });
    assert_eq!(pid, child.id());
    (child, pid)
}

/// A's holder is a stand-in that dies by `kill_self_now()`, the claim still held: nobody
/// signals it and nobody removes its file, yet B, which waited behind A, is admitted on the
/// very next poll. Negative control: while the stand-in lives, B's polls keep waiting.
#[cfg(unix)]
#[test]
fn a_dead_holder_releases_its_claim_on_the_next_poll() {
    use std::os::unix::process::ExitStatusExt as _;
    let s = scratch("dies");
    let m = mac48();
    let (mut child, pid) = standin(&s);
    let b = enter(&s, &m, Need::new("B").mem_gib(20));
    let (b, short) = waits(b, &m);
    assert_eq!(
        short.holders.iter().map(|h| h.pid).collect::<Vec<_>>(),
        [pid],
        "{short}"
    );
    let (b, _) = waits(b, &m);
    std::fs::write(s.dir.path().join("die"), "").expect("told to die");
    let status = child.wait().expect("the stand-in's end");
    assert_eq!(
        status.signal(),
        Some(9),
        "the stand-in died by SIGKILL: {status}"
    );
    let claim_file = s
        .ledger
        .dir()
        .read_dir()
        .expect("the ledger")
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy().ends_with(".claim"));
    assert!(
        claim_file,
        "the dead holder's file is still there before the poll"
    );
    let b = admitted(b, &m);
    assert_eq!(b.record().need.what, "B");
    assert_eq!(
        s.ledger.list().expect("listed").len(),
        1,
        "the poll removed the dead holder's file"
    );
}

/// A stream of 1 GiB claims does not overtake a 20 GiB claim waiting for memory (10 GiB is
/// free, but all of it is the waiter's), and each is admitted once B is. Then, with B waiting
/// for a quiet machine instead, the stream backfills: each 1 GiB claim that fits beside B's
/// 20 GiB reservation goes first, and the first that would not, waits.
#[test]
fn a_stream_of_small_claims_does_not_overtake_a_waiting_large_one() {
    let s = scratch("stream");
    let m = mac48();
    let a = admitted(enter(&s, &m, Need::new("A").mem_gib(30)), &m);
    let (b, _) = waits(enter(&s, &m, Need::new("B").mem_gib(20)), &m);
    let mut stream = Vec::new();
    for i in 0..5 {
        let (c, short) = waits(enter(&s, &m, Need::new(format!("C{i}")).mem_gib(1)), &m);
        assert_eq!(
            short.shorts,
            vec![Short::Memory {
                need: GIB,
                available: 40 * GIB,
                reserved: (50 + i) * GIB
            }],
            "C{i} waits behind A and B"
        );
        assert!(
            short.holders.iter().any(|h| !h.held && h.mem == 20 * GIB),
            "{short}"
        );
        stream.push(c);
    }
    drop(a);
    let b = admitted(b, &m);
    let held: Vec<_> = stream.into_iter().map(|c| admitted(c, &m)).collect();
    assert_eq!(held.len(), 5);
    drop((b, held));

    let s = scratch("backfill");
    let mut busy = Reading::of_gib(48, 40, 14);
    busy.load1 = Some(50.0);
    let m = Fixed::new(busy);
    let (b, short) = waits(enter(&s, &m, Need::new("B").mem_gib(20).quiet(6.0)), &m);
    assert_eq!(
        short.shorts,
        vec![Short::Load {
            load1: 50.0,
            max: 6.0
        }]
    );
    let mut admitted_c = Vec::new();
    let waiting = loop {
        match enter(&s, &m, Need::new("C").mem_gib(1)).poll(&m) {
            Polled::Admitted(c) => admitted_c.push(c),
            Polled::Waiting(c, short) => break (c, short),
        }
        assert!(admitted_c.len() <= 40, "the stream never stopped");
    };
    assert_eq!(
        admitted_c.len(),
        20,
        "40 GiB less B's 20 GiB holds 20 claims of 1 GiB"
    );
    assert_eq!(
        waiting.1.shorts,
        vec![Short::Memory {
            need: GIB,
            available: 40 * GIB,
            reserved: 40 * GIB
        }]
    );
    assert!(
        waiting.1.holders.iter().any(|h| !h.held && h.what == "B"),
        "the waiter ahead is named: {}",
        waiting.1
    );
    drop(b);
}

/// Negative control: a 60 GiB need on that machine is refused at once, before it enters the
/// ledger, and no wait line is said. A need that only has to wait says its line at the first
/// look and is admitted once its holder lets go.
#[test]
fn a_need_that_can_never_fit_is_refused_at_once_without_a_wait_line() {
    let s = scratch("never");
    let m = mac48();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let ledger = s.ledger.clone();
    let refusing = std::thread::spawn(move || {
        admit_with(
            &ledger,
            &mac48(),
            Need::new("big").mem_gib(60),
            Admit::Wait,
            Pace::default(),
            &mut |l| {
                let _ = tx.send(l.to_owned());
            },
        )
        .map(|c| c.record().need.what.clone())
    });
    until_within(MUST_HAPPEN, "the 60 GiB need's answer", || {
        refusing.is_finished().then_some(())
    });
    match refusing.join().expect("the refusal") {
        Err(Error::Never(why)) => {
            names(&why, &["big", "60 GiB", "48 GiB"]);
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    let said: Vec<String> = rx.try_iter().collect();
    assert!(said.is_empty(), "no wait line: {said:?}");
    assert!(
        s.ledger.list().expect("listed").is_empty(),
        "nothing entered"
    );

    let a = admitted(enter(&s, &m, Need::new("A").mem_gib(30)), &m);
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let ledger = s.ledger.clone();
    let waiting = std::thread::spawn(move || {
        let m = mac48();
        admit_with(
            &ledger,
            &m,
            Need::new("waiter-b").mem_gib(20),
            Admit::Wait,
            Pace {
                poll: Duration::from_millis(10),
                line_every: Duration::from_secs(60),
            },
            &mut |l| {
                let _ = tx.send(l.to_owned());
            },
        )
        .map(|c| c.record().need.what.clone())
    });
    let first = rx.recv_timeout(MUST_HAPPEN).expect("B's first wait line");
    names(&first, &["waiter-b", "10 GiB", "20 GiB"]);
    drop(a);
    let admitted_b = waiting.join().expect("B's admission").expect("admitted");
    assert_eq!(admitted_b, "waiter-b");
    let last = rx.recv_timeout(MUST_HAPPEN).expect("B's admitted line");
    names(&last, &["waiter-b"]);
    assert!(rx.try_recv().is_err(), "two lines in all");
}

/// `--admit now` takes the claim at once on a machine that has no room, and says what it would
/// have waited for; under `wait` the same need waits.
#[test]
fn admit_now_takes_the_claim_and_says_what_it_would_wait_for() {
    let s = scratch("now");
    let m = mac48();
    let _a = admitted(enter(&s, &m, Need::new("A").mem_gib(30)), &m);
    let mut said = Vec::new();
    let claim = admit_with(
        &s.ledger,
        &m,
        Need::new("timed").mem_gib(20).cores(4),
        Admit::Now,
        Pace::default(),
        &mut |l| said.push(l.to_owned()),
    )
    .expect("admitted now");
    assert!(claim.record().held.is_some());
    assert_eq!(claim.cores(), 4, "the cores it asked for");
    assert_eq!(said.len(), 1, "{said:?}");
    names(&said[0], &["timed", "10 GiB", "20 GiB"]);
    assert_eq!(Admit::parse("now"), Some(Admit::Now));
    assert_eq!(Admit::parse("wait"), Some(Admit::Wait));
    assert_eq!(Admit::parse("later"), None);
}

fn record(pid: u32, start_ms: u64, need: Need, held: Option<u32>) -> Record {
    Record {
        pid,
        start_ms,
        seq: 0,
        checkout: "/ck".to_owned(),
        need,
        held: held.map(|cores| crate::Held {
            admitted_ms: start_ms,
            cores,
        }),
    }
}

/// A claim's reservation is its peak less what its process tree already uses: memory in use
/// is not counted twice. Negative control: with no sweep, the whole peak is reserved.
#[test]
fn a_holders_own_use_is_not_reserved_twice() {
    let m = Reading::of_gib(48, 40, 14);
    let a = record(10, 0, Need::new("A").mem_gib(30), Some(1));
    let b = Need::new("B").mem_gib(20);
    let using = |footprint| {
        vec![Other {
            record: &a,
            used: TreeUse {
                footprint,
                written: 0,
            },
        }]
    };
    assert!(decide(&b, (1, 1, 0), &m, &using(0), 1).is_err());
    let short = decide(&b, (1, 1, 0), &m, &using(5 * GIB), 1).expect_err("25 GiB still reserved");
    assert_eq!(
        short.shorts,
        vec![Short::Memory {
            need: 20 * GIB,
            available: 40 * GIB,
            reserved: 25 * GIB
        }]
    );
    assert!(decide(&b, (1, 1, 0), &m, &using(10 * GIB), 1).is_ok());
}

/// One process holding two claims is credited with its tree's use once across them, not once
/// for each: A (30 GiB) and B (30 GiB) are held by this process, whose tree uses 20 GiB, so
/// they still reserve 40 GiB, and with 40 GiB available C (20 GiB) waits. Positive control:
/// with 60 GiB available the 20 GiB credit leaves C its room, and C is admitted.
#[test]
fn one_holders_tree_is_credited_once_across_its_claims() {
    let s = scratch("one-holder");
    let m = Fixed::new(Reading::of_gib(64, 64, 14));
    let a = admitted(enter(&s, &m, Need::new("A").mem_gib(30)), &m);
    let b = admitted(enter(&s, &m, Need::new("B").mem_gib(30)), &m);
    m.set(Reading::of_gib(64, 40, 14));
    m.set_procs(Some(Procs::of(vec![Proc {
        pid: std::process::id(),
        ppid: 1,
        footprint: 20 * GIB,
        written: 0,
    }])));
    let (c, short) = waits(enter(&s, &m, Need::new("C").mem_gib(20)), &m);
    assert_eq!(
        short.shorts,
        vec![Short::Memory {
            need: 20 * GIB,
            available: 40 * GIB,
            reserved: 40 * GIB
        }],
        "60 GiB of peaks less the tree's 20 GiB, credited once"
    );
    assert_eq!(
        short.holders.iter().map(|h| h.mem).collect::<Vec<_>>(),
        [10 * GIB, 30 * GIB],
        "the older claim takes the credit first"
    );
    m.set(Reading::of_gib(64, 60, 14));
    let c = admitted(c, &m);
    drop((a, b, c));
}

/// A claimant's tree stops at another claimant's. This test process holds P (30 GiB) and its
/// child, the stand-in, holds Q (30 GiB); the child's 20 GiB is credited to Q alone, so P
/// still reserves its 30 GiB less its own 2 GiB, and with 40 GiB available C (20 GiB) waits.
/// Were Q's use credited to P as well, P would reserve 8 GiB and C would be admitted on an
/// over-committed machine.
#[cfg(unix)]
#[test]
fn a_child_claimants_use_is_credited_to_it_alone() {
    let p = |pid, ppid, gib: u64| Proc {
        pid,
        ppid,
        footprint: gib * GIB,
        written: 0,
    };
    let procs = Procs::of(vec![
        p(100, 1, 2),
        p(101, 100, 18),
        p(102, 101, 1),
        p(103, 100, 3),
    ]);
    let trees = procs.trees(&[100, 101]);
    assert_eq!(trees[&100].footprint, 5 * GIB, "P and its own child");
    assert_eq!(trees[&101].footprint, 19 * GIB, "Q and its child");
    assert_eq!(
        procs.tree(100).footprint,
        24 * GIB,
        "with Q no claimant, P's tree holds all of it"
    );

    let s = scratch("child");
    let (mut child, q) = standin(&s);
    let m = Fixed::new(Reading::of_gib(64, 64, 14));
    let me = std::process::id();
    let held_p = admitted(enter(&s, &m, Need::new("P").mem_gib(30)), &m);
    m.set(Reading::of_gib(64, 40, 14));
    m.set_procs(Some(Procs::of(vec![p(me, 1, 2), p(q, me, 20)])));
    let (c, short) = waits(enter(&s, &m, Need::new("C").mem_gib(20)), &m);
    assert_eq!(
        short.shorts,
        vec![Short::Memory {
            need: 20 * GIB,
            available: 40 * GIB,
            reserved: 38 * GIB
        }],
        "P's 28 GiB and Q's 10 GiB"
    );
    std::fs::write(s.dir.path().join("die"), "").expect("told to die");
    let _ = child.wait().expect("the stand-in's end");
    drop((c, held_p));
}

/// A tree's use is its root's and every descendant's, never a sibling's or a parent's.
#[test]
fn a_tree_sums_its_descendants() {
    let p = |pid, ppid, mib: u64| Proc {
        pid,
        ppid,
        footprint: mib << 20,
        written: mib,
    };
    let procs = Procs::of(vec![
        p(1, 0, 1000),
        p(10, 1, 1),
        p(11, 10, 2),
        p(12, 11, 4),
        p(13, 10, 8),
        p(20, 1, 16),
        p(21, 21, 32),
    ]);
    assert_eq!(
        procs.tree(10),
        TreeUse {
            footprint: 15 << 20,
            written: 15
        }
    );
    assert_eq!(procs.tree(20).written, 16);
    assert_eq!(
        procs.tree(21).written,
        32,
        "a self-parented pid ends its own tree"
    );
    assert_eq!(procs.tree(99), TreeUse::default());
}

/// A disk need is held to the volume's free space above its floor, less what claims ahead on
/// that volume have yet to write; a claim on another volume reserves nothing here.
#[test]
fn disk_is_held_above_the_floor() {
    let mut r = Reading::of_gib(48, 40, 14);
    r.volume = Some(Volume {
        mount: PathBuf::from("/vol"),
        total: 1000 * GIB,
        free: 100 * GIB,
        floor: 32 * GIB,
        room: 16 * GIB,
    });
    let need = Need::new("verify").disk(27 * GIB, "/vol");
    let here = record(10, 0, Need::new("trust").disk(50 * GIB, "/vol"), Some(0));
    let there = record(11, 0, Need::new("lab").disk(500 * GIB, "/other"), Some(0));
    let other = |rec, written| Other {
        record: rec,
        used: TreeUse {
            footprint: 0,
            written,
        },
    };
    assert!(decide(&need, (1, 1, 0), &r, &[other(&there, 0)], 1).is_ok());
    let short = decide(&need, (1, 1, 0), &r, &[other(&here, 0)], 1).expect_err("68 less 50");
    assert_eq!(
        short.shorts,
        vec![Short::Disk {
            need: 27 * GIB,
            mount: PathBuf::from("/vol"),
            above_floor: 68 * GIB,
            floor: 32 * GIB,
            reserved: 50 * GIB
        }]
    );
    names(
        &short.to_string(),
        &["9 GiB", "27 GiB", "/vol", "68 GiB", "50 GiB"],
    );
    assert!(decide(&need, (1, 1, 0), &r, &[other(&here, 41 * GIB)], 1).is_ok());
    let never = crate::never(&Need::new("huge").disk(990 * GIB, "/vol"), &r).expect("refused");
    names(&never, &["huge", "990 GiB", "/vol", "968 GiB", "32 GiB"]);
}

/// A quiet claim waits for the load to fall to its bound and for any other quiet claim to end.
#[test]
fn a_quiet_claim_is_alone_on_a_quiet_machine() {
    let mut r = Reading::of_gib(48, 40, 14);
    r.load1 = Some(3.0);
    let need = Need::new("measure").quiet(6.0);
    assert!(decide(&need, (1, 1, 0), &r, &[], 1).is_ok());
    let other = record(10, 0, Need::new("perf").quiet(6.0), Some(0));
    let o = [Other {
        record: &other,
        used: TreeUse::default(),
    }];
    let short = decide(&need, (1, 1, 0), &r, &o, 1).expect_err("one quiet claim at a time");
    assert_eq!(
        short.shorts,
        vec![Short::Quiet {
            pid: 10,
            what: "perf".to_owned(),
            held: true
        }]
    );
    r.load1 = Some(9.5);
    let short = decide(&need, (1, 1, 0), &r, &[], 1).expect_err("too busy");
    assert_eq!(
        short.shorts,
        vec![Short::Load {
            load1: 9.5,
            max: 6.0
        }]
    );
    assert!(short.holders.is_empty(), "no claim holds it");
    names(&short.to_string(), &["9.5", "6.0"]);
}

/// Cores are granted, never waited for: as many idle as were asked for, less what claims
/// admitted within the load average's reach were granted, and at least one.
#[test]
fn cores_are_granted_from_the_idle_ones() {
    let mut r = Reading::of_gib(48, 40, 14);
    r.load5 = Some(4.5);
    let need = Need::new("guest").cores(8);
    assert_eq!(decide(&need, (1, 1, 0), &r, &[], 1), Ok(8));
    let recent = record(10, 1_000, Need::new("vm").cores(6), Some(6));
    let o = [Other {
        record: &recent,
        used: TreeUse::default(),
    }];
    assert_eq!(
        decide(&need, (1, 1, 0), &r, &o, 2_000),
        Ok(3),
        "9 idle less 6"
    );
    assert_eq!(
        decide(&need, (1, 1, 0), &r, &o, 1_000 + 300_001),
        Ok(8),
        "in the load now"
    );
    r.load5 = Some(60.0);
    assert_eq!(decide(&need, (1, 1, 0), &r, &[], 1), Ok(1), "at least one");
    assert_eq!(decide(&Need::new("none"), (1, 1, 0), &r, &[], 1), Ok(0));
}

/// A record reads back as it was written, and its admission line with it.
#[test]
fn a_record_reads_back() {
    let s = scratch("record");
    let m = Fixed::new(Reading::of_gib(48, 40, 14));
    let w = enter(
        &s,
        &m,
        Need::new("trust check \"aterm-render\"")
            .mem(15 * GIB + 7)
            .cores(2)
            .quiet(4.0),
    );
    let laid = w.record().clone();
    let c = admitted(w, &m);
    let listed = s.ledger.list().expect("listed");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].need, laid.need);
    assert_eq!(listed[0].pid, std::process::id());
    assert_eq!(listed[0].held.map(|h| h.cores), Some(2));
    assert_eq!(listed[0], c.record().clone());
    let path = c.path().to_path_buf();
    drop(c);
    assert!(
        !Path::new(&path).exists(),
        "a dropped claim leaves the ledger"
    );
    assert!(Record::decode("{\"v\":2}", false).is_none());
}

/// The volume's floor is the window's: 5% of the volume up to 32 GiB, or 10 GiB held to 10% of a
/// small volume, raised to the room's least.
#[test]
fn the_floor_follows_the_volume() {
    assert_eq!(floor::default_floor(1858 * GIB), 32 * GIB);
    assert_eq!(floor::default_floor(256 * GIB), 13 * GIB);
    assert_eq!(floor::default_floor(64 * GIB), 6 * GIB);
    let pinned = floor::Knobs {
        auto_free_gib: 20,
        written: floor::Written {
            auto_free_gib: true,
            warn_free_gib: false,
        },
        ..floor::Knobs::default()
    };
    assert_eq!(
        floor::lines(pinned, Some(1858 * GIB), Some(70 * GIB)).floor,
        20 * GIB,
        "a written floor is read as written"
    );
    assert_eq!(
        floor::lines(floor::Knobs::default(), Some(1858 * GIB), Some(70 * GIB)).floor,
        70 * GIB,
        "an unwritten one rises to the room"
    );
}

/// The reading's floor is the one the window's disk watch holds: on this Mac's 1858 GiB volume
/// with a measured room of 75529 MiB the floor is 74 GiB, not the volume's 32 GiB share; a
/// volume the watch has no room for is held to the share. A floor the owner wrote is read as
/// written, a room the owner pinned is the room, and a floor turned off leaves admission the
/// emergency line.
#[test]
fn the_floor_is_the_watchs_own() {
    const MIB: u64 = 1 << 20;
    const DAY: i64 = 86_400;
    let today = 20_750;
    let now = today * DAY + 12 * 3600;
    let total = 1858 * GIB;
    let rooms = crate::room::Rooms::decode(&format!(
        "{{\"schema\":1,\"volumes\":{{\"container:disk3\":[[{},{}],[{},{}]]}}}}",
        today - 1,
        90_000 * MIB,
        today,
        75_529 * MIB
    ));
    let watch = floor::Watch {
        knobs: floor::Knobs::default(),
        rooms,
    };
    assert_eq!(watch.room("container:disk3", total, now), 75_529 * MIB);
    assert_eq!(watch.lines("container:disk3", total, now).floor, 74 * GIB);
    assert_eq!(
        watch.lines("container:disk9", total, now).floor,
        32 * GIB,
        "no room measured: the volume's share"
    );
    assert_eq!(
        floor::Watch::default()
            .lines("container:disk3", total, now)
            .floor,
        floor::default_floor(total)
    );

    let written = |text: &str| floor::Watch {
        knobs: floor::Knobs::from_toml(text),
        rooms: watch.rooms.clone(),
    };
    assert_eq!(
        written("[disk]\nauto_free_gib = 20\n")
            .lines("container:disk3", total, now)
            .floor,
        20 * GIB,
        "a written floor is read as written"
    );
    let pinned = written("[disk]\nroom_gib = 100\n");
    assert_eq!(pinned.room("container:disk3", total, now), 100 * GIB);
    assert_eq!(pinned.lines("container:disk3", total, now).floor, 100 * GIB);
    let off = written("[disk]\nauto_free_gib = 0\n").lines("container:disk3", total, now);
    assert_eq!(off.floor, 0);
    assert_eq!(floor::admission_floor(&off), 8 * GIB, "the emergency line");
    assert_eq!(
        written("[disk]\nauto_free_gib = \"lots\"\n[other]\nauto_free_gib = 99\n").knobs,
        floor::Knobs::default(),
        "a knob that is no whole number, or below another table, is unwritten"
    );
    assert_eq!(written("not toml [").knobs, floor::Knobs::default());
}

/// A local volume is placed with the platform's error boxes off, and the caller's mode is
/// restored after.
#[test]
fn a_volume_is_placed_with_the_error_boxes_off() {
    let s = scratch("volume-quiet");
    let table = aterm_sysprobe::volumes::table();
    let unguarded = !cfg!(windows);
    crate::reading::VOLUME_ERRORS_QUIET.set(None);
    assert_eq!(aterm_sysprobe::errors_quiet(), unguarded, "no guard before");
    assert!(
        crate::reading::volume_of(s.dir.path(), &table).is_ok(),
        "a local volume"
    );
    assert_eq!(
        crate::reading::VOLUME_ERRORS_QUIET.get(),
        Some(true),
        "inside the resolve"
    );
    assert_eq!(
        aterm_sysprobe::errors_quiet(),
        unguarded,
        "the caller's mode restored"
    );
}

/// A waiter kept from the ledger's lock (its holder stopped, say, while it decides) is told
/// which pid holds it, and says so once that holder has kept it a whole minute's pace; it is
/// admitted once the lock is let go.
#[test]
fn a_held_ledger_lock_names_its_holder() {
    let s = scratch("turn");
    let m = mac48();
    let turn = s.ledger.try_turn().expect("the lock").expect("free");
    let me = std::process::id();
    let (b, short) = waits(enter(&s, &m, Need::new("B").mem_gib(1)), &m);
    assert_eq!(short.shorts, vec![Short::Turn { holder: Some(me) }]);
    drop(b);
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let ledger = s.ledger.clone();
    let waiting = std::thread::spawn(move || {
        admit_with(
            &ledger,
            &mac48(),
            Need::new("waiter-c").mem_gib(1),
            Admit::Wait,
            Pace {
                poll: Duration::from_millis(5),
                line_every: Duration::from_millis(50),
            },
            &mut |l| {
                let _ = tx.send(l.to_owned());
            },
        )
        .map(|c| c.record().need.what.clone())
    });
    let line = rx.recv_timeout(MUST_HAPPEN).expect("C's lock line");
    names(&line, &["waiter-c", &me.to_string()]);
    assert!(!waiting.is_finished(), "C waits while the lock is held");
    drop(turn);
    let admitted_c = waiting.join().expect("C's admission").expect("admitted");
    assert_eq!(admitted_c, "waiter-c");
}
