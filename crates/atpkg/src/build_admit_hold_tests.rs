// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The holder's parts, in this process: its words, its watch of the front's end, and the claim it
//! keeps for what a killed build left running. Its whole life (spawned by a front, adopted by
//! launchd, armed, the build exec'd) runs through the real binary in `tests/build_hop.rs`.

use super::*;

use std::io::{BufRead as _, Write as _};
use std::sync::mpsc;

use aterm_capacity::book::Key;
use aterm_capacity::{Admit, Fixed, Ledger, Need, Pace, Reading};
use aterm_spec::wait::{MUST_HAPPEN, until_within};

fn key() -> Key {
    Key {
        stage: "targo test".to_owned(),
        checkout: "/w/ck".to_owned(),
        crates: Some(vec!["a".to_owned()]),
        variant: None,
    }
}

/// A holder's words read back whole, with a book and without. NEGATIVE CONTROL: launchd's or a
/// zero pid, a relative claim or book, a key that is not one, and a word short read as none.
#[test]
fn a_holders_words_read_back_whole() {
    let words = Words {
        pid: 4242,
        serial: 77,
        claim: "/l/0000000000001-4242-0.claim".into(),
        key: key(),
        own: Some("/s/verify/targo-memory.json".into()),
        leaf: Some("/c/w/.cargo-build".into()),
    };
    assert_eq!(Words::parse(&words.render()), Some(words.clone()));
    let bookless = Words {
        own: None,
        leaf: None,
        ..words.clone()
    };
    assert_eq!(Words::parse(&bookless.render()), Some(bookless));
    for (at, word) in [
        (0, "1"),
        (0, "0"),
        (2, "l/0000000000001-4242-0.claim"),
        (3, "{\"stage\":1}"),
        (4, "s/targo-memory.json"),
        (5, "c/w/.cargo-build"),
    ] {
        let mut bad = words.render();
        bad[at] = word.into();
        assert_eq!(Words::parse(&bad), None, "{at}: {word}");
    }
    assert_eq!(Words::parse(&words.render()[..5]), None, "a word short");
}

/// THE METER'S FIRST READ IS VOUCHED FOR BY THE FRONT'S IDENTITY READ AFTER IT: the meter starts
/// first, and a front whose serial changed by the identity read keeps no meter. NEGATIVE CONTROL:
/// an unchanged serial keeps the meter, started once, before the read.
#[test]
fn the_front_is_read_after_its_meters_first_read() {
    for (serial_holds, kept) in [(true, Some("meter")), (false, None)] {
        let order = std::cell::RefCell::new(Vec::new());
        let meter = meter_for_front(
            || {
                order.borrow_mut().push("meter");
                "meter"
            },
            || {
                order.borrow_mut().push("front");
                serial_holds
            },
        );
        assert_eq!(meter, kept, "serial held: {serial_holds}");
        assert_eq!(*order.borrow(), ["meter", "front"]);
    }
}

fn pid_t(pid: u32) -> i32 {
    i32::try_from(pid).expect("a pid")
}

fn serial_of(pid: u32) -> u64 {
    aterm_uds::process::process_serial(pid_t(pid)).expect("its serial")
}

/// A shell that waits for a line on its stdin, then exits 3.
fn waiting_shell() -> aterm_uds::spawnfd::Child {
    aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "read x; exit 3"])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("a shell")
}

/// A WATCH READS HOW THE FRONT'S PROCESS ENDED, as a non-parent may: its exit's status and its
/// death by a signal. NEGATIVE CONTROL: a process that already exited (a zombie, unreaped) is
/// refused, so a holder never arms for a front already gone.
#[test]
fn a_watch_reads_how_its_process_ended() {
    let mut exits = waiting_shell();
    let watch = Watch::on(exits.id()).expect("watched");
    let serial = serial_of(exits.id());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(watch.wait(serial, &mut || {}));
    });
    drop(exits.stdin.take());
    let End::Status(status) = rx
        .recv_timeout(MUST_HAPPEN)
        .expect("hang detector: its exit")
    else {
        panic!("no status");
    };
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 3,
        "{status:#x}"
    );
    let _ = exits.wait();

    let mut killed = waiting_shell();
    let watch = Watch::on(killed.id()).expect("watched");
    let serial = serial_of(killed.id());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(watch.wait(serial, &mut || {}));
    });
    killed.kill().expect("killed");
    let End::Status(status) = rx
        .recv_timeout(MUST_HAPPEN)
        .expect("hang detector: its death")
    else {
        panic!("no status");
    };
    assert!(
        libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGKILL,
        "{status:#x}"
    );
    let _ = killed.wait();

    let mut gone = waiting_shell();
    let pid = gone.id();
    drop(gone.stdin.take());
    until_within(MUST_HAPPEN, "the shell's exit", || {
        aterm_uds::process::process_serial(i32::try_from(pid).ok()?)
            .is_none()
            .then_some(())
    });
    assert!(Watch::on(pid).is_none(), "a zombie is refused");
    let _ = gone.wait();
}

