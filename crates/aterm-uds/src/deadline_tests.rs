// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::mpsc;
use std::time::{Duration, Instant};

#[cfg(target_vendor = "apple")]
use super::Held;
#[cfg(unix)]
use super::{
    ExitWait, OUTPUT_KEEP, RunError, output_until, output_while_progressing, outputs_until,
};
use super::{Gate, Refused};

#[test]
fn a_copy_ages_on_the_awake_clock_from_its_exec() {
    use super::{
        EXEC_STAMP, FIRST_EXEC_HANG, RAN, copy_wait_left_at, exec_stamp, saturating_after,
        stamp_exec, try_stamp_exec,
    };
    let copy_may_go_at =
        |home: &std::path::Path, now, awake| copy_wait_left_at(home, now, awake).is_none();
    let hang = u64::try_from(FIRST_EXEC_HANG.as_millis()).expect("ms");
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("1-2");
    std::fs::create_dir(&home).expect("mkdir");
    let now = std::time::SystemTime::now();
    let later = now + FIRST_EXEC_HANG + Duration::from_secs(60);
    assert!(!copy_may_go_at(&home, now, Some(5)));
    assert!(copy_may_go_at(&home, later, Some(5)));
    assert_eq!(exec_stamp(&home), None, "no stamp");
    std::fs::write(home.join(EXEC_STAMP), "1000\n").expect("stamp");
    assert_eq!(exec_stamp(&home), Some(1000));
    assert!(
        !copy_may_go_at(&home, later, Some(1000 + hang - 1)),
        "slept through"
    );
    assert!(copy_may_go_at(&home, now, Some(1000 + hang)));
    assert!(copy_may_go_at(&home, now, Some(999)), "a later boot");
    assert_eq!(
        copy_wait_left_at(&home, later, Some(1000 + 60_000)),
        Some(FIRST_EXEC_HANG - Duration::from_secs(60)),
        "what is left is awake time too"
    );
    assert!(!copy_may_go_at(&home, now, None));
    assert!(copy_may_go_at(&home, later, None));
    std::fs::write(home.join(EXEC_STAMP), "+1000\n").expect("signed stamp");
    assert_eq!(exec_stamp(&home), None, "signed stamp holds no reading");
    std::fs::write(home.join(EXEC_STAMP), "").expect("blank stamp");
    assert_eq!(exec_stamp(&home), None, "a blank stamp holds no reading");
    assert!(!copy_may_go_at(&home, now, Some(1000 + hang)));
    assert!(copy_may_go_at(&home, later, Some(1000 + hang)));
    std::fs::write(home.join(RAN), "").expect("ran");
    assert!(copy_may_go_at(&home, now, Some(0)));
    let fresh = dir.path().join("3-4");
    std::fs::create_dir(&fresh).expect("mkdir");
    stamp_exec(&fresh);
    let text = std::fs::read_to_string(fresh.join(EXEC_STAMP)).expect("stamped");
    assert!(text.is_empty() || text.parse::<u64>().is_ok(), "{text:?}");
    let gone = dir.path().join("5-6");
    assert!(try_stamp_exec(&gone).is_err(), "nowhere to write it");
    stamp_exec(&gone);
    assert!(!gone.exists(), "a stamp that failed made nothing");
    assert!(try_stamp_exec(&fresh).is_ok());
    let first = crate::process::awake_ms();
    let second = crate::process::awake_ms();
    assert!(first <= second, "{first:?} then {second:?}");
    let now = Instant::now();
    assert!(saturating_after(now, Duration::MAX) > now);
}

/// A stamp another program planted as a FIFO, a link to one or a long file reads as none, at
/// once: the copy then ages by its folder's time, as with a blank stamp. A link to a regular
/// stamp reads what it names.
#[cfg(unix)]
#[test]
fn a_planted_stamp_reads_without_waiting() {
    use std::os::unix::ffi::OsStrExt as _;

    use super::{EXEC_STAMP, FIRST_EXEC_HANG, copy_wait_left_at, exec_stamp};
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let read = |home: std::path::PathBuf| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(exec_stamp(&home));
        });
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the read never waits")
    };
    let fifo = dir.path().join("1-2");
    std::fs::create_dir(&fifo).expect("mkdir");
    let name =
        std::ffi::CString::new(fifo.join(EXEC_STAMP).as_os_str().as_bytes()).expect("no NUL");
    // SAFETY: a NUL-terminated path the call reads and does not retain.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
    assert_eq!(read(fifo.clone()), None, "a FIFO");
    let now = std::time::SystemTime::now();
    assert!(
        copy_wait_left_at(&fifo, now, Some(u64::MAX)).is_some(),
        "a fresh folder with no reading is young"
    );
    assert!(
        copy_wait_left_at(&fifo, now + FIRST_EXEC_HANG, Some(u64::MAX)).is_none(),
        "and ages by its time"
    );

    let to_fifo = dir.path().join("7-8");
    std::fs::create_dir(&to_fifo).expect("mkdir");
    std::os::unix::fs::symlink(fifo.join(EXEC_STAMP), to_fifo.join(EXEC_STAMP)).expect("symlink");
    assert_eq!(read(to_fifo), None, "a link to a FIFO");

    let linked = dir.path().join("3-4");
    std::fs::create_dir(&linked).expect("mkdir");
    let real = dir.path().join("stamp");
    std::fs::write(&real, "1000\n").expect("stamp");
    std::os::unix::fs::symlink(&real, linked.join(EXEC_STAMP)).expect("symlink");
    assert_eq!(
        read(linked),
        Some(1000),
        "a link to a regular stamp reads what it names"
    );

    let long = dir.path().join("5-6");
    std::fs::create_dir(&long).expect("mkdir");
    std::fs::write(long.join(EXEC_STAMP), format!("1000{}", " ".repeat(64))).expect("stamp");
    assert_eq!(read(long.clone()), None, "past the cap");
    std::fs::write(long.join(EXEC_STAMP), format!("1000{}", " ".repeat(20))).expect("stamp");
    assert_eq!(read(long), Some(1000), "within it");
}

