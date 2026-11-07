// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Strict stdout draining with injected reads and actual owned native pipes.

use super::*;
use std::io::Cursor;
use std::time::Duration;

const HANG: Duration = Duration::from_secs(60);

#[cfg(unix)]
#[test]
fn retained_reader_unwind_keeps_cleanup_ownership_until_its_actual_child_reaps() {
    struct SignalReaped(std::sync::mpsc::Sender<()>);
    impl Drop for SignalReaped {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let mut command = crate::spawnfd::command(crate::deadline::stand_in(std::path::Path::new(
        "/bin/sleep",
    )));
    command.arg("30");
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = capture_retained(
            &mut command,
            Instant::now() + HANG,
            Duration::ZERO,
            SignalReaped(tx),
            |_, _| panic!("injected retained reader-start unwind"),
        );
    }));
    assert!(panic.is_err());
    rx.recv_timeout(HANG)
        .expect("retained guards eventually drop after the actual child reaps");
}

#[cfg(unix)]
#[derive(Debug)]
struct RetainedGuard(std::sync::Arc<std::sync::atomic::AtomicUsize>);

#[cfg(unix)]
impl Drop for RetainedGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(unix)]
#[test]
fn retained_complete_capture_returns_guards_after_full_output_and_owning_reap() {
    use std::sync::{Arc, atomic::AtomicUsize, atomic::Ordering};
    let drops = Arc::new(AtomicUsize::new(0));
    let mut command =
        crate::spawnfd::command(crate::deadline::stand_in(std::path::Path::new("/bin/echo")));
    command.arg("complete retained answer");
    let (output, guard) = output_until_complete_stdout_retained(
        &mut command,
        Instant::now() + HANG,
        Duration::ZERO,
        RetainedGuard(Arc::clone(&drops)),
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"complete retained answer\n");
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(guard);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[cfg(unix)]
#[test]
fn retained_complete_spawn_refusal_preserves_no_child_and_original_guards() {
    use std::sync::{Arc, atomic::AtomicUsize, atomic::Ordering};
    let drops = Arc::new(AtomicUsize::new(0));
    let world = aterm_tempfile::tempdir().unwrap();
    let mut command = crate::spawnfd::command(world.path().join("no-executable"));
    let (error, mut custody) = output_until_complete_stdout_retained(
        &mut command,
        Instant::now() + HANG,
        Duration::ZERO,
        RetainedGuard(Arc::clone(&drops)),
    )
    .unwrap_err()
    .into_parts();
    assert!(matches!(
        error,
        CompleteOutputError::Run(RunError::Spawn(_))
    ));
    assert_eq!(
        custody.completion(),
        super::super::ChildCompletion::NotStarted
    );
    assert_eq!(custody.observe_until(Instant::now()).unwrap(), None);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(custody.try_into_retained().unwrap());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[cfg(unix)]
#[test]
fn retained_complete_timeout_preserves_cause_and_guards_until_positive_reap() {
    use std::sync::{Arc, atomic::AtomicUsize, atomic::Ordering};
    let drops = Arc::new(AtomicUsize::new(0));
    let mut command = crate::spawnfd::command(crate::deadline::stand_in(std::path::Path::new(
        "/bin/sleep",
    )));
    command.arg("30");
    let (error, mut custody) = output_until_complete_stdout_retained(
        &mut command,
        Instant::now(),
        Duration::ZERO,
        RetainedGuard(Arc::clone(&drops)),
    )
    .unwrap_err()
    .into_parts();
    assert!(matches!(
        error,
        CompleteOutputError::Run(RunError::TimedOut)
    ));
    assert_ne!(
        custody.completion(),
        super::super::ChildCompletion::NotStarted
    );
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let status = custody
        .observe_until(Instant::now() + HANG)
        .unwrap()
        .unwrap();
    assert!(!status.success());
    assert_eq!(
        custody.completion(),
        super::super::ChildCompletion::Reaped(status)
    );
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(custody.try_into_retained().unwrap());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[cfg(unix)]
#[test]
fn dispatch_witness_distinguishes_spawn_from_post_spawn_reader_failure() {
    if let Some(ready) = std::env::var_os("ATERM_COMPLETE_CANCEL_CHILD") {
        use std::io::Write as _;
        std::io::stdout()
            .write_all(b"owned cancellation prefix\n")
            .unwrap();
        std::io::stdout().flush().unwrap();
        std::fs::write(ready, std::process::id().to_string()).unwrap();
        // The parent keeps this private pipe open until the capture settles its child.
        std::io::stdin().read_exact(&mut [0]).unwrap();
        return;
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let started = std::cell::Cell::new(false);
    let mut missing = crate::spawnfd::command(dir.path().join("absent"));
    let error = output_until_complete_stdout_observed(&mut missing, Instant::now() + HANG, || {
        started.set(true)
    })
    .expect_err("no executable can be dispatched");
    assert!(matches!(
        error,
        CompleteOutputError::Run(RunError::Spawn(_))
    ));
    assert!(!started.get());

    let mut echo =
        crate::spawnfd::command(crate::deadline::stand_in(std::path::Path::new("/bin/echo")));
    echo.arg("owned dispatch witness");
    let error = capture(
        &mut echo,
        Stdio::null(),
        Instant::now() + HANG,
        || started.set(true),
        |pipe, stream| {
            assert!(
                started.get(),
                "the process-start witness precedes output readers"
            );
            let bytes = drain_stdout(pipe, stream)?
                .collect(&Grace::at(Instant::now() + HANG))
                .map_err(io::Error::other)?;
            assert_eq!(
                bytes, b"owned dispatch witness\n",
                "the child actually executed"
            );
            Err(io::Error::other("injected reader-thread spawn failure"))
        },
        None,
    )
    .expect_err("post-spawn reader failure is reported");
    assert!(matches!(
        error,
        CompleteOutputError::Run(RunError::Capture(error))
            if error.kind() == io::ErrorKind::Other
                && error.to_string() == "injected reader-thread spawn failure"
    ));
    assert!(
        started.get(),
        "a reader failure cannot erase possible child effects"
    );

    for panics in [false, true] {
        let ready = dir.path().join(format!("cancel-child-ran-{panics}"));
        let mut command = crate::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "deadline::complete::tests::dispatch_witness_distinguishes_spawn_from_post_spawn_reader_failure",
                "--nocapture",
            ])
            .env("ATERM_COMPLETE_CANCEL_CHILD", &ready);
        let (reader, writer) = std::io::pipe().unwrap();
        let pid = std::cell::Cell::new(None);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut stopped = || match std::fs::read_to_string(&ready) {
                Ok(ran) => {
                    pid.set(Some(ran.parse::<i32>().unwrap()));
                    assert!(!panics, "injected cancellation observer unwind");
                    true
                }
                Err(_) => false,
            };
            if panics {
                super::super::output_until_from_unless(
                    &mut command,
                    Stdio::from(reader),
                    Instant::now() + HANG,
                    &mut stopped,
                )
                .map_err(CompleteOutputError::Run)
            } else {
                output_until_complete_stdout_from_unless(
                    &mut command,
                    Stdio::from(reader),
                    Instant::now() + HANG,
                    &mut stopped,
                )
            }
        }));
        let pid = pid
            .get()
            .expect("the owned child executed before cancellation");
        let mut status = 0;
        // SAFETY: poll only the child whose own ready record identified it; sends no signal.
        let waited = unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) };
        let wait_error = std::io::Error::last_os_error().raw_os_error();
        drop(writer);
        if waited == 0 {
            // The negative control leaves it alive: stdin EOF ends its Rust read, so settle
            // it before asserting without ever signalling a possibly handed-off number.
            // SAFETY: an owning wait for the child identified above, never another child.
            let _ = unsafe { libc::waitpid(pid, &raw mut status, 0) };
        }
        assert_eq!(waited, -1, "the capture must perform its owning reap");
        assert_eq!(wait_error, Some(libc::ECHILD));
        if panics {
            assert!(result.is_err());
        } else {
            assert!(matches!(
                result,
                Ok(Err(CompleteOutputError::Run(RunError::TimedOut)))
            ));
        }
    }
}

