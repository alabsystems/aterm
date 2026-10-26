// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The standby bridge: while the holder holds the state dir, its take sleeps in `flock(2)` on the
//! bridge lock, the kernel's queue for it, as the kernel itself reports ([`aterm_uds::asleep`]),
//! so the holder's release is what hands it the dir, with no timer between; a polling take in its
//! place sleeps elsewhere between its tries and takes the released dir only on its next tick; a
//! standby told to stop returns without the lock; and a holder killed by `SIGKILL` is
//! succeeded.

use super::*;

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;

use aterm_uds::asleep::{asleep, queued_for, this_thread, Asleep, Thread};

/// The one-minute hang detector on every wait here that must end.
const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;

/// What the stand-in holder of [`a_holder_killed_by_sigkill_is_succeeded`] reads its state dir
/// from: set, the test is that stand-in.
const HOLDER_CHILD_ENV: &str = "ATERM_LINK_TEST_STANDBY_HOLDER";

/// The line the stand-in prints once it holds the lock.
const HOLDER_HELD: &str = "standby-holder=held";

/// [`stand_by`] on a thread, answering on the returned channel: the push lane comes back with
/// the answer, so a test can read what is left on it.
fn standing_by(
    lane: UnixStream,
    take: impl FnOnce() -> io::Result<BridgeLock> + Send + 'static,
) -> mpsc::Receiver<(io::Result<BridgeLock>, UnixStream)> {
    let (said, heard) = mpsc::channel();
    std::thread::spawn(move || {
        let answer = stand_by(&lane, take);
        let _ = said.send((answer, lane));
    });
    heard
}

/// The standby's own take of `st` ([`crate::state::BridgeLockWait::wait`]), and the thread
/// that runs it ([`STANDBY_THREAD`]), named on it just before the take begins.
fn queued_take(
    st: &StateDir,
) -> (
    impl FnOnce() -> io::Result<BridgeLock> + Send + 'static,
    mpsc::Receiver<Thread>,
) {
    let queued = st.bridge_lock_wait().expect("the standby's descriptor");
    let (named, taking) = mpsc::channel();
    let take = move || {
        let _ = named.send(this_thread());
        queued.wait()
    };
    (take, taking)
}

/// Waits, up to [`HANG`], until `thread` sleeps where `wanted` says, as the kernel reports it,
/// and returns that; panics with `what` and the last reading otherwise.
fn seen_asleep(thread: Thread, what: &str, wanted: impl Fn(&Asleep) -> bool) -> Asleep {
    let mut last = None;
    aterm_spec::wait::within(HANG, Duration::from_millis(1), || {
        let at = asleep(thread).unwrap_or_else(|e| panic!("{what}: read the thread: {e}"));
        let hit = at.clone().filter(&wanted);
        last = at;
        hit
    })
    .unwrap_or_else(|| panic!("{what}: last seen {last:?}"))
}

/// The threads of this process queued in the kernel for `st`'s bridge lock: asleep in
/// `flock(2)` on a descriptor for its `lock` file ([`queued_for`]).
fn queued_on(st: &StateDir) -> Vec<Thread> {
    queued_for(&st.root().join("lock")).expect("the threads queued for the bridge lock")
}

/// Waits, up to [`HANG`], until `thread` is queued in the kernel for `st`'s bridge lock
/// ([`queued_on`]); panics with `what` and where it sleeps otherwise.
fn seen_queued(st: &StateDir, thread: Thread, what: &str) {
    aterm_spec::wait::within(HANG, Duration::from_millis(1), || {
        queued_on(st).contains(&thread).then_some(())
    })
    .unwrap_or_else(|| {
        panic!(
            "{what}: never asleep in flock(2) on the bridge lock, the kernel's queue for it; it \
             sleeps in {:?}",
            asleep(thread)
        )
    });
}

/// Whether a take of `st` now is refused because another holds it.
fn held(st: &StateDir) -> bool {
    st.lock_bridge()
        .err()
        .is_some_and(|e| e.kind() == io::ErrorKind::WouldBlock)
}