/// A WATCH WHOSE KEVENT FAILS WAITS FOR ITS PROCESS BY READING ITS PID, never taking the failure
/// for an exit: its wait comes back only once the process is gone, with no status. NEGATIVE
/// CONTROL (by hand: a failed kevent taken for the process's end, `End::Unknown` at once, as
/// before): the wait comes back while the process still waits for its line.
#[test]
fn a_watch_whose_kevent_fails_waits_by_reading_its_pid() {
    let mut shell = waiting_shell();
    let null = std::fs::File::open("/dev/null").expect("not a kqueue");
    let watch = Watch {
        kq: null.into(),
        pid: shell.id(),
    };
    let (tx, rx) = mpsc::channel();
    let ticks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&ticks);
    std::thread::spawn(move || {
        let serial = serial_of(watch.pid);
        let end = watch.wait(serial, &mut || {
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        let _ = tx.send((
            end,
            aterm_uds::process::process_serial(pid_t(watch.pid)).is_some(),
        ));
    });
    let mut early = None;
    until_within(MUST_HAPPEN, "a wake of the watch, or its end", || {
        if let Ok(end) = rx.try_recv() {
            early = Some(end);
            return Some(());
        }
        (ticks.load(std::sync::atomic::Ordering::Relaxed) > 0).then_some(())
    });
    assert_eq!(early, None, "the watch came back while its process waited");
    drop(shell.stdin.take());
    let (end, lived) = rx
        .recv_timeout(MUST_HAPPEN)
        .expect("hang detector: the watch's end");
    assert_eq!((end, lived), (End::Unknown, false));
    let _ = shell.wait();
}

/// A WATCH THAT READS ITS PROCESS'S PID GONE BEFORE THE EXIT'S EVENT WAITS FOR THAT EVENT AND
/// ITS STATUS: macOS stops reading a killed process's pid before it queues `NOTE_EXIT`, and here
/// the read says gone at every wake while the process lives on, holding that gap open until the
/// test kills it. NEGATIVE CONTROL (by hand: the first wake that reads the pid gone taken for the
/// end, `End::Unknown`, as the holder's one look at an empty queue took it): the wait comes back
/// with no status while its process lives, and a killed build's survivors lost its claim so.
#[test]
fn a_watch_that_reads_its_pid_gone_before_the_exit_waits_for_its_status() {
    let mut shell = waiting_shell();
    let watch = Watch::on(shell.id()).expect("watched");
    let (tx, rx) = mpsc::channel();
    let ticks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&ticks);
    std::thread::spawn(move || {
        let end = watch.wait_reading(&mut || true, EXIT_EVENT_WAKES, &mut || {
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        let _ = tx.send(end);
    });
    let mut early = None;
    until_within(MUST_HAPPEN, "two wakes of the watch, or its end", || {
        if let Ok(end) = rx.try_recv() {
            early = Some(end);
            return Some(());
        }
        (ticks.load(std::sync::atomic::Ordering::Relaxed) >= 2).then_some(())
    });
    assert_eq!(early, None, "the watch came back while its process lived");
    shell.kill().expect("killed");
    let end = rx
        .recv_timeout(MUST_HAPPEN)
        .expect("hang detector: its death");
    let End::Status(status) = end else {
        panic!("no status: {end:?}");
    };
    assert!(
        libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGKILL,
        "{status:#x}"
    );
    let _ = shell.wait();
}

/// A WATCH TAKES ITS PID READ GONE FOR AN END WITH NO STATUS ONLY AT `wakes` WAKES IN A ROW with
/// no exit event, and a read of it live counts anew: the reads say gone, live, then gone at a
/// process that lives on, `wakes` 2, and the end (`End::Unknown`) comes back at the fourth wake.
/// NEGATIVE CONTROLS (each by hand): the bound dropped, so only an event ends the wait, fires the
/// hang detector; the count kept across the live read ends it at the third wake.
#[test]
fn a_watch_takes_its_pid_gone_for_an_end_only_at_wakes_in_a_row() {
    let mut shell = waiting_shell();
    let watch = Watch::on(shell.id()).expect("watched");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reads = 0u32;
        let mut ticks = 0u32;
        let end = watch.wait_reading(
            &mut || {
                reads += 1;
                reads != 2
            },
            2,
            &mut || ticks += 1,
        );
        let _ = tx.send((end, ticks));
    });
    let ended = rx
        .recv_timeout(MUST_HAPPEN)
        .expect("hang detector: the watch's end");
    assert_eq!(ended, (End::Unknown, 4));
    assert!(
        shell.try_wait().expect("the shell's state").is_none(),
        "the shell still waits for its line"
    );
    drop(shell.stdin.take());
    let _ = shell.wait();
}