#[cfg(unix)]
#[test]
fn an_observer_unwind_terminates_and_reaps_its_owned_child() {
    let mut child = crate::spawnfd::command(crate::deadline::stand_in(std::path::Path::new(
        "/bin/sleep",
    )))
    .arg("30")
    .process_group(0)
    .spawn()
    .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        observe_started(&mut child, || panic!("injected observer unwind"));
    }));
    assert!(result.is_err());
    assert!(
        child.try_wait().unwrap().is_some(),
        "owned child was reaped"
    );

    let mut child = crate::spawnfd::command(crate::deadline::stand_in(std::path::Path::new(
        "/bin/sleep",
    )))
    .arg("30")
    .process_group(0)
    .spawn()
    .unwrap();
    let mut waits = 0;
    super::super::settle_child_with(&mut child, |child| {
        waits += 1;
        if waits == 1 {
            assert!(!child.is_reaped());
            Err(io::Error::other("injected owning wait failure"))
        } else {
            child.wait()
        }
    });
    assert!(waits >= 2, "a wait error cannot release pending ownership");
    assert!(child.is_reaped());
}

fn collect(stream: impl Read + Send + 'static) -> Result<Vec<u8>, CompleteOutputError> {
    drain_stdout(None, stream)
        .expect("the pure reader starts")
        .collect(&Grace::at(Instant::now() + HANG))
}

