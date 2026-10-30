// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ledger's admissions on an injected machine ([`Fixed`]): no test reads the live one.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use aterm_spec::wait::{MUST_HAPPEN, until_within};

use crate::admit::{Other, UNSERVABLE, counted, decide};
use crate::floor::{self, GIB};
use crate::reading::{Proc, Procs, TreeUse};
use crate::{
    Admit, Error, Fixed, Ledger, Need, Pace, Patience, Polled, Reading, Record, Short, Volume,
    Waiter, admit_until, admit_with, never,
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

/// What `thread` returned, once it ends, which it must: within the one-minute hang detector, so
/// an admission that never comes fails the test instead of hanging it.
#[track_caller]
fn joined<T>(thread: std::thread::JoinHandle<T>, what: &str) -> T {
    until_within(MUST_HAPPEN, what, || thread.is_finished().then_some(()));
    thread.join().expect(what)
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

/// A stream of 1 GiB claims from other processes does not overtake a 20 GiB claim waiting for
/// memory (10 GiB is free, but all of it is the waiter's), and each is admitted once B is; a
/// claim of B's own process is not held behind B (its process orders its own claims). Then,
/// with B waiting for a quiet machine instead, the stream backfills: each 1 GiB claim that fits
/// beside B's 20 GiB reservation goes first, and the first that would not, waits. Each claim is
/// another process's record, as the ledger reads them.
#[test]
fn a_stream_of_small_claims_does_not_overtake_a_waiting_large_one() {
    let m = Reading::of_gib(48, 40, 14);
    let a = record(1, 0, Need::new("A").mem_gib(30), Some(1));
    let b = record(2, 1, Need::new("B").mem_gib(20), None);
    let c = |i: u32, held: bool| {
        record(
            3 + i,
            10 + u64::from(i),
            Need::new(format!("C{i}")).mem_gib(1),
            held.then_some(1),
        )
    };
    let stream: Vec<Record> = (0..5).map(|i| c(i, false)).collect();
    for (i, ci) in stream.iter().enumerate() {
        let mut ahead = vec![plain(&a), plain(&b)];
        ahead.extend(stream[..i].iter().map(plain));
        let short = decide(&ci.need, ci.order(), &m, &ahead, 20).expect_err("waits");
        assert_eq!(
            short.shorts,
            vec![Short::Memory {
                need: GIB,
                margin: 0,
                available: 40 * GIB,
                reserved: (50 + i as u64) * GIB
            }],
            "C{i} waits behind A and B"
        );
        assert!(
            short.holders.iter().any(|h| !h.held && h.mem == 20 * GIB),
            "{short}"
        );
    }
    let own = record(2, 10, Need::new("B's own").mem_gib(1), None);
    assert!(
        decide(&own.need, own.order(), &m, &[plain(&a), plain(&b)], 20).is_ok(),
        "B's own process is not held behind B"
    );
    let b_held = record(2, 1, Need::new("B").mem_gib(20), Some(1));
    let later: Vec<Record> = stream
        .iter()
        .map(|r| record(r.pid, r.start_ms, r.need.clone(), None))
        .collect();
    let ahead: Vec<Other<'_>> = later.iter().map(plain).collect();
    assert!(
        decide(&b.need, b.order(), &m, &ahead, 20).is_ok(),
        "once A ends, B is admitted ahead of the stream"
    );
    let held: Vec<Record> = (0..5).map(|i| c(i, true)).collect();
    for (i, ci) in held.iter().enumerate() {
        let mut ahead = vec![plain(&b_held)];
        ahead.extend(held[..i].iter().map(plain));
        assert!(
            decide(&ci.need, ci.order(), &m, &ahead, 20).is_ok(),
            "C{i} is admitted beside B"
        );
    }

    let mut busy = Reading::of_gib(48, 40, 14);
    busy.load1 = Some(50.0);
    let b = record(2, 1, Need::new("B").mem_gib(20).quiet(6.0), None);
    let short = decide(&b.need, b.order(), &busy, &[], 2).expect_err("waits for the load");
    assert_eq!(
        short.shorts,
        vec![Short::Load {
            load1: 50.0,
            max: 6.0
        }]
    );
    let mut admitted_c: Vec<Record> = Vec::new();
    let waiting = loop {
        let n = u32::try_from(admitted_c.len()).expect("few");
        let next = record(100 + n, 100 + u64::from(n), Need::new("C").mem_gib(1), None);
        let mut ahead = vec![plain(&b)];
        ahead.extend(admitted_c.iter().map(plain));
        match decide(&next.need, next.order(), &busy, &ahead, 200) {
            Ok(_) => admitted_c.push(record(next.pid, next.start_ms, next.need, Some(1))),
            Err(short) => break short,
        }
        assert!(admitted_c.len() <= 40, "the stream never stopped");
    };
    assert_eq!(
        admitted_c.len(),
        20,
        "40 GiB less B's 20 GiB holds 20 claims of 1 GiB"
    );
    assert_eq!(
        waiting.shorts,
        vec![Short::Memory {
            need: GIB,
            margin: 0,
            available: 40 * GIB,
            reserved: 40 * GIB
        }]
    );
    assert!(
        waiting.holders.iter().any(|h| !h.held && h.what == "B"),
        "the waiter ahead is named: {waiting}"
    );
}

/// A need of all the memory that can ever be available or more can never fit: what is available
/// never holds a wired page ([`Reading::reachable`]). On a 28 GiB machine with 4 GiB wired, 16
/// GiB with 8 GiB spare beside it reaches the 24 GiB ceiling and is refused, naming it, and a
/// byte less is not. With nothing wired read, the ceiling is all of the memory.
#[test]
fn a_need_of_all_the_memory_that_can_ever_be_available_can_never_fit() {
    let r = Reading::of_gib(28, 18, 14).wired_gib(4);
    assert_eq!(r.reachable(), Some(24 * GIB));
    let boot = never(&Need::new("boot").mem_gib(16).margin(8 * GIB), &r).expect("refused");
    names(
        &boot,
        &[
            "boot",
            "needs 16 GiB and 8 GiB spare beside it",
            "more than 24 GiB of its 28 GiB of memory available",
            "4 GiB of it is wired",
        ],
    );
    let less = Need::new("less").mem(16 * GIB - 1).margin(8 * GIB);
    assert_eq!(never(&less, &r), None);
    let r = Reading::of_gib(48, 40, 14);
    let all = never(&Need::new("all").mem_gib(48), &r).expect("refused");
    names(&all, &["all", "needs 48 GiB", "48 GiB of memory"]);
    assert_eq!(never(&Need::new("less").mem(48 * GIB - 1), &r), None);
}

/// NEVER-REACHABLE's acceptance: on a 28 GiB machine with 4 GiB wired and 18 GiB available,
/// lima's 16 GiB boot with 8 GiB spare beside it is refused at once, before it enters the
/// ledger, naming the 24 GiB ceiling, with no wait line. On this Mac's shape (48 GiB, 3.5 GiB
/// wired) the same boot is admitted at once with 27 GiB available, and with 20 GiB it waits,
/// short by 4 GiB, until 24 GiB is. Negative control: judged against all of the memory (the
/// old rule), the 28 GiB machine's boot waits for memory it can never have, until the hang
/// detector.
#[test]
fn a_need_above_what_can_ever_be_available_is_refused_at_once_naming_the_ceiling() {
    let s = scratch("reach");
    let boot = || Need::new("lima boot").mem_gib(16).margin(8 * GIB);
    let quick = Pace {
        poll: Duration::from_millis(10),
        line_every: Duration::from_secs(60),
    };
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let ledger = s.ledger.clone();
    let refusing = std::thread::spawn(move || {
        admit_with(
            &ledger,
            &Fixed::new(Reading::of_gib(28, 18, 14).wired_gib(4)),
            boot(),
            Admit::Wait,
            quick,
            &mut |l| {
                let _ = tx.send(l.to_owned());
            },
        )
        .map(|c| c.record().need.what.clone())
    });
    match joined(refusing, "the boot's answer on a 28 GiB machine") {
        Err(e @ Error::Never(_)) => names(
            &e.to_string(),
            &[
                "lima boot: needs 16 GiB and 8 GiB spare beside it",
                "more than 24 GiB of its 28 GiB of memory available (4 GiB of it is wired)",
                "it can never be admitted here",
            ],
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let said: Vec<String> = rx.try_iter().collect();
    assert!(said.is_empty(), "no wait line: {said:?}");
    assert!(
        s.ledger.list().expect("listed").is_empty(),
        "nothing entered"
    );

    let mac = |available: u64| Reading {
        wired: Some(7 * GIB / 2),
        ..Reading::of_gib(48, available, 14)
    };
    let mut said = Vec::new();
    let claim = admit_with(
        &s.ledger,
        &Fixed::new(mac(27)),
        boot(),
        Admit::Wait,
        quick,
        &mut |l| said.push(l.to_owned()),
    )
    .expect("admitted on this Mac's shape");
    assert!(said.is_empty(), "no wait line: {said:?}");
    drop(claim);

    let m = Arc::new(Fixed::new(mac(20)));
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let (ledger, machine) = (s.ledger.clone(), Arc::clone(&m));
    let waiting = std::thread::spawn(move || {
        admit_with(&ledger, &*machine, boot(), Admit::Wait, quick, &mut |l| {
            let _ = tx.send(l.to_owned());
        })
        .map(|c| c.record().need.what.clone())
    });
    let first = rx.recv_timeout(MUST_HAPPEN).expect("the boot's wait line");
    names(
        &first,
        &["lima boot", "memory short by 4 GiB", "20 GiB available"],
    );
    assert!(!waiting.is_finished(), "it waits while there is no room");
    m.set(mac(24));
    let admitted = joined(waiting, "the boot's admission at 24 GiB available").expect("admitted");
    assert_eq!(admitted, "lima boot");
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
    match joined(refusing, "the 60 GiB need's answer") {
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
    let admitted_b = joined(waiting, "B's admission").expect("admitted");
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
            alone: false,
        }),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
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

/// The volume of the live stall of 2026-10-27.
const DATA: &str = "/System/Volumes/Data";

/// `t` tenths of a GiB, as a wait line reads them (`43.6 GiB`).
const fn tenths(t: u64) -> u64 {
    t * GIB / 10
}

/// A 48 GiB machine with 40 GiB available whose [`DATA`] volume (862 GiB) has `above` tenths
/// of a GiB free above its 32 GiB floor.
fn data(above: u64) -> Reading {
    let mut r = Reading::of_gib(48, 40, 14);
    r.volume = Some(Volume {
        mount: PathBuf::from(DATA),
        total: 862 * GIB,
        free: 32 * GIB + tenths(above),
        floor: 32 * GIB,
        room: 16 * GIB,
    });
    r
}

/// A claim in the ledger whose holder's tree uses nothing.
fn plain(record: &Record) -> Other<'_> {
    Other {
        record,
        used: TreeUse::default(),
        credited: TreeUse::default(),
    }
}

/// THE LIVE STALL OF 2026-10-27, AS A TABLE. [`DATA`] held 43.6 GiB above its 32 GiB floor, and
/// three verify runs waited ahead needing 63.7, 56.5 and 67.4 GiB: each more than that room, so
/// no claim's end could ever admit one, only space freed. A held claim has 3 GiB left to write
/// (taken so that, with the two waiters ahead of it, the third reads the live line's 123.2 GiB
/// reserved under the old rule). No unservable waiter reserves disk, so a later run needing
/// 5 GiB is admitted at once, and each waiter is held to the held claim alone, its shortfall
/// unservable. Negative control: with every waiter reserving its need (the rule before), the
/// 5 GiB run waits, `less 190.6 GiB reserved`.
#[test]
fn a_waiter_no_claims_end_can_admit_reserves_no_disk() {
    let r = data(436);
    let held = record(
        10,
        0,
        Need::new("verify test -p aterm-ctl").disk(3 * GIB, DATA),
        Some(4),
    );
    let cells = record(
        11,
        1,
        Need::new("verify cells").disk(tenths(637), DATA),
        None,
    );
    let agent = record(
        12,
        2,
        Need::new("verify fmt census lint test doc cells -p aterm-agent -p aterm-cli -p aterm")
            .disk(tenths(565), DATA),
        None,
    );
    let lab = record(
        13,
        3,
        Need::new("verify fmt census test doc -p aterm-lab").disk(tenths(674), DATA),
        None,
    );
    let others: Vec<Other<'_>> = [&held, &cells, &agent, &lab]
        .into_iter()
        .map(plain)
        .collect();
    let later = Need::new("verify lint -p aterm-capacity").disk(5 * GIB, DATA);
    if let Err(short) = decide(&later, (4, 14, 0), &r, &others, 5) {
        panic!("the 5 GiB run waits behind waiters no claim's end can admit: {short}");
    }
    for w in [&cells, &agent, &lab] {
        let short = decide(&w.need, w.order(), &r, &others, 5).expect_err("more than the room");
        assert_eq!(
            short.shorts,
            vec![Short::Disk {
                need: w.need.disk,
                mount: PathBuf::from(DATA),
                above_floor: tenths(436),
                floor: 32 * GIB,
                reserved: 3 * GIB,
            }],
            "{}: held to the held claim alone: {short}",
            w.need.what
        );
        assert!(short.shorts[0].unservable());
        assert_eq!(
            short.holders.iter().map(|h| h.pid).collect::<Vec<_>>(),
            [10],
            "no unservable waiter is named as holding disk: {short}"
        );
        let need = crate::gib(w.need.disk);
        names(&short.to_string(), &[need.as_str(), "43.6 GiB", "3 GiB"]);
    }
}

/// The order is kept for every waiter whose need fits the room, and for every held claim. On
/// [`DATA`] 43.6 GiB above its floor beside a held claim with 3 GiB left: a waiter needing
/// 40 GiB, or exactly the room, holds back a later 5 GiB run, which is named short beside it; a
/// byte more than the room and it holds back nothing. The waiter's own shortfall is unservable
/// by the same test: not at exactly the room, at a byte more. A claim held by `--admit now` for
/// 50 GiB, more than the room, keeps its whole reservation.
#[test]
fn a_waiter_the_room_can_hold_and_a_held_claim_still_reserve_their_disk() {
    let r = data(436);
    let held = record(10, 0, Need::new("held").disk(3 * GIB, DATA), Some(4));
    let later = Need::new("verify lint -p aterm-capacity").disk(5 * GIB, DATA);
    for (ahead, reserved, own) in [
        (40 * GIB, Some(43 * GIB), None),
        (tenths(436), Some(3 * GIB + tenths(436)), Some(false)),
        (tenths(436) + 1, None, Some(true)),
    ] {
        let waiting = record(11, 1, Need::new("ahead").disk(ahead, DATA), None);
        let others = [plain(&held), plain(&waiting)];
        let theirs = decide(&waiting.need, waiting.order(), &r, &others, 5);
        assert_eq!(
            theirs
                .as_ref()
                .err()
                .map(|s| s.shorts.iter().any(Short::unservable)),
            own,
            "ahead needing {ahead} B: {theirs:?}"
        );
        match (decide(&later, (4, 14, 0), &r, &others, 5), reserved) {
            (Ok(_), None) => {}
            (Err(short), Some(reserved)) => {
                assert_eq!(
                    short.shorts,
                    vec![Short::Disk {
                        need: 5 * GIB,
                        mount: PathBuf::from(DATA),
                        above_floor: tenths(436),
                        floor: 32 * GIB,
                        reserved,
                    }],
                    "{short}"
                );
                assert!(!short.shorts[0].unservable(), "{short}");
                assert!(
                    short
                        .holders
                        .iter()
                        .any(|h| h.pid == 11 && !h.held && h.disk == ahead),
                    "the waiter ahead is named: {short}"
                );
            }
            (got, want) => panic!("ahead needing {ahead} B: {got:?}, expected {want:?} reserved"),
        }
    }
    let forced = record(10, 0, Need::new("forced").disk(50 * GIB, DATA), Some(4));
    let short = decide(&later, (4, 14, 0), &r, &[plain(&forced)], 5)
        .expect_err("a held claim reserves whatever the room");
    assert_eq!(
        short.shorts,
        vec![Short::Disk {
            need: 5 * GIB,
            mount: PathBuf::from(DATA),
            above_floor: tenths(436),
            floor: 32 * GIB,
            reserved: 50 * GIB,
        }]
    );
}

/// An unservable waiter's memory is reserved as any waiter's, since memory comes back as its
/// holders end: a waiter ahead needing 63.7 GiB of disk and 20 GiB of memory leaves a later run
/// short of memory alone (25 GiB of the 40 available less its 20), naming it for its memory;
/// 20 GiB fits beside it.
#[test]
fn an_unservable_waiters_memory_is_still_reserved() {
    let r = data(436);
    let waiting = record(
        11,
        1,
        Need::new("verify cells")
            .mem_gib(20)
            .disk(tenths(637), DATA),
        None,
    );
    let others = [plain(&waiting)];
    let short = decide(
        &Need::new("trust").mem_gib(25).disk(5 * GIB, DATA),
        (4, 14, 0),
        &r,
        &others,
        5,
    )
    .expect_err("short of memory");
    assert_eq!(
        short.shorts,
        vec![Short::Memory {
            need: 25 * GIB,
            margin: 0,
            available: 40 * GIB,
            reserved: 20 * GIB,
        }]
    );
    assert_eq!(
        short
            .holders
            .iter()
            .map(|h| (h.pid, h.mem, h.disk))
            .collect::<Vec<_>>(),
        [(11, 20 * GIB, 0)]
    );
    let fits = Need::new("trust").mem_gib(20).disk(5 * GIB, DATA);
    assert!(decide(&fits, (4, 14, 0), &r, &others, 5).is_ok());
}

/// WHAT THE LISTING SAYS EACH CLAIM RESERVES ([`crate::reserved`]) IS WHAT A LATER LOOK HOLDS
/// ITS NEED TO, holder by holder, each claim on its own volume. Beside [`DATA`] (43.6 GiB above
/// its floor) a scratch volume holds 68 GiB above its floor. Each reservation is below its need
/// where some of it is written: by the holder's tree (the held claim, 20 GiB less 5), its own
/// count (8 less 2), a process credited to it (4 less 1); an unservable waiter reserves 0 on
/// either volume. A later run short of disk and memory on each volume names each holder with
/// the figures listed for it. Negative controls: reckoned with no sweep, the held claim lists
/// 20 GiB where the look holds 15; reckoned on [`DATA`] alone, the scratch claims list none.
#[test]
fn the_listed_reservations_are_what_a_later_look_holds_its_need_to() {
    const SCRATCH: &str = "/Volumes/scratch";
    let data_read = data(436);
    let mut scratch_read = Reading::of_gib(48, 40, 14);
    scratch_read.volume = Some(Volume {
        mount: PathBuf::from(SCRATCH),
        total: 500 * GIB,
        free: 100 * GIB,
        floor: 32 * GIB,
        room: 16 * GIB,
    });
    let with = |mut r: Record, wrote: Option<u64>, credit: &[u32]| {
        r.wrote = wrote;
        r.credit = credit.to_vec();
        r
    };
    let records = [
        record(
            10,
            0,
            Need::new("held").mem_gib(10).disk(20 * GIB, DATA),
            Some(4),
        ),
        with(
            record(11, 1, Need::new("wrote").disk(8 * GIB, DATA), None),
            Some(2 * GIB),
            &[],
        ),
        with(
            record(12, 2, Need::new("far").disk(tenths(637), DATA), None),
            Some(GIB),
            &[],
        ),
        with(
            record(13, 3, Need::new("vm").mem_gib(6).disk(4 * GIB, DATA), None),
            None,
            &[30],
        ),
        with(
            record(14, 4, Need::new("there").disk(10 * GIB, SCRATCH), None),
            Some(4 * GIB),
            &[],
        ),
        record(15, 5, Need::new("too far").disk(70 * GIB, SCRATCH), None),
    ];
    let trees = std::collections::HashMap::from([
        (
            10,
            TreeUse {
                footprint: 4 * GIB,
                written: 5 * GIB,
            },
        ),
        (
            30,
            TreeUse {
                footprint: 2 * GIB,
                written: GIB,
            },
        ),
    ]);
    let volumes = [&data_read, &scratch_read].map(|r| r.volume.clone().expect("a volume"));
    let listed = crate::reserved(&records, Some(&trees), &volumes);
    assert_eq!(
        listed
            .iter()
            .map(|r| (r.mem, r.disk, r.unservable))
            .collect::<Vec<_>>(),
        [
            (6 * GIB, Some(15 * GIB), false),
            (0, Some(6 * GIB), false),
            (0, Some(0), true),
            (4 * GIB, Some(3 * GIB), false),
            (0, Some(6 * GIB), false),
            (0, Some(0), true),
        ]
    );
    let refs: Vec<&Record> = records.iter().collect();
    let later = 99;
    let taken = records.iter().map(|r| r.pid).chain([later]).collect();
    let others = crate::admit::others(&refs, taken, &trees);
    for (read, on, need) in [
        (
            &data_read,
            DATA,
            Need::new("later").mem_gib(35).disk(40 * GIB, DATA),
        ),
        (
            &scratch_read,
            SCRATCH,
            Need::new("later").disk(65 * GIB, SCRATCH),
        ),
    ] {
        let short = decide(&need, (9, later, 0), read, &others, 9).expect_err("short");
        assert!(
            short.shorts.iter().any(|s| matches!(s, Short::Disk { .. })),
            "{short}"
        );
        let held_to: Vec<(u32, u64, u64)> = short
            .holders
            .iter()
            .map(|h| (h.pid, h.mem, h.disk))
            .collect();
        let want: Vec<(u32, u64, u64)> = records
            .iter()
            .zip(&listed)
            .map(|(r, l)| {
                let here = r.need.path.as_deref() == Some(Path::new(on));
                (r.pid, l.mem, if here { l.disk.unwrap_or(0) } else { 0 })
            })
            .filter(|(_, mem, disk)| (need.mem > 0 && *mem > 0) || *disk > 0)
            .collect();
        assert_eq!(held_to, want, "on {on}: {short}");
    }
}

/// On the ledger: a 63.7 GiB waiter on 43.6 GiB of room waits, its line saying only space freed
/// admits it, and a 5 GiB run that asks after it is admitted at once. Space freed to exactly its
/// need, then to 65 GiB, fits its need but not beside the run: it still waits, its line no longer
/// saying so, and it reserves again, so a second 5 GiB run waits behind it, naming it. Freed to
/// 75 GiB, it is admitted, then the second run beside it: the order is the queue's again.
#[test]
fn an_unservable_waiter_is_admitted_once_room_comes_and_holds_its_place_once_it_fits() {
    let s = scratch("unservable");
    let m = Fixed::new(data(436));
    let (big, short) = waits(
        enter(&s, &m, Need::new("verify cells").disk(tenths(637), DATA)),
        &m,
    );
    assert!(short.shorts[0].unservable(), "{short}");
    assert!(big.line(&short).contains(UNSERVABLE), "{short}");
    let first = admitted(enter(&s, &m, Need::new("first").disk(5 * GIB, DATA)), &m);
    m.set(data(637));
    let (big, short) = waits(big, &m);
    assert_eq!(
        short.shorts,
        vec![Short::Disk {
            need: tenths(637),
            mount: PathBuf::from(DATA),
            above_floor: tenths(637),
            floor: 32 * GIB,
            reserved: 5 * GIB,
        }]
    );
    assert!(!short.shorts[0].unservable(), "{short}");
    assert!(!big.line(&short).contains(UNSERVABLE), "{short}");
    m.set(data(650));
    let (big, short) = waits(big, &m);
    assert_eq!(
        short.shorts,
        vec![Short::Disk {
            need: tenths(637),
            mount: PathBuf::from(DATA),
            above_floor: tenths(650),
            floor: 32 * GIB,
            reserved: 5 * GIB,
        }]
    );
    assert!(!short.shorts[0].unservable(), "{short}");
    let (second, short) = waits(enter(&s, &m, Need::new("second").disk(5 * GIB, DATA)), &m);
    assert_eq!(
        short.shorts,
        vec![Short::Disk {
            need: 5 * GIB,
            mount: PathBuf::from(DATA),
            above_floor: tenths(650),
            floor: 32 * GIB,
            reserved: 5 * GIB + tenths(637),
        }]
    );
    assert!(
        short
            .holders
            .iter()
            .any(|h| !h.held && h.what == "verify cells"),
        "{short}"
    );
    m.set(data(750));
    let big = admitted(big, &m);
    let second = admitted(second, &m);
    drop((first, big, second));
}

/// An unservable waiter's wait line says what admits it and how to start it now, keeping the
/// need, the free space above the floor and what is reserved; once space freed reaches its need
/// it is admitted. A need the room holds says none of it (above).
#[test]
fn an_unservable_waiters_line_says_only_freed_space_admits_it() {
    let s = scratch("unservable-line");
    let m = Arc::new(Fixed::new(data(436)));
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let (ledger, theirs) = (s.ledger.clone(), Arc::clone(&m));
    let waiting = std::thread::spawn(move || {
        admit_with(
            &ledger,
            &*theirs,
            Need::new("verify cells").disk(tenths(637), DATA),
            Admit::Wait,
            Pace {
                poll: Duration::from_millis(5),
                line_every: Duration::from_secs(60),
            },
            &mut |l| {
                let _ = tx.send(l.to_owned());
            },
        )
        .map(|c| c.record().need.what.clone())
    });
    let line = rx.recv_timeout(MUST_HAPPEN).expect("the wait line");
    names(
        &line,
        &[
            "verify cells",
            "63.7 GiB",
            DATA,
            "43.6 GiB",
            "32 GiB",
            "0 GiB",
            "--admit now",
        ],
    );
    assert!(line.contains(UNSERVABLE), "{line}");
    m.set(data(637));
    let got = joined(waiting, "the waiter's admission once room came").expect("admitted");
    assert_eq!(got, "verify cells");
}

/// Only a waiter's line says an unservable need waits for space freed and holds back no claim:
/// the line `--admit now` says of the same need admitted at once on 43.6 GiB of room, a claim
/// that now holds its whole 63.7 GiB against every later one (a later 5 GiB run waits, that
/// much reserved), names the shortfall without it; so does the error of a wait an interrupt
/// ended after its first wait line, which did say it.
#[test]
fn only_a_waiters_line_says_an_unservable_need_waits_for_space_freed() {
    let s = scratch("unservable-now");
    let m = Fixed::new(data(436));
    let mut said = Vec::new();
    let forced = admit_with(
        &s.ledger,
        &m,
        Need::new("verify cells").disk(tenths(637), DATA),
        Admit::Now,
        Pace::default(),
        &mut |l| said.push(l.to_owned()),
    )
    .expect("admitted by --admit now");
    assert_eq!(said.len(), 1, "{said:?}");
    names(
        &said[0],
        &["verify cells", "--admit now", "63.7 GiB", "43.6 GiB"],
    );
    assert!(!said[0].contains(UNSERVABLE), "{said:?}");
    let (later, short) = waits(enter(&s, &m, Need::new("later").disk(5 * GIB, DATA)), &m);
    assert!(
        matches!(short.shorts[..], [Short::Disk { reserved, .. }] if reserved == tenths(637)),
        "{short}"
    );
    drop((later, forced));

    let stop = AtomicBool::new(false);
    let mut said = Vec::new();
    let ended = admit_until(
        &s.ledger,
        &m,
        Need::new("verify cells").disk(tenths(637), DATA),
        Admit::Wait,
        Pace {
            poll: Duration::from_millis(5),
            line_every: Duration::from_secs(60),
        },
        &mut |l| {
            said.push(l.to_owned());
            stop.store(true, Ordering::SeqCst);
        },
        &|| stop.load(Ordering::SeqCst),
    )
    .expect_err("stopped after its first wait line");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains(UNSERVABLE), "{said:?}");
    let Error::Stopped {
        signal: None,
        short: Some(last),
    } = &ended
    else {
        panic!("stopped by its caller after a look: {ended}");
    };
    assert!(last.shorts[0].unservable(), "{ended}");
    names(&ended.to_string(), &["63.7 GiB", "43.6 GiB"]);
    assert!(!ended.to_string().contains(UNSERVABLE), "{ended}");
}

/// An unservable waiter overtaken while it could not fit is relieved again at the look at which
/// space freed brings its need within the room but the claims admitted around it hold the rest.
/// On a volume 16 GiB above its 4 GiB floor a 27 GiB run is relieved (nothing freed), and a 5 GiB
/// run is admitted around it; freed to 27 GiB above the floor, it fits the room but is short by
/// that run's 5 GiB, so the relief is asked again, with those 5 GiB reserved, and once that
/// frees 32 GiB above the floor the run is admitted. Negative control: relieved at its first
/// look alone, it waits for space no relief is asked for (the hang detector ends the test).
#[test]
fn an_overtaken_unservable_waiter_is_relieved_again_once_it_fits_the_room() {
    /// Records the reserved figure each call is sized by; the first waits for the test's word,
    /// the second frees 32 GiB above the floor.
    struct Again {
        machine: Arc<Fixed>,
        asked: std::sync::mpsc::Sender<u64>,
        go: std::sync::mpsc::Receiver<()>,
        calls: usize,
    }
    impl crate::Relief for Again {
        fn relieve(
            &mut self,
            short: &crate::Shortfall,
            _say: &mut dyn FnMut(&str),
            _stopped: &dyn Fn() -> bool,
        ) {
            let [Short::Disk { reserved, .. }] = short.shorts[..] else {
                panic!("a disk shortfall alone: {short}");
            };
            self.calls += 1;
            let _ = self.asked.send(reserved);
            if self.calls == 1 {
                self.go.recv_timeout(MUST_HAPPEN).expect("the test's word");
            } else {
                self.machine.set(with_vol(vol(100, 36, 4)));
            }
        }
        fn holders(
            &mut self,
            _short: &crate::Shortfall,
            _stopped: &dyn Fn() -> bool,
        ) -> Option<String> {
            None
        }
    }
    let s = scratch("relief-again");
    let m = Arc::new(on(vol(100, 20, 4)));
    let (asked, asks) = std::sync::mpsc::channel::<u64>();
    let (go, word) = std::sync::mpsc::channel::<()>();
    let (ledger, theirs) = (s.ledger.clone(), Arc::clone(&m));
    let waiting = std::thread::spawn(move || {
        let mut relief = Again {
            machine: Arc::clone(&theirs),
            asked,
            go: word,
            calls: 0,
        };
        crate::admit_relieved(
            &ledger,
            &*theirs,
            Need::new("verify").disk(27 * GIB, "/vol"),
            Admit::Wait,
            Pace {
                poll: Duration::from_millis(5),
                line_every: Duration::from_secs(60),
            },
            &mut relief,
            &mut |_| {},
        )
        .map(|c| c.record().need.what.clone())
    });
    assert_eq!(asks.recv_timeout(MUST_HAPPEN).expect("the first ask"), 0);
    let around = admitted(enter(&s, &m, Need::new("around").disk(5 * GIB, "/vol")), &m);
    m.set(with_vol(vol(100, 31, 4)));
    go.send(()).expect("the waiter's relief listens");
    assert_eq!(
        asks.recv_timeout(MUST_HAPPEN)
            .expect("asked again once it fits the room"),
        5 * GIB
    );
    let got = joined(waiting, "its admission once the second relief freed room");
    assert_eq!(got.expect("admitted").as_str(), "verify");
    drop(around);
}

/// An unservable waiter whose first relief itself brings its need within the room, while the
/// claim held before it asked keeps it short, is not relieved again: that relief was sized for
/// that claim. A 10 GiB claim is held before a 30 GiB run asks, on a volume 20 GiB above its
/// 4 GiB floor; the relief, sized with those 10 GiB reserved, frees to 35 GiB above the floor,
/// so the run fits the room but is short by 5 GiB of what that claim holds, and it waits, its
/// line naming that claim, with no second call. Once 45 GiB are free above the floor it is
/// admitted, the relief asked once. Negative control: relieving at every look that turns
/// servable asks again at once, with the same 10 GiB reserved, before the wait line.
#[test]
fn a_waiter_its_own_relief_brought_within_the_room_is_not_relieved_again_for_the_same_claims() {
    /// Records each call's reserved figure and whether its look was unservable; frees to 35 GiB
    /// above the floor.
    struct Once {
        machine: Arc<Fixed>,
        calls: Arc<std::sync::Mutex<Vec<(u64, bool)>>>,
    }
    impl crate::Relief for Once {
        fn relieve(
            &mut self,
            short: &crate::Shortfall,
            _say: &mut dyn FnMut(&str),
            _stopped: &dyn Fn() -> bool,
        ) {
            let [disk] = &short.shorts[..] else {
                panic!("one shortfall: {short}");
            };
            let Short::Disk { reserved, .. } = *disk else {
                panic!("a disk shortfall alone: {short}");
            };
            self.calls
                .lock()
                .expect("calls")
                .push((reserved, disk.unservable()));
            self.machine.set(with_vol(vol(100, 39, 4)));
        }
        fn holders(
            &mut self,
            _short: &crate::Shortfall,
            _stopped: &dyn Fn() -> bool,
        ) -> Option<String> {
            None
        }
    }
    let s = scratch("relief-once");
    let m = Arc::new(on(vol(100, 24, 4)));
    let held = admitted(enter(&s, &m, Need::new("held").disk(10 * GIB, "/vol")), &m);
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let (ledger, theirs, their_calls) = (s.ledger.clone(), Arc::clone(&m), Arc::clone(&calls));
    let waiting = std::thread::spawn(move || {
        let mut relief = Once {
            machine: Arc::clone(&theirs),
            calls: their_calls,
        };
        crate::admit_relieved(
            &ledger,
            &*theirs,
            Need::new("verify").disk(30 * GIB, "/vol"),
            Admit::Wait,
            Pace {
                poll: Duration::from_millis(5),
                line_every: Duration::from_secs(60),
            },
            &mut relief,
            &mut |l| {
                let _ = tx.send(l.to_owned());
            },
        )
        .map(|c| c.record().need.what.clone())
    });
    let line = rx.recv_timeout(MUST_HAPPEN).expect("the wait line");
    names(
        &line,
        &[
            "verify",
            "short by 5 GiB",
            "35 GiB",
            "10 GiB reserved",
            "\"held\"",
        ],
    );
    assert!(!line.contains(UNSERVABLE), "it fits the room: {line}");
    assert_eq!(*calls.lock().expect("calls"), [(10 * GIB, true)]);
    m.set(with_vol(vol(100, 49, 4)));
    let got = joined(waiting, "its admission once room came");
    assert_eq!(got.expect("admitted").as_str(), "verify");
    assert_eq!(*calls.lock().expect("calls"), [(10 * GIB, true)]);
    drop(held);
}

/// A NEED SIZED AGAIN AFTER ITS RELIEF RE-ENTERS THE WAIT WITH IT ([`crate::Relief::resized`]).
/// A 2 GiB run on a volume 1 GiB above its 4 GiB floor is relieved, which takes 100 ms and frees
/// to 10 GiB above the floor but removes what the need was sized on, so it is sized again at 12
/// GiB: it waits with that need, relieved afresh, sized for 12 GiB, and once that relief frees
/// to 16 GiB above the floor it is admitted holding 12 GiB, its wait counted from when it first
/// entered (at least the first relief's 100 ms). Sized again to more than the volume holds above
/// its floor, it is refused at once, naming that figure, and leaves nothing in the ledger.
/// Negative control: sized again at or below its need, nothing changes: one relief, and it is
/// admitted holding 2 GiB. Each runs on a thread of its own, joined within the hang detector.
#[test]
fn a_need_sized_again_after_its_relief_re_enters_the_wait_with_it() {
    /// How long the first call takes, as a relief's pass does.
    const FIRST: Duration = Duration::from_millis(100);
    /// Each call frees to `frees[i]` GiB, then sizes the need again to `sized[i]`; records the
    /// need each call was sized for. The first takes [`FIRST`].
    struct Emptying {
        machine: Arc<Fixed>,
        frees: Vec<u64>,
        sized: Vec<Option<u64>>,
        needs: Vec<u64>,
    }
    impl crate::Relief for Emptying {
        fn relieve(
            &mut self,
            short: &crate::Shortfall,
            _say: &mut dyn FnMut(&str),
            _stopped: &dyn Fn() -> bool,
        ) {
            let [Short::Disk { need, .. }] = short.shorts[..] else {
                panic!("a disk shortfall alone: {short}");
            };
            if self.needs.is_empty() {
                std::thread::sleep(FIRST);
            }
            self.needs.push(need);
            self.machine
                .set(with_vol(vol(100, self.frees[self.needs.len() - 1], 4)));
        }
        fn holders(
            &mut self,
            _short: &crate::Shortfall,
            _stopped: &dyn Fn() -> bool,
        ) -> Option<String> {
            None
        }
        fn resized(
            &mut self,
            say: &mut dyn FnMut(&str),
            _stopped: &dyn Fn() -> bool,
        ) -> Option<u64> {
            let to = self.needs.len().checked_sub(1).and_then(|i| self.sized[i]);
            if let Some(bytes) = to {
                say(&format!("sized again: {}", crate::gib(bytes)));
            }
            to
        }
    }
    let pace = Pace {
        poll: Duration::from_millis(5),
        line_every: Duration::from_secs(60),
    };
    let s = scratch("resized");
    let run = |frees: &[u64], sized: &[Option<u64>]| {
        let ledger = s.ledger.clone();
        let mut relief = Emptying {
            machine: Arc::new(on(vol(100, 5, 4))),
            frees: frees.to_vec(),
            sized: sized.to_vec(),
            needs: Vec::new(),
        };
        let waiting = std::thread::spawn(move || {
            let mut said = Vec::new();
            let machine = Arc::clone(&relief.machine);
            let got = crate::admit_relieved(
                &ledger,
                &*machine,
                Need::new("verify").disk(2 * GIB, "/vol"),
                Admit::Wait,
                pace,
                &mut relief,
                &mut |l| said.push(l.to_owned()),
            );
            (got, relief.needs, said)
        });
        joined(waiting, "its admission or refusal")
    };

    let (got, needs, said) = run(&[14, 20], &[Some(12 * GIB), None]);
    let claim = got.expect("admitted once the second relief freed room");
    assert_eq!(claim.record().need.disk, 12 * GIB);
    assert!(
        claim.waited() >= FIRST,
        "counted from when it first entered: {:?}",
        claim.waited()
    );
    assert_eq!(
        needs,
        [2 * GIB, 12 * GIB],
        "relieved afresh, sized for 12 GiB"
    );
    assert_eq!(said, ["sized again: 12 GiB"]);
    let listed = s.ledger.list().expect("listed");
    assert_eq!(
        listed.len(),
        1,
        "the first entry left the queue: {listed:?}"
    );
    drop(claim);

    let (got, needs, _) = run(&[14], &[Some(200 * GIB)]);
    match got {
        Err(Error::Never(why)) => names(&why, &["verify", "200 GiB", "/vol", "96 GiB"]),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(needs, [2 * GIB]);
    assert!(s.ledger.list().expect("listed").is_empty());

    let (got, needs, said) = run(&[14], &[Some(GIB)]);
    let claim = got.expect("admitted once the relief freed room");
    assert_eq!(claim.record().need.disk, 2 * GIB);
    assert_eq!(needs, [2 * GIB]);
    assert_eq!(said, ["sized again: 1 GiB", "sized again: 1 GiB"]);
}

/// A NEED THAT GREW WHILE IT WAITED IS SIZED AGAIN AS IT IS ADMITTED: a 2 GiB run waits on a
/// volume at its floor, its relief freeing nothing and sizing nothing again; then room comes,
/// but what it was sized on went meanwhile (another run's pass, the watch's own), so at the look
/// that admits it the need is sized again at 12 GiB: the 2 GiB claim is let go and the need
/// re-enters, admitted holding 12 GiB, one entry in the ledger. Negative control: a run
/// admitted at its first look, having waited for nothing, is not sized again.
#[test]
fn a_need_that_grew_while_it_waited_is_sized_again_as_it_is_admitted() {
    /// Frees nothing; sizes the need again to `grown` once the test has said so.
    struct Grown {
        grown: Arc<std::sync::Mutex<Option<u64>>>,
        asked: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl crate::Relief for Grown {
        fn relieve(
            &mut self,
            _short: &crate::Shortfall,
            _say: &mut dyn FnMut(&str),
            _stopped: &dyn Fn() -> bool,
        ) {
        }
        fn holders(
            &mut self,
            _short: &crate::Shortfall,
            _stopped: &dyn Fn() -> bool,
        ) -> Option<String> {
            None
        }
        fn resized(
            &mut self,
            _say: &mut dyn FnMut(&str),
            _stopped: &dyn Fn() -> bool,
        ) -> Option<u64> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            *self.grown.lock().expect("grown")
        }
    }
    let pace = Pace {
        poll: Duration::from_millis(5),
        line_every: Duration::from_secs(60),
    };
    let s = scratch("grown-waiting");
    let m = Arc::new(on(vol(100, 4, 4)));
    let grown = Arc::new(std::sync::Mutex::new(None));
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let (ledger, theirs) = (s.ledger.clone(), Arc::clone(&m));
    let mut relief = Grown {
        grown: Arc::clone(&grown),
        asked: Arc::clone(&asked),
    };
    let waiting = std::thread::spawn(move || {
        crate::admit_relieved(
            &ledger,
            &*theirs,
            Need::new("verify").disk(2 * GIB, "/vol"),
            Admit::Wait,
            pace,
            &mut relief,
            &mut |l| {
                let _ = tx.send(l.to_owned());
            },
        )
    });
    let line = rx.recv_timeout(MUST_HAPPEN).expect("the wait line");
    names(&line, &["verify", "short by 2 GiB"]);
    *grown.lock().expect("grown") = Some(12 * GIB);
    m.set(with_vol(vol(100, 20, 4)));
    let claim = joined(waiting, "its admission once room came").expect("admitted");
    assert_eq!(claim.record().need.disk, 12 * GIB);
    let listed = s.ledger.list().expect("listed");
    assert_eq!(listed.len(), 1, "the 2 GiB claim was let go: {listed:?}");
    drop(claim);

    let before = asked.load(Ordering::SeqCst);
    let mut relief = Grown {
        grown: Arc::clone(&grown),
        asked: Arc::clone(&asked),
    };
    let at_once = crate::admit_relieved(
        &s.ledger,
        &*m,
        Need::new("lint").disk(GIB, "/vol"),
        Admit::Wait,
        pace,
        &mut relief,
        &mut |_| {},
    )
    .expect("admitted at once");
    assert_eq!(at_once.record().need.disk, GIB);
    assert_eq!(asked.load(Ordering::SeqCst), before, "not sized again");
}

/// AN UNSERVABLE WAITER IS RELIEVED AGAIN FOR A CLAIM ITS LAST RELIEF DID NOT COUNT, whatever the
/// figure: a 10 GiB claim held before a 30 GiB run asks, on a volume 20 GiB above its 4 GiB
/// floor, is what the first relief was sized for; that claim ends (its writes stay: 10 GiB above
/// the floor), a 5 GiB claim is admitted around the run, and space freed to 33 GiB above the
/// floor brings the run within the room, short by those 5 GiB: less reserved than the first
/// relief was sized for, but by a claim it did not count, so the relief is asked again, sized for
/// it, and once that frees room the run is admitted. Negative control: compared by the reserved
/// figure alone (5 GiB is not above 10), it is never asked again, and the run waits for a person
/// (the hang detector ends the test).
#[test]
fn an_unservable_waiter_is_relieved_again_for_a_claim_its_last_relief_did_not_count() {
    /// Records the reserved figure and unservability each call is sized by; the first waits for
    /// the test's word, the second frees to 45 GiB above the floor.
    struct Counted {
        machine: Arc<Fixed>,
        asked: std::sync::mpsc::Sender<(u64, bool)>,
        go: std::sync::mpsc::Receiver<()>,
        calls: usize,
    }
    impl crate::Relief for Counted {
        fn relieve(
            &mut self,
            short: &crate::Shortfall,
            _say: &mut dyn FnMut(&str),
            _stopped: &dyn Fn() -> bool,
        ) {
            let [disk] = &short.shorts[..] else {
                panic!("one shortfall: {short}");
            };
            let Short::Disk { reserved, .. } = *disk else {
                panic!("a disk shortfall alone: {short}");
            };
            self.calls += 1;
            let _ = self.asked.send((reserved, disk.unservable()));
            if self.calls == 1 {
                self.go.recv_timeout(MUST_HAPPEN).expect("the test's word");
            } else {
                self.machine.set(with_vol(vol(100, 49, 4)));
            }
        }
        fn holders(
            &mut self,
            _short: &crate::Shortfall,
            _stopped: &dyn Fn() -> bool,
        ) -> Option<String> {
            None
        }
    }
    let s = scratch("relief-counted");
    let m = Arc::new(on(vol(100, 24, 4)));
    let first = admitted(enter(&s, &m, Need::new("first").disk(10 * GIB, "/vol")), &m);
    let (asked, asks) = std::sync::mpsc::channel::<(u64, bool)>();
    let (go, word) = std::sync::mpsc::channel::<()>();
    let (ledger, theirs) = (s.ledger.clone(), Arc::clone(&m));
    let waiting = std::thread::spawn(move || {
        let mut relief = Counted {
            machine: Arc::clone(&theirs),
            asked,
            go: word,
            calls: 0,
        };
        crate::admit_relieved(
            &ledger,
            &*theirs,
            Need::new("verify").disk(30 * GIB, "/vol"),
            Admit::Wait,
            Pace {
                poll: Duration::from_millis(5),
                line_every: Duration::from_secs(60),
            },
            &mut relief,
            &mut |_| {},
        )
        .map(|c| c.record().need.what.clone())
    });
    assert_eq!(
        asks.recv_timeout(MUST_HAPPEN).expect("the first ask"),
        (10 * GIB, true)
    );
    drop(first);
    m.set(with_vol(vol(100, 14, 4)));
    let around = admitted(enter(&s, &m, Need::new("around").disk(5 * GIB, "/vol")), &m);
    m.set(with_vol(vol(100, 37, 4)));
    go.send(()).expect("the waiter's relief listens");
    assert_eq!(
        asks.recv_timeout(MUST_HAPPEN)
            .expect("asked again for the claim it did not count"),
        (5 * GIB, false)
    );
    let got = joined(waiting, "its admission once the second relief freed room");
    assert_eq!(got.expect("admitted").as_str(), "verify");
    drop(around);
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

/// A patience of `reserve`, `start` and `hold` seconds.
fn patience(reserve: u64, start: u64, hold: u64) -> Patience {
    Patience {
        reserve: Duration::from_secs(reserve),
        start: Duration::from_secs(start),
        hold: Duration::from_secs(hold),
    }
}

/// [`mac48`] at a load of 40, no core idle.
fn busy48() -> Fixed {
    let mut r = Reading::of_gib(48, 40, 14);
    r.load1 = Some(40.0);
    r.load5 = Some(40.0);
    Fixed::new(r)
}

/// A quiet claim's reservation, on its record: none until it has been next for its reserve
/// age, then while it waits, and for its hold once it starts, which it does by its bound; none
/// for a claim that started before it reserved, nor for one with no patience (the old rule) or
/// never next. Times are unix ms, next at 1 000 000.
#[test]
fn a_reservation_lasts_from_its_age_into_the_works_first_minutes() {
    let next = 1_000_000;
    let mut r = record(
        10,
        0,
        Need::new("measure")
            .quiet(28.0)
            .patience(patience(120, 600, 900)),
        None,
    );
    assert_eq!(r.reserves(next + 500_000), None, "never next");
    r.next_ms = Some(next);
    assert_eq!(
        r.reserves(next + 119_999),
        None,
        "younger than its reserve age"
    );
    assert_eq!(
        r.reserves(next + 120_000),
        Some(Duration::from_secs(1380)),
        "from its age: until its bound and its hold after"
    );
    assert_eq!(r.reserves(next + 1_499_999), Some(Duration::from_millis(1)));
    assert_eq!(r.reserves(next + 1_500_000), None, "spent, waiting or not");
    r.held = Some(crate::Held {
        admitted_ms: next + 300_000,
        cores: 0,
        alone: false,
    });
    assert_eq!(
        r.reserves(next + 400_000),
        Some(Duration::from_secs(800)),
        "started at 5 minutes: its hold from then"
    );
    assert_eq!(r.reserves(next + 1_200_000), None, "its hold is over");
    r.held = Some(crate::Held {
        admitted_ms: next + 60_000,
        cores: 0,
        alone: false,
    });
    assert_eq!(
        r.reserves(next + 400_000),
        None,
        "started before it reserved"
    );
    r.held = None;
    r.need.patience = None;
    assert_eq!(
        r.reserves(next + 400_000),
        None,
        "the old rule reserves nothing"
    );
}

/// What a reservation holds back: claimed work (memory, cores or disk) that asked after the
/// quiet claim, from a process of no kin to it. Work that asked first keeps its place; a quiet
/// claim, a need of nothing and the quiet claim's own kin pass.
#[test]
fn a_reservation_holds_back_later_claimed_work_outside_its_kin() {
    let next = 1_000_000;
    let mut quiet = record(
        10,
        500,
        Need::new("measure")
            .quiet(28.0)
            .patience(patience(120, 600, 900)),
        None,
    );
    quiet.next_ms = Some(next);
    let now = next + 200_000;
    let ahead = [&quiet];
    let none = |_: u32| false;
    let later = |need: Need| record(20, 900, need, None);
    let (r, left) =
        crate::admit::reserving(&later(Need::new("trust").mem_gib(8)), &ahead, &none, now)
            .expect("held back");
    assert_eq!((r.pid, left), (10, Duration::from_secs(1300)));
    for need in [
        Need::new("guest jobs").cores(8),
        Need::new("verify").disk(GIB, "/vol"),
    ] {
        assert!(crate::admit::reserving(&later(need), &ahead, &none, now).is_some());
    }
    let first = record(20, 100, Need::new("trust").mem_gib(8), None);
    assert!(
        crate::admit::reserving(&first, &ahead, &none, now).is_none(),
        "asked first"
    );
    for need in [Need::new("paint").quiet(28.0), Need::new("nothing")] {
        assert!(crate::admit::reserving(&later(need), &ahead, &none, now).is_none());
    }
    let kin = |pid: u32| pid == 10;
    assert!(
        crate::admit::reserving(&later(Need::new("trust").mem_gib(8)), &ahead, &kin, now).is_none()
    );
    assert!(
        crate::admit::reserving(
            &later(Need::new("trust").mem_gib(8)),
            &ahead,
            &none,
            next + 60_000
        )
        .is_none(),
        "not yet of age"
    );
}

/// Through the ledger: a quiet claim with a patience waits for the load, and once it has been
/// next for its bound (none here) it starts whatever the load and says so. Its patience counts
/// from when it is next: behind another quiet claim (a stand-in's) it waits however long that
/// holds, then starts only after a look of its own short of the load alone. Negative control:
/// the same claim with no patience, the old rule, still waits past the bound.
#[cfg(unix)]
#[test]
fn a_patient_quiet_claim_starts_at_its_bound_and_the_old_rule_still_waits() {
    let s = scratch("bound");
    let (mut child, holder) = standin_quiet(&s, 14.0);
    let m = busy48();
    let need = Need::new("measure")
        .cores(4)
        .quiet(14.0)
        .patience(patience(0, 0, 60));
    let (w, short) = waits(enter(&s, &m, need), &m);
    assert!(short.shorts.contains(&Short::Quiet {
        pid: holder,
        what: "stand-in A".to_owned(),
        held: true
    }));
    let (w, _) = waits(w, &m);
    assert_eq!(w.record().next_ms, None, "not next behind the stand-in");
    std::fs::write(s.dir.path().join("die"), "").expect("told to die");
    let _ = child.wait().expect("the stand-in's end");
    let (w, short) = waits(w, &m);
    assert_eq!(
        short.shorts,
        vec![Short::Load {
            load1: 40.0,
            max: 14.0
        }]
    );
    assert!(w.record().next_ms.is_some(), "next from this look");
    assert_eq!(
        s.ledger.list().expect("listed")[0].next_ms,
        w.record().next_ms,
        "other sessions read when"
    );
    let claim = admitted(w, &m);
    let q = claim.quiet().expect("how it started");
    assert!(
        q.at_bound && q.load1 == Some(40.0) && (q.max - 14.0).abs() < 1e-9,
        "{q:?}"
    );
    names(&q.to_string(), &["at its bound", "load 40.0 > 14.0"]);
    assert_eq!(
        claim.cores(),
        4,
        "its share at load 40 (CORES-GRANT): 14 / (1 + 40/14), rounded, all it asked"
    );
    assert_eq!(
        s.ledger.list().expect("listed")[0],
        claim.record().clone(),
        "its admission reads back past its next line"
    );
    drop(claim);
    let mut old = enter(&s, &m, Need::new("old").quiet(14.0));
    for _ in 0..3 {
        let (w, short) = waits(old, &m);
        assert_eq!(short.shorts.len(), 1, "{short}");
        assert_eq!(w.record().next_ms, None);
        old = w;
    }
}

/// The quiet waiter a reservation's line names, and its own line: once next, it says when it
/// reserves the machine and when it starts whatever the load.
#[test]
fn a_patient_waiters_line_says_when_it_reserves_and_starts() {
    let s = scratch("line");
    let m = busy48();
    let need = Need::new("measure")
        .quiet(14.0)
        .patience(patience(120, 600, 900));
    let (w, short) = waits(enter(&s, &m, need), &m);
    let line = w.line(&short);
    names(
        &line,
        &[
            "load 40.0 > 14.0",
            "it reserves the machine in 2m00s",
            "starts whatever the load in 10m00s",
        ],
    );
}

/// Set, the patient stand-in's ledger is under this directory.
#[cfg(unix)]
const PATIENT: &str = "ATERM_CAPACITY_TEST_PATIENT";
/// Set, the patient stand-in's quiet claim has no patience: the old rule.
#[cfg(unix)]
const PATIENT_OLD: &str = "ATERM_CAPACITY_TEST_PATIENT_OLD";
#[cfg(unix)]
const PATIENT_TEST: &str = "tests::standin_waits_patiently_for_a_quiet_machine";

/// Run by [`claimed_work_waits_behind_a_reserving_quiet_waiter_naming_it`] in a process of its
/// own: a quiet claim of bound 14 on a machine at load 40, reserving from its first look and
/// starting only after ten hang detectors (unless [`PATIENT_OLD`] says it has no patience), in
/// the ledger under the directory [`PATIENT`] names. It looks once, writes `waiting` with its
/// wait line, and naps until told to stop: no later look takes the ledger's lock while the test
/// looks. A test that ended without telling it leaves no stop file to read, so it also ends,
/// failing, once its parent is gone or after two hang detectors.
#[cfg(unix)]
#[test]
fn standin_waits_patiently_for_a_quiet_machine() {
    let Some(dir) = std::env::var_os(PATIENT).map(PathBuf::from) else {
        return;
    };
    let mut need = Need::new("timed stand-in").quiet(14.0);
    if std::env::var_os(PATIENT_OLD).is_none() {
        need = need.patience(Patience {
            reserve: Duration::ZERO,
            start: MUST_HAPPEN * 10,
            hold: MUST_HAPPEN,
        });
    }
    let waiting = dir.join("waiting");
    let stop = dir.join("stop");
    let parent = std::os::unix::process::parent_id();
    let born = std::time::Instant::now();
    let ended = admit_until(
        &Ledger::at(dir.join("ledger")),
        &busy48(),
        need,
        Admit::Wait,
        Pace {
            poll: MUST_HAPPEN * 2,
            line_every: Duration::from_secs(60),
        },
        &mut |line| {
            if !waiting.exists() {
                // Written aside and renamed in, so the test never reads the file between its
                // creation and its line.
                let part = waiting.with_extension("part");
                std::fs::write(&part, line).expect("said waiting");
                std::fs::rename(&part, &waiting).expect("said waiting");
            }
        },
        &|| {
            stop.exists()
                || std::os::unix::process::parent_id() != parent
                || born.elapsed() >= MUST_HAPPEN * 2
        },
    );
    assert!(stop.exists(), "ended untold: {ended:?}");
    assert!(
        matches!(ended, Err(Error::Stopped { signal: None, .. })),
        "it waited until told to stop: {ended:?}"
    );
}

/// A running [`standin_waits_patiently_for_a_quiet_machine`]: told to stop and waited for when
/// dropped still running (a test that failed before [`Patient::stop`]), so none outlives its test.
#[cfg(unix)]
struct Patient {
    child: aterm_uds::spawnfd::Child,
    pid: u32,
    dir: PathBuf,
}

#[cfg(unix)]
impl Patient {
    /// End it, which it does of itself once told, and expect it to have waited until then.
    fn stop(mut self) {
        std::fs::write(self.dir.join("stop"), "").expect("told to stop");
        let status = self.child.wait().expect("the stand-in's end");
        assert!(status.success(), "the stand-in waited until told: {status}");
        let _ = std::fs::remove_file(self.dir.join("stop"));
        let _ = std::fs::remove_file(self.dir.join("waiting"));
    }
}

#[cfg(unix)]
impl Drop for Patient {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = std::fs::write(self.dir.join("stop"), "");
            let _ = self.child.wait();
        }
    }
}

/// Start [`standin_waits_patiently_for_a_quiet_machine`] on `s`'s ledger, with no patience when
/// `old`, and wait for its wait line.
#[cfg(unix)]
fn patient(s: &Scratch, old: bool) -> Patient {
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
    if old {
        cmd.env(PATIENT_OLD, "1");
    }
    let child = cmd
        .args(["--exact", PATIENT_TEST, "--test-threads=1", "--nocapture"])
        .env(PATIENT, s.dir.path())
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the patient stand-in");
    let pid = child.id();
    let patient = Patient {
        child,
        pid,
        dir: s.dir.path().to_path_buf(),
    };
    let waiting = s.dir.path().join("waiting");
    let line = until_within(MUST_HAPPEN, "the stand-in to wait", || {
        std::fs::read_to_string(&waiting).ok()
    });
    names(&line, &["timed stand-in", "load 40.0 > 14.0"]);
    patient
}

/// A claimed heavy stand-in for a Trust check that asks while another process's quiet claim
/// reserves the machine waits, naming that quiet waiter, however much memory is free; the same
/// claim from a descendant of the quiet waiter (its own timed work, which no claim of its
/// ancestors holds back) is admitted, as is one from its ancestor (the run its timed work is part
/// of, which the reservation spares). Negative control: behind the same quiet waiter with no
/// patience, the old rule, it is admitted at once.
#[cfg(unix)]
#[test]
fn claimed_work_waits_behind_a_reserving_quiet_waiter_naming_it() {
    let s = scratch("reserving");
    let m = mac48();
    let me = std::process::id();
    let p = |pid, ppid| Proc {
        pid,
        ppid,
        ..Proc::default()
    };
    let timed = patient(&s, false);
    let quiet = timed.pid;
    m.set_procs(Some(Procs::of(vec![p(quiet, 1), p(me, 1)])));
    let (w, short) = waits(enter(&s, &m, Need::new("trust check").mem_gib(4)), &m);
    match &short.shorts[..] {
        [
            Short::Reserved {
                pid,
                what,
                held: false,
                left,
            },
        ] => {
            assert_eq!((*pid, what.as_str()), (quiet, "timed stand-in"));
            assert!(*left > MUST_HAPPEN * 9, "{left:?}");
        }
        other => panic!("expected the reservation alone, got {other:?}"),
    }
    assert_eq!(
        short.holders.iter().map(|h| h.pid).collect::<Vec<_>>(),
        [quiet]
    );
    names(
        &w.line(&short),
        &[
            "capacity: trust check waits",
            &format!("pid {quiet} (\"timed stand-in\") reserves the machine"),
            "while it waits for a quiet machine",
            &format!("pid {quiet} waits ahead for the quiet machine"),
        ],
    );
    m.set_procs(Some(Procs::of(vec![p(quiet, 1), p(me, quiet)])));
    let mine = admitted(w, &m);
    drop(mine);
    m.set_procs(Some(Procs::of(vec![p(me, 1), p(quiet, me)])));
    let ancestor = admitted(enter(&s, &m, Need::new("verify").disk(GIB, "/")), &m);
    drop(ancestor);
    timed.stop();
    let old = patient(&s, true);
    m.set_procs(None);
    let claim = admitted(enter(&s, &m, Need::new("trust check").mem_gib(4)), &m);
    drop(claim);
    old.stop();
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

/// A machine of 14 cores at a 5-minute (and 1-minute) load of `load`, with room for any memory.
fn cores14_at(load: f64) -> Reading {
    Reading {
        load1: Some(load),
        load5: Some(load),
        ..Reading::of_gib(48, 40, 14)
    }
}

/// CORES-GRANT's acceptance: cores are granted, never waited for, as a share of the machine
/// weighed against the work that runs on it. On 14 cores at a 5-minute load of 60, two claims
/// that each ask for lima's 8 are each granted the stated share, the cores over the parties: the
/// two claims and the 60 of load no claim holds, in machines' worth. CAP-1's first rule (the idle
/// cores) granted each 1. Alone at that load a claim gets 3; on an idle machine, the whole
/// machine.
#[test]
fn two_claims_at_load_60_on_14_cores_each_get_their_share() {
    let s = scratch("share");
    let m = Fixed::new(cores14_at(60.0));
    let stated = (14.0_f64 / (2.0 + 60.0 / 14.0)).round() as u32;
    assert_eq!(stated, 2, "the share this test states");
    let a = enter(&s, &m, Need::new("lima guest").cores(8));
    let b = enter(&s, &m, Need::new("windows guest").cores(8));
    let a = admitted(a, &m);
    assert_eq!(
        a.cores(),
        stated,
        "the first, beside the second not yet admitted"
    );
    let b = admitted(b, &m);
    assert_eq!(
        b.cores(),
        stated,
        "the second, beside the first's jobs held"
    );
    drop((a, b));
    let alone = admitted(enter(&s, &m, Need::new("alone").cores(8)), &m);
    assert_eq!(alone.cores(), 3, "14 / (1 + 60/14), rounded");
    drop(alone);
    m.set(cores14_at(0.0));
    let whole = admitted(enter(&s, &m, Need::new("whole").cores(14)), &m);
    assert_eq!(whole.cores(), 14, "the whole machine, at load 0");
}

/// A claim's share is of the room, the cores less what the held claims were granted: on an
/// idle 14-core machine lima's 8 leave Windows its 6, and a third claim gets 1. A held claim's
/// jobs leave the unclaimed load as they reach the 5-minute load, and a claim of cores not yet
/// admitted, whatever it waits for, is a party. A grant is 1 only where the work really is that
/// contended (a load of about 117 alone), never more than asked, and 0 when none was asked; a
/// load that reads as no number is the load unread.
#[test]
fn a_grant_is_a_share_of_the_room_the_held_claims_leave() {
    fn cores_of(
        need: &Need,
        mine: (u64, u32, u64),
        reading: &Reading,
        others: &[Other<'_>],
        now: u64,
    ) -> Result<u32, crate::Shortfall> {
        decide(need, mine, reading, others, now).map(|g| g.cores)
    }
    let idle = cores14_at(0.0);
    let lima = record(10, 1_000, Need::new("lima").cores(8), Some(8));
    let windows = record(11, 1_000, Need::new("windows").cores(6), Some(6));
    let other = |r| Other {
        record: r,
        used: TreeUse::default(),
        credited: TreeUse::default(),
    };
    let want = |n| Need::new("guest").cores(n);
    let held = [other(&lima)];
    assert_eq!(cores_of(&want(6), (1, 1, 0), &idle, &held, 2_000), Ok(6));
    let both = [other(&lima), other(&windows)];
    assert_eq!(
        cores_of(&want(8), (1, 1, 0), &idle, &both, 2_000),
        Ok(1),
        "the room is spent"
    );

    let six = record(12, 0, Need::new("held").cores(6), Some(6));
    let held = [other(&six)];
    let at20 = cores14_at(20.0);
    assert_eq!(
        cores_of(&want(8), (1, 1, 0), &at20, &held, 1),
        Ok(3),
        "8 of room over 1 + 20/14: its jobs not yet in the load"
    );
    assert_eq!(
        cores_of(&want(8), (1, 1, 0), &at20, &held, 3_600_000),
        Ok(4),
        "8 over 1 + 14/14: its 6 jobs are in the load now"
    );

    let waiting = record(13, 0, Need::new("waits").cores(8), None);
    assert_eq!(
        cores_of(&want(14), (1, 1, 0), &idle, &[other(&waiting)], 1),
        Ok(7),
        "half, beside a claim of cores not yet admitted"
    );
    assert_eq!(
        cores_of(&want(8), (1, 1, 0), &cores14_at(11.0), &[], 1),
        Ok(8)
    );
    assert_eq!(
        cores_of(&want(8), (1, 1, 0), &cores14_at(116.0), &[], 1),
        Ok(2)
    );
    assert_eq!(
        cores_of(&want(8), (1, 1, 0), &cores14_at(118.0), &[], 1),
        Ok(1)
    );
    let unread = Reading {
        load5: None,
        ..idle.clone()
    };
    assert_eq!(cores_of(&want(8), (1, 1, 0), &unread, &[], 1), Ok(8));
    let nan = Reading {
        load5: Some(f64::NAN),
        ..idle.clone()
    };
    assert_eq!(cores_of(&want(8), (1, 1, 0), &nan, &[], 1), Ok(8));
    assert_eq!(
        cores_of(&want(8), (1, 1, 0), &cores14_at(f64::INFINITY), &[], 1),
        Ok(1)
    );
    assert_eq!(
        cores_of(&Need::new("none"), (1, 1, 0), &idle, &[], 1),
        Ok(0)
    );
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

/// A waiter whose claim file is removed while it waits (the ledger's directory cleaned) is in no
/// other session's queue and can never be named a claim: its wait ends at its next look, naming
/// the removal. Negative control: with its file in place, the same wait is still waiting three
/// lines on, when the test ends it. Unix only: on Windows a file a handle holds open may keep its
/// name after it is removed.
#[cfg(unix)]
#[test]
fn a_waiter_whose_claim_file_is_removed_ends_its_wait() {
    for remove in [true, false] {
        let s = scratch("removed");
        let m = mac48();
        let a = admitted(enter(&s, &m, Need::new("A").mem_gib(30)), &m);
        let lines = std::cell::Cell::new(0_u32);
        let ended = admit_until(
            &s.ledger,
            &m,
            Need::new("B").mem_gib(20),
            Admit::Wait,
            Pace {
                poll: Duration::from_millis(10),
                line_every: Duration::ZERO,
            },
            &mut |_| {
                lines.set(lines.get() + 1);
                if remove {
                    for e in s.ledger.dir().read_dir().expect("the ledger").flatten() {
                        if e.file_name().to_string_lossy().ends_with(".wait") {
                            std::fs::remove_file(e.path()).expect("B's file removed");
                        }
                    }
                }
            },
            &|| lines.get() >= 3,
        );
        match (remove, ended) {
            (true, Err(Error::Ledger(e))) => {
                names(&e.to_string(), &[".wait", "was removed while it waited"]);
                assert_eq!(lines.get(), 1, "it ended at the look after");
            }
            (false, Err(Error::Stopped { signal: None, .. })) => {}
            (_, other) => panic!("removed {remove}: {other:?}"),
        }
        drop(a);
    }
}

/// A claim's grant is written once: an admission whose rename fails (here the ledger's directory
/// made unwritable; in use, the file removed meanwhile) and is tried again writes no second grant
/// and keeps the first, and the claim then reads back with it.
#[cfg(unix)]
#[test]
fn a_retried_admission_writes_its_grant_once() {
    use std::os::unix::fs::PermissionsExt as _;
    let s = scratch("grant");
    let (mut record, mut laid) = s
        .ledger
        .lay(&Need::new("A").mem_gib(1), 1_000)
        .expect("laid");
    let first = crate::Held {
        admitted_ms: 2_000,
        cores: 2,
        alone: false,
    };
    let dir = s.ledger.dir().to_path_buf();
    let mode = |m| std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(m));
    mode(0o500).expect("the ledger made unwritable");
    for admitted_ms in [2_000, 3_000] {
        let held = crate::Held {
            admitted_ms,
            cores: 2,
            alone: false,
        };
        let refused = s.ledger.admit(&mut laid, &mut record, held);
        assert!(refused.is_err(), "the rename was refused");
    }
    mode(0o700).expect("the ledger made writable");
    let text = std::fs::read_to_string(laid.path()).expect("the claim file");
    assert_eq!(text.matches("admitted_ms").count(), 1, "{text}");
    s.ledger
        .admit(
            &mut laid,
            &mut record,
            crate::Held {
                admitted_ms: 4_000,
                cores: 3,
                alone: false,
            },
        )
        .expect("admitted");
    assert_eq!(record.held, Some(first));
    assert_eq!(s.ledger.list().expect("listed"), [record]);
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
            .quiet(4.0)
            .patience(patience(1, 2, 3)),
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

/// A parallel-build machine's large volume must reach its automatic pressure floor before
/// it is 97% full, even before two days of room history exist. The old 32 GiB cap classified
/// this 8 TB volume as healthy with 262 GB left. Explicit owner knobs keep their own bounds;
/// no owner setting is silently raised.
#[test]
fn a_large_build_volume_reaches_pressure_before_ninety_seven_percent_full() {
    let total = 7_998_000_000_000;
    let available = 261_600_000_000;
    let watch = floor::Watch::default();
    let lines = watch.lines("container:large-build-volume", total, 0);
    assert!(
        available < lines.floor,
        "not enough room for parallel builds: {lines:?}"
    );
    assert!(
        available < lines.warn,
        "the volume must warn before its floor"
    );
    let written = floor::Knobs::from_toml("[disk]\nauto_free_gib = 20\nwarn_free_gib = 40\n");
    let explicit = floor::lines(written, Some(total), Some(200 * GIB));
    assert_eq!((explicit.floor, explicit.warn), (20 * GIB, 40 * GIB));
    let off = floor::Knobs::from_toml("[disk]\nauto_free_gib = 0\n");
    assert_eq!(floor::lines(off, Some(total), None).floor, 0);
}

/// The volume's floor is the window's: 5% of the volume up to 256 GiB, or 10 GiB held to 10% of a
/// small volume, raised to the room's least.
#[test]
fn the_floor_follows_the_volume() {
    assert_eq!(floor::default_floor(1858 * GIB), 93 * GIB);
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
        floor::lines(floor::Knobs::default(), Some(1858 * GIB), Some(120 * GIB)).floor,
        120 * GIB,
        "an unwritten one rises to the room"
    );
}

/// The reading's floor is the one the window's disk watch holds: on a 1858 GiB volume
/// with a measured room of 75529 MiB the floor is the volume's larger 93 GiB share; a
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
    assert_eq!(watch.lines("container:disk3", total, now).floor, 93 * GIB);
    assert_eq!(
        watch.lines("container:disk9", total, now).floor,
        93 * GIB,
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
    let admitted_c = joined(waiting, "C's admission").expect("admitted");
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
    Fixed::new(with_vol(v))
}

/// The reading of [`on`]`(v)`.
fn with_vol(v: Volume) -> Reading {
    let mut r = Reading::of_gib(48, 40, 14);
    r.volume = Some(v);
    r
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
    let (claim, asked) = joined(waiting, "the waiter's admission once room came");
    assert_eq!(claim.expect("admitted once room came").as_str(), "verify");
    assert_eq!(asked, 1, "relieved once");
}

/// The tag of the directories the footprint tests' runs claim on ([`crate::Footprint::dirs`]).
const HERE: u64 = 0x5eed_0001;

/// What the next run claims: until a run of its kind is measured, the cold budget less what
/// its directories hold already (nothing where they hold it all), then the most any kept run
/// grew its directories by, or held at its peak beyond what they hold now, that at most the
/// cold budget. The file keeps the newest [`crate::footprint::KEPT`] runs and reads back as
/// written.
#[test]
fn a_run_claims_what_the_last_runs_wrote() {
    let mut f = crate::Footprints::default();
    let empty = f.need(0, 27 * GIB, HERE);
    assert_eq!((empty.bytes, empty.runs), (27 * GIB, 0));
    names(&empty.to_string(), &["27 GiB", "cold budget"]);
    let cold = f.need(5 * GIB, 27 * GIB, HERE);
    assert_eq!((cold.bytes, cold.runs), (22 * GIB, 0), "less what is there");
    names(&cold.to_string(), &["22 GiB", "27 GiB", "5 GiB"]);
    assert_eq!(
        f.need(35 * GIB, 27 * GIB, HERE).bytes,
        0,
        "warm, with no record: nothing to claim"
    );
    f.push(crate::Footprint {
        peak: 40 * GIB,
        grew: 3 * GIB,
        at_ms: 1,
        dirs: None,
    });
    assert_eq!(
        f.need(38 * GIB, 27 * GIB, HERE).bytes,
        3 * GIB,
        "warm: its growth"
    );
    assert_eq!(
        f.need(4 * GIB, 72 * GIB, HERE).bytes,
        36 * GIB,
        "reclaimed since: its peak again"
    );
    let capped = f.need(4 * GIB, 27 * GIB, HERE);
    assert_eq!(
        (capped.bytes, capped.capped),
        (27 * GIB, true),
        "reclaimed since, past what a run writes into empty directories: that"
    );
    f.push(crate::Footprint {
        peak: 41 * GIB,
        grew: GIB,
        at_ms: 2,
        dirs: None,
    });
    assert_eq!(
        f.need(40 * GIB, 27 * GIB, HERE).bytes,
        3 * GIB,
        "a light run does not hide a heavier one"
    );
    let mut light = crate::Footprints::default();
    light.push(crate::Footprint {
        peak: 24 << 20,
        grew: 24 << 20,
        at_ms: 1,
        dirs: None,
    });
    names(
        &light.need(24 << 20, 27 * GIB, HERE).to_string(),
        &["24 MiB", "1 run(s)"],
    );
    for at_ms in 3..20 {
        f.push(crate::Footprint {
            peak: 41 * GIB,
            grew: GIB,
            at_ms,
            dirs: None,
        });
    }
    assert_eq!(f.runs().len(), crate::footprint::KEPT);
    assert_eq!(
        f.need(41 * GIB, 27 * GIB, HERE).bytes,
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

/// THE LIVE RECORD OF 2026-10-27, AS A TABLE: a worktree's `<target>/verify/footprint.json`
/// kept 8 runs. The first 4 were measured before the build cache moved roots, on a build
/// directory grown to 283.9-287.5 GiB of stale builds; the last 4 after, on the new root's
/// 20.2-24.3 GiB. No run grew its directories by more than 0.2 GiB. They held 24.2 GiB, and
/// verify's cold budget is 72 GiB. A peak beyond what they hold now claims no more than that
/// budget, what a run writes into empty directories, so the record claims 72 GiB, said as the
/// budget (it can never be admitted by waiting on a volume whose room is less); the 4 runs on
/// the new root alone claim 208 MiB. Negative control: without the cap it claims 263.3 GiB, the
/// oldest peaks less what the directories hold now.
#[test]
fn a_peak_of_more_than_one_run_writes_claims_the_cold_budget() {
    const LIVE: &str = "{\"runs\":[\
        {\"at_ms\":1793120044229,\"grew\":0,\"peak\":304814723072},\
        {\"at_ms\":1793121031594,\"grew\":0,\"peak\":304814899200},\
        {\"at_ms\":1793125433232,\"grew\":77213696,\"peak\":308589371392},\
        {\"at_ms\":1793126135718,\"grew\":58859520,\"peak\":308669722624},\
        {\"at_ms\":1793134312916,\"grew\":2838528,\"peak\":21710241792},\
        {\"at_ms\":1793134664810,\"grew\":95920128,\"peak\":21805182976},\
        {\"at_ms\":1793139575534,\"grew\":0,\"peak\":25914556416},\
        {\"at_ms\":1793144355648,\"grew\":217149440,\"peak\":26130784256}],\"schema\":1}";
    let (holds, cold) = (tenths(242), 72 * GIB);
    let live = crate::Footprints::decode(LIVE);
    assert_eq!(live.runs().len(), 8, "the record reads back");
    let claim = live.need(holds, cold, HERE);
    assert_eq!(
        (claim.bytes, claim.runs, claim.capped),
        (cold, 8, true),
        "{claim}"
    );
    names(&claim.to_string(), &["72 GiB", "24.2 GiB"]);
    let mut moved = crate::Footprints::default();
    for run in &live.runs()[4..] {
        moved.push(*run);
    }
    let after = moved.need(holds, cold, HERE);
    assert_eq!((after.bytes, after.capped), (217_149_440, false), "{after}");
    names(&after.to_string(), &["208 MiB", "24.2 GiB"]);
}

/// A RUN MEASURED ON OTHER DIRECTORIES IS NOT COUNTED: each run is tagged with the directories it
/// measured ([`crate::Footprint::dirs`]), so a record whose 2 oldest runs measured other
/// directories (a build root before the cache moved, peaking at 280 GiB) and whose 2 newest
/// measured these (24.2 GiB held, peaks of 26 and 25 GiB) claims what the 2 newest alone claim,
/// 1.8 GiB from 2 runs, not the 72 GiB cold budget that caps the old peaks; where every run
/// measured other directories, it claims as no record does, the budget less what they hold, and
/// from emptied directories the whole budget, not capped (no run of theirs held more). A record
/// kept before runs were tagged (no `dirs` key) claims as it always did, its old peaks capped.
/// Each run's tag reads back as written, a full 64-bit digest included, and an untagged run is
/// written without the key, which older builds ignore; a meter's run carries the tag it was
/// started with. Negative controls: with the tag ignored, the mixed record claims the capped
/// 72 GiB from 4 runs; with the cap judged on every run kept, the emptied claim is capped.
#[test]
fn a_run_measured_on_other_directories_is_not_counted() {
    const THERE: u64 = u64::MAX - 1;
    let (holds, cold) = (tenths(242), 72 * GIB);
    let run = |peak: u64, grew: u64, dirs: Option<u64>| crate::Footprint {
        peak,
        grew,
        at_ms: 1,
        dirs,
    };
    let of = |runs: &[crate::Footprint]| {
        let mut f = crate::Footprints::default();
        for r in runs {
            f.push(*r);
        }
        f
    };
    let there = [
        run(280 * GIB, 0, Some(THERE)),
        run(tenths(2805), 0, Some(THERE)),
    ];
    let here = [
        run(26 * GIB, 200 << 20, Some(HERE)),
        run(25 * GIB, 0, Some(HERE)),
    ];
    let mixed = of(&[there[0], there[1], here[0], here[1]]);
    let claim = mixed.need(holds, cold, HERE);
    assert_eq!(claim, of(&here).need(holds, cold, HERE), "{claim}");
    assert_eq!(
        (claim.bytes, claim.runs, claim.capped),
        (26 * GIB - holds, 2, false),
        "{claim}"
    );
    let elsewhere = of(&there).need(holds, cold, HERE);
    assert_eq!(
        (elsewhere.bytes, elsewhere.runs, elsewhere.capped),
        (cold - holds, 0, false),
        "{elsewhere}"
    );
    names(&elsewhere.to_string(), &["47.8 GiB", "72 GiB", "24.2 GiB"]);
    let emptied = of(&there).need(0, cold, HERE);
    assert_eq!(
        (emptied.bytes, emptied.runs, emptied.capped),
        (cold, 0, false),
        "{emptied}"
    );

    const OLD: &str = "{\"runs\":[\
        {\"at_ms\":1793120044229,\"grew\":0,\"peak\":304814723072},\
        {\"at_ms\":1793144355648,\"grew\":217149440,\"peak\":26130784256}],\"schema\":1}";
    let old = crate::Footprints::decode(OLD);
    assert!(old.runs().iter().all(|r| r.dirs.is_none()), "{old:?}");
    let today = old.need(holds, cold, HERE);
    assert_eq!(
        (today.bytes, today.runs, today.capped),
        (cold, 2, true),
        "{today}"
    );

    let mut kept = mixed.clone();
    kept.push(run(GIB, GIB, None));
    let text = kept.encode();
    assert_eq!(crate::Footprints::decode(&text), kept, "{text}");
    assert_eq!(text.matches("\"dirs\"").count(), 4, "{text}");

    let s = scratch("meter-tag");
    let meter = crate::Meter::start(vec![s.dir.path().to_path_buf()], HERE, 0, None);
    assert_eq!(meter.finish().dirs, Some(HERE));
}

/// What a run grew its directories by is believed whatever its size, and a peak within the
/// cold budget is written again whole: a run that grew empty directories by 90 GiB, more than
/// the 72 GiB budget, claims 90 GiB, warm or emptied since, beside a stale peak too; directories
/// emptied since a 40 GiB peak claim 40 GiB; with no run kept, the budget less what they hold.
/// Only a peak more than the budget beyond what they hold is capped, not one exactly that. The
/// whole of what a run wrote, its growth with what it rewrote, is held to the budget: one that
/// grew warm 50 GiB directories by 30 GiB claims 72 GiB once they are emptied, not its 80 GiB
/// peak, nor the budget with its growth on top (a run from empty writes its growth within the
/// budget). Negative controls: with the growth capped too, the warm 90 GiB run claims 72 GiB;
/// with only what the run found capped (`grew + min(found - holds, cold)`), the 50 GiB row
/// claims 80 GiB.
#[test]
fn a_runs_growth_is_believed_and_a_peak_within_the_cold_budget_is_written_again() {
    let cold = 72 * GIB;
    let run = |peak: u64, grew: u64| crate::Footprint {
        peak,
        grew,
        at_ms: 1,
        dirs: None,
    };
    let table: [(&str, Vec<crate::Footprint>, u64, u64, bool); 9] = [
        (
            "grew 90 GiB from empty, warm",
            vec![run(90 * GIB, 90 * GIB)],
            90 * GIB,
            90 * GIB,
            false,
        ),
        (
            "grew 90 GiB from empty, emptied since",
            vec![run(90 * GIB, 90 * GIB)],
            0,
            90 * GIB,
            false,
        ),
        (
            "grew 90 GiB, beside a stale 300 GiB peak",
            vec![run(300 * GIB, GIB), run(90 * GIB, 90 * GIB)],
            24 * GIB,
            90 * GIB,
            false,
        ),
        (
            "grew 30 GiB on 50 GiB warm, emptied since",
            vec![run(80 * GIB, 30 * GIB)],
            0,
            cold,
            true,
        ),
        (
            "emptied since a 40 GiB peak",
            vec![run(40 * GIB, 3 * GIB)],
            0,
            40 * GIB,
            false,
        ),
        ("no run kept", vec![], 5 * GIB, 67 * GIB, false),
        ("no run kept, warm", vec![], 80 * GIB, 0, false),
        (
            "a peak exactly the budget beyond what they hold",
            vec![run(96 * GIB, GIB)],
            24 * GIB,
            cold,
            false,
        ),
        (
            "a byte more",
            vec![run(96 * GIB + 1, GIB)],
            24 * GIB,
            cold,
            true,
        ),
    ];
    for (what, runs, holds, claims, capped) in table {
        let mut f = crate::Footprints::default();
        for r in &runs {
            f.push(*r);
        }
        let e = f.need(holds, cold, HERE);
        assert_eq!(
            (e.bytes, e.runs, e.capped),
            (claims, runs.len(), capped),
            "{what}: {e}"
        );
        names(&e.to_string(), &[crate::size(claims).as_str()]);
    }
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

/// HONEST ROOM, PRESSURE (VERIFY-MEM): while the kernel's memory pressure is critical (about to
/// end processes) a claim of memory waits, whatever reads as available, and is admitted once the
/// pressure eases; it is never refused for it. At `warn`, and at `normal`, it is admitted by what
/// is available; a claim of no memory is not held. NEGATIVE CONTROL: the same claim on the same
/// machine at `normal` is admitted at once.
#[test]
fn a_memory_claim_waits_while_memory_pressure_is_critical() {
    use aterm_sysprobe::MemoryLevel;
    let room = Reading::of_gib(48, 40, 14);
    let b = Need::new("B").mem_gib(4);
    for level in [MemoryLevel::Normal, MemoryLevel::Warn] {
        assert!(
            decide(&b, (1, 1, 0), &room.clone().at_pressure(level), &[], 1).is_ok(),
            "{level:?}"
        );
    }
    let critical = room.clone().at_pressure(MemoryLevel::Critical);
    let short = decide(&b, (1, 1, 0), &critical, &[], 1).expect_err("critical");
    assert_eq!(short.shorts, vec![Short::Pressure]);
    names(&short.to_string(), &["memory pressure is critical"]);
    assert!(decide(&Need::new("cores").cores(2), (1, 1, 0), &critical, &[], 1).is_ok());
    assert_eq!(never(&b, &critical), None, "never refused for it");

    let s = scratch("pressure");
    let m = Fixed::new(critical);
    let (w, short) = waits(enter(&s, &m, b), &m);
    names(&short.to_string(), &["memory pressure is critical"]);
    m.set(room.at_pressure(MemoryLevel::Warn));
    drop(admitted(w, &m));
}

/// HONEST ROOM, RESIDENT (VERIFY-MEM): a claim's tree is credited the memory it holds, its
/// footprint or, where less, its resident size: a 10 GiB footprint in 2 GiB of resident pages
/// leaves its 12 GiB claim reserving 10 GiB, since the 8 GiB the kernel compressed or swapped is
/// room the claim takes back as its tree touches it. A resident size at or above the footprint
/// (file-backed pages it maps), or one unread, credits the footprint. NEGATIVE CONTROL: crediting
/// the footprint whole, as before, reserves 2 GiB.
#[test]
fn a_tree_is_credited_what_it_holds_in_memory_not_what_was_swapped_out() {
    let held = record(10, 0, Need::new("A").mem_gib(12), Some(1));
    let reserved = |footprint: u64, resident: Option<u64>| {
        let p = aterm_sysprobe::ProcessReading {
            pid: 10,
            ppid: 1,
            footprint: Some(footprint),
            resident,
            ..aterm_sysprobe::ProcessReading::default()
        };
        let trees = Procs::of(vec![Proc::of_reading(&p)]).trees(&[10]);
        crate::reserved(std::slice::from_ref(&held), Some(&trees), &[])[0].mem
    };
    assert_eq!(reserved(10 * GIB, Some(2 * GIB)), 10 * GIB);
    assert_eq!(reserved(10 * GIB, Some(10 * GIB)), 2 * GIB);
    assert_eq!(reserved(10 * GIB, Some(30 * GIB)), 2 * GIB, "mapped files");
    assert_eq!(reserved(10 * GIB, None), 2 * GIB, "unread");
}

/// ALONE, DECIDED (VERIFY-MEM's binding rule): on a 24 GiB Mac with 8 GiB available, a 12 GiB
/// claim that no other claim's memory keeps out (nothing held, nothing waiting ahead: the 4 GiB
/// it is short of is other work on this Mac) is admitted at once, alone, and says so; a waiting
/// claim of its own process ahead of it, or another process's that asked later, does not keep
/// it out. Another process's claim that holds memory, or waits ahead for it, does: the 12 GiB
/// claim waits for it, naming it, first in, first out. A need with a margin (a VM's boot) is
/// never admitted alone, and one that fits is admitted beside the rest, not alone. NEGATIVE
/// CONTROL: the same 12 GiB claim beside another process's held 1 GiB claim waits.
#[test]
fn a_claim_short_only_of_memory_no_claim_holds_is_admitted_alone() {
    let r = Reading::of_gib(24, 8, 14);
    let big = Need::new("verify test compile").mem_gib(12);
    let mine = (5, 1, 0);
    let alone = decide(&big, mine, &r, &[], 1)
        .expect("admitted")
        .alone
        .expect("alone");
    assert_eq!(
        alone,
        crate::Alone {
            need: 12 * GIB,
            available: Some(8 * GIB),
            asked: false
        }
    );
    names(
        &alone.to_string(),
        &["admitted alone", "12 GiB", "8 GiB", "other work"],
    );
    let own = record(1, 1, Need::new("verify tippy lint").mem_gib(12), None);
    let later = record(22, 9, Need::new("a later claim").mem_gib(12), None);
    for (ahead, why) in [(&own, "its own process's waiter"), (&later, "a later one")] {
        assert!(
            decide(&big, mine, &r, &[plain(ahead)], 1).is_ok_and(|g| g.alone.is_some()),
            "{why} keeps nothing out"
        );
    }
    let held = record(20, 1, Need::new("trust check").mem_gib(1), Some(1));
    let short = decide(&big, mine, &r, &[plain(&held)], 1).expect_err("waits for the holder");
    assert!(
        matches!(short.shorts[..], [Short::Memory { .. }]),
        "{short}"
    );
    assert_eq!(
        short.holders.iter().map(|h| h.pid).collect::<Vec<_>>(),
        [20]
    );
    let ahead = record(21, 1, Need::new("another verify").mem_gib(12), None);
    let short = decide(&big, mine, &r, &[plain(&ahead)], 1).expect_err("first in, first out");
    names(&short.to_string(), &["pid 21", "waits ahead", "12 GiB"]);
    let boot = Need::new("lab boot").mem_gib(12).margin(2 * GIB);
    assert!(
        matches!(
            decide(&boot, mine, &r, &[], 1)
                .expect_err("room beside it")
                .shorts[..],
            [Short::Memory { .. }]
        ),
        "a margin asks for room"
    );
    assert_eq!(
        decide(&Need::new("small").mem_gib(4), mine, &r, &[], 1).map(|g| g.alone),
        Ok(None)
    );
    let wired = Reading {
        wired: Some(12 * GIB),
        ..r
    };
    assert!(
        matches!(
            decide(&big, mine, &wired, &[], 1)
                .expect_err("wired memory keeps an unasked claim from fitting even alone")
                .shorts[..],
            [Short::Memory { .. }]
        ),
        "only Need::alone runs at or above reachable"
    );
}

/// ALONE, IN THE LEDGER: a 12 GiB claim on a 24 GiB Mac with 8 GiB available and no other claim
/// is admitted within the hang detector (at its first look), its record held alone; while it is
/// held, a 1 GiB claim waits, naming the holder that holds the machine's memory alone, and is
/// admitted once it ends. NEGATIVE CONTROL: a rule that waits for the room no claim holds never
/// admits it, and the hang detector fails the test by name.
#[test]
fn a_claim_short_of_memory_no_claim_holds_is_admitted_alone_in_bounded_time() {
    let s = scratch("alone");
    let ledger = s.ledger.clone();
    let heavy = std::thread::spawn(move || {
        admit_with(
            &ledger,
            &Fixed::new(Reading::of_gib(24, 8, 14)),
            Need::new("verify test compile (-p aterm-gui)").mem_gib(12),
            Admit::Wait,
            Pace {
                poll: Duration::from_millis(20),
                ..Pace::default()
            },
            &mut |_| {},
        )
    });
    let heavy = joined(heavy, "the 12 GiB claim admitted alone").expect("admitted");
    assert!(heavy.alone().is_some_and(|a| !a.asked), "{heavy:?}");
    let listed = s.ledger.list().expect("listed");
    assert!(
        listed[0].held.is_some_and(|h| h.alone),
        "its record says alone: {listed:?}"
    );
    let m = Fixed::new(Reading::of_gib(24, 8, 14));
    let (w, short) = waits(enter(&s, &m, Need::new("verify test").mem_gib(1)), &m);
    assert!(matches!(short.shorts[..], [Short::Alone { .. }]), "{short}");
    names(
        &short.to_string(),
        &["holds the machine's memory alone", "12 GiB of memory alone"],
    );
    drop(heavy);
    drop(admitted(w, &m));
}

/// ALONE, ASKED: work that needs more than can ever be available here (30 GiB on a 24 GiB Mac
/// with 2.5 GiB wired, R = 21.5) asks to run alone ([`Need::alone`]); [`never`] refuses it
/// otherwise. It waits while another process's claim holds memory, whatever that claim's tree
/// holds (f of 0, 5 or 20 GiB, beside 3 GiB of other use), and with none held it is admitted
/// alone however little is available. While it is held, another run's 3 GiB claim waits whatever
/// the tree holds, and so does a like 30 GiB one. NEGATIVE CONTROL: held beside others as half
/// of R, its old claim, the 3 GiB claim is admitted beside it at f = 0 and 5.
#[test]
fn a_claim_that_can_never_fit_runs_alone_and_keeps_every_other_memory_claim_waiting() {
    let reach = 43 * GIB / 2;
    let on = |f: u64| Reading {
        available: Some((reach - 3 * GIB).saturating_sub(f * GIB)),
        wired: Some(5 * GIB / 2),
        ..Reading::of_gib(24, 0, 14)
    };
    let whole = Need::new("verify cells").mem_gib(30);
    assert!(never(&whole, &on(0)).is_some());
    let asked = whole.clone().alone();
    assert_eq!(never(&asked, &on(0)), None, "asked to run alone");
    let theirs = record(30, 0, Need::new("trust check").mem_gib(3), Some(1));
    fn holding(r: &Record, f: u64) -> Other<'_> {
        Other {
            record: r,
            used: TreeUse {
                footprint: f * GIB,
                written: 0,
            },
            credited: TreeUse::default(),
        }
    }
    for f in [0, 5, 20] {
        assert!(
            decide(&asked, (1, 20, 0), &on(f), &[holding(&theirs, f)], 1).is_err(),
            "waits beside a held claim at f = {f}"
        );
    }
    let granted = decide(&asked, (1, 20, 0), &on(20), &[], 1).expect("nothing held");
    assert!(granted.alone.is_some_and(|a| a.asked), "{granted:?}");
    let mut held = record(10, 0, asked, Some(1));
    if let Some(h) = &mut held.held {
        h.alone = true;
    }
    let small = Need::new("another run's verify tippy lint").mem_gib(3);
    for f in [0, 5, 20] {
        let short = decide(&small, (1, 20, 0), &on(f), &[holding(&held, f)], 1)
            .expect_err("no memory claim beside it");
        assert_eq!(
            short.shorts,
            vec![Short::Alone {
                pid: 10,
                what: "verify cells".to_owned()
            }],
            "f = {f}"
        );
        assert!(
            decide(
                &whole.clone().alone(),
                (1, 20, 0),
                &on(f),
                &[holding(&held, f)],
                1
            )
            .is_err()
        );
    }
    let half = record(10, 0, Need::new("verify cells").mem(reach / 2), Some(1));
    for f in [0, 5] {
        assert!(
            decide(&small, (1, 20, 0), &on(f), &[holding(&half, f)], 1).is_ok(),
            "beside half of R at f = {f}"
        );
    }
}

/// CLAIMABLE: the room a claim has on a Mac is the free pages (speculative included), the
/// inactive pages file-backed ones could fill, the purgeable ones, and what compressing the
/// anonymous rest of the inactive queue frees at the compressor's ratio. At the look of
/// 2026-10-29 03:48 on this 24 GiB Mac (0.09 GiB free, 9.92 GiB inactive, 2.62 GiB file-backed,
/// 1.51 GiB stored in 0.64 GiB of compressor), about 6.9 GiB: 2.7 dropped or free, 4.2 by
/// compressing 7.3. With nothing in the compressor no ratio is read, and the anonymous pages
/// count for nothing; the room is never more than the machine. NEGATIVE CONTROL: counting every
/// inactive page reads 10.0 GiB, and counting none of the anonymous ones 2.7.
#[test]
fn the_room_a_claim_has_is_what_dropping_and_compressing_inactive_pages_frees() {
    let hundredths = |h: u64| h * GIB / 100;
    let detail = aterm_sysprobe::MemoryDetail {
        total: 24 * GIB,
        free: hundredths(9),
        speculative: Some(0),
        inactive: Some(hundredths(992)),
        file_backed: hundredths(262),
        purgeable: Some(0),
        compressed: Some(hundredths(64)),
        stored: Some(hundredths(151)),
        ..aterm_sysprobe::MemoryDetail::default()
    };
    let room = crate::claimable(&detail).expect("a Mac's page queues");
    let tenths = |b: u64| (b * 10 + GIB / 2) / GIB;
    assert_eq!(tenths(room), 69, "{room}");
    assert_ne!(tenths(room), 100, "every inactive page");
    assert_ne!(tenths(room), 27, "no anonymous page");
    let empty = aterm_sysprobe::MemoryDetail {
        compressed: Some(0),
        stored: Some(0),
        ..detail
    };
    assert_eq!(
        crate::claimable(&empty).map(tenths),
        Some(27),
        "no ratio read"
    );
    let huge = aterm_sysprobe::MemoryDetail {
        free: 30 * GIB,
        ..detail
    };
    assert_eq!(
        crate::claimable(&huge),
        Some(24 * GIB),
        "capped at the machine"
    );
    let linux = aterm_sysprobe::MemoryDetail {
        purgeable: None,
        ..detail
    };
    assert_eq!(crate::claimable(&linux), None, "no Mac page queues");
}

/// WHAT A WAIT WAS FOR ([`crate::Claim::waited_for`]): a 20 GiB claim that waited behind a 30
/// GiB one on 40 GiB available says, once admitted, that it was short of memory and that the 30
/// GiB claim held it; one admitted at its first look waited for nothing. NEGATIVE CONTROL: a
/// claim that does not keep its last look says it waited for nothing, and the first assertion
/// fails.
#[test]
fn an_admitted_claim_says_what_its_wait_was_for() {
    let s = scratch("waited-for");
    let m = mac48();
    let a = admitted(enter(&s, &m, Need::new("A").mem_gib(30)), &m);
    let ledger = s.ledger.clone();
    let looked = Arc::new(AtomicBool::new(false));
    let saw = Arc::clone(&looked);
    let b = std::thread::spawn(move || {
        admit_with(
            &ledger,
            &mac48(),
            Need::new("B").mem_gib(20),
            Admit::Wait,
            Pace {
                poll: Duration::from_millis(20),
                ..Pace::default()
            },
            &mut |_| saw.store(true, Ordering::SeqCst),
        )
    });
    until_within(MUST_HAPPEN, "B's first look to wait", || {
        looked.load(Ordering::SeqCst).then_some(())
    });
    drop(a);
    let b = joined(b, "B admitted once A ended").expect("admitted");
    let short = b.waited_for().expect("it waited");
    assert!(
        matches!(short.shorts[..], [Short::Memory { .. }]),
        "{short}"
    );
    assert_eq!(
        short
            .holders
            .iter()
            .map(|h| h.what.as_str())
            .collect::<Vec<_>>(),
        ["A"]
    );
    drop(b);
    let c = admitted(enter(&s, &m, Need::new("C").mem_gib(1)), &m);
    assert!(c.waited_for().is_none(), "{:?}", c.waited_for());
}

#[test]
fn saturating_reservations_and_wrote_bounds_never_wrap_or_pass_record_max() {
    let mut m = Reading::of_gib(48, 40, 14);
    m.volume = Some(vol(200, 100, 10));
    let a = record(
        10,
        0,
        Need::new("A").mem(u64::MAX).disk(u64::MAX, "/vol"),
        Some(1),
    );
    let b = record(
        11,
        1,
        Need::new("B").mem(u64::MAX).disk(u64::MAX, "/vol"),
        Some(1),
    );
    let others = [
        Other {
            record: &a,
            used: TreeUse::default(),
            credited: TreeUse::default(),
        },
        Other {
            record: &b,
            used: TreeUse::default(),
            credited: TreeUse::default(),
        },
    ];
    let short = decide(
        &Need::new("C").mem_gib(1).disk(GIB, "/vol"),
        (2, 2, 0),
        &m,
        &others,
        2,
    )
    .expect_err("saturated reservations block admission rather than wrapping");
    assert_eq!(
        short.shorts,
        vec![
            Short::Memory {
                need: GIB,
                margin: 0,
                available: 40 * GIB,
                reserved: u64::MAX,
            },
            Short::Disk {
                need: GIB,
                mount: PathBuf::from("/vol"),
                above_floor: 90 * GIB,
                floor: 10 * GIB,
                reserved: u64::MAX,
            },
        ]
    );

    let s = scratch("wrote-cap");
    let (_, mut laid) = s.ledger.lay(&Need::new("X"), 1).expect("laid");
    let mut past = Some(crate::ledger::RECORD_MAX);
    let err = laid
        .wrote(&mut past, 1)
        .expect_err("wrote past RECORD_MAX must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::FileTooLarge);

    let (neg_quiet, _neg_laid) = s
        .ledger
        .lay(&Need::new("neg").quiet(-2.0), 2)
        .expect("negative quiet lays clamped");
    let listed = s.ledger.list().expect("listed");
    assert!(
        listed
            .iter()
            .any(|r| r.seq == neg_quiet.seq && r.need.quiet == Some(0.0)),
        "negative quiet normalizes to 0.0 and remains visible to peers"
    );

    assert!(
        Record::decode(
            "{\"v\":1,\"pid\":1,\"start_ms\":1,\"seq\":1,\"checkout\":\"\",\"what\":\"w\",\"mem\":0,\"cores\":0,\"disk\":0,\"quiet\":\"bad\"}\n",
            false
        )
        .is_none()
    );
    assert!(
        Record::decode(
            "{\"v\":1,\"pid\":1,\"start_ms\":1,\"seq\":1,\"checkout\":\"\",\"what\":\"w\",\"mem\":0,\"cores\":0,\"disk\":0,\"quiet\":-1.0}\n",
            false
        )
        .is_none()
    );
    assert!(
        Record::decode(
            "{\"v\":1,\"pid\":1,\"start_ms\":1,\"seq\":1,\"checkout\":\"\",\"what\":\"w\",\"mem\":0,\"cores\":0,\"disk\":0,\"quiet\":1.0,\"patience\":{\"reserve_ms\":\"bad\"}}\n",
            false
        )
        .is_none()
    );
}

/// On macOS the live machine's lineage of a child is the walk up its parents, this process among
/// them, as covering reads it. NEGATIVE CONTROL: a sibling's lineage does not hold the child.
#[cfg(target_os = "macos")]
#[test]
fn the_live_lineage_of_a_child_walks_up_to_this_process() {
    use crate::reading::{Live, Machine as _};
    let spawn = || {
        aterm_uds::spawnfd::command("/bin/cat")
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("a child")
    };
    let mut child = spawn();
    let mut sibling = spawn();
    let me = std::process::id();
    let line = Live.lineage(child.id()).expect("its lineage");
    assert!(
        line.ancestors(child.id()).contains(&me),
        "{:?}",
        line.ancestors(child.id())
    );
    let theirs = Live.lineage(sibling.id()).expect("its lineage");
    assert!(theirs.ancestors(sibling.id()).contains(&me));
    assert!(!theirs.ancestors(sibling.id()).contains(&child.id()));
    for c in [&mut child, &mut sibling] {
        drop(c.stdin.take());
        assert!(c.wait().expect("its end").success());
    }
}