/// A root that starts a child of its own reading the root's stdin, says that child's pid, and
/// waits; in a process group of its own, as a job's leader.
fn root_with_a_child() -> (aterm_uds::spawnfd::Child, u32) {
    root_with("sh -c 'read x'", &[])
}

/// Set, the test binary is [`standin_touches_and_waits`]: the MiB it touches.
const TOUCH: &str = "ATPKG_HOLD_STANDIN_TOUCH";

/// Where [`standin_touches_and_waits`] says it touched its memory.
const TOUCHED: &str = "ATPKG_HOLD_STANDIN_TOUCHED";

/// The stand-in a killed build leaves working: touches the MiB [`TOUCH`] names, says so in the
/// file [`TOUCHED`] names, and holds them until stdin ends. Run alone, it does nothing.
#[test]
fn standin_touches_and_waits() {
    use std::io::Read as _;
    let Some(mib) = std::env::var(TOUCH)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    else {
        return;
    };
    let held = vec![1u8; mib << 20];
    if let Some(said) = std::env::var_os(TOUCHED) {
        let _ = std::fs::write(said, "touched\n");
    }
    let mut rest = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut rest);
    assert_eq!(held.len(), mib << 20);
}

/// A root that starts a working child ([`standin_touches_and_waits`], touching `mib` MiB) reading
/// the root's stdin, says its pid, and waits until that child said it touched them.
fn root_with_a_working_child(mib: u64, said: &std::path::Path) -> (aterm_uds::spawnfd::Child, u32) {
    let exe = std::env::current_exe().expect("test binary");
    let mib = mib.to_string();
    let said_s = said.to_str().expect("UTF-8");
    let child = format!(
        "{} --exact build_admit::hold::tests::standin_touches_and_waits --test-threads=1 \
         --nocapture",
        crate::platform::sh_quote_str(exe.to_str().expect("UTF-8"))
    );
    let (root, pid) = root_with(&child, &[(TOUCH, mib.as_str()), (TOUCHED, said_s)]);
    until_within(MUST_HAPPEN, "the child's memory touched", || {
        std::fs::metadata(said).ok()
    });
    (root, pid)
}

/// A root that starts `child` (a shell command) reading the root's stdin, with `env` set, says
/// that child's pid, and waits; in a process group of its own, as a job's leader.
fn root_with(child: &str, env: &[(&str, &str)]) -> (aterm_uds::spawnfd::Child, u32) {
    let mut root = aterm_uds::spawnfd::command("/bin/sh");
    root.args([
        "-c",
        &format!("exec 3<&0; {child} <&3 >/dev/null 2>&1 & echo \"$!\"; wait"),
    ])
    .process_group(0)
    .stdin(aterm_uds::spawnfd::Stdio::piped())
    .stdout(aterm_uds::spawnfd::Stdio::piped())
    .stderr(aterm_uds::spawnfd::Stdio::null());
    for (k, v) in env {
        root.env(k, v);
    }
    let mut root = root.spawn().expect("the root");
    let out = root.stdout.take().expect("its stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::BufReader::new(out).read_line(&mut line);
        let _ = tx.send(line);
    });
    let child = rx
        .recv_timeout(MUST_HAPPEN)
        .expect("hang detector: the root's child")
        .trim()
        .parse()
        .expect("its pid");
    (root, child)
}