/// A complete prefix followed by one injected read error (or one interrupted read first).
struct Ending {
    prefix: Cursor<Vec<u8>>,
    error: Option<io::ErrorKind>,
    interrupt: bool,
}

impl Read for Ending {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        if std::mem::take(&mut self.interrupt) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let n = self.prefix.read(into)?;
        if n != 0 {
            return Ok(n);
        }
        match self.error.take() {
            Some(error) => Err(error.into()),
            None => Ok(0),
        }
    }
}

#[test]
fn complete_stdout_accepts_eof_through_the_inclusive_limit() {
    for bytes in [
        Vec::new(),
        b"complete\x00\xff".to_vec(),
        vec![b'x'; OUTPUT_KEEP],
    ] {
        assert_eq!(collect(Cursor::new(bytes.clone())).unwrap(), bytes);
    }
    assert_eq!(
        collect(Ending {
            prefix: Cursor::new(b"after an interrupted read".to_vec()),
            error: None,
            interrupt: true,
        })
        .unwrap(),
        b"after an interrupted read"
    );
}

#[test]
fn complete_stdout_refuses_a_read_error_without_publishing_its_prefix() {
    assert_eq!(
        collect(Cursor::new(b"ordinary".to_vec())).unwrap(),
        b"ordinary"
    );
    let error = collect(Ending {
        prefix: Cursor::new(b"apparently valid short prefix".to_vec()),
        error: Some(io::ErrorKind::PermissionDenied),
        interrupt: false,
    })
    .expect_err("a read failure is not an empty or short complete report");
    assert!(matches!(error, CompleteOutputError::StdoutRead(ref e)
        if e.kind() == io::ErrorKind::PermissionDenied));
}

#[test]
fn complete_stdout_refuses_every_discarded_byte_after_eof() {
    assert_eq!(
        collect(Cursor::new(vec![b'x'; OUTPUT_KEEP])).unwrap().len(),
        OUTPUT_KEEP
    );
    let error = collect(Cursor::new(vec![b'x'; OUTPUT_KEEP + 1]))
        .expect_err("one discarded byte prevents complete capture");
    assert!(matches!(error, CompleteOutputError::StdoutTooLarge { limit } if limit == OUTPUT_KEEP));
}

#[cfg(unix)]
#[test]
fn explicit_stdout_limit_accepts_the_native_boundary_without_changing_the_default() {
    let world = aterm_tempfile::tempdir().unwrap();
    let path = world.path().join("complete-output");
    let limit = OUTPUT_KEEP + 1;
    let mut expected = vec![b'x'; limit];
    expected[limit - 1] = b'!';
    std::fs::write(&path, &expected).unwrap();
    let cat = crate::deadline::stand_in(std::path::Path::new("/bin/cat"));
    let command = || {
        let mut command = crate::spawnfd::command(&cat);
        command.arg(&path);
        command
    };
    assert!(matches!(
        output_until_complete_stdout_from_unless(
            &mut command(), Stdio::null(), Instant::now() + HANG, &mut || false,
        ),
        Err(CompleteOutputError::StdoutTooLarge { limit }) if limit == OUTPUT_KEEP
    ));
    let output =
        output_until_complete_stdout_with_limit(&mut command(), Instant::now() + HANG, limit)
            .unwrap();
    assert!(output.status.success());
    assert_eq!(
        output.stdout, expected,
        "the final byte came through actual child EOF"
    );
    assert!(matches!(
        output_until_complete_stdout_with_limit(
            &mut command(), Instant::now() + HANG, limit - 1,
        ),
        Err(CompleteOutputError::StdoutTooLarge { limit }) if limit == OUTPUT_KEEP
    ));
}