/// A stamp is written only into a regular file with one name: a FIFO another program planted
/// where it goes is an error at once, not a wait for a reader, and neither a symbolic nor a
/// hard link is written through or truncated.
#[cfg(unix)]
#[test]
fn a_planted_stamp_is_never_written_through() {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    use super::{EXEC_STAMP, try_stamp_exec};
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let stamp = |home: std::path::PathBuf| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(try_stamp_exec(&home).is_ok());
        });
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the write never waits")
    };
    let fifo = dir.path().join("1-2");
    std::fs::create_dir(&fifo).expect("mkdir");
    let name =
        std::ffi::CString::new(fifo.join(EXEC_STAMP).as_os_str().as_bytes()).expect("no NUL");
    // SAFETY: a NUL-terminated path the call reads and does not retain.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
    assert!(!stamp(fifo.clone()), "a FIFO with no reader");
    let reader = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(fifo.join(EXEC_STAMP))
        .expect("a reader");
    assert!(!stamp(fifo), "a FIFO with one");
    drop(reader);

    let linked = dir.path().join("3-4");
    std::fs::create_dir(&linked).expect("mkdir");
    let real = dir.path().join("someone-elses");
    std::fs::write(&real, "theirs").expect("file");
    std::os::unix::fs::symlink(&real, linked.join(EXEC_STAMP)).expect("symlink");
    assert!(!stamp(linked), "a link");
    assert_eq!(std::fs::read_to_string(&real).expect("kept"), "theirs");

    let hard = dir.path().join("7-8");
    std::fs::create_dir(&hard).expect("mkdir");
    let named = dir.path().join("someone-elses-too");
    std::fs::write(&named, "theirs").expect("file");
    std::fs::hard_link(&named, hard.join(EXEC_STAMP)).expect("hard link");
    assert!(!stamp(hard), "a hard link");
    assert_eq!(
        std::fs::read_to_string(&named).expect("kept"),
        "theirs",
        "neither truncated nor written"
    );

    let fresh = dir.path().join("5-6");
    std::fs::create_dir(&fresh).expect("mkdir");
    assert!(stamp(fresh.clone()), "a fresh copy");
    std::fs::write(fresh.join(EXEC_STAMP), "x".repeat(64)).expect("stamp");
    assert!(stamp(fresh.clone()), "a regular one again");
    assert!(
        std::fs::read_to_string(fresh.join(EXEC_STAMP))
            .expect("stamp")
            .len()
            < 64,
        "truncated, not appended to"
    );
}

const DEADLINE: Duration = Duration::from_millis(200);
const HANG: Duration = Duration::from_secs(60);

/// A reader that first waits for its release, as one the scheduler has not run yet.
#[cfg(unix)]
pub(super) struct Late<R> {
    pub(super) held: mpsc::Receiver<()>,
    pub(super) reader: R,
}

/// The pid of a child that led a process group of its own, exited and reaped: a group
/// nothing is left in.
#[cfg(unix)]
pub(super) fn an_empty_group() -> u32 {
    let mut child = crate::spawnfd::command("/bin/sh")
        .args(["-c", "exit 0"])
        .process_group(0)
        .spawn()
        .expect("a child");
    child.wait().expect("reaped");
    child.id()
}

/// A child that led a process group of its own and exited, left unreaped: a group whose
/// one member is a zombie, which Darwin answers a signal for with `EPERM`, not `ESRCH`.
/// Dropping it reaps it.
#[cfg(target_vendor = "apple")]
pub(super) fn a_group_left_only_a_zombie() -> crate::spawnfd::Child {
    const P_PID: i32 = 1;
    const WNOHANG: i32 = 0x01;
    const WEXITED: i32 = 0x04;
    const WNOWAIT: i32 = 0x20;
    unsafe extern "C" {
        fn waitid(idtype: i32, id: u32, info: *mut u8, options: i32) -> i32;
    }
    let child = crate::spawnfd::command("/bin/sh")
        .args(["-c", "exit 0"])
        .process_group(0)
        .spawn()
        .expect("a child");
    let since = Instant::now();
    loop {
        let mut info = [0u8; 128];
        // SAFETY: `info` is writable and larger than Darwin's 104-byte `siginfo_t`, which `waitid` fills and does not retain; `WNOWAIT` leaves the child waitable, so its `Child` still reaps it.
        let rc = unsafe {
            waitid(
                P_PID,
                child.id(),
                info.as_mut_ptr(),
                WEXITED | WNOWAIT | WNOHANG,
            )
        };
        assert_eq!(rc, 0, "waitid: {}", std::io::Error::last_os_error());
        if info[12..16] != [0; 4] {
            return child;
        }
        assert!(since.elapsed() < HANG, "the child never exited");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// `pipe`'s writer writes `more` and closes a moment from now: a holder outside the
/// child's group letting go at its exec.
#[cfg(unix)]
pub(super) fn lets_go(mut writer: std::io::PipeWriter, more: &'static [u8]) {
    use std::io::Write as _;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        let _ = writer.write_all(more);
    });
}

#[cfg(unix)]
impl<R: std::io::Read> std::io::Read for Late<R> {
    fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
        let _ = self.held.recv_timeout(HANG);
        self.reader.read(into)
    }
}

#[cfg(unix)]
#[test]
fn advancing_progress_does_not_renew_an_absolute_native_child_deadline() {
    let mut command = crate::spawnfd::command("/bin/sleep");
    command.arg("2");
    let mut observations = 0;
    let result = super::output_while_progressing_until(
        &mut command,
        HANG,
        Instant::now() + Duration::from_millis(200),
        &mut || {
            observations += 1;
            observations
        },
    );
    assert!(matches!(result, Err(super::RunError::TimedOut)));
    assert!(
        observations > 0,
        "the actual child was observed before its original bound"
    );
    let mut never = crate::spawnfd::command("/must-not-be-spawned");
    assert!(matches!(
        super::output_while_progressing_until(&mut never, HANG, Instant::now(), &mut || 0,),
        Err(super::RunError::TimedOut)
    ));
}

#[cfg(unix)]
#[test]
fn output_within_from_feeds_its_stdin_and_ends_the_child_at_its_bound() {
    use std::io::Write as _;
    let cat = || crate::spawnfd::command("/bin/cat");
    let (reader, mut writer) = crate::pipe().expect("pipe");
    writer.write_all(b"fed\n").expect("write");
    drop(writer);
    let out = super::output_within_from(&mut cat(), reader.into(), HANG).expect("cat");
    assert_eq!(out.stdout, b"fed\n");
    let (reader, writer) = crate::pipe().expect("pipe");
    let (done, ended) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(super::output_within_from(
            &mut cat(),
            reader.into(),
            Duration::from_millis(100),
        ));
    });
    let result = ended
        .recv_timeout(HANG)
        .expect("a child still reading ends at its bound within the hang detector");
    drop(writer);
    assert!(matches!(result, Err(RunError::TimedOut)), "{result:?}");
}