/// A claim in `dir`'s scratch ledger, held for this test.
fn claim(dir: &std::path::Path) -> (Ledger, super::super::Held) {
    let ledger = Ledger::at(dir.join("ledger"));
    let claim = aterm_capacity::admit_with(
        &ledger,
        &Fixed::new(Reading::of_gib(24, 20, 14)),
        Need::new("targo test [a]").mem(1 << 30).covers(),
        Admit::Now,
        Pace::default(),
        &mut |_| {},
    )
    .expect("admitted");
    let held = super::super::Held {
        claim,
        key: key(),
        own: None,
        cohort: false,
        build_dir: None,
    };
    (ledger, held)
}

/// A KILLED BUILD'S CLAIM IS KEPT FOR ITS SURVIVORS WHILE THEY WORK, AND NO LONGER: once the root
/// a read saw its working child under (one holding 96 MiB) is killed, the claim stays held,
/// credited with that child, until the child ends. NEGATIVE CONTROL: a killed root that left
/// nothing running ends its claim's hold at once, crediting nothing; and an idle child (a shell
/// waiting for a line) lets it go at once while it still lives
/// (`an_idle_survivor_lets_its_claim_go`).
#[test]
fn a_killed_builds_claim_is_kept_for_its_survivors_and_no_longer() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let (ledger, held) = claim(dir.path());
    let (mut root, survivor) = root_with_a_working_child(96, &dir.path().join("touched"));
    let meter = aterm_capacity::tree_meter::TreeMeter::start(root.id());
    meter.read();
    let words = Words {
        pid: root.id(),
        serial: aterm_uds::process::process_serial(i32::try_from(root.id()).expect("a pid"))
            .expect("its serial"),
        claim: held.claim.path().to_path_buf(),
        key: key(),
        own: None,
        leaf: None,
    };
    let mut tell = root.stdin.take().expect("the survivor's stdin");
    root.kill().expect("the root killed");
    root.wait().expect("the root reaped");
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let _ = tx.send(survive(held, &meter, &words));
        });
        until_within(MUST_HAPPEN, "the survivor credited", || {
            let listed = ledger.list().ok()?;
            (listed.len() == 1 && listed[0].credit == [survivor]).then_some(())
        });
        tell.write_all(b"done\n").expect("told");
        drop(tell);
        let held = rx
            .recv_timeout(MUST_HAPPEN)
            .expect("hang detector: the hold's end once its survivor ended");
        drop(held);
    });
    assert!(ledger.list().expect("listed").is_empty(), "let go");

    let (ledger, held) = claim(dir.path());
    let mut lone = waiting_shell();
    let meter = aterm_capacity::tree_meter::TreeMeter::start(lone.id());
    meter.read();
    let words = Words {
        pid: lone.id(),
        serial: aterm_uds::process::process_serial(i32::try_from(lone.id()).expect("a pid"))
            .expect("its serial"),
        claim: held.claim.path().to_path_buf(),
        key: key(),
        own: None,
        leaf: None,
    };
    lone.kill().expect("killed");
    lone.wait().expect("reaped");
    let held = survive(held, &meter, &words);
    assert!(held.claim.record().credit.is_empty(), "nothing credited");
    drop(held);
    assert!(ledger.list().expect("listed").is_empty());
}