/// A LISTING IS READ WHOLE PAST THE SHARED HARD MAXIMUM, UP TO ITS OWN: one byte past
/// [`MAX_COMPLETE_STDOUT_BYTES`] comes back whole. NEGATIVE CONTROLS: the shared entry point
/// refuses that limit before dispatch, and the listing's refuses one past its own maximum.
#[cfg(unix)]
#[test]
fn a_listing_reads_past_the_shared_hard_maximum_up_to_its_own() {
    let world = aterm_tempfile::tempdir().unwrap();
    let mut missing = crate::spawnfd::command(world.path().join("absent"));
    for refused in [
        output_until_complete_listing(
            &mut missing,
            Instant::now() + HANG,
            MAX_COMPLETE_LISTING_BYTES + 1,
        ),
        output_until_complete_stdout_with_limit(
            &mut missing,
            Instant::now() + HANG,
            MAX_COMPLETE_STDOUT_BYTES + 1,
        ),
    ] {
        assert!(matches!(
            refused,
            Err(CompleteOutputError::Run(RunError::Spawn(error)))
                if error.kind() == io::ErrorKind::InvalidInput
        ));
    }
    let past = MAX_COMPLETE_STDOUT_BYTES + 1;
    let dd = crate::deadline::stand_in(std::path::Path::new("/bin/dd"));
    let out = output_until_complete_listing(
        crate::spawnfd::command(&dd).args([
            "if=/dev/zero".to_owned(),
            format!("bs={past}"),
            "count=1".to_owned(),
        ]),
        Instant::now() + HANG,
        past,
    )
    .unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout.len(), past);
}

#[test]
fn explicit_stdout_limit_has_a_hard_maximum_and_supports_empty_output() {
    let world = aterm_tempfile::tempdir().unwrap();
    let mut missing = crate::spawnfd::command(world.path().join("absent"));
    assert!(
        matches!(
            output_until_complete_stdout_with_limit(
                &mut missing, Instant::now() + HANG, MAX_COMPLETE_STDOUT_BYTES + 1,
            ),
            Err(CompleteOutputError::Run(RunError::Spawn(error)))
                if error.kind() == io::ErrorKind::InvalidInput
        ),
        "the hard-limit refusal precedes even an absent program's spawn error"
    );
    assert!(
        read_stdout(Cursor::new(Vec::<u8>::new()), 0)
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        read_stdout(Cursor::new(b"x"), 0),
        Err(CompleteOutputError::StdoutTooLarge { limit: 0 })
    ));
    assert_eq!(
        read_stdout(
            Cursor::new(vec![b'x'; MAX_COMPLETE_STDOUT_BYTES]),
            MAX_COMPLETE_STDOUT_BYTES
        )
        .unwrap()
        .len(),
        MAX_COMPLETE_STDOUT_BYTES
    );
    assert!(
        matches!(read_stdout(Cursor::new(vec![b'x'; MAX_COMPLETE_STDOUT_BYTES+1]),
        MAX_COMPLETE_STDOUT_BYTES), Err(CompleteOutputError::StdoutTooLarge { limit })
        if limit == MAX_COMPLETE_STDOUT_BYTES)
    );
}

/// The test knows the reader is waiting for release before EOF.
struct GatedEof {
    prefix: Cursor<Vec<u8>>,
    entered: mpsc::SyncSender<()>,
    release: mpsc::Receiver<()>,
    dropped: mpsc::SyncSender<()>,
}

