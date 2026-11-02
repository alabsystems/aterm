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
    };
    assert_eq!(Words::parse(&words.render()), Some(words.clone()));
    let bookless = Words {
        own: None,
        ..words.clone()
    };
    assert_eq!(Words::parse(&bookless.render()), Some(bookless));
    for (at, word) in [
        (0, "1"),
        (0, "0"),
        (2, "l/0000000000001-4242-0.claim"),
        (3, "{\"stage\":1}"),
        (4, "s/targo-memory.json"),
    ] {
        let mut bad = words.render();
        bad[at] = word.into();
        assert_eq!(Words::parse(&bad), None, "{at}: {word}");
    }
    assert_eq!(Words::parse(&words.render()[..4]), None, "a word short");
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