/// AN IDLE SURVIVOR LETS ITS CLAIM GO: once the root a read saw its idle child under (a shell
/// waiting for a line, as a background job left by a killed build would) is killed, the hold ends
/// at its first look while that child still lives, crediting nothing. NEGATIVE CONTROL (by hand:
/// `survive` keeping the claim for any survivor, as before): the hold lasts until the child is
/// told to end, and this test's hang detector fires.
#[test]
fn an_idle_survivor_lets_its_claim_go() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let (ledger, held) = claim(dir.path());
    let (mut root, idle) = root_with_a_child();
    let meter = aterm_capacity::tree_meter::TreeMeter::start(root.id());
    meter.read();
    let words = Words {
        pid: root.id(),
        serial: serial_of(root.id()),
        claim: held.claim.path().to_path_buf(),
        key: key(),
        own: None,
        leaf: None,
    };
    let tell = root.stdin.take().expect("the child's stdin");
    root.kill().expect("the root killed");
    root.wait().expect("the root reaped");
    assert_eq!(meter.survivors(), [idle], "it survived");
    let (tx, rx) = mpsc::channel();
    // Not scoped: a hold that never ends must fail this test, not hang it on the join.
    std::thread::spawn(move || {
        let _ = tx.send(survive(held, &meter, &words));
    });
    let held = rx
        .recv_timeout(MUST_HAPPEN)
        .expect("hang detector: the hold's end while its idle survivor lives");
    assert!(
        aterm_uds::process::process_serial(pid_t(idle)).is_some(),
        "the idle child still lives"
    );
    assert!(held.claim.record().credit.is_empty(), "nothing credited");
    drop(held);
    assert!(ledger.list().expect("listed").is_empty(), "let go");
    drop(tell);
}

/// A KILLED BUILD'S SURVIVORS KEEP ITS CLAIM WHILE THEY WORK, read by read: none left lets it go;
/// so do survivors holding less than `SURVIVORS_IDLE_UNDER` together, ones whose footprint has not
/// grown for `SURVIVORS_STALL_WITHIN`, and any at `SURVIVORS_HELD_WITHIN`; working ones that grow
/// keep it. NEGATIVE CONTROL: the same reads one step short of each bound keep it.
#[test]
fn a_survivor_set_keeps_its_claim_while_it_works() {
    use std::time::Duration;
    let mib = 1_u64 << 20;
    let s = |secs: u64| Duration::from_secs(secs);
    assert_eq!(Survival::default().look(s(0), false, 0), Some(Gone::None));
    assert_eq!(
        Survival::default().look(s(0), true, SURVIVORS_IDLE_UNDER - 1),
        Some(Gone::Idle)
    );
    assert_eq!(
        Survival::default().look(s(0), true, SURVIVORS_IDLE_UNDER),
        None
    );
    let mut working = Survival::default();
    assert_eq!(working.look(s(0), true, 300 * mib), None);
    assert_eq!(working.look(s(40), true, 900 * mib), None, "it grew");
    let stall = s(40) + SURVIVORS_STALL_WITHIN;
    assert_eq!(working.look(stall - s(1), true, 500 * mib), None);
    assert_eq!(working.look(stall, true, 500 * mib), Some(Gone::Stalled));
    let mut growing = Survival::default();
    let mut at = s(0);
    let mut footprint = 100 * mib;
    while at < SURVIVORS_HELD_WITHIN {
        assert_eq!(growing.look(at, true, footprint), None, "at {at:?}");
        at += s(30);
        footprint += mib;
    }
    assert_eq!(growing.look(at, true, footprint), Some(Gone::Spent));
}

/// A claim in `dir`'s scratch ledger whose run phase claims 1 GiB of its 4 GiB, held for this test.
fn phased_claim(dir: &std::path::Path) -> (Ledger, aterm_capacity::Claim) {
    let ledger = Ledger::at(dir.join("ledger"));
    let claim = aterm_capacity::admit_with(
        &ledger,
        &Fixed::new(Reading::of_gib(24, 20, 14)),
        Need::new("targo test [a]")
            .mem(4 << 30)
            .run_mem(1 << 30)
            .covers(),
        Admit::Now,
        Pace::default(),
        &mut |_| {},
    )
    .expect("admitted");
    (ledger, claim)
}