impl Read for GatedEof {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        let n = self.prefix.read(into)?;
        if n != 0 {
            return Ok(n);
        }
        self.entered
            .send(())
            .map_err(|_| io::ErrorKind::BrokenPipe)?;
        self.release.recv().map_err(|_| io::ErrorKind::BrokenPipe)?;
        Ok(0)
    }
}

impl Drop for GatedEof {
    fn drop(&mut self) {
        let _ = self.dropped.send(());
    }
}

fn gated() -> (
    GatedEof,
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
    mpsc::Receiver<()>,
) {
    let (entered, observed) = mpsc::sync_channel(0);
    let (release, held) = mpsc::channel();
    let (dropped, gone) = mpsc::sync_channel(1);
    (
        GatedEof {
            prefix: Cursor::new(b"owned prefix".to_vec()),
            entered,
            release: held,
            dropped,
        },
        observed,
        release,
        gone,
    )
}

#[test]
fn complete_stdout_refuses_a_still_open_stream_and_accepts_proven_completion() {
    for limit in [OUTPUT_KEEP, MAX_COMPLETE_STDOUT_BYTES] {
        let (input, entered, release, dropped) = gated();
        let reader = drain_stdout_with_limit(None, input, limit).unwrap();
        entered
            .recv_timeout(HANG)
            .expect("the reader reached the EOF gate");
        let incomplete = reader.collect(&Grace::at(Instant::now()));
        release.send(()).expect("release this owned reader");
        dropped
            .recv_timeout(HANG)
            .expect("the incomplete reader released its input");
        assert!(matches!(
            incomplete,
            Err(CompleteOutputError::StdoutIncomplete)
        ));

        let (input, entered, release, dropped) = gated();
        let reader = drain_stdout_with_limit(None, input, limit).unwrap();
        entered
            .recv_timeout(HANG)
            .expect("the second reader reached the gate");
        release.send(()).expect("permit actual EOF");
        assert_eq!(
            reader.collect(&Grace::at(Instant::now() + HANG)).unwrap(),
            b"owned prefix"
        );
        dropped
            .recv_timeout(HANG)
            .expect("the completed reader released its input");
    }
}

#[cfg(unix)]
#[test]
fn complete_stdout_reads_a_pipe_no_writer_holds_to_its_end() {
    use crate::deadline::tests::{Late, an_empty_group};
    use std::io::Write as _;
    let (reader, mut writer) = std::io::pipe().expect("pipe");
    writer.write_all(b"owned prefix").expect("write");
    drop(writer);
    let pipe = pipe_end(&reader);
    let (release, held) = mpsc::channel();
    let drain = drain_stdout(pipe, Late { held, reader }).expect("reader");
    let grace = Grace {
        by: Instant::now(),
        until: Instant::now() + HANG,
        group: Some(an_empty_group()),
    };
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        let _ = release.send(());
    });
    assert_eq!(drain.collect(&grace).unwrap(), b"owned prefix");
}

#[cfg(unix)]
#[test]
fn complete_stdout_reads_a_pipe_held_outside_the_childs_group_until_it_lets_go() {
    use crate::deadline::tests::{an_empty_group, lets_go};
    use std::io::Write as _;
    let (reader, mut writer) = std::io::pipe().expect("pipe");
    writer.write_all(b"one ").expect("write");
    let pipe = pipe_end(&reader);
    let drain = drain_stdout(pipe, reader).expect("reader");
    let grace = Grace {
        by: Instant::now(),
        until: Instant::now() + HANG,
        group: Some(an_empty_group()),
    };
    lets_go(writer, b"two");
    assert_eq!(drain.collect(&grace).unwrap(), b"one two");
}

/// A pipe held outside the exited child's group is read until it lets go while the kernel
/// still lists that group with no live member: Darwin answers a signal for a group left
/// only zombies with `EPERM`, and a loaded test binary saw a reaped `git`'s group answer so
/// past the grace while its stdout was still held (the repository probe read unread).
/// Negative control: with `EPERM` read as a live descendant, `collect` refuses at once.
#[cfg(target_vendor = "apple")]
#[test]
fn complete_stdout_reads_a_pipe_held_outside_a_group_left_only_a_zombie_until_it_lets_go() {
    use crate::deadline::tests::{a_group_left_only_a_zombie, lets_go};
    use std::io::Write as _;
    let zombie = a_group_left_only_a_zombie();
    let (reader, mut writer) = std::io::pipe().expect("pipe");
    writer.write_all(b"one ").expect("write");
    let pipe = pipe_end(&reader);
    let drain = drain_stdout(pipe, reader).expect("reader");
    let grace = Grace {
        by: Instant::now(),
        until: Instant::now() + HANG,
        group: Some(zombie.id()),
    };
    lets_go(writer, b"two");
    let read = drain.collect(&grace);
    drop(zombie);
    assert_eq!(read.unwrap(), b"one two");
}

