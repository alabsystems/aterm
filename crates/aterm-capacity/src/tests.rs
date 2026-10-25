// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ledger's admissions on an injected machine ([`Fixed`]): no test reads the live one.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use aterm_spec::wait::{MUST_HAPPEN, until_within};

use crate::admit::{Other, counted, decide};
use crate::floor::{self, GIB};
use crate::reading::{Proc, Procs, TreeUse};
use crate::{
    Admit, Error, Fixed, Ledger, Need, Pace, Polled, Reading, Record, Short, Volume, Waiter,
    admit_until, admit_with, never,
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
            margin: 0,
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
/// Set, the stand-in's claim is this many GiB of disk on [`vol`]`(200, 100, 10)` instead.
const STANDIN_DISK: &str = "ATERM_CAPACITY_TEST_STANDIN_DISK";
/// Set, the stand-in's claim is a quiet one of this bound instead, admitted on a machine with no
/// load.
const STANDIN_QUIET: &str = "ATERM_CAPACITY_TEST_STANDIN_QUIET";
#[cfg(unix)]
const STANDIN_TEST: &str = "tests::standin_holds_a_claim_then_dies";

/// Run by [`a_dead_holder_releases_its_claim_on_the_next_poll`] in a process of its own:
/// holds a 30 GiB claim (of disk where [`STANDIN_DISK`] says, the quiet machine where
/// [`STANDIN_QUIET`] does) in the ledger under the directory [`STANDIN`] names, says so, and once told to dies by `SIGKILL`, with the claim
/// still held.
#[test]
fn standin_holds_a_claim_then_dies() {
    let Some(dir) = std::env::var_os(STANDIN).map(PathBuf::from) else {
        return;
    };
    let disk: Option<u64> = std::env::var(STANDIN_DISK)
        .ok()
        .and_then(|g| g.parse().ok());
    let quiet: Option<f64> = std::env::var(STANDIN_QUIET)
        .ok()
        .and_then(|b| b.parse().ok());
    let (m, need) = match (disk, quiet) {
        (Some(gib), _) => (
            on(vol(200, 100, 10)),
            Need::new("stand-in A").disk(gib * GIB, "/vol"),
        ),
        (None, Some(bound)) => (mac48(), Need::new("stand-in A").quiet(bound)),
        (None, None) => (mac48(), Need::new("stand-in A").mem_gib(30)),
    };
    let claim = admit_with(
        &Ledger::at(dir.join("ledger")),
        &m,
        need,
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
    standin_needing(s, None)
}

/// [`standin`], its claim `disk` GiB of disk on [`vol`]`(200, 100, 10)` where given.
#[cfg(unix)]
fn standin_needing(s: &Scratch, disk: Option<u64>) -> (aterm_uds::spawnfd::Child, u32) {
    standin_with(s, disk.map(|gib| (STANDIN_DISK, gib.to_string())))
}

/// [`standin`], its claim the quiet machine below a load of `bound`.
#[cfg(unix)]
fn standin_quiet(s: &Scratch, bound: f64) -> (aterm_uds::spawnfd::Child, u32) {
    standin_with(s, Some((STANDIN_QUIET, bound.to_string())))
}

/// [`standin`], with `env` set to say what its claim is.
#[cfg(unix)]
fn standin_with(s: &Scratch, env: Option<(&str, String)>) -> (aterm_uds::spawnfd::Child, u32) {
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
    if let Some((key, value)) = env {
        cmd.env(key, value);
    }
    let child = cmd
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

/// Set, the waiting stand-in's ledger is under this directory.
#[cfg(unix)]
const WAITER: &str = "ATERM_CAPACITY_TEST_WAITER";
#[cfg(unix)]
const WAITER_TEST: &str = "tests::standin_waits_for_room";

/// Run by [`a_waiter_marks_its_wait_for_a_ceiling_until_admitted`] in a process of its own, its
/// [`aterm_toolchain::waits::VAR`] set: waits for a 20 GiB claim in the ledger under the
/// directory [`WAITER`] names, then exits once admitted.
#[cfg(unix)]
#[test]
fn standin_waits_for_room() {
    let Some(dir) = std::env::var_os(WAITER).map(PathBuf::from) else {
        return;
    };
    let pace = Pace {
        poll: Duration::from_millis(10),
        line_every: Duration::from_secs(60),
    };
    let claim = admit_with(
        &Ledger::at(dir.join("ledger")),
        &mac48(),
        Need::new("B").mem_gib(20),
        Admit::Wait,
        pace,
        &mut |_| {},
    )
    .expect("the stand-in's claim");
    drop(claim);
}

/// A waiter in a tree whose ceiling names a marks' directory marks its wait there, saying what
/// it is short of and who holds it, and its mark goes once it is admitted: a ceiling reads that
/// it waits, not works. Negative control: before A lets go, the mark is there.
#[cfg(unix)]
#[test]
fn a_waiter_marks_its_wait_for_a_ceiling_until_admitted() {
    let s = scratch("marks");
    let m = mac48();
    let marks = s.dir.path().join("waits");
    std::fs::create_dir(&marks).expect("the marks' dir");
    let a = admitted(enter(&s, &m, Need::new("A").mem_gib(30)), &m);
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
        .args(["--exact", WAITER_TEST, "--test-threads=1", "--nocapture"])
        .env(WAITER, s.dir.path())
        .env(aterm_toolchain::waits::VAR, &marks)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the waiting stand-in");
    let mark = until_within(MUST_HAPPEN, "the stand-in to mark its wait", || {
        aterm_toolchain::waits::waiting(&marks)
            .pop()
            .filter(|w| w.text.contains("short"))
    });
    assert_eq!(mark.pid, child.id());
    names(
        &mark.text,
        &[
            "capacity: B waits",
            "memory short by 10 GiB",
            &format!("pid {} holds 30 GiB", std::process::id()),
        ],
    );
    drop(a);
    let status = child.wait().expect("the stand-in's end");
    assert!(status.success(), "the stand-in was admitted: {status}");
    assert!(
        aterm_toolchain::waits::waiting(&marks).is_empty(),
        "the mark outlived the wait"
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
                margin: 0,
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
            margin: 0,
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

/// A need of all the memory can never fit: what is available never holds the kernel's wired
/// pages. One byte less can, once the machine is that free.
#[test]
fn a_need_of_all_the_memory_can_never_fit() {
    let r = Reading::of_gib(48, 40, 14);
    let all = crate::never(&Need::new("all").mem_gib(48), &r).expect("refused");
    names(&all, &["all", "needs 48 GiB", "48 GiB of memory"]);
    assert_eq!(crate::never(&Need::new("less").mem(48 * GIB - 1), &r), None);
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
        credit: Vec::new(),
        wrote: None,
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
            credited: TreeUse::default(),
        }]
    };
    assert!(decide(&b, (1, 1, 0), &m, &using(0), 1).is_err());
    let short = decide(&b, (1, 1, 0), &m, &using(5 * GIB), 1).expect_err("25 GiB still reserved");
    assert_eq!(
        short.shorts,
        vec![Short::Memory {
            need: 20 * GIB,
            margin: 0,
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
            margin: 0,
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
            margin: 0,
            available: 40 * GIB,
            reserved: 38 * GIB
        }],
        "P's 28 GiB and Q's 10 GiB"
    );
    std::fs::write(s.dir.path().join("die"), "").expect("told to die");
    let _ = child.wait().expect("the stand-in's end");
    drop((c, held_p));
}

/// A claim made inside held work is held to the other held claims only: verify (pid 10) holds
/// its run's disk, and its child, the lab (20), claims its VM's. The lab counts neither its
/// ancestors' claims nor the waiter (12) that came after verify, which waits for verify's work
/// in turn. Negative controls: a lab no claimant started counts all three; with no sweep no
/// ancestor is known; an ancestor that only waits makes its child no part of held work, so the
/// child keeps its place behind the other waiter.
#[test]
fn a_claim_inside_held_work_is_held_to_the_other_held_claims_only() {
    let verify = record(10, 0, Need::new("verify").disk(27 * GIB, "/vol"), Some(0));
    let trust = record(
        11,
        1,
        Need::new("ship trust run").disk(6 * GIB, "/vol"),
        Some(0),
    );
    let boot = record(
        12,
        2,
        Need::new("lab windows boot").disk(9 * GIB, "/vol"),
        None,
    );
    let all = || vec![&verify, &trust, &boot];
    let pids = |rs: Vec<&Record>| rs.iter().map(|r| r.pid).collect::<Vec<_>>();
    let p = |pid, ppid| Proc {
        pid,
        ppid,
        footprint: 0,
        written: 0,
    };
    let started_by_verify = Procs::of(vec![p(1, 0), p(10, 1), p(9, 10), p(20, 9), p(11, 1)]);
    assert_eq!(started_by_verify.ancestors(20), [9, 10, 1, 0]);
    assert_eq!(
        pids(counted(all(), 20, Some(&started_by_verify))),
        [11],
        "the trust run alone"
    );
    let unrelated = Procs::of(vec![p(1, 0), p(10, 1), p(20, 1)]);
    assert_eq!(pids(counted(all(), 20, Some(&unrelated))), [10, 11, 12]);
    assert_eq!(pids(counted(all(), 20, None)), [10, 11, 12]);
    let waiting = record(10, 0, Need::new("verify").disk(27 * GIB, "/vol"), None);
    assert_eq!(
        pids(counted(
            vec![&waiting, &trust, &boot],
            20,
            Some(&started_by_verify)
        )),
        [11, 12]
    );
}

/// The same on the ledger: the stand-in holds 60 GiB of the 90 above /vol's floor, and this
/// process, its child by the sweep, asks for 40. Admitted, though its ancestor's reservation
/// leaves it 30: held to that claim it would wait for its parent, which waits for it. Negative
/// control: the same holder, no ancestor of this process, makes it wait, short of disk and
/// naming the holder.
#[cfg(unix)]
#[test]
fn a_holders_child_is_admitted_beside_its_ancestors_claim() {
    let s = scratch("nested");
    let (mut child, holder) = standin_needing(&s, Some(60));
    let m = on(vol(200, 100, 10));
    let me = std::process::id();
    let p = |pid, ppid| Proc {
        pid,
        ppid,
        footprint: 0,
        written: 0,
    };
    m.set_procs(Some(Procs::of(vec![p(holder, 1), p(me, 1)])));
    let need = Need::new("lab linux boot").disk(40 * GIB, "/vol");
    let (w, short) = waits(enter(&s, &m, need), &m);
    assert_eq!(
        short.shorts,
        vec![Short::Disk {
            need: 40 * GIB,
            mount: PathBuf::from("/vol"),
            above_floor: 90 * GIB,
            floor: 10 * GIB,
            reserved: 60 * GIB
        }]
    );
    assert_eq!(
        short.holders.iter().map(|h| h.pid).collect::<Vec<_>>(),
        [holder],
        "{short}"
    );
    m.set_procs(Some(Procs::of(vec![p(holder, 1), p(me, holder)])));
    let w = admitted(w, &m);
    assert_eq!(w.record().need.what, "lab linux boot");
    std::fs::write(s.dir.path().join("die"), "").expect("told to die");
    let _ = child.wait().expect("the stand-in's end");
    drop(w);
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
        credited: TreeUse::default(),
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
        credited: TreeUse::default(),
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

/// A quiet claim held by a waiter's own process or one of its ancestors covers the waiter's quiet
/// need: verify's measuring child never waits behind verify. Negative controls: a holder that is
/// no ancestor, a quiet claim an ancestor only waits for, and an ancestor's claim that is not
/// quiet cover nothing.
#[test]
fn a_quiet_claim_covers_its_holders_descendants() {
    let p = |pid, ppid| Proc {
        pid,
        ppid,
        ..Proc::default()
    };
    let chain = Procs::of(vec![p(1, 1), p(10, 1), p(20, 10), p(30, 20), p(40, 1)]);
    assert_eq!(chain.ancestors(30), [20, 10, 1]);
    let verify = record(10, 0, Need::timing("verify measure"), Some(0));
    let held = [&verify];
    assert!(
        crate::admit::covered(30, &held, Some(&chain)),
        "a grandchild"
    );
    assert!(crate::admit::covered(10, &held, None), "the holder itself");
    assert!(
        !crate::admit::covered(40, &held, Some(&chain)),
        "no ancestor"
    );
    assert!(
        !crate::admit::covered(30, &held, None),
        "no sweep, no ancestry"
    );
    let waiting = record(10, 0, Need::timing("verify measure"), None);
    assert!(!crate::admit::covered(30, &[&waiting], Some(&chain)));
    let build = record(10, 0, Need::new("trust").mem_gib(8), Some(0));
    assert!(!crate::admit::covered(30, &[&build], Some(&chain)));
}

/// Through the ledger: a second quiet claim of a process that holds one is admitted on a busy
/// machine, as a drive's second window is, while a process holding none waits for the load.
#[test]
fn a_quiet_holders_second_quiet_claim_is_admitted_on_a_busy_machine() {
    let s = scratch("quiet-own");
    let m = mac48();
    let first = admitted(enter(&s, &m, Need::new("first window").quiet(14.0)), &m);
    let mut busy = Reading::of_gib(48, 40, 14);
    busy.load1 = Some(40.0);
    m.set(busy);
    let second = admitted(enter(&s, &m, Need::new("second window").quiet(14.0)), &m);
    drop((first, second));
    let (_alone, short) = waits(enter(&s, &m, Need::new("alone").quiet(14.0)), &m);
    assert_eq!(
        short.shorts,
        vec![Short::Load {
            load1: 40.0,
            max: 14.0
        }]
    );
    names(
        &short.to_string(),
        &["waiting for a quiet machine: load 40.0 > 14.0"],
    );
}

/// The same through the ledger with an ancestor: the stand-in holds a quiet claim, and this
/// process, its child by the sweep, asks for one on a busy machine (load 40 over a bound of 14),
/// as verify's measuring child does under verify's run claim. Admitted: the ancestor's held claim
/// covers it, read before `counted` leaves that claim out (read after, the child would still wait
/// for the load). Negative control: the same holder, no ancestor of this process, makes it wait
/// for both the quiet machine and the load, naming the holder.
#[cfg(unix)]
#[test]
fn a_quiet_holders_child_is_admitted_on_a_busy_machine() {
    let s = scratch("quiet-child");
    let (mut child, holder) = standin_quiet(&s, 14.0);
    let m = mac48();
    let mut busy = Reading::of_gib(48, 40, 14);
    busy.load1 = Some(40.0);
    m.set(busy);
    let me = std::process::id();
    let p = |pid, ppid| Proc {
        pid,
        ppid,
        ..Proc::default()
    };
    m.set_procs(Some(Procs::of(vec![p(holder, 1), p(me, 1)])));
    let (w, short) = waits(enter(&s, &m, Need::new("paint matrix").quiet(14.0)), &m);
    assert_eq!(
        short.shorts,
        vec![
            Short::Quiet {
                pid: holder,
                what: "stand-in A".to_owned(),
                held: true
            },
            Short::Load {
                load1: 40.0,
                max: 14.0
            }
        ]
    );
    names(
        &short.to_string(),
        &[&holder.to_string(), "load 40.0 > 14.0"],
    );
    m.set_procs(Some(Procs::of(vec![p(holder, 1), p(me, holder)])));
    let w = admitted(w, &m);
    assert_eq!(w.record().need.what, "paint matrix");
    std::fs::write(s.dir.path().join("die"), "").expect("told to die");
    let _ = child.wait().expect("the stand-in's end");
    drop(w);
}

/// The quiet bound scales with the cores; a timing need carries it.
#[test]
fn the_quiet_bound_is_per_core() {
    assert!((crate::quiet_bound(14) - 14.0 * crate::QUIET_LOAD_PER_CORE).abs() < 1e-9);
    assert!((crate::quiet_bound(0) - crate::QUIET_LOAD_PER_CORE).abs() < 1e-9);
    let need = Need::timing("measure");
    assert_eq!(need.quiet, Some(crate::quiet_bound(crate::cores())));
    assert_eq!((need.mem, need.cores, need.disk), (0, 0, 0));
}

/// Two readings in one line: a figure that held is said once, one that moved as from→to, and a
/// figure the OS does not report is left out.
#[test]
fn two_readings_read_as_one_line() {
    let mut a = Reading::of_gib(48, 30, 14);
    a.load1 = Some(3.24);
    let mut b = a.clone();
    b.load1 = Some(5.06);
    b.available = Some(29 * GIB + GIB / 5);
    let line = crate::between(&a, &b);
    names(
        &line,
        &["3.2→5.1", "14 cores", "30.0→29.2", "48 GiB", "swap 0.0 GiB"],
    );
    let held = a.brief();
    names(&held, &["3.2", "30.0", "48 GiB"]);
    assert!(!held.contains('→'), "{held}");
    let bare = Reading {
        cores: 4,
        ..Reading::default()
    }
    .brief();
    assert!(!bare.contains("GiB"), "{bare}");
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
        credited: TreeUse::default(),
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

/// A margin is asked of the room and never reserved. On a 48 GiB machine with 30 GiB available
/// and lima's 16 GiB held, an 8 GiB need with 8 GiB spare beside it waits, short by 2 GiB, naming
/// lima's holder; the same need with no margin is admitted (the negative control). Admitted once
/// 40 GiB is available, it reserves its 8 GiB alone, so a second such need fits beside both. A
/// need whose peak and margin reach the Mac's memory is refused at once, and one just below it
/// is not.
#[test]
fn a_margin_is_asked_of_the_room_and_never_reserved() {
    let s = scratch("margin");
    let m = Fixed::new(Reading::of_gib(48, 30, 14));
    let windows = || Need::new("windows").mem_gib(8).margin(8 * GIB);
    let lima = admitted(enter(&s, &m, Need::new("lima").mem_gib(16)), &m);
    let (w, short) = waits(enter(&s, &m, windows()), &m);
    assert_eq!(
        short.shorts,
        vec![Short::Memory {
            need: 8 * GIB,
            margin: 8 * GIB,
            available: 30 * GIB,
            reserved: 16 * GIB
        }]
    );
    assert_eq!(
        short
            .holders
            .iter()
            .map(|h| h.what.as_str())
            .collect::<Vec<_>>(),
        ["lima"]
    );
    names(
        &short.to_string(),
        &[
            "short by 2 GiB",
            "8 GiB and 8 GiB spare beside it",
            "\"lima\"",
        ],
    );
    drop(w);
    drop(admitted(
        enter(&s, &m, Need::new("no margin").mem_gib(8)),
        &m,
    ));
    m.set(Reading::of_gib(48, 40, 14));
    let first = admitted(enter(&s, &m, windows()), &m);
    let second = admitted(enter(&s, &m, windows()), &m);
    let mac = Reading::of_gib(48, 48, 14);
    let refused = never(&Need::new("big").mem_gib(44).margin(8 * GIB), &mac).expect("never");
    names(
        &refused,
        &["big", "44 GiB and 8 GiB spare beside it", "48 GiB"],
    );
    assert!(never(&Need::new("all").mem_gib(40).margin(8 * GIB), &mac).is_some());
    assert_eq!(
        never(&Need::new("fits").mem_gib(39).margin(8 * GIB), &mac),
        None
    );
    drop((lima, first, second));
}

/// A VM's processes run outside the tree of the run that booted it (the system parents them to
/// launchd), so its claim is credited with them ([`crate::Claim::credit`]) and reserves only the
/// part of its peak they do not hold yet. Lima's 16 GiB boot claim is held and its VM process
/// (ppid 1) holds 14 GiB of a 48 GiB Mac with 30 GiB available. Uncredited, lima still reserves
/// its 16 GiB, and Windows' 8 GiB boot with 8 GiB spare beside it waits on memory that is there
/// (the negative control); credited, lima reserves 2 GiB and the same boot is admitted.
#[test]
fn a_vms_own_processes_are_credited_against_its_claim() {
    const VM: u32 = 90_001;
    let s = scratch("credit");
    let m = Fixed::new(Reading::of_gib(48, 30, 14));
    m.set_procs(Some(Procs::of(vec![Proc {
        pid: VM,
        ppid: 1,
        footprint: 14 * GIB,
        written: 0,
    }])));
    let windows = || Need::new("windows").mem_gib(8).margin(8 * GIB);
    let mut lima = admitted(
        enter(&s, &m, Need::new("lima").mem_gib(16).margin(8 * GIB)),
        &m,
    );
    let (w, short) = waits(enter(&s, &m, windows()), &m);
    assert_eq!(
        short.shorts,
        vec![Short::Memory {
            need: 8 * GIB,
            margin: 8 * GIB,
            available: 30 * GIB,
            reserved: 16 * GIB
        }],
        "uncredited, the VM's 14 GiB is reserved again"
    );
    drop(w);
    lima.credit(&[VM]).expect("credited");
    assert_eq!(
        s.ledger.list().expect("listed")[0].credit,
        [VM],
        "every session reads the credit"
    );
    let first = admitted(enter(&s, &m, windows()), &m);
    let (w, short) = waits(enter(&s, &m, Need::new("more").mem_gib(21)), &m);
    assert_eq!(
        short.shorts,
        vec![Short::Memory {
            need: 21 * GIB,
            margin: 0,
            available: 30 * GIB,
            reserved: 10 * GIB
        }],
        "lima's 2 GiB not yet resident and Windows' 8"
    );
    drop((w, first, lima));
}

/// A wait its caller stops (an interrupt it caught) ends with what it was short of, and its
/// claim leaves the queue, holding no place ahead of later claims. Negative control: until the
/// stop, the same wait goes on past its first line, in the queue.
#[test]
fn a_stopped_wait_leaves_the_queue_naming_what_it_was_short_of() {
    let s = scratch("stopped");
    let m = mac48();
    let a = admitted(enter(&s, &m, Need::new("A").mem_gib(30)), &m);
    let stop = Arc::new(AtomicBool::new(false));
    let (lines, heard) = std::sync::mpsc::channel::<String>();
    let (ended, end) = std::sync::mpsc::channel();
    {
        let (ledger, stop) = (s.ledger.clone(), Arc::clone(&stop));
        std::thread::spawn(move || {
            let got = admit_until(
                &ledger,
                &mac48(),
                Need::new("B").mem_gib(20),
                Admit::Wait,
                Pace {
                    poll: Duration::from_millis(10),
                    line_every: Duration::from_secs(60),
                },
                &mut |l| {
                    let _ = lines.send(l.to_owned());
                },
                &|| stop.load(Ordering::SeqCst),
            );
            let _ = ended.send(got.map(|c| c.record().need.what.clone()));
        });
    }
    let first = heard
        .recv_timeout(MUST_HAPPEN)
        .expect("B's first wait line");
    names(&first, &["B", "short by 10 GiB"]);
    assert!(
        s.ledger
            .list()
            .expect("listed")
            .iter()
            .any(|r| r.need.what == "B"),
        "B waits in the queue"
    );
    assert!(end.try_recv().is_err(), "B still waits");
    stop.store(true, Ordering::SeqCst);
    match end
        .recv_timeout(MUST_HAPPEN)
        .expect("the stopped wait ends")
    {
        Err(Error::Stopped {
            signal: None,
            short: Some(short),
        }) => {
            names(&short.to_string(), &["short by 10 GiB", "\"A\""]);
        }
        other => panic!("expected a stopped wait, got {other:?}"),
    }
    assert_eq!(
        s.ledger
            .list()
            .expect("listed")
            .iter()
            .map(|r| r.need.what.as_str())
            .collect::<Vec<_>>(),
        ["A"],
        "B left the queue"
    );
    drop(a);
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
            .margin(8 * GIB)
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
    assert!(listed[0].credit.is_empty());
    let mut c = c;
    c.credit(&[77, 78]).expect("credited");
    c.credit(&[79, 0, 1, std::process::id()])
        .expect("credited again");
    assert_eq!(
        c.record().credit,
        [79],
        "the latest credit, less 0, 1 and its holder"
    );
    assert_eq!(s.ledger.list().expect("listed")[0], c.record().clone());
    let text = std::fs::read_to_string(c.path()).expect("the claim file");
    assert_eq!(
        Record::decode(&format!("{text}{{\"credit\":[8"), true).map(|r| r.credit),
        Some(vec![79]),
        "a credit line read half written leaves the last whole one standing"
    );
    c.wrote(3 * GIB).expect("measured");
    c.credit(&[80]).expect("credited after the measure");
    c.wrote(5 * GIB).expect("measured again, in place");
    let read = s.ledger.list().expect("listed").remove(0);
    assert_eq!(
        (read.credit, read.wrote),
        (vec![80], Some(5 * GIB)),
        "credits and the measured writes read back beside each other, whichever came first"
    );
    let path = c.path().to_path_buf();
    drop(c);
    assert!(
        !Path::new(&path).exists(),
        "a dropped claim leaves the ledger"
    );
    assert!(Record::decode("{\"v\":2}", false).is_none());
}

/// A claim names the checkout its need says it runs for (a tool whose working directory is `/`,
/// as the lab's is), in its record and in every wait line behind it; a need that names none is
/// the working directory's enclosing checkout's.
#[test]
fn a_claim_names_the_checkout_it_runs_for() {
    let s = scratch("checkout");
    let m = mac48();
    let lab = admitted(
        enter(
            &s,
            &m,
            Need::new("lab").mem_gib(30).checkout("/checkouts/lab"),
        ),
        &m,
    );
    assert_eq!(lab.record().checkout, "/checkouts/lab");
    assert_eq!(lab.record().need.checkout, None, "kept as the record's own");
    let (w, short) = waits(enter(&s, &m, Need::new("B").mem_gib(20)), &m);
    names(&short.to_string(), &["\"lab\" in /checkouts/lab"]);
    let listed = s.ledger.list().expect("listed");
    assert!(
        listed
            .iter()
            .any(|r| r.need.what == "lab" && r.checkout == "/checkouts/lab"),
        "{listed:?}"
    );
    let cwd = std::env::current_dir().expect("a working directory");
    let here = &w.record().checkout;
    assert!(
        cwd.starts_with(here) && Path::new(here).join(".git").symlink_metadata().is_ok(),
        "{here} is the checkout around {}",
        cwd.display()
    );
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

/// A volume of `total` GiB with `free` GiB free and a `floor` GiB floor, mounted at `/vol`.
fn vol(total: u64, free: u64, floor: u64) -> Volume {
    Volume {
        mount: PathBuf::from("/vol"),
        total: total * GIB,
        free: free * GIB,
        floor: floor * GIB,
        room: 2 * GIB,
    }
}

/// A machine whose volume is `vol`.
fn on(v: Volume) -> Fixed {
    let mut r = Reading::of_gib(48, 40, 14);
    r.volume = Some(v);
    Fixed::new(r)
}

/// A holder that measures its own writes is credited them, whatever its tree's bytes written
/// say: the free space has lost them already, so the room it leaves stays what it was. Its
/// claim file carries the figure, written over in place. Negative control: once the volume has
/// lost H's 41 GiB and before H says so, H's whole 50 GiB is still reserved and B waits.
#[test]
fn a_holders_measured_writes_are_credited() {
    let s = scratch("wrote");
    let m = on(vol(200, 100, 10));
    let mut h = admitted(enter(&s, &m, Need::new("H").disk(50 * GIB, "/vol")), &m);
    let need = Need::new("verify").disk(27 * GIB, "/vol");
    drop(admitted(enter(&s, &m, need.clone()), &m));
    m.set({
        let mut r = Reading::of_gib(48, 40, 14);
        r.volume = Some(vol(200, 59, 10));
        r
    });
    let (w, short) = waits(enter(&s, &m, need.clone()), &m);
    assert_eq!(
        short.shorts,
        vec![Short::Disk {
            need: 27 * GIB,
            mount: PathBuf::from("/vol"),
            above_floor: 49 * GIB,
            floor: 10 * GIB,
            reserved: 50 * GIB
        }]
    );
    h.wrote(12 * GIB).expect("said");
    h.wrote(41 * GIB).expect("said again, in place");
    let listed = s.ledger.list().expect("listed");
    assert_eq!(listed[0].wrote, Some(41 * GIB));
    assert_eq!(listed[0], h.record().clone());
    let w = admitted(w, &m);
    drop((w, h));
}

/// Free space a [`crate::Relief`] can give back is given back before the first wait line, and
/// a need it meets is admitted with no line at all. One it cannot meet waits, its lines naming
/// the need, the free space and what the relief says holds the rest. Negative control: with
/// room, the relief is never asked and nothing is said.
#[test]
fn a_disk_need_is_relieved_before_it_waits() {
    struct Freeing<'a> {
        machine: &'a Fixed,
        to: u64,
        asked: usize,
    }
    impl crate::Relief for Freeing<'_> {
        fn relieve(
            &mut self,
            short: &crate::Shortfall,
            say: &mut dyn FnMut(&str),
            _stopped: &dyn Fn() -> bool,
        ) {
            assert!(matches!(short.shorts[..], [Short::Disk { .. }]), "{short}");
            self.asked += 1;
            self.machine.set({
                let mut r = Reading::of_gib(48, 40, 14);
                r.volume = Some(vol(40, self.to, 4));
                r
            });
            say("reclaimed");
        }
        fn holders(
            &mut self,
            _short: &crate::Shortfall,
            _stopped: &dyn Fn() -> bool,
        ) -> Option<String> {
            Some("largest: /vol/big 9 GiB".to_owned())
        }
    }
    let pace = Pace {
        poll: Duration::from_millis(5),
        line_every: Duration::from_secs(60),
    };
    let s = scratch("relief");
    let need = || Need::new("verify").disk(27 * GIB, "/vol");

    let m = on(vol(40, 36, 4));
    let mut relief = Freeing {
        machine: &m,
        to: 36,
        asked: 0,
    };
    let mut said = Vec::new();
    let c = crate::admit_relieved(
        &s.ledger,
        &m,
        need(),
        Admit::Wait,
        pace,
        &mut relief,
        &mut |l| {
            said.push(l.to_owned());
        },
    )
    .expect("room");
    assert_eq!((relief.asked, said.len()), (0, 0), "{said:?}");
    drop(c);

    let m = on(vol(40, 20, 4));
    let mut relief = Freeing {
        machine: &m,
        to: 32,
        asked: 0,
    };
    let mut said = Vec::new();
    let c = crate::admit_relieved(
        &s.ledger,
        &m,
        need(),
        Admit::Wait,
        pace,
        &mut relief,
        &mut |l| {
            said.push(l.to_owned());
        },
    )
    .expect("relieved");
    assert_eq!(relief.asked, 1);
    assert_eq!(said, ["reclaimed"], "admitted with no wait line");
    drop(c);

    let s2 = scratch("relief-short");
    let m = std::sync::Arc::new(on(vol(40, 20, 4)));
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let ledger = s2.ledger.clone();
    let theirs = std::sync::Arc::clone(&m);
    let waiting = std::thread::spawn(move || {
        let mut relief = Freeing {
            machine: &theirs,
            to: 22,
            asked: 0,
        };
        let claim = crate::admit_relieved(
            &ledger,
            &*theirs,
            need(),
            Admit::Wait,
            pace,
            &mut relief,
            &mut |l| {
                let _ = tx.send(l.to_owned());
            },
        );
        (claim.map(|c| c.record().need.what.clone()), relief.asked)
    });
    let first = rx.recv_timeout(MUST_HAPPEN).expect("the relief's line");
    assert_eq!(first, "reclaimed");
    let line = rx.recv_timeout(MUST_HAPPEN).expect("the wait line");
    names(
        &line,
        &[
            "verify",
            "27 GiB",
            "18 GiB",
            "/vol",
            "largest: /vol/big 9 GiB",
        ],
    );
    assert!(!waiting.is_finished(), "it waits, short of room");
    m.set({
        let mut r = Reading::of_gib(48, 40, 14);
        r.volume = Some(vol(40, 36, 4));
        r
    });
    let (claim, asked) = waiting.join().expect("the waiter");
    assert_eq!(claim.expect("admitted once room came").as_str(), "verify");
    assert_eq!(asked, 1, "relieved once");
}

/// What the next run claims: until a run of its kind is measured, the cold budget less what
/// its directories hold already (nothing where they hold it all), then the most any kept run
/// grew its directories by, or held at its peak beyond what they hold now. The file keeps the
/// newest [`crate::footprint::KEPT`] runs and reads back as written.
#[test]
fn a_run_claims_what_the_last_runs_wrote() {
    let mut f = crate::Footprints::default();
    let empty = f.need(0, 27 * GIB);
    assert_eq!((empty.bytes, empty.runs), (27 * GIB, 0));
    names(&empty.to_string(), &["27 GiB", "cold budget"]);
    let cold = f.need(5 * GIB, 27 * GIB);
    assert_eq!((cold.bytes, cold.runs), (22 * GIB, 0), "less what is there");
    names(&cold.to_string(), &["22 GiB", "27 GiB", "5 GiB"]);
    assert_eq!(
        f.need(35 * GIB, 27 * GIB).bytes,
        0,
        "warm, with no record: nothing to claim"
    );
    f.push(crate::Footprint {
        peak: 40 * GIB,
        grew: 3 * GIB,
        at_ms: 1,
    });
    assert_eq!(
        f.need(38 * GIB, 27 * GIB).bytes,
        3 * GIB,
        "warm: its growth"
    );
    assert_eq!(
        f.need(4 * GIB, 27 * GIB).bytes,
        36 * GIB,
        "reclaimed since: its peak again"
    );
    f.push(crate::Footprint {
        peak: 41 * GIB,
        grew: GIB,
        at_ms: 2,
    });
    assert_eq!(
        f.need(40 * GIB, 27 * GIB).bytes,
        3 * GIB,
        "a light run does not hide a heavier one"
    );
    let mut light = crate::Footprints::default();
    light.push(crate::Footprint {
        peak: 24 << 20,
        grew: 24 << 20,
        at_ms: 1,
    });
    names(
        &light.need(24 << 20, 27 * GIB).to_string(),
        &["24 MiB", "1 run(s)"],
    );
    for at_ms in 3..20 {
        f.push(crate::Footprint {
            peak: 41 * GIB,
            grew: GIB,
            at_ms,
        });
    }
    assert_eq!(f.runs().len(), crate::footprint::KEPT);
    assert_eq!(
        f.need(41 * GIB, 27 * GIB).bytes,
        GIB,
        "the heavy run aged out"
    );
    let s = scratch("footprints");
    let path = s.dir.path().join("verify/footprint.json");
    f.save(&path).expect("saved");
    assert_eq!(crate::Footprints::load(&path), f);
    assert_eq!(
        crate::Footprints::load(&s.dir.path().join("none")),
        crate::Footprints::default()
    );
    assert_eq!(
        crate::Footprints::decode("{\"schema\":2}"),
        crate::Footprints::default()
    );
}

/// A meter tells its claim what the run has written so far, so a waiter counts only the rest
/// as reserved, and returns the most its directories held. Its directories' sizes here are a
/// counter the test moves.
#[test]
fn a_meter_says_what_its_run_wrote() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let s = scratch("meter");
    let m = on(vol(200, 100, 10));
    let claim = admitted(
        enter(&s, &m, Need::new("verify").disk(27 * GIB, "/vol")),
        &m,
    );
    let size = std::sync::Arc::new(AtomicU64::new(10 * GIB));
    let theirs = std::sync::Arc::clone(&size);
    let meter = crate::Meter::start_with(
        10 * GIB,
        Box::new(move || theirs.load(Ordering::SeqCst)),
        Some(claim),
        Duration::from_millis(5),
    );
    size.store(30 * GIB, Ordering::SeqCst);
    until_within(MUST_HAPPEN, "the claim to say 20 GiB written", || {
        let listed = s.ledger.list().ok()?;
        (listed.first()?.wrote == Some(20 * GIB)).then_some(())
    });
    size.store(25 * GIB, Ordering::SeqCst);
    let run = meter.finish();
    assert_eq!((run.peak, run.grew), (30 * GIB, 20 * GIB));
    assert!(
        s.ledger.list().expect("listed").is_empty(),
        "the claim went with the run"
    );
}