/// THE HOLDER STEPS ITS CLAIM DOWN ONCE ITS TREE RUNS A PROGRAM ITS BUILD MADE (CAP3), read from
/// the live tree as the holder's watch reads it: a root running a shell holds its claim's 4 GiB;
/// a root running this test binary (a Cargo test binary, `atpkg-<16 hex>`) as its child steps the
/// claim to its run phase's 1 GiB in its record, as another process reads it. NEGATIVE CONTROL
/// (by hand: `step_phase` handed an image probe that reads nothing): the claim never steps, and
/// the second wait's hang detector fires.
#[test]
fn the_holder_steps_its_claim_down_once_its_tree_runs_what_its_build_made() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let (ledger, mut claim) = phased_claim(dir.path());
    let stepped = |ledger: &Ledger| ledger.list().ok().and_then(|l| l.first()?.need.stepped);

    let (mut shell, _) = root_with_a_child();
    let meter = aterm_capacity::tree_meter::TreeMeter::start(shell.id());
    let mut phases = aterm_capacity::phase::Phases::new();
    until_within(MUST_HAPPEN, "the shell's child read in its tree", || {
        meter.read();
        (meter.survivors_using().0.len() >= 2).then_some(())
    });
    assert_eq!(step_phase(&mut phases, &mut claim, &meter), None);
    assert_eq!(stepped(&ledger), None, "a tree running no program it built");
    assert_eq!(phases.run_peak(), None);
    shell.kill().expect("killed");
    shell.wait().expect("reaped");
    drop(shell.stdin.take());
    drop(meter);

    let (mut root, child) = root_with_a_working_child(16, &dir.path().join("touched"));
    let meter = aterm_capacity::tree_meter::TreeMeter::start(root.id());
    let mut phases = aterm_capacity::phase::Phases::new();
    until_within(
        MUST_HAPPEN,
        "the claim stepped as its tree runs the test binary",
        || {
            meter.read();
            if meter.survivors_using().0.contains(&child) {
                step_phase(&mut phases, &mut claim, &meter);
            }
            stepped(&ledger)
        },
    );
    assert_eq!(stepped(&ledger), Some(1 << 30));
    assert!(
        phases.run_peak().is_some_and(|p| p > 0),
        "its run's peak read"
    );
    let mut tell = root.stdin.take().expect("the child's stdin");
    tell.write_all(b"done\n").expect("told");
    drop(tell);
    root.wait().expect("reaped");
}

/// A unit of the build directory `leaf`'s `debug` profile in the layout that keeps one folder per
/// unit: a test build of `aterm-grid` under `hash`, 64 KiB, nothing written in it for `ago`.
fn grid_unit(leaf: &std::path::Path, hash: &str, ago: std::time::Duration) -> std::path::PathBuf {
    let dir = leaf.join("debug/build/aterm-grid").join(hash);
    std::fs::create_dir_all(dir.join("fingerprint")).unwrap();
    std::fs::create_dir_all(dir.join("out")).unwrap();
    std::fs::write(dir.join("fingerprint/test-lib-aterm_grid"), b"h").unwrap();
    std::fs::write(
        dir.join("fingerprint/test-lib-aterm_grid.json"),
        b"{\"rustc\":2,\"features\":\"[]\"}",
    )
    .unwrap();
    let out = dir.join(format!("out/aterm_grid-{hash}"));
    std::fs::write(&out, vec![7u8; 64 << 10]).unwrap();
    let at = std::time::SystemTime::now() - ago;
    for path in [
        dir.join("fingerprint/test-lib-aterm_grid"),
        dir.join("fingerprint/test-lib-aterm_grid.json"),
        out,
        dir.join("fingerprint"),
        dir.join("out"),
        dir.clone(),
    ] {
        std::fs::File::open(&path)
            .unwrap()
            .set_modified(at)
            .unwrap();
    }
    dir
}

/// `targo`'s receipt of the selection `name` in `leaf`: a build begun `ago` that used `hash`.
fn receipt_of(leaf: &std::path::Path, name: &str, hash: &str, ago: std::time::Duration) {
    let began = (std::time::SystemTime::now() - ago)
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let dir = leaf.join(crate::build_cache::stale::RECEIPTS);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{name}.json")),
        format!(
            "{{\"began_ms\":{began},\"built\":{{}},\"incremental\":{{}},\"members\":[\"aterm-grid\"],\
             \"output_bytes\":0,\"profiles\":{{\"debug\":[\"{hash}\"]}},\"receipt\":1,\
             \"selection\":{{}}}}"
        ),
    )
    .unwrap();
}