#[test]
fn record_capture_reads_past_its_limit_keeping_only_the_records_it_accepts() {
    let mut long = Vec::new();
    for i in 0..10_000 {
        long.extend_from_slice(format!("skip {i}\0").as_bytes());
    }
    long.extend_from_slice(b"keep a\0skip\0keep b");
    let kept = read_records(Cursor::new(long.clone()), 0, 16, |r| r.starts_with(b"keep"))
        .expect("the whole stream, little of it kept");
    assert_eq!(kept, b"keep a\0keep b");
    // NEGATIVE CONTROL: the same stream kept whole passes the limit and is refused.
    assert!(matches!(
        read_records(Cursor::new(long), 0, 16, |_| true),
        Err(CompleteOutputError::StdoutTooLarge { limit: 16 })
    ));
    // One record longer than the limit is refused even unkept: a runaway holds no memory.
    assert!(matches!(
        read_records(Cursor::new(vec![b'x'; 17]), 0, 16, |_| false),
        Err(CompleteOutputError::StdoutTooLarge { limit: 16 })
    ));
    assert_eq!(
        read_records(Cursor::new(vec![b'x'; 16]), 0, 16, |_| false).expect("within"),
        b""
    );
    // The limit bounds a record's body: an ended one of exactly `limit` bytes is within it too.
    let mut ended = vec![b'x'; 16];
    ended.push(0);
    assert_eq!(
        read_records(Cursor::new(ended), 0, 16, |_| false).expect("an ended record within"),
        b""
    );
}

/// A CHILD STILL RUNNING AT ITS DEADLINE IS ENDED, NOT LEFT RUNNING: the complete reader refuses
/// it as timed out, and once the call has returned no process has the child's pid. The child
/// writes its own pid on a pipe it holds as its stdin before it becomes `sleep 600`, so the check
/// is of that child alone, not of the pipe, whose write end a sibling test's child parked between
/// fork and exec can hold a moment too ([`crate::fork_window`]); no timing decides it.
/// NEGATIVE CONTROL: with `capture`'s cleanup guard removed, the `sleep 600` is still there when
/// the call returns.
#[cfg(unix)]
#[test]
fn a_child_past_its_deadline_is_ended_not_left_running() {
    let (read, write) = std::io::pipe().expect("a pipe");
    let mut command =
        crate::spawnfd::command(crate::deadline::stand_in(std::path::Path::new("/bin/sh")));
    command
        .args(["-c", "printf '%s\\n' \"$$\" >&0 && exec \"$1\" 600", "sh"])
        .arg(crate::deadline::stand_in(std::path::Path::new(
            "/bin/sleep",
        )));
    let (said, heard) = std::sync::mpsc::channel();
    let mut named = None;
    let got = capture(
        &mut command,
        Stdio::from(write),
        Instant::now(),
        || {
            std::thread::spawn(move || {
                let mut line = String::new();
                let read = io::BufRead::read_line(&mut io::BufReader::new(read), &mut line);
                let _ = said.send(read.map(|_| line));
            });
            named = Some(heard.recv_timeout(HANG).expect("the child writes its pid"));
        },
        |pipe, stream| drain_stdout_with_limit(pipe, stream, 1024),
        None,
    );
    assert!(
        matches!(got, Err(CompleteOutputError::Run(RunError::TimedOut))),
        "{got:?}"
    );
    let line = named.expect("the child started").expect("its pid line");
    let pid = line.trim().parse::<i32>().expect("a pid");
    assert_eq!(
        crate::process::process_presence(pid),
        crate::process::ProcessPresence::Absent,
        "the timed-out child {pid} when the call returned"
    );
}