/// **WHILE THE HOLDER HOLDS THE DIR, THE STANDBY'S TAKE SLEEPS IN `flock(2)`,
/// AND THE RELEASE HANDS IT THE DIR.** A thread asleep in `flock(2)` is queued
/// in the kernel for the lock, and the kernel wakes it the moment the holder
/// lets go: no timer of any period stands between. A take that polls instead
/// sleeps between its tries in something else (a timer's, a channel's), and is
/// never seen in `flock(2)`, however long the test waits, so a poll put back
/// into the take fails here at the hang detector, whatever its period.
///
/// The negative control first, on the same dir and the same reading: a take
/// that polls (tries the dir with [`StateDir::lock_bridge`], then waits for its
/// clock) is seen asleep outside the kernel's lock queue after a refused try,
/// with nothing queued for the held dir, and when the holder lets go it takes
/// nothing until its clock, which never advances on its own, ticks once. Then
/// the standby's own take ([`crate::state::BridgeLockWait::wait`] under
/// [`stand_by`]) on the dir held the same way: it is seen asleep in `flock(2)`
/// on the dir's bridge lock, it has not answered, and the holder's release
/// alone gives it the dir. Nothing is read from the push lane while it stood
/// by: what aterm writes there next is all there.
#[test]
fn the_standby_sleeps_in_the_kernels_lock_queue_where_a_polling_take_needs_a_tick() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let st = StateDir::open(dir.path()).expect("open");

    let holder = st.lock_bridge().expect("the holder's take");
    let (lane, _aterm) = UnixStream::pair().expect("a push lane");
    let (parked_tx, parked) = mpsc::channel();
    let (tick, ticks) = mpsc::channel::<()>();
    let root = dir.path().to_owned();
    let polling = move || -> io::Result<BridgeLock> {
        let st = StateDir::open(&root)?;
        loop {
            match st.lock_bridge() {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                taken => return taken,
            }
            let _ = parked_tx.send(this_thread());
            ticks
                .recv()
                .map_err(|_| io::Error::other("the clock stopped"))?;
        }
    };
    let heard = standing_by(lane, polling);
    let poller = parked
        .recv_timeout(HANG)
        .expect("CONTROL: the polling take tried the held dir");
    let at = seen_asleep(
        poller,
        "CONTROL: the polling take parked on its clock",
        |_| true,
    );
    assert!(
        matches!(at, Asleep::Elsewhere(_)),
        "CONTROL: a polling take parked on its clock reads {at:?}, so the reading cannot tell \
         it from a take queued in the kernel"
    );
    assert_eq!(
        queued_on(&st),
        vec![],
        "CONTROL: nothing is queued in the kernel for the held dir while the polling take \
         waits on its clock"
    );
    drop(holder);
    assert!(
        matches!(heard.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "CONTROL: a polling take parked on its clock took the released dir with no tick"
    );
    tick.send(()).expect("the polling take waits on its clock");
    let (taken, _lane) = heard
        .recv_timeout(HANG)
        .expect("CONTROL: the polling take answered after its tick");
    let taken = taken.expect("CONTROL: one tick, and the polling take holds the dir");
    assert!(
        parked.try_recv().is_err(),
        "CONTROL: it took the dir on that one tick"
    );
    assert!(held(&st), "CONTROL: the polling take holds the dir");
    drop(taken);

    let holder = st.lock_bridge().expect("the holder's second take");
    let (lane, mut aterm) = UnixStream::pair().expect("a push lane");
    let (take, taking) = queued_take(&st);
    let heard = standing_by(lane, take);
    let standby = taking
        .recv_timeout(HANG)
        .expect("the standby began its take");
    seen_queued(&st, standby, "the standby's take of the held dir");
    assert!(
        matches!(heard.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the standby answered while the dir was held"
    );
    drop(holder);
    let (taken, lane) = heard
        .recv_timeout(HANG)
        .expect("the standby, queued in the kernel, took the released dir");
    let taken = taken.expect("the standby holds the dir");
    assert!(held(&st), "a take beside the standby's is refused");
    aterm
        .write_all(b"x")
        .expect("aterm writes on the push lane");
    let mut first = [0_u8; 1];
    (&lane)
        .read_exact(&mut first)
        .expect("the lane's next byte");
    assert_eq!(&first, b"x", "the standby read nothing from the push lane");
    drop(taken);
    let _again = st.lock_bridge().expect("free once the standby let go");
}

/// **A STANDBY TOLD TO STOP RETURNS WITHOUT THE LOCK**: while its take sleeps
/// in `flock(2)` for the held dir, aterm closing the push lane, or writing on
/// it before any subscription, ends the standby with that error; the holder
/// keeps the dir, and once it lets go the dir is free again, the standby's
/// queued take having no reader to hand it to.
#[test]
fn a_standby_told_to_stop_returns_without_the_lock() {
    for (stop, kind) in [
        ("closed", io::ErrorKind::ConnectionAborted),
        ("wrote", io::ErrorKind::InvalidData),
    ] {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let st = StateDir::open(dir.path()).expect("open");
        let holder = st.lock_bridge().expect("the holder's take");
        let (lane, mut aterm) = UnixStream::pair().expect("a push lane");
        let (take, taking) = queued_take(&st);
        let heard = standing_by(lane, take);
        let standby = taking
            .recv_timeout(HANG)
            .unwrap_or_else(|_| panic!("{stop}: the standby began its take"));
        seen_queued(
            &st,
            standby,
            &format!("{stop}: the standby's take of the held dir"),
        );
        if stop == "closed" {
            drop(aterm);
        } else {
            aterm
                .write_all(b"x")
                .expect("aterm writes on the push lane");
        }
        let (answer, _lane) = heard
            .recv_timeout(HANG)
            .unwrap_or_else(|_| panic!("{stop}: the standby stopped"));
        let refused = answer
            .err()
            .unwrap_or_else(|| panic!("{stop}: the standby took a held dir"));
        assert_eq!(refused.kind(), kind, "{stop}: {refused}");
        assert!(held(&st), "{stop}: the holder still holds the dir");
        drop(holder);
        aterm_spec::wait::within(HANG, Duration::from_millis(1), || st.lock_bridge().ok())
            .unwrap_or_else(|| panic!("{stop}: the stopped standby kept the dir"));
    }
}

/// **A HOLDER KILLED BY SIGKILL IS SUCCEEDED**, through the whole standby: a
/// stand-in process (this test binary) takes the dir and names itself in the
/// pid file; the standby is refused by its name, reports `link down
/// reason=state-locked` on its verb lane and stands by on a silent push lane,
/// its take asleep in `flock(2)` on the bridge lock (one thread of this test
/// queued in the kernel for it, as in the test above, here through
/// [`hold_state_dir`] itself, so a poll put back there fails too); the stand-in
/// dies by `SIGKILL`, letting go only by the kernel's close of its
/// descriptors, and the standby holds the dir.
#[test]
fn a_holder_killed_by_sigkill_is_succeeded() {
    if let Some(root) = std::env::var_os(HOLDER_CHILD_ENV) {
        let st = StateDir::open(&root).expect("the stand-in opens the dir");
        let _held = st.lock_bridge().expect("the stand-in's take");
        std::fs::write(
            Path::new(&root).join("pid"),
            format!("{}\n", std::process::id()),
        )
        .expect("the stand-in's pid file");
        let mut out = std::io::stdout();
        writeln!(out, "\n{HOLDER_HELD}").expect("say it holds");
        out.flush().expect("flush");
        let _ = std::io::stdin().read(&mut [0_u8; 1]);
        aterm_uds::process::kill_self_now();
    }
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let st = StateDir::open(dir.path()).expect("open");
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
    child
        .args([
            "bridge::standby_tests::a_holder_killed_by_sigkill_is_succeeded",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HOLDER_CHILD_ENV, dir.path())
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    let mut child = child.spawn().expect("spawn the stand-in holder");
    let stand_in = child.id();
    let (said, lines) = mpsc::channel();
    let out = child.stdout.take().expect("the stand-in's stdout");
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines() {
            let Ok(line) = line else { break };
            if said.send(line).is_err() {
                break;
            }
        }
    });
    loop {
        let line = lines
            .recv_timeout(HANG)
            .expect("the stand-in says it holds the dir");
        if line == HOLDER_HELD {
            break;
        }
    }

    let (verb, aterm_verb) = UnixStream::pair().expect("a verb lane");
    let (reported, report) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let mut reader = BufReader::new(&aterm_verb);
        if reader.read_line(&mut line).is_ok() {
            let _ = (&aterm_verb).write_all(b"OK\n");
            let _ = reported.send(line);
        }
        let _ = reader.read_line(&mut String::new());
    });
    let (push, _aterm_push) = UnixStream::pair().expect("a push lane");
    let root = dir.path().to_owned();
    let (said, heard) = mpsc::channel();
    std::thread::spawn(move || {
        let st = StateDir::open(&root).expect("the standby opens the dir");
        let mut ctl = Ctl::from_stream(verb).expect("the verb lane");
        let push = Ctl::from_stream(push).expect("the push lane");
        let _ = said.send(hold_state_dir(
            &st,
            &mut ctl,
            Some(&push),
            Attachment::Inherited,
        ));
    });
    assert_eq!(
        report.recv_timeout(HANG).expect("the standby's report"),
        "link down reason=state-locked\n"
    );
    let refused = st.lock_bridge().err().expect("the stand-in holds the dir");
    assert!(
        refused.to_string().contains(&format!("(pid {stand_in})")),
        "the refusal names the stand-in: {refused}"
    );
    assert!(
        matches!(heard.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the standby took a held dir"
    );
    let queued = aterm_spec::wait::within(HANG, Duration::from_millis(1), || {
        Some(queued_on(&st)).filter(|q| !q.is_empty())
    })
    .expect(
        "hold_state_dir's standby never slept in flock(2) on the bridge lock, the kernel's \
         queue for it",
    );
    assert_eq!(
        queued.len(),
        1,
        "one take, the standby's, is queued for the held dir"
    );

    drop(child.stdin.take());
    let taken = heard
        .recv_timeout(HANG)
        .expect("the standby answered after the stand-in's death")
        .expect("the standby holds the dir");
    let status = child.wait().expect("reap the stand-in");
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(9),
        "the stand-in died by SIGKILL: {status:?}"
    );
    assert!(held(&st), "the standby holds the dir");
    drop(taken);
}