/// THE HOLDER TRIMS ITS BUILD'S DIRECTORY WHEN THE BUILD ENDS (DC-12d, [`build_ended`] with the
/// run-end trim, `crate::build_cache::trim_run_end`): a selection's first build, two hours ago,
/// used the test unit `1111…`, and another selection's build the unit `3333…`; its holder takes
/// their receipts into the build directory's record and trims nothing. The selection's second
/// build, ten minutes ago, used `2222…` in `1111…`'s place; its holder retires `1111…`, an hour
/// old, and the trim removes it whole and frees its bytes, while the unit the second build used,
/// the other selection's unit, the profile's lock, the record and the receipts stay. NEGATIVE
/// CONTROLS: a build directory with no receipt is left exactly as it was (no record is written,
/// nothing goes); a second build whose receipt is torn takes it for none, retires nothing, and
/// `1111…` stays; and a build directory that is a link is not read.
#[test]
fn the_holder_trims_its_builds_directory_when_the_build_ends() {
    use crate::build_cache::stale;
    const MINUTE: std::time::Duration = std::time::Duration::from_secs(60);
    let (old, new, other) = ("1111111111111111", "2222222222222222", "3333333333333333");
    let (selection, scoped) = ("00000000000000a1", "00000000000000b2");
    let scratch = aterm_tempfile::Builder::new()
        .prefix("build-hold-trim-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let world = |name: &str| {
        let leaf = scratch.path().join(name);
        std::fs::create_dir_all(leaf.join("debug")).unwrap();
        std::fs::write(leaf.join("debug/.cargo-build-lock"), b"").unwrap();
        let units = [
            grid_unit(&leaf, old, 60 * MINUTE),
            grid_unit(&leaf, new, 5 * MINUTE),
            grid_unit(&leaf, other, 60 * MINUTE),
        ];
        (leaf, units)
    };
    let trim = |dir: &std::path::Path| {
        crate::build_cache::trim_run_end(dir, std::time::SystemTime::now(), &|| true)
    };

    let (bare, units) = world("bare");
    assert_eq!(build_ended(&bare, &trim), None, "no receipt");
    assert!(!bare.join(stale::USED).exists(), "no record written");
    assert!(units.iter().all(|u| u.is_dir()));

    let (leaf, [first_only, second, others]) = world("built");
    receipt_of(&leaf, scoped, other, 180 * MINUTE);
    receipt_of(&leaf, selection, old, 120 * MINUTE);
    assert_eq!(
        build_ended(&leaf, &trim),
        None,
        "the first take retires nothing"
    );
    assert_eq!(stale::Kept::read(&leaf).receipts.len(), 2);
    receipt_of(&leaf, selection, new, 10 * MINUTE);
    let trimmed = build_ended(&leaf, &trim).expect("the second build retired a unit");
    assert_eq!(
        trimmed
            .went
            .iter()
            .map(|(_, why)| why.clone())
            .collect::<Vec<_>>(),
        vec![crate::build_cache::Unused::Stale {
            incremental: 0,
            units: 0,
            superseded: 1,
            after: crate::build_cache::RECLAIM_STALE,
        }],
        "{trimmed:?}"
    );
    assert!(trimmed.freed >= 64 << 10, "{trimmed:?}");
    assert!(!first_only.exists(), "the unit only the first build used");
    assert!(second.is_dir() && others.is_dir());
    assert!(leaf.join("debug/.cargo-build-lock").is_file() && leaf.join(stale::USED).is_file());
    assert_eq!(stale::Receipts::read(&leaf).runs.len(), 2);

    let (torn, [kept_old, _, _]) = world("torn");
    receipt_of(&torn, selection, old, 120 * MINUTE);
    assert_eq!(build_ended(&torn, &trim), None);
    std::fs::write(
        torn.join(stale::RECEIPTS).join(format!("{selection}.json")),
        b"{\"began_ms\":",
    )
    .unwrap();
    assert_eq!(build_ended(&torn, &trim), None, "a torn receipt is none");
    assert!(kept_old.is_dir());

    let link = scratch.path().join("linked");
    std::os::unix::fs::symlink(&torn, &link).unwrap();
    assert_eq!(build_ended(&link, &trim), None, "a link is not read");
}