/// A pipe no writer holds is read to its end past the grace, however late its reader
/// runs: the grace is for a descendant holding the pipe open, and a reader a loaded
/// test binary left unscheduled lost `ps`'s one line. Negative control: a `collect`
/// that returns at `by` whatever the pipe gives returns nothing here.
#[cfg(unix)]
#[test]
fn a_pipe_no_writer_holds_is_read_to_its_end_past_the_grace() {
    use std::io::Write as _;
    let (reader, mut writer) = crate::pipe().expect("pipe");
    writer
        .write_all(b"Thu Oct 22 16:44:35 2026\n")
        .expect("write");
    drop(writer);
    let pipe = super::pipe_end(&reader);
    let (release, held) = mpsc::channel();
    let drain = super::drain(Late { held, reader }, pipe, "late").expect("reader");
    let grace = super::Grace {
        by: Instant::now(),
        until: Instant::now() + HANG,
        group: Some(an_empty_group()),
    };
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        let _ = release.send(());
    });
    assert_eq!(drain.collect(&grace), b"Thu Oct 22 16:44:35 2026\n");

    struct Released<R>(Late<R>, mpsc::Sender<()>);
    impl<R: std::io::Read> std::io::Read for Released<R> {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            std::io::Read::read(&mut self.0, bytes)
        }
    }
    impl<R> Drop for Released<R> {
        fn drop(&mut self) {
            let _ = self.1.send(());
        }
    }
    for panics in [false, true] {
        let (reader, mut writer) = crate::pipe().expect("cancellable pipe");
        writer.write_all(b"unpublished tail").unwrap();
        drop(writer);
        let pipe = super::pipe_end(&reader);
        let (release, held) = mpsc::channel();
        let (dropped, gone) = mpsc::channel();
        let drain = super::drain(
            Released(Late { held, reader }, dropped),
            pipe,
            "cancellable-late",
        )
        .unwrap();
        let grace = super::Grace {
            by: Instant::now(),
            until: Instant::now() + HANG,
            group: Some(an_empty_group()),
        };
        let mut first = true;
        let mut stopped = || {
            if std::mem::take(&mut first) {
                false
            } else {
                assert!(!panics, "injected post-exit observer unwind");
                true
            }
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drain.collect_unless(&grace, &mut Some(&mut stopped))
        }));
        release.send(()).expect("release this capture's reader");
        drop(release);
        gone.recv_timeout(HANG)
            .expect("the owned reader released its pipe");
        if panics {
            assert!(result.is_err());
        } else {
            assert!(matches!(result, Ok(Err(RunError::TimedOut))));
        }
    }
}

/// A pipe only something outside the exited child's group holds is read until it lets
/// go, within the call's deadline: a child another thread of this process forked holds
/// it until its exec, and a loaded test binary kept one past the grace. Negative
/// control: a drain that stops at `by` here returns at most `one `.
#[cfg(unix)]
#[test]
fn a_pipe_held_outside_the_childs_group_is_read_until_it_lets_go() {
    use std::io::Write as _;
    let (reader, mut writer) = crate::pipe().expect("pipe");
    writer.write_all(b"one ").expect("write");
    let pipe = super::pipe_end(&reader);
    let drain = super::drain(reader, pipe, "outside").expect("reader");
    let grace = super::Grace {
        by: Instant::now(),
        until: Instant::now() + HANG,
        group: Some(an_empty_group()),
    };
    lets_go(writer, b"two");
    assert_eq!(drain.collect(&grace), b"one two");
}

/// CONTROL: a pipe held while the exited child's group still has a member (its
/// descendant) gives at `by` what it gave by then.
#[cfg(unix)]
#[test]
fn a_pipe_held_while_the_childs_group_lives_gives_what_it_gave_by_the_grace() {
    let mut member = crate::spawnfd::command("/bin/sleep")
        .arg("600")
        .process_group(0)
        .spawn()
        .expect("a member of the group");
    let (reader, writer) = crate::pipe().expect("pipe");
    let pipe = super::pipe_end(&reader);
    let drain = super::drain(reader, pipe, "descendant").expect("reader");
    let begun = Instant::now();
    let grace = super::Grace {
        by: Instant::now(),
        until: Instant::now() + HANG,
        group: Some(member.id()),
    };
    let got = drain.collect(&grace);
    let waited = begun.elapsed();
    let _ = member.kill();
    let _ = member.wait();
    drop(writer);
    assert!(got.is_empty(), "{got:?}");
    assert!(waited < HANG, "waited on a pipe a descendant holds");
}

fn witness() -> (impl FnOnce() + Send + 'static, mpsc::Receiver<()>) {
    let (ran, seen) = mpsc::channel::<()>();
    (
        move || {
            let _ = ran.send(());
        },
        seen,
    )
}

fn never_ran(seen: &mpsc::Receiver<()>) -> bool {
    seen.recv_timeout(HANG) == Err(mpsc::RecvTimeoutError::Disconnected)
}