/// A SHIM'S PLACEMENT IS READ BACK AS THE BUILD DIRECTORY ITS RECEIPTS ARE IN ([`receipt_leaf`]):
/// a placement under a target directory names the target directory, whose quiet build folder
/// (`<target>/.ubd/build.noindex`) is where the build writes, so that folder is read; a
/// placement with no such folder is the build directory itself. NEGATIVE CONTROL: a quiet build
/// folder that is a link is not followed, and the placement stands.
#[test]
fn a_shims_placement_is_read_back_as_the_build_directory_its_receipts_are_in() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("build-hold-leaf-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let plain = scratch.path().join("cache/w/.cargo-build");
    std::fs::create_dir_all(&plain).unwrap();
    assert_eq!(receipt_leaf(&plain), plain);
    let target = scratch.path().join("w/target");
    let quiet = target.join(crate::build_cache::target_build_relative());
    std::fs::create_dir_all(&quiet).unwrap();
    assert_eq!(receipt_leaf(&target), quiet);
    let linked = scratch.path().join("v/target");
    let at = linked.join(crate::build_cache::target_build_relative());
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&quiet, &at).unwrap();
    assert_eq!(receipt_leaf(&linked), linked, "a link is not followed");
}

/// A HOLDER'S WORDS CARRY ITS BUILD'S BUILD DIRECTORY BOTH WAYS ([`Words::of`], `Words::held`):
/// the words a front hands its holder name the build directory its claim holds
/// (`Held::build_dir`), and the claim the holder holds from those words names it again, so the
/// holder's end ([`at_end`]) trims the directory the front read from the placement. NEGATIVE
/// CONTROL: a claim with no build directory hands `-`, and the holder's claim names none.
#[test]
fn a_holders_words_carry_its_builds_build_directory_both_ways() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    for build_dir in [Some(std::path::PathBuf::from("/c/w/.cargo-build")), None] {
        let (_ledger, mut held) = claim(dir.path());
        held.build_dir.clone_from(&build_dir);
        let words = Words::of(&held, 4242, 7);
        assert_eq!(words.leaf, build_dir);
        assert_eq!(
            (words.pid, words.serial, words.claim.as_path()),
            (4242, 7, held.claim.path())
        );
        assert_eq!(Words::parse(&words.render()), Some(words.clone()));
        let (claim, _ended) = held.into_epoch_parts();
        let back = words.held(claim);
        assert_eq!(back.build_dir, build_dir);
        assert_eq!(Words::of(&back, 4242, 7), words);
    }
}

/// THE HOLDER'S END TRIMS THE BUILD DIRECTORY ITS CLAIM NAMES ([`at_end`]): once a build ends,
/// its claim is let go, and the receipts of the build directory its claim names are taken: a
/// selection's second build replaced the first's receipt, so the unit only the first used goes.
/// NEGATIVE CONTROL: the same end of a claim that names no build directory takes no receipt and
/// trims nothing, and the unit stays; its claim is let go all the same.
#[test]
fn the_holders_end_trims_the_build_directory_its_claim_names() {
    use crate::build_cache::stale;
    const MINUTE: std::time::Duration = std::time::Duration::from_secs(60);
    let (old, new) = ("1111111111111111", "2222222222222222");
    let selection = "00000000000000a1";
    let scratch = aterm_tempfile::Builder::new()
        .prefix("build-hold-end-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    for named in [true, false] {
        let leaf = scratch.path().join(format!("named-{named}"));
        std::fs::create_dir_all(leaf.join("debug")).unwrap();
        std::fs::write(leaf.join("debug/.cargo-build-lock"), b"").unwrap();
        let first_only = grid_unit(&leaf, old, 60 * MINUTE);
        let second = grid_unit(&leaf, new, 5 * MINUTE);
        receipt_of(&leaf, selection, old, 120 * MINUTE);
        let none = |_: &std::path::Path| -> crate::build_cache::Trimmed {
            panic!("the first take retires nothing")
        };
        assert_eq!(build_ended(&leaf, &none), None);
        receipt_of(&leaf, selection, new, 10 * MINUTE);
        let (ledger, mut held) = claim(scratch.path());
        held.build_dir = named.then(|| leaf.clone());
        let trimmed = at_end(held, crate::build_admit::BuildEnd::Unknown, None, || 0);
        assert!(ledger.list().expect("listed").is_empty(), "let go");
        assert!(second.is_dir());
        if named {
            let trimmed = trimmed.expect("the end trimmed its build directory");
            assert!(trimmed.freed >= 64 << 10, "{trimmed:?}");
            assert!(!first_only.exists(), "the unit only the first build used");
        } else {
            assert!(trimmed.is_none());
            assert!(
                first_only.is_dir(),
                "no build directory named: nothing taken"
            );
            assert_eq!(stale::Kept::read(&leaf).receipts.len(), 1);
        }
    }
}