fn drained(gate: &Gate) {
    let since = Instant::now();
    while gate.running() != 0 {
        assert!(since.elapsed() < HANG, "a released helper never returned");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn started(gate: &Gate, n: usize) {
    let since = Instant::now();
    while gate.running() < n {
        assert!(since.elapsed() < HANG, "a helper never started");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn work_that_never_answers_is_refused_at_the_deadline_and_piles_up_no_threads() {
    static GATE: Gate = Gate::new("test-gate");

    let (release, held) = mpsc::channel::<()>();
    let begun = Instant::now();
    let first = GATE.run(HANG, DEADLINE, move || {
        let _ = held.recv();
        "late"
    });
    let waited = begun.elapsed();
    assert_eq!(first, Err(Refused::Unanswered));
    assert!(
        waited >= DEADLINE,
        "returned before the deadline: {waited:?}"
    );
    assert!(waited < HANG, "the call did not come back: {waited:?}");
    assert_eq!(GATE.running(), 1, "the stuck helper is still running");

    let (work, seen) = witness();
    assert_eq!(GATE.run(HANG, DEADLINE, work), Err(Refused::Unanswered));
    assert!(
        never_ran(&seen),
        "a second helper started behind a stuck one"
    );
    assert_eq!(GATE.running(), 1);

    release.send(()).expect("the stuck helper is still waiting");
    drained(&GATE);
    assert_eq!(GATE.run(HANG, HANG, || 7), Ok(7));
}

#[test]
fn calls_that_arrive_after_the_suspect_age_start_no_helper_while_the_first_still_waits() {
    const SUSPECT: Duration = Duration::from_millis(100);
    static GATE: Gate = Gate::new("test-gate");

    let (release, held) = mpsc::channel::<()>();
    let first = std::thread::spawn(|| {
        GATE.run(SUSPECT, HANG, move || {
            let _ = held.recv();
        })
    });
    started(&GATE, 1);
    for _ in 0..2 {
        std::thread::sleep(SUSPECT + Duration::from_millis(20));
        let (work, seen) = witness();
        let asked = Instant::now();
        assert_eq!(GATE.run(HANG, HANG, work), Err(Refused::Unanswered));
        assert!(
            asked.elapsed() < HANG,
            "a later call waited instead of refusing"
        );
        assert!(never_ran(&seen), "a later call started a helper");
        assert_eq!(GATE.running(), 1, "only the first call has a helper");
    }

    release.send(()).expect("the stuck helper is still waiting");
    assert_eq!(first.join().expect("the first caller"), Ok(()));
    drained(&GATE);
    assert_eq!(GATE.run(HANG, HANG, || "answered"), Ok("answered"));
}

#[test]
fn a_long_honest_call_does_not_make_a_short_one_refuse_until_it_is_overdue_itself() {
    static GATE: Gate = Gate::new("test-gate");
    const SHORT: Duration = Duration::from_millis(50);

    let (release, held) = mpsc::channel::<()>();
    let slow = std::thread::spawn(|| {
        GATE.run(HANG, HANG, move || {
            let _ = held.recv();
            "hashed"
        })
    });
    started(&GATE, 1);
    std::thread::sleep(SHORT * 3);
    assert_eq!(GATE.run(SHORT, HANG, || "quick"), Ok("quick"));

    release.send(()).expect("the slow helper is still waiting");
    assert_eq!(slow.join().expect("the slow caller"), Ok("hashed"));
    drained(&GATE);
}

#[test]
fn a_started_call_past_its_suspect_age_is_still_its_callers_answer_and_refuses_the_others() {
    const SUSPECT: Duration = Duration::from_millis(50);
    static GATE: Gate = Gate::new("test-gate");

    let (release, held) = mpsc::channel::<()>();
    let mut pending = GATE
        .start(SUSPECT, move || {
            let _ = held.recv();
            "answered"
        })
        .expect("the gate is idle");
    std::thread::sleep(SUSPECT * 2);
    for _ in 0..3 {
        pending = match pending.wait(Duration::from_millis(5)) {
            Err(still) => still,
            Ok(answer) => panic!("answered before its work returned: {answer:?}"),
        };
    }
    let (work, seen) = witness();
    assert_eq!(GATE.run(HANG, HANG, work), Err(Refused::Unanswered));
    assert!(never_ran(&seen), "a call started behind an overdue one");

    release.send(()).expect("the helper is still waiting");
    let answer = pending.wait(HANG);
    assert!(matches!(answer, Ok(Ok("answered"))), "{answer:?}");
    drained(&GATE);
    assert_eq!(GATE.run(HANG, HANG, || 7), Ok(7));
}

#[test]
fn work_that_panics_is_a_failure_and_does_not_read_as_a_wedged_service() {
    static GATE: Gate = Gate::new("test-gate");
    let result: Result<(), Refused> = GATE.run(HANG, HANG, || panic!("stand-in failure"));
    assert!(matches!(result, Err(Refused::Failed(_))), "{result:?}");
    drained(&GATE);
    assert_eq!(GATE.run(HANG, HANG, || "answered"), Ok("answered"));
}

#[cfg(unix)]
#[test]
fn a_child_that_finishes_gives_its_status_and_both_streams() {
    let out = output_until(
        crate::spawnfd::command("/bin/sh")
            .arg("-c")
            .arg("printf out; printf err >&2; exit 3"),
        Instant::now() + HANG,
    )
    .expect("ran");
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(out.stdout, b"out");
    assert_eq!(out.stderr, b"err");
}

/// SIGKILLs `pid` if it is a number (a test's own stray, by pid).
#[cfg(unix)]
fn kill_stray(pid: Option<u32>) {
    if let Some(pid) = pid {
        let _ = crate::spawnfd::command("/bin/kill")
            .args(["-KILL", &pid.to_string()])
            .status();
    }
}

/// The pid a shell's `echo` wrote to `file`, once its whole line is there.
#[cfg(unix)]
fn pid_in(file: &std::path::Path) -> Option<u32> {
    std::fs::read_to_string(file).ok().and_then(|text| {
        text.strip_suffix('\n')
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))?
            .parse::<u32>()
            .ok()
    })
}

/// Waits (under the hang detector) until a shell has written its pid to `file`.
#[cfg(unix)]
fn wrote_pid(file: &std::path::Path) {
    let since = Instant::now();
    while pid_in(file).is_none() && since.elapsed() < HANG {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// How often a run under a [`ClockHeld`] looks at its clock while it is held.
#[cfg(unix)]
const LOOK: Duration = Duration::from_millis(10);

/// This thread's deadline clock, held at the moment it was made until `ready()` holds or
/// the hang detector runs out, then the real one; dropped, the real one.
#[cfg(unix)]
struct ClockHeld {
    at: Instant,
    /// The first reading past the hold.
    ran: std::rc::Rc<std::cell::Cell<Option<Instant>>>,
}

#[cfg(unix)]
impl ClockHeld {
    fn until(ready: impl Fn() -> bool + 'static) -> Self {
        let at = Instant::now();
        let ran = std::rc::Rc::new(std::cell::Cell::new(None));
        let theirs = std::rc::Rc::clone(&ran);
        super::set_deadline_clock_for_test(Some(Box::new(move || {
            if theirs.get().is_none() && at.elapsed() < HANG && !ready() {
                return at;
            }
            let now = Instant::now();
            theirs.set(theirs.get().or(Some(now)));
            now
        })));
        Self { at, ran }
    }

    /// A deadline one look past the held clock.
    fn deadline(&self) -> Instant {
        self.at + LOOK
    }

    /// How long since the clock ran past its hold; zero if it never did.
    fn since_it_ran(&self) -> Duration {
        self.ran.get().map_or(Duration::ZERO, |ran| ran.elapsed())
    }
}

#[cfg(unix)]
impl Drop for ClockHeld {
    fn drop(&mut self) {
        super::set_deadline_clock_for_test(None);
    }
}

/// Waits (under the hang detector) until `pid` is gone; its new parent reaps it.
#[cfg(unix)]
fn gone(pid: u32) -> bool {
    let since = Instant::now();
    while crate::process::pid_alive(pid) {
        if since.elapsed() >= HANG {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

/// The call returns after the drain grace, not at the deadline, with what the child wrote.
#[cfg(unix)]
#[test]
fn a_child_that_exits_while_a_descendant_holds_its_pipe_returns_what_it_wrote() {
    let dir = std::env::temp_dir().join(format!("uds-deadline-gc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let pid_file = dir.join("pid");
    let mut cmd = crate::spawnfd::command("/bin/sh");
    cmd.arg("-c")
        .arg("sleep 600 & echo $! > \"$0\"; echo hi")
        .arg(&pid_file);
    let begun = Instant::now();
    let out = output_until(&mut cmd, Instant::now() + HANG * 2);
    let waited = begun.elapsed();
    kill_stray(pid_in(&pid_file));
    let _ = std::fs::remove_dir_all(&dir);
    let out = out.expect("ran");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.stdout, b"hi\n");
    assert!(waited < HANG, "waited out the deadline: {waited:?}");
}

/// A child past its deadline is killed and reaped, its descendants with it. The deadline's
/// clock stands short of it until the shell has written its descendant's pid and its own,
/// so both are checked on every run. Negative control: a `kill_group` that signals only
/// the child leaves the descendant running past the hang detector.
#[cfg(unix)]
#[test]
fn a_child_past_its_deadline_is_killed_and_reaped() {
    let dir = scratch("past");
    let pid_file = dir.join("pid");
    let grandchild_file = dir.join("grandchild");
    let mut cmd = sh(
        "sleep 600 & echo $! > \"$1\"; echo $$ > \"$0\"; exec sleep 600",
        &[&pid_file, &grandchild_file],
    );
    let both = [pid_file.clone(), grandchild_file.clone()];
    let clock = ClockHeld::until(move || both.iter().all(|file| pid_in(file).is_some()));
    let result = output_until(&mut cmd, clock.deadline());
    let waited = clock.since_it_ran();
    drop(clock);
    let child = pid_in(&pid_file);
    let grandchild = pid_in(&grandchild_file);
    let _ = std::fs::remove_dir_all(&dir);
    let (Some(child), Some(grandchild)) = (child, grandchild) else {
        panic!("the shell wrote no pids in a minute: {child:?} {grandchild:?}");
    };
    assert!(matches!(result, Err(RunError::TimedOut)), "{result:?}");
    assert!(waited < HANG, "the call did not come back: {waited:?}");
    assert!(
        !crate::process::pid_alive(child),
        "pid {child} outlived the call"
    );
    let dead = gone(grandchild);
    kill_stray((!dead).then_some(grandchild));
    assert!(
        dead,
        "the child's descendant {grandchild} outlived the call"
    );
}

/// A stop ends the running children long before their deadline, each killed and reaped
/// within `reap` with its group, as the deadline would, and the others keep their answers (a
/// tool that could not start is a spawn error): the stop reads true once the wedged child has
/// written its pid. Negative control: an `outputs_until_unless` that never reads its stop runs
/// to the hang detector's deadline and fails the `waited` check.
#[cfg(unix)]
#[test]
fn a_stop_ends_the_running_children_as_the_deadline_does() {
    let dir = scratch("many-stopped");
    let pid_file = dir.join("pid");
    let mut wedged = sh("echo $$ > \"$0\"; exec sleep 600", &[&pid_file]);
    let mut missing = crate::spawnfd::command("/nonexistent/tool");
    let began = Instant::now();
    let mut stopped = || pid_in(&pid_file).is_some();
    let results = super::outputs_until_unless(
        &mut [&mut wedged, &mut missing],
        began + HANG,
        Duration::from_secs(1),
        &mut stopped,
    );
    let waited = began.elapsed();
    let child = pid_in(&pid_file);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(child) = child else {
        panic!("the shell wrote no pid");
    };
    assert!(waited < HANG, "the stop did not end the call: {waited:?}");
    let mut results = results.into_iter();
    assert!(matches!(results.next(), Some(Err(RunError::TimedOut))));
    assert!(matches!(results.next(), Some(Err(RunError::Spawn(_)))));
    assert!(
        !crate::process::pid_alive(child),
        "pid {child} outlived the stop"
    );
}

/// A stop ends the child long before its deadline, killed and reaped with its descendants,
/// as a deadline would: the stop reads true once the shell has written both pids, so both
/// are checked on every run. Negative control: an `output_until_from_unless` that never
/// reads its stop runs to the hang detector's deadline and fails the `waited` check.
#[cfg(unix)]
#[test]
fn a_stop_ends_the_child_and_its_group_before_the_deadline() {
    let dir = scratch("stopped");
    let pid_file = dir.join("pid");
    let grandchild_file = dir.join("grandchild");
    let mut cmd = sh(
        "sleep 600 & echo $! > \"$1\"; echo $$ > \"$0\"; exec sleep 600",
        &[&pid_file, &grandchild_file],
    );
    let began = Instant::now();
    let both = [pid_file.clone(), grandchild_file.clone()];
    let mut stopped = || both.iter().all(|file| pid_in(file).is_some());
    let result = super::output_until_from_unless(
        &mut cmd,
        crate::spawnfd::Stdio::null(),
        began + HANG,
        &mut stopped,
    );
    let waited = began.elapsed();
    let child = pid_in(&pid_file);
    let grandchild = pid_in(&grandchild_file);
    let _ = std::fs::remove_dir_all(&dir);
    let (Some(child), Some(grandchild)) = (child, grandchild) else {
        panic!("the shell wrote no pids: {child:?} {grandchild:?}");
    };
    assert!(matches!(result, Err(RunError::TimedOut)), "{result:?}");
    assert!(waited < HANG, "the stop did not end the call: {waited:?}");
    assert!(
        !crate::process::pid_alive(child),
        "pid {child} outlived the stop"
    );
    let dead = gone(grandchild);
    kill_stray((!dead).then_some(grandchild));
    assert!(
        dead,
        "the child's descendant {grandchild} outlived the stop"
    );
}

/// A child still forking at its deadline is killed with every child it forks. The shell
/// forks a `sleep` and kills it, over and over, so the deadline's group `SIGKILL` lands while
/// it forks, and on XNU the child of that fork joins the group unsignalled
/// ([`crate::process::kill_group_until_empty`]). Negative control: a `kill_group` that
/// signals the group once leaves a `sleep 600` in it past the hang detector.
#[cfg(unix)]
#[test]
fn a_group_still_forking_at_its_deadline_is_killed_whole() {
    let dir = scratch("forking");
    let pid_file = dir.join("pid");
    let mut cmd = sh(
        "echo $$ > \"$0\"; while :; do sleep 600 & kill $!; done",
        &[&pid_file],
    );
    let ready = pid_file.clone();
    let clock = ClockHeld::until(move || pid_in(&ready).is_some());
    let result = output_until(&mut cmd, clock.deadline());
    drop(clock);
    let group = pid_in(&pid_file);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(group) = group.and_then(|pid| i32::try_from(pid).ok()) else {
        panic!("the shell wrote no pid in a minute");
    };
    assert!(matches!(result, Err(RunError::TimedOut)), "{result:?}");
    assert!(
        group_emptied(group),
        "a child forked as group {group} was killed ran on"
    );
}

/// Whether process group `group`, whose leader is reaped, has emptied (`ESRCH`) within the
/// hang detector. One that has not is sent `SIGKILL`, so a failing run leaves nothing behind.
#[cfg(unix)]
fn group_emptied(group: i32) -> bool {
    let since = Instant::now();
    loop {
        // SAFETY: a plain syscall; signal 0 delivers nothing.
        if unsafe { libc::kill(-group, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return true;
        }
        if since.elapsed() >= HANG {
            // SAFETY: a plain syscall to the group this test's child made, which a member
            // still holds (the id of a group with a member is no new process's).
            unsafe { libc::kill(-group, libc::SIGKILL) };
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn a_missing_program_fails_at_spawn() {
    assert!(matches!(
        output_until(
            &mut crate::spawnfd::command("/nonexistent/tool"),
            Instant::now() + HANG
        ),
        Err(RunError::Spawn(_))
    ));
}

#[cfg(unix)]
#[test]
fn legacy_capture_failure_after_a_child_effect_is_not_a_spawn_refusal() {
    let world = aterm_tempfile::tempdir().unwrap();
    let touch = super::stand_in(std::path::Path::new("/usr/bin/touch"));
    let marker = world.path().join("child-ran");
    let observed = marker.clone();
    let witnessed = std::rc::Rc::new(std::cell::Cell::new(false));
    let callback_witness = std::rc::Rc::clone(&witnessed);
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let callback_calls = std::rc::Rc::clone(&calls);
    let failure = super::fail_next_drain_start(move || {
        callback_calls.set(callback_calls.get() + 1);
        let until = Instant::now() + HANG;
        while !observed.is_file() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        callback_witness.set(observed.is_file());
        std::io::Error::other("injected legacy reader-start failure")
    });

    let mut missing = crate::spawnfd::command(world.path().join("no-executable"));
    assert!(matches!(
        output_until(&mut missing, Instant::now() + HANG),
        Err(RunError::Spawn(_))
    ));
    assert_eq!(
        calls.get(),
        0,
        "a no-child refusal never initializes a reader"
    );

    let mut child = crate::spawnfd::command(&touch);
    child.arg(&marker);
    let result = output_until(&mut child, Instant::now() + HANG);
    assert!(
        witnessed.get(),
        "the real child must demonstrate its file effect before injection"
    );
    assert!(marker.is_file());
    assert!(matches!(result, Err(RunError::Capture(error))
        if error.kind() == std::io::ErrorKind::Other
            && error.to_string() == "injected legacy reader-start failure"));
    assert_eq!(calls.get(), 1);

    let following = world.path().join("following-child-ran");
    let mut next = crate::spawnfd::command(&touch);
    next.arg(&following);
    assert!(
        output_until(&mut next, Instant::now() + HANG)
            .unwrap()
            .status
            .success()
    );
    assert!(following.is_file(), "the injection was consumed once");
    drop(failure);
}

/// A shell running `script` with `args` as `$0`, `$1`, ….
#[cfg(unix)]
fn sh(script: &str, args: &[&std::path::Path]) -> crate::spawnfd::Command {
    let mut cmd = crate::spawnfd::command("/bin/sh");
    cmd.arg("-c").arg(script).args(args);
    cmd
}

/// A scratch directory of this test's own.
#[cfg(unix)]
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("uds-deadline-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}

/// The wait ends at the exit, not at its timeout (the hang detector).
#[cfg(unix)]
#[test]
fn an_exit_wait_ends_at_the_exit() {
    let mut child = crate::spawnfd::command("/bin/sh")
        .args(["-c", "exit 4"])
        .spawn()
        .expect("spawn");
    let mut exit = ExitWait::new(&child);
    let begun = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        exit.wait(HANG);
    };
    assert!(begun.elapsed() < HANG, "the wait outlived the exit");
    assert_eq!(status.code(), Some(4));
}

/// Where the kernel gives no word of the exit, the fallback sleep doubles from 1 ms up to
/// its cap: [`super::POLL_MAX`]'s 25 ms by default, a caller's `ticking_at_most` (no less
/// than 1 ms, never a spin) when it asks for less. The sleeps are read off the handle, not
/// timed: each wait's timeout is zero.
#[cfg(unix)]
#[test]
fn an_exit_wait_without_the_kernels_word_sleeps_no_longer_than_its_cap() {
    let sleeps = |exit: ExitWait| {
        let mut exit = ExitWait { fd: None, ..exit };
        (0..7)
            .map(|_| {
                let sleep = exit.tick.as_millis();
                exit.wait(Duration::ZERO);
                sleep
            })
            .collect::<Vec<_>>()
    };
    let pid = std::process::id();
    assert_eq!(sleeps(ExitWait::on_pid(pid)), [1, 2, 4, 8, 16, 25, 25]);
    assert_eq!(
        sleeps(ExitWait::on_pid(pid).ticking_at_most(Duration::from_millis(10))),
        [1, 2, 4, 8, 10, 10, 10]
    );
    assert_eq!(
        sleeps(ExitWait::on_pid(pid).ticking_at_most(Duration::ZERO)),
        [1, 1, 1, 1, 1, 1, 1]
    );
}

/// The window the two progress tests hold a child to: short, so a stall is named in well
/// under a second.
#[cfg(unix)]
const WINDOW: Duration = Duration::from_millis(300);

/// Injected clock and probe generations: byte and descendant-work progress keep an
/// operation alive beyond the former two-minute wall budget. Mutating either generation
/// comparison away makes one of the later observations read as stalled.
#[test]
fn process_progress_outlives_a_wall_budget_while_evidence_advances() {
    let start = Instant::now();
    let window = Duration::from_secs(60);
    let mut progress = super::ProgressWindow::new(window, start);
    assert!(!progress.remaining(start, 0, Some(0)).is_zero());
    assert!(
        !progress
            .remaining(start + Duration::from_secs(61), 1, Some(0))
            .is_zero()
    );
    assert!(
        !progress
            .remaining(start + Duration::from_secs(122), 1, Some(1))
            .is_zero()
    );
    assert!(
        !progress
            .remaining(start + Duration::from_secs(183), 2, Some(1))
            .is_zero(),
        "progressing work is not a two-minute latency budget"
    );
}

#[test]
fn descendant_progress_excludes_polling_but_counts_worker_work() {
    let mut probe = super::ProcessProgressProbe::default();
    let mut reading = vec![
        super::ProcessProgressMember {
            pid: 1,
            start: 1,
            cpu_ns: 10,
            io_bytes: Some(10),
        },
        super::ProcessProgressMember {
            pid: 2,
            start: 2,
            cpu_ns: 20,
            io_bytes: Some(20),
        },
    ];
    let read = |probe: &mut super::ProcessProgressProbe, reading: &[_]| {
        let mut reading = reading.to_vec();
        super::exclude_supervisor_work(&mut reading, 1);
        probe.read_over(Some(reading))
    };
    assert_eq!(read(&mut probe, &reading), Some(0));
    reading[0].cpu_ns += 10;
    reading[0].io_bytes = Some(30);
    assert_eq!(
        read(&mut probe, &reading),
        Some(0),
        "polling is not progress"
    );
    reading[1].cpu_ns += 10;
    assert_eq!(read(&mut probe, &reading), Some(1));
    reading[1].io_bytes = Some(30);
    assert_eq!(read(&mut probe, &reading), Some(2));
    assert_eq!(probe.read_over(None), None);
}

/// Negative control: a complete probe whose bytes and descendant-work generation stay flat
/// reaches zero at exactly one injected evidence window, without a production-minute sleep.
#[test]
fn process_progress_stalls_after_one_whole_quiet_window() {
    let start = Instant::now();
    let window = Duration::from_secs(60);
    let mut progress = super::ProgressWindow::new(window, start);
    assert!(!progress.remaining(start, 7, Some(11)).is_zero());
    assert!(
        !progress
            .remaining(start + window - Duration::from_nanos(1), 7, Some(11))
            .is_zero()
    );
    assert!(progress.remaining(start + window, 7, Some(11)).is_zero());
}

/// The collector uses the same injected clock/probe evidence to end and reap a stalled
/// process group, and preserves the caller's safe operation label. NEGATIVE CONTROL: a
/// collector that only returns the finding without `settle_child` leaves the sleep alive.
#[cfg(unix)]
#[test]
fn stalled_process_progress_is_named_and_the_owned_group_ends() {
    let dir = scratch("process-progress-stalled");
    let pid_file = dir.join("pid");
    let mut cmd = sh("echo $$ > \"$0\"; exec sleep 600", &[&pid_file]);
    let start = Instant::now();
    let mut reads = 0u32;
    let mut now = || {
        let at = start + Duration::from_secs(u64::from(reads) * 61);
        reads = reads.saturating_add(1);
        at
    };
    let proof = pid_file.clone();
    let mut probe = move |_| proof.is_file().then_some(0);
    let result = super::output_from_while_process_progressing_with(
        &mut cmd,
        crate::spawnfd::Stdio::null(),
        Duration::from_secs(60),
        "git fetch origin/main".to_owned(),
        &mut now,
        &mut probe,
        None,
    );
    let child = pid_in(&pid_file).expect("the stalled child wrote its pid");
    let _ = std::fs::remove_dir_all(&dir);
    let error = result.expect_err("flat injected progress must stall");
    assert!(
        matches!(
            &error,
            super::ProgressError::Stalled {
                window,
                ..
            } if *window == Duration::from_secs(60)
        ),
        "{error:?}"
    );
    assert!(
        error
            .to_string()
            .contains("waiting on git fetch origin/main"),
        "{error}"
    );
    assert!(
        !crate::process::pid_alive(child),
        "stalled child {child} outlived the progress collector"
    );
}

/// NEGATIVE CONTROL of [`stalled_process_progress_is_named_and_the_owned_group_ends`]: the same
/// collector, window and clock (one that steps past a whole window on each read) keep a child
/// that writes nothing alive while the probe's reading advances on every read, and its output
/// comes back. The probe drops the flag the child waits for only on its fourth read, four
/// windows in on the injected clock, so a collector that stalled on the clock alone, or read
/// only captured bytes, would have ended the child at the second. The probe stops advancing
/// after [`HANG`], so a child that never answers stalls rather than hangs the test.
#[cfg(unix)]
#[test]
fn an_advancing_progress_probe_keeps_a_quiet_child_past_its_window() {
    let dir = scratch("process-progress-advancing");
    let flag = dir.join("flag");
    let mut cmd = sh(
        "while [ ! -e \"$0\" ]; do sleep 0.02; done; printf done",
        &[&flag],
    );
    let start = Instant::now();
    let mut reads = 0u32;
    let mut now = || {
        let at = start + Duration::from_secs(u64::from(reads) * 61);
        reads = reads.saturating_add(1);
        at
    };
    let mut reading = 0u64;
    let mut dropped = false;
    let mut probe = |_| {
        if start.elapsed() < HANG {
            reading += 1;
        }
        if reading >= 4 && !dropped {
            std::fs::write(&flag, b"").expect("the flag");
            dropped = true;
        }
        Some(reading)
    };
    let result = super::output_from_while_process_progressing_with(
        &mut cmd,
        crate::spawnfd::Stdio::null(),
        Duration::from_secs(60),
        "the quiet fixture".to_owned(),
        &mut now,
        &mut probe,
        None,
    );
    let _ = std::fs::remove_dir_all(&dir);
    let out = result.expect("an advancing probe is no stall");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.stdout, b"done");
    assert!(
        dropped && reads > 4,
        "the child answered only after the probe's fourth read: {reads} clock reads"
    );
}

/// An unread or incomparable descendant-tree probe is missing evidence, never zero work: it
/// disarms the old window and a later complete observation establishes a new baseline.
#[test]
fn unread_process_progress_disarms_and_rebaselines_the_window() {
    let start = Instant::now();
    let window = Duration::from_secs(60);
    let mut progress = super::ProgressWindow::new(window, start);
    assert!(!progress.remaining(start, 0, Some(0)).is_zero());
    assert!(
        !progress
            .remaining(start + Duration::from_secs(600), 0, None)
            .is_zero()
    );
    assert!(
        !progress
            .remaining(start + Duration::from_secs(1_200), 0, Some(0))
            .is_zero()
    );
    assert!(
        progress
            .remaining(start + Duration::from_secs(1_260), 0, Some(0))
            .is_zero()
    );
}

/// A child that keeps making progress outlives its window and answers: the probe's
/// reading moves every quarter window, and after three windows the probe drops the flag
/// the child waits for.
#[cfg(unix)]
#[test]
fn a_child_that_keeps_making_progress_outlives_its_window() {
    let dir = scratch("progressing");
    let flag = dir.join("flag");
    let mut cmd = sh(
        "while [ ! -e \"$0\" ]; do sleep 0.02; done; printf done",
        &[&flag],
    );
    let begun = Instant::now();
    let mut dropped = false;
    let mut probe = || {
        let age = begun.elapsed().min(HANG);
        if age >= WINDOW * 3 && !dropped {
            std::fs::write(&flag, b"").expect("the flag");
            dropped = true;
        }
        u64::try_from(age.as_millis() / (WINDOW / 4).as_millis()).unwrap_or(u64::MAX)
    };
    let out = output_while_progressing(&mut cmd, WINDOW, &mut probe);
    let waited = begun.elapsed();
    let _ = std::fs::remove_dir_all(&dir);
    let out = out.expect("a child making progress is no hang");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.stdout, b"done");
    assert!(
        waited >= WINDOW * 3,
        "the child ran past three windows: {waited:?}"
    );
}

/// A child that stops making progress is killed, with its group, one whole window after
/// its LAST step, not one window after it started, and the probe's last reading names the
/// step it stopped after: the reading moves on each of the probe's first five looks and
/// never again, while the child sleeps for ten minutes. The first look waits for the
/// shell's pid, so the kill is checked on every run. Negative control: an `output_by` that
/// returns at the stall without ending the child leaves it running.
#[cfg(unix)]
#[test]
fn a_child_that_stops_making_progress_is_killed_one_window_after_its_last_step() {
    const STEPS: u64 = 5;
    let dir = scratch("stalled");
    let pid_file = dir.join("pid");
    let mut cmd = sh("echo $$ > \"$0\"; exec sleep 600", &[&pid_file]);
    let begun = Instant::now();
    let mut reading = 0;
    let mut last_step = begun;
    let mut probe = || {
        if reading == 0 {
            wrote_pid(&pid_file);
        }
        if reading < STEPS {
            reading += 1;
            last_step = Instant::now();
        }
        reading
    };
    let result = output_while_progressing(&mut cmd, WINDOW, &mut probe);
    let killed = Instant::now();
    let child = pid_in(&pid_file);
    let _ = std::fs::remove_dir_all(&dir);
    let child = child.expect("the shell wrote no pid in a minute");
    assert!(matches!(result, Err(RunError::TimedOut)), "{result:?}");
    assert_eq!(reading, STEPS, "the stall is named by the last step");
    let quiet = killed.duration_since(last_step);
    assert!(
        quiet >= WINDOW,
        "killed {quiet:?} after the last step, inside the window"
    );
    assert!(
        killed.duration_since(last_step) < HANG,
        "the call did not come back"
    );
    assert!(
        !crate::process::pid_alive(child),
        "pid {child} outlived the call"
    );
}

/// An expired batch admits no spawn, including of a missing command. In a live batch,
/// the first child cannot finish until the third has run, which a one-after-another run
/// would reach only at the deadline.
#[cfg(unix)]
#[test]
fn several_children_run_at_once_and_each_answers_for_itself() {
    for stopped_variant in [false, true] {
        let mut missing = crate::spawnfd::command("/nonexistent/tool");
        let expired = Instant::now();
        let results = if stopped_variant {
            super::outputs_until_unless(&mut [&mut missing], expired, Duration::ZERO, &mut || false)
        } else {
            outputs_until(&mut [&mut missing], expired, Duration::ZERO)
        };
        assert!(
            matches!(results.as_slice(), [Err(RunError::TimedOut)]),
            "an expired batch must not attempt the command's spawn: {results:?}"
        );
    }
    let dir = scratch("many");
    let flag = dir.join("flag");
    let holder_file = dir.join("holder");
    let mut waits = sh(
        "while [ ! -e \"$0\" ]; do sleep 0.01; done; printf first; printf e1 >&2; exit 3",
        &[&flag],
    );
    let mut missing = crate::spawnfd::command("/nonexistent/tool");
    let mut signals = sh(": > \"$0\"; printf second", &[&flag]);
    let mut leaves = sh("sleep 600 & echo $! > \"$0\"; echo hi", &[&holder_file]);
    let begun = Instant::now();
    let results = outputs_until(
        &mut [&mut waits, &mut missing, &mut signals, &mut leaves],
        Instant::now() + HANG * 2,
        Duration::from_secs(1),
    );
    let waited = begun.elapsed();
    kill_stray(pid_in(&holder_file));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(waited < HANG, "waited out the deadline: {waited:?}");
    let mut results = results.into_iter();
    let first = results.next().expect("one").expect("the first ran");
    assert_eq!(first.status.code(), Some(3));
    assert_eq!(first.stdout, b"first");
    assert_eq!(first.stderr, b"e1");
    assert!(matches!(results.next(), Some(Err(RunError::Spawn(_)))));
    let third = results.next().expect("three").expect("the third ran");
    assert!(third.status.success());
    assert_eq!(third.stdout, b"second");
    let leaves = results.next().expect("four").expect("the fourth ran");
    assert!(leaves.status.success());
    assert_eq!(leaves.stdout, b"hi\n");
    assert!(results.next().is_none());
}

/// A child writing more than a pipe holds, on both streams, is drained while it runs and
/// keeps the tail past [`OUTPUT_KEEP`].
#[cfg(unix)]
#[test]
fn a_child_writing_more_than_a_pipe_holds_is_drained_while_it_runs() {
    let mut chatty = sh(
        "head -c 300000 /dev/zero; head -c 300000 /dev/zero >&2; exit 0",
        &[],
    );
    let mut flood = sh("head -c 1100000 /dev/zero | tr '\\0' a; printf END", &[]);
    let results = outputs_until(
        &mut [&mut chatty, &mut flood],
        Instant::now() + HANG,
        Duration::from_secs(1),
    );
    let mut results = results.into_iter();
    let chatty = results.next().expect("one").expect("ran");
    assert!(chatty.status.success());
    assert_eq!(chatty.stdout.len(), 300_000);
    assert_eq!(chatty.stderr.len(), 300_000);
    let flood = results.next().expect("two").expect("ran");
    assert_eq!(flood.stdout.len(), OUTPUT_KEEP);
    assert!(flood.stdout.ends_with(b"aaaEND"), "the tail is kept");
}

/// At the deadline a running child is killed with its group and reaped within `reap`; the
/// others keep their own answers (a tool that could not start is a spawn error, not a
/// timeout). The deadline's clock stands short of it until the shell has written its
/// descendant's pid and its own, so both are checked on every run. Negative control: a
/// `kill_group` that signals only the child leaves the descendant running past the hang
/// detector.
#[cfg(unix)]
#[test]
fn at_the_deadline_the_running_child_is_killed_and_reaped_and_the_rest_keep_their_answers() {
    let dir = scratch("many-deadline");
    let pid_file = dir.join("pid");
    let grandchild_file = dir.join("grandchild");
    let mut wedged = sh(
        "sleep 600 & echo $! > \"$1\"; echo $$ > \"$0\"; exec sleep 600",
        &[&pid_file, &grandchild_file],
    );
    let mut missing = crate::spawnfd::command("/nonexistent/tool");
    let both = [pid_file.clone(), grandchild_file.clone()];
    let clock = ClockHeld::until(move || both.iter().all(|file| pid_in(file).is_some()));
    // The missing command's own spawn is observed before the real child's proof
    // can advance the clock to the deadline.
    let results = outputs_until(&mut [&mut missing, &mut wedged], clock.deadline(), HANG);
    let waited = clock.since_it_ran();
    drop(clock);
    let child = pid_in(&pid_file);
    let grandchild = pid_in(&grandchild_file);
    let _ = std::fs::remove_dir_all(&dir);
    let (Some(child), Some(grandchild)) = (child, grandchild) else {
        panic!("the shell wrote no pids in a minute: {child:?} {grandchild:?}");
    };
    assert!(waited < HANG, "the call did not come back: {waited:?}");
    let mut results = results.into_iter();
    assert!(matches!(results.next(), Some(Err(RunError::Spawn(_)))));
    assert!(matches!(results.next(), Some(Err(RunError::TimedOut))));
    assert!(
        !crate::process::pid_alive(child),
        "pid {child} outlived the call"
    );
    let dead = gone(grandchild);
    kill_stray((!dead).then_some(grandchild));
    assert!(
        dead,
        "the child's descendant {grandchild} outlived the call"
    );
}

/// **A HELD CHILD RUNS ONLY ONCE RESUMED, TO ITS OUTPUT.** Held, the shell is exec'd and
/// stopped before its first instruction (`ps`: `T`) and has left no marker;
/// [`Held::output_until`] runs it to its status and both streams, as
/// [`output_until`] does from a spawn.
#[cfg(target_vendor = "apple")]
#[test]
fn a_held_child_runs_only_once_resumed_and_one_dropped_never_runs() {
    let state = |pid: u32| {
        let out = crate::spawnfd::command("/bin/ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .expect("run ps");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let dir = scratch("held");
    let marker = dir.join("ran");
    let held = Held::start(&mut sh(
        "printf ran > \"$0\"; printf out; printf err >&2; exit 3",
        &[&marker],
    ))
    .expect("held");
    assert!(state(held.id()).starts_with('T'), "{:?}", state(held.id()));
    assert!(!marker.exists(), "a held child ran");
    let out = held
        .output_until(Instant::now() + HANG, Duration::from_secs(1))
        .expect("ran");
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(out.stdout, b"out");
    assert_eq!(out.stderr, b"err");
    assert!(marker.exists(), "the resumed child ran");

    let unrun = dir.join("unrun");
    let dropped = Held::start(&mut sh("printf ran > \"$0\"", &[&unrun])).expect("held");
    let pid = dropped.id();
    drop(dropped);
    assert!(gone(pid), "the dropped held child {pid} outlived its drop");
    assert!(!unrun.exists(), "a held child dropped unresumed ran");

    let _ = std::fs::remove_dir_all(&dir);
}
