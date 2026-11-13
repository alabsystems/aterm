// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `spawnfd`'s tests.

use super::*;

#[test]
fn lost_wait_custody_is_sticky_without_numeric_child_operations() {
    let mut custody = WaitCustody::default();
    custody.observe_error(
        &std::io::Error::from(std::io::ErrorKind::Interrupted),
        false,
    );
    assert!(custody.require().is_ok());
    custody.observe_error(&std::io::Error::from_raw_os_error(libc::ECHILD), false);
    assert_eq!(
        custody.require().unwrap_err().raw_os_error(),
        Some(libc::ECHILD)
    );
    custody.observe_error(&std::io::Error::from(std::io::ErrorKind::Interrupted), true);
    assert!(
        custody.require().is_err(),
        "later observations cannot recreate custody"
    );
    let mut reaped = WaitCustody::default();
    reaped.observe_error(&std::io::Error::from_raw_os_error(libc::ECHILD), true);
    assert!(
        reaped.require().is_ok(),
        "a cached positive reap is not forfeited"
    );
}
use crate::host::could_not_run;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::time::Duration;

/// Present in the environment means "you ARE the staged child".
pub(super) const STAGE_ENV: &str = "ATERM_UDS_SPAWNFD_STAGE";

const BOUND_NAME: &str = if cfg!(target_vendor = "apple") {
    "kern.maxfilesperproc"
} else {
    "the hard RLIMIT_NOFILE"
};

#[test]
fn failed_guarded_exec_keeps_parent_guard_close_on_exec() {
    use std::os::fd::AsFd as _;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let file = std::fs::File::create(scratch.path().join("guard")).unwrap();
    let mut cmd = command(scratch.path().join("missing-program"));
    cmd.inherit_guards(vec![file.as_fd().try_clone_to_owned().unwrap()])
        .unwrap();
    let fd = cmd.guard_targets().next().unwrap();
    // SAFETY: the command exclusively owns this live guard slot throughout both queries.
    let before = unsafe { fcntl(fd, F_GETFD) };
    assert_ne!(before & FD_CLOEXEC, 0);
    let error = cmd.exec();
    #[cfg(not(target_vendor = "apple"))]
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported, "{error:?}");
    #[cfg(target_vendor = "apple")]
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{error:?}");
    // SAFETY: failed exec retained the command and its occupied descriptor.
    assert_eq!(unsafe { fcntl(fd, F_GETFD) }, before);
}

/// Signal-mask inheritance is an explicit in-place-exec boundary, never a way to give an
/// ordinary child the caller's blocked signals.
#[cfg(target_vendor = "apple")]
#[test]
fn an_exec_signal_mask_cannot_be_inherited_by_a_spawned_child() {
    let mut child = command("/usr/bin/true");
    child.inherit_exec_signal_mask().unwrap();
    assert_eq!(
        child.spawn().unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_guarded_exec_keeps_only_the_advertised_guard_at_the_same_number() {
    use std::os::{fd::AsFd as _, unix::fs::MetadataExt as _};
    const NAME: &str =
        "spawnfd::tests::a_guarded_exec_keeps_only_the_advertised_guard_at_the_same_number";
    const PATH_ENV: &str = "ATERM_UDS_EXEC_GUARD_PATH";
    const FD_ENV: &str = "ATERM_UDS_EXEC_GUARD_FD";
    const PID_ENV: &str = "ATERM_UDS_EXEC_GUARD_PID";
    match std::env::var(STAGE_ENV).as_deref() {
        Ok("guard-exec-observer") => {
            let path = std::path::PathBuf::from(std::env::var_os(PATH_ENV).unwrap());
            let fd: RawFd = std::env::var(FD_ENV).unwrap().parse().unwrap();
            assert_eq!(
                std::env::var(PID_ENV).unwrap(),
                std::process::id().to_string()
            );
            let expected = std::fs::metadata(&path).unwrap();
            let same_inode = |metadata: &std::fs::Metadata| {
                (metadata.dev(), metadata.ino()) == (expected.dev(), expected.ino())
            };
            assert!(same_inode(&descriptor_metadata(fd).unwrap()));
            assert_eq!(flags(fd) & FD_CLOEXEC, 0);
            let guards: Vec<_> = std::fs::read_dir("/dev/fd")
                .unwrap()
                .map(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_str()
                        .unwrap()
                        .parse::<RawFd>()
                        .unwrap()
                })
                .filter(|&number| descriptor_metadata(number).is_ok_and(|meta| same_inode(&meta)))
                .collect();
            assert_eq!(
                guards,
                [fd],
                "temporary exec copies must not reach the image"
            );
            let fresh = std::fs::File::open(path).unwrap();
            #[expect(
                clippy::disallowed_methods,
                reason = "negative control: only the inherited description keeps this fixture lock"
            )]
            let locked = fresh.try_lock();
            assert!(matches!(locked, Err(std::fs::TryLockError::WouldBlock)));
            println!("guarded-exec-observer-kept-original-number-and-lock");
            return;
        }
        Ok("guard-exec-actor") => {
            let file = std::fs::File::options()
                .read(true)
                .write(true)
                .open(std::env::var_os(PATH_ENV).unwrap())
                .unwrap();
            #[expect(
                clippy::disallowed_methods,
                reason = "fixture description must survive a real in-place exec"
            )]
            file.try_lock().unwrap();
            let mut exec = command(std::env::current_exe().unwrap());
            exec.inherit_guards(vec![file.as_fd().try_clone_to_owned().unwrap()])
                .unwrap();
            let fd = exec.guard_targets().next().unwrap();
            exec.args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env(STAGE_ENV, "guard-exec-observer")
                .env(FD_ENV, fd.to_string())
                .env(PID_ENV, std::process::id().to_string());
            panic!("guarded in-place exec returned: {:?}", exec.exec());
        }
        _ => {}
    }
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("guard");
    std::fs::write(&path, b"guarded exec fixture").unwrap();
    let mut actor = command(std::env::current_exe().unwrap());
    actor
        .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
        .env(STAGE_ENV, "guard-exec-actor")
        .env(PATH_ENV, &path);
    let output = crate::deadline::output_within(&mut actor, Duration::from_secs(60)).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("guarded-exec-observer-kept-original-number-and-lock")
    );
}

#[test]
fn explicit_guards_keep_the_shared_lock_after_parent_descriptors_close() {
    use std::os::fd::AsFd as _;
    use std::os::unix::fs::MetadataExt as _;
    use std::time::Instant;
    const CHILD: &str = "ATERM_UDS_GUARD_LIFETIME_CHILD";
    const HANG: Duration = Duration::from_secs(60);
    if std::env::var_os(CHILD).is_some() {
        let fd: RawFd = std::env::var("ATERM_UDS_GUARD_FD")
            .unwrap()
            .parse()
            .unwrap();
        let path = std::path::PathBuf::from(std::env::var_os("ATERM_UDS_GUARD_PATH").unwrap());
        let go = std::path::PathBuf::from(std::env::var_os("ATERM_UDS_GUARD_GO").unwrap());
        let expected = std::fs::metadata(&path).unwrap();
        let inherited = descriptor_metadata(fd).expect("explicit descriptor inherited");
        assert_eq!(
            (inherited.dev(), inherited.ino()),
            (expected.dev(), expected.ino())
        );
        // SAFETY: read-only flag query for the inherited descriptor admitted above.
        assert_eq!(unsafe { fcntl(fd, F_GETFD) } & FD_CLOEXEC, 0);
        let unrelated: RawFd = std::env::var("ATERM_UDS_UNRELATED_FD")
            .unwrap()
            .parse()
            .unwrap();
        let unrelated_path = std::env::var_os("ATERM_UDS_UNRELATED_PATH").unwrap();
        let other = std::fs::metadata(unrelated_path).unwrap();
        assert!(match descriptor_metadata(unrelated) {
            Err(_) => true,
            Ok(meta) => (meta.dev(), meta.ino()) != (other.dev(), other.ino()),
        });
        let until = Instant::now() + HANG;
        while !go.exists() {
            assert!(Instant::now() < until, "parent descriptor-close signal");
            std::thread::sleep(Duration::from_millis(10));
        }
        let fresh = std::fs::File::open(path).unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "negative control: only the inherited description must retain this fixture lock"
        )]
        let locked = fresh.try_lock();
        assert!(matches!(locked, Err(std::fs::TryLockError::WouldBlock)));
        println!("guard-description-kept-lock");
        return;
    }
    let scratch = aterm_tempfile::tempdir().expect("private guard fixture");
    let path = scratch.path().join("held");
    let file = std::fs::File::create(&path).unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "fixture open-description lifetime, with no unlock before the child proof"
    )]
    file.try_lock().unwrap();
    let mut cmd = command(std::env::current_exe().unwrap());
    cmd.inherit_guards(vec![file.as_fd().try_clone_to_owned().unwrap()])
        .unwrap();
    let guard = cmd.guard_targets().next().unwrap();
    let unrelated_path = scratch.path().join("unrelated");
    let unrelated = std::fs::File::create(&unrelated_path).unwrap();
    // SAFETY: this owned fixture descriptor carries no private data. Intentionally unmark it
    // to prove the default strip still excludes unrequested descriptors beside named guards.
    assert_eq!(unsafe { fcntl(unrelated.as_raw_fd(), F_SETFD, 0) }, 0);
    let go = scratch.path().join("parent-closed");
    cmd.args([
        "--exact",
        "spawnfd::tests::explicit_guards_keep_the_shared_lock_after_parent_descriptors_close",
        "--nocapture",
    ])
    .env(CHILD, "1")
    .env("ATERM_UDS_GUARD_FD", guard.to_string())
    .env("ATERM_UDS_GUARD_PATH", &path)
    .env("ATERM_UDS_GUARD_GO", &go)
    .env("ATERM_UDS_UNRELATED_FD", unrelated.as_raw_fd().to_string())
    .env("ATERM_UDS_UNRELATED_PATH", &unrelated_path)
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit());
    let mut child = cmd.spawn().expect("guard child");
    crate::keep_out_of_children(&unrelated).unwrap();
    drop(cmd);
    drop(file);
    std::fs::write(go, b"closed").unwrap();
    let until = Instant::now() + HANG;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            panic!("guard child hang detector");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut output = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    assert!(status.success(), "{output}");
    assert!(
        output.contains("guard-description-kept-lock"),
        "actual child body output: {output}"
    );
}

/// Skip the test as COULD NOT RUN unless [`descriptor_bound`] is above `need`.
#[track_caller]
pub(super) fn require_host_holds(need: RawFd, what: &str) {
    let bound = descriptor_bound();
    if bound <= need {
        could_not_run(format!(
            "{what}needs {BOUND_NAME} above {need}, this host allows {bound}"
        ));
    }
}

/// THE RAISED SOFT LIMIT IS THE FLOOR CAPPED AT THE HARD LIMIT, and only
/// where that raises anything: a stock 256 shell gets the floor, a low
/// hard limit caps it, an already-high soft limit is left alone.
#[test]
fn the_raised_soft_nofile_target_is_the_floor_capped_at_the_hard_limit() {
    assert_eq!(
        raised_soft_nofile_target(256, u64::MAX),
        Some(SOFT_NOFILE_FLOOR)
    );
    assert_eq!(raised_soft_nofile_target(256, 1024), Some(1024));
    assert_eq!(raised_soft_nofile_target(8192, u64::MAX), None);
    assert_eq!(raised_soft_nofile_target(256, 256), None);
}

/// Raise the soft descriptor limit toward `request`, and end the calling test COULD NOT RUN when it
/// cannot pass `floor`.
#[track_caller]
pub(super) fn require_soft_limit(request: u64, floor: RawFd) {
    if raise_soft_limit(request) <= u64::try_from(floor).unwrap_or(0) {
        could_not_run(format!("the hard descriptor limit is below {floor}"));
    }
}

/// The binary translator this process runs under ([`crate::process::translator`]).
pub(crate) use crate::process::translator;

/// Explanation printed when skipping `std` spawn comparisons under Rosetta on Linux.
pub(super) const STD_SPAWN_MUTE: &str = "std's answer comes from glibc's posix_spawn, whose \
    child reports a failed exec or chdir through memory it shares with its parent \
    (clone with CLONE_VM|CLONE_VFORK); Rosetta runs that child in a copy, where the store \
    is lost (measured 2026-10-17), so std answers a child that exited 127; a real x86_64 \
    CPU decides these rows";

/// `fcntl(F_GETFD)` on `fd`, or -1 when the number is not open.
pub(super) fn flags(fd: RawFd) -> i32 {
    // SAFETY: `F_GETFD` reads one descriptor's flags.
    unsafe { fcntl(fd, F_GETFD) }
}

fn echo_command(probe: Option<RawFd>) -> LaneCommand {
    let check = probe.map_or(String::new(), |fd| {
        format!("{} && s=held || s=clear; ", test_support::sh_held(fd))
    });
    let mut cmd = LaneCommand::new("/bin/sh");
    cmd.arg("-c")
        .arg(format!("read line <&3; {check}echo \"got:$line:$s\" >&4"))
        .stdin(ChildStdio::Null);
    cmd
}

fn round_trip(far_verb: OwnedFd, far_push: OwnedFd, near: (UnixStream, UnixStream)) -> String {
    round_trip_probe(far_verb, far_push, near, None)
}

fn round_trip_probe(
    far_verb: OwnedFd,
    far_push: OwnedFd,
    (mut near_verb, mut near_push): (UnixStream, UnixStream),
    probe: Option<RawFd>,
) -> String {
    test_support::bound_read(&near_push).expect("bound the read");
    let mut child =
        spawn_with_two_fds(echo_command(probe), far_verb, far_push).expect("spawn /bin/sh");
    near_verb.write_all(b"hello\n").expect("write fd 3");
    let mut got = String::new();
    near_push.read_to_string(&mut got).expect("read fd 4");
    let status = child.wait().expect("reap the child");
    assert!(status.success(), "child exited {status:?}");
    got.trim_end().to_string()
}

const HELD: std::ops::RangeInclusive<RawFd> = 3..=8;

const HELD_ENV: &str = "ATERM_UDS_SPAWNFD_HELD";

/// Re-runs this test binary as the staged child for `test` / `stage` and returns its stdout.
pub(super) fn run_staged(test: &str, stage: &str) -> String {
    let mut launch = std::process::Command::new(std::env::current_exe().expect("the test binary"));
    launch.args(["--exact", test, "--nocapture", "--test-threads=1"]);
    launch_staged(launch, stage)
}

/// [`run_staged`] with `SIGHUP` ignored across the exec, as `nohup` (or any launcher that ignores
/// it) leaves a test run.
pub(super) fn run_staged_hup_ignored(test: &str, stage: &str) -> String {
    let mut launch = std::process::Command::new("/bin/sh");
    launch
        .arg("-c")
        .arg("trap '' HUP; exec \"$0\" \"$@\"")
        .arg(std::env::current_exe().expect("the test binary"))
        .args(["--exact", test, "--nocapture", "--test-threads=1"]);
    launch_staged(launch, stage)
}

fn run_staged_held(test: &str, stage: &str) -> String {
    let redirections: String = HELD.map(|fd| format!(" {fd}</dev/null")).collect();
    let mut launch = std::process::Command::new("/bin/sh");
    launch
        .arg("-c")
        .arg(format!("exec \"$0\" \"$@\"{redirections}"))
        .arg(std::env::current_exe().expect("the test binary"))
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(HELD_ENV, "1");
    launch_staged(launch, stage)
}

fn release_held() {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
    if std::env::var_os(HELD_ENV).is_none() {
        return;
    }
    let null = std::fs::metadata("/dev/null")
        .expect("stat /dev/null")
        .rdev();
    for fd in HELD {
        assert_ne!(flags(fd), -1, "fd {fd}, which the launcher held, is closed");
        // SAFETY: `fd` is open (just checked); the `File` is never dropped, so it only borrows the
        // descriptor for one `fstat`.
        let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
        let meta = file.metadata().expect("fstat a held descriptor");
        assert!(
            meta.file_type().is_char_device() && meta.rdev() == null,
            "fd {fd} is not the /dev/null the launcher held there: {meta:?}"
        );
        // SAFETY: the launcher's descriptor, which nothing else here owns.
        drop(unsafe { OwnedFd::from_raw_fd(fd) });
    }
}

/// How long a staged child may run: a hang detector past its stage's own, never a budget. A
/// stage waits in turn on things that must happen, each under its one-minute hang detector
/// (`shell-probe` races two of them, each overrun by its last round, and `signals` waits on two
/// probes and a child), so a stage whose own wait hangs fails by its own message first.
const STAGE_HANG: Duration = Duration::from_secs(10 * crate::deadline::ECHO_HANG.as_secs());

/// Runs `launch` as the staged child for `stage` and returns its stdout. The launcher, and the
/// stage it execs, start with SIGHUP, SIGINT, SIGQUIT and SIGTERM at their defaults, set between
/// its fork and exec, not this test binary's: a `&` or `nohup` runner starts the binary with some
/// ignored, which every exec keeps, and a stage's child sent one would never end by it. A
/// launcher that ignores one itself ([`run_staged_hup_ignored`]) still does so after this.
fn launch_staged(mut launch: std::process::Command, stage: &str) -> String {
    launch.env(STAGE_ENV, stage);
    let strip = InheritStrip::prepare();
    // SAFETY: the closure runs between fork and exec and calls only `InheritStrip::apply` and
    // `signal`, both async-signal-safe; `signal` with SIG_DFL installs no handler.
    unsafe {
        launch.pre_exec(move || {
            strip.apply();
            for sig in [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM] {
                libc::signal(sig, libc::SIG_DFL);
            }
            Ok(())
        });
    }
    let out = crate::deadline::std_ends_within(&mut launch, STAGE_HANG);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success()
        && let Some(why) = String::from_utf8_lossy(&out.stderr).lines().find_map(|l| {
            l.split_once(crate::host::COULD_NOT_RUN)
                .map(|(_, why)| why.to_owned())
        })
    {
        could_not_run(format!("stage {stage}{why}"));
    }
    assert!(
        out.status.success(),
        "stage {stage}: {:?}\nstdout: {stdout}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

/// Moves `fd` to the lowest free close-on-exec number at or above `floor`.
pub(super) fn park(fd: OwnedFd, floor: RawFd) -> OwnedFd {
    // SAFETY: `fd` is live; the copy is a fresh descriptor owned only here.
    let copy = unsafe { fcntl(fd.as_raw_fd(), F_DUPFD_CLOEXEC, floor) };
    assert!(
        copy >= floor,
        "park at {floor}: {}",
        io::Error::last_os_error()
    );
    // SAFETY: just created here.
    unsafe { OwnedFd::from_raw_fd(copy) }
}

fn place(fd: OwnedFd, number: RawFd) -> OwnedFd {
    unsafe extern "C" {
        fn dup2(old: RawFd, new: RawFd) -> RawFd;
    }
    assert_eq!(
        flags(number),
        -1,
        "fd {number} is already open in the staged child"
    );
    // SAFETY: `fd` is live, `number` free; `from_raw_fd` takes exactly the descriptor `dup2`
    // created, after the original is dropped.
    unsafe {
        assert_eq!(dup2(fd.as_raw_fd(), number), number, "dup2 onto {number}");
        assert_ne!(fcntl(number, F_SETFD, FD_CLOEXEC), -1, "flag {number}");
        drop(fd);
        OwnedFd::from_raw_fd(number)
    }
}

/// An unflagged duplicate of `/dev/null` at the lowest free number at or above `floor`.
pub(super) fn unflagged_at_or_above(floor: RawFd) -> OwnedFd {
    const F_DUPFD: i32 = 0;
    let file = std::fs::File::open("/dev/null").expect("open /dev/null");
    // SAFETY: `file` is live; `F_DUPFD` returns a fresh descriptor or -1.
    let fd = unsafe { fcntl(file.as_raw_fd(), F_DUPFD, floor) };
    assert!(
        fd >= floor,
        "F_DUPFD at {floor}: {}",
        io::Error::last_os_error()
    );
    assert_eq!(
        flags(fd) & FD_CLOEXEC,
        0,
        "PRECONDITION: the stand-in is inheritable"
    );
    // SAFETY: just created here.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

/// `struct rlimit` and `RLIMIT_NOFILE`, for the tests that move the soft limit.
#[repr(C)]
struct Rl {
    cur: u64,
    max: u64,
}
#[cfg(target_vendor = "apple")]
const NOFILE: i32 = 8;
#[cfg(not(target_vendor = "apple"))]
const NOFILE: i32 = 7;
unsafe extern "C" {
    fn getrlimit(resource: i32, limit: *mut Rl) -> i32;
    fn setrlimit(resource: i32, limit: *const Rl) -> i32;
}

/// Serialises this binary's soft-limit writers. A move is a read and then a write, and the tests
/// run at once: a raise that read before another raise wrote would put back its own, lower
/// target, under a test that already parked a descriptor above it.
static SOFT_LIMIT: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn soft_limit() -> Rl {
    let mut limit = Rl { cur: 0, max: 0 };
    // SAFETY: a live out-parameter.
    assert_eq!(unsafe { getrlimit(NOFILE, &mut limit) }, 0, "getrlimit");
    limit
}

fn write_soft_limit(mut limit: Rl, soft: u64) -> u64 {
    limit.cur = soft.min(limit.max);
    // SAFETY: a live, initialised limit that keeps the hard one; moving the soft limit below the
    // hard one needs no privilege.
    assert_eq!(unsafe { setrlimit(NOFILE, &limit) }, 0, "setrlimit");
    limit.cur
}

fn set_soft_limit(soft: u64) -> u64 {
    let _serial = SOFT_LIMIT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    write_soft_limit(soft_limit(), soft)
}

/// Raise the soft limit to at least `want` (capped by the hard limit).
pub(super) fn raise_soft_limit(want: u64) -> u64 {
    raise_soft_limit_between(want, || ())
}

/// [`raise_soft_limit`], running `between` after it read the limit and before it writes one.
fn raise_soft_limit_between(want: u64, between: impl FnOnce()) -> u64 {
    let _serial = SOFT_LIMIT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let limit = soft_limit();
    between();
    if limit.cur >= want {
        limit.cur
    } else {
        write_soft_limit(limit, want)
    }
}

/// A raise holds [`SOFT_LIMIT`] from its read to its write, so no other raise of this binary
/// writes in that span, to be put back by this one.
#[test]
fn a_soft_limit_raise_holds_its_lock_from_its_read_to_its_write() {
    let mut held = None;
    raise_soft_limit_between(0, || held = Some(SOFT_LIMIT.try_lock().is_err()));
    assert_eq!(
        held,
        Some(true),
        "a raise another raise can write under between its read and its write"
    );
}

#[test]
fn the_two_fd_child_inherits_nothing_but_its_two_lanes() {
    let held = unflagged_at_or_above(64);
    let (near_verb, far_verb) = UnixStream::pair().expect("socketpair");
    let (near_push, far_push) = UnixStream::pair().expect("socketpair");
    let got = round_trip_probe(
        far_verb.into(),
        far_push.into(),
        (near_verb, near_push),
        Some(held.as_raw_fd()),
    );
    drop(held);
    assert_eq!(got, "got:hello:clear");
}

const HIGH_VERB: RawFd = 10_240;

fn staged_placement(which: &str) {
    release_held();
    let (near_verb, far_verb) = UnixStream::pair().expect("socketpair");
    let (near_push, far_push) = UnixStream::pair().expect("socketpair");
    let near_verb = UnixStream::from(park(near_verb.into(), 20));
    let near_push = UnixStream::from(park(near_push.into(), 20));
    let far_verb = park(far_verb.into(), 20);
    let far_push = park(far_push.into(), 20);
    let (far_verb, far_push, want) = match which {
        "verb" => (place(far_verb, BRIDGE_VERB_FD), far_push, (3, -1)),
        "push" => (far_verb, place(far_push, BRIDGE_PUSH_FD), (-1, 4)),
        "swapped" => (
            place(far_verb, BRIDGE_PUSH_FD),
            place(far_push, BRIDGE_VERB_FD),
            (4, 3),
        ),
        "high" => {
            let push = 20_000.min(descriptor_bound() - 1);
            let want = u64::try_from(push).unwrap();
            assert!(
                raise_soft_limit(want + 1) > want,
                "a soft limit above {push}"
            );
            (
                place(far_verb, HIGH_VERB),
                place(far_push, push),
                (HIGH_VERB, push),
            )
        }
        other => panic!("unknown stage {other:?}"),
    };
    if want.0 >= 0 {
        assert_eq!(far_verb.as_raw_fd(), want.0);
    }
    if want.1 >= 0 {
        assert_eq!(far_push.as_raw_fd(), want.1);
    }
    assert_eq!(
        round_trip(far_verb, far_push, (near_verb, near_push)),
        "got:hello:",
        "stage {which}: a lane was lost on the way to the child"
    );
    println!("STAGED {which} ok");
}

#[test]
fn every_lane_source_reaches_its_number_wherever_it_started() {
    const NAME: &str = "spawnfd::tests::every_lane_source_reaches_its_number_wherever_it_started";
    if let Ok(stage) = std::env::var(STAGE_ENV) {
        staged_placement(&stage);
        return;
    }
    for stage in ["verb", "push", "swapped", "high"] {
        if stage == "high" {
            require_host_holds(HIGH_VERB + 1, "stage high ");
        }
        let out = run_staged_held(NAME, stage);
        assert!(
            out.contains(&format!("STAGED {stage} ok")),
            "stage {stage}: {out}"
        );
    }
}

#[test]
fn off_the_targets_moves_only_what_a_file_action_cannot_take() {
    let file = std::fs::File::open("/dev/null").expect("open /dev/null");
    let fd = park(OwnedFd::from(file), 64);
    let raw = fd.as_raw_fd();
    let kept = off_the_targets(fd).expect("in range");
    assert_eq!(
        kept.as_raw_fd(),
        raw,
        "a source in range is left where it is"
    );

    #[cfg(target_vendor = "apple")]
    {
        require_host_holds(
            ACTION_FD_LIMIT,
            "the move from above the file-action limit ",
        );
        let floor = (ACTION_FD_LIMIT + 7).min(descriptor_bound() - 1);
        require_soft_limit(u64::try_from(floor).unwrap() + 1, floor);
        let high = park(kept, floor);
        let high_raw = high.as_raw_fd();
        let moved = off_the_targets(high).expect("a free number below the limit");
        assert!(
            placeable(moved.as_raw_fd()),
            "moved to {}",
            moved.as_raw_fd()
        );
        assert_ne!(
            flags(moved.as_raw_fd()) & FD_CLOEXEC,
            0,
            "the copy is close-on-exec"
        );
        assert_eq!(flags(high_raw), -1, "the original is closed");
    }
}

fn lanes() -> (OwnedFd, OwnedFd) {
    let (_, a) = UnixStream::pair().expect("socketpair");
    let (_, b) = UnixStream::pair().expect("socketpair");
    (a.into(), b.into())
}

fn staged_exec_failure() {
    release_held();
    let dir = std::env::temp_dir().join(format!("spawnfd-noexec-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let not_executable = dir.join("plain");
    std::fs::write(&not_executable, b"#!/bin/sh\nexit 0\n").expect("write");
    let (a, b) = lanes();
    let (a, b) = (park(a, 20), park(b, 20));
    for fd in HELD {
        assert_eq!(flags(fd), -1, "fd {fd} must be free for this stage");
    }
    let missing = spawn_with_two_fds(LaneCommand::new("/nonexistent/aterm-spawnfd"), a, b);
    let (a, b) = lanes();
    let (a, b) = (park(a, 20), park(b, 20));
    let refused = spawn_with_two_fds(LaneCommand::new(&not_executable), a, b);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        missing.as_ref().map_err(io::Error::kind).err(),
        Some(io::ErrorKind::NotFound),
        "a program that does not exist is an Err, not a child: {missing:?}"
    );
    assert_eq!(
        refused.as_ref().map_err(io::Error::raw_os_error).err(),
        Some(Some(13)),
        "a file without an execute bit is EACCES, not a child: {refused:?}"
    );
    println!("STAGED exec-failure ok");
}

#[test]
fn a_failed_exec_is_an_error_never_a_child() {
    const NAME: &str = "spawnfd::tests::a_failed_exec_is_an_error_never_a_child";
    if std::env::var(STAGE_ENV).is_ok() {
        staged_exec_failure();
        return;
    }
    let out = run_staged_held(NAME, "exec-failure");
    assert!(out.contains("STAGED exec-failure ok"), "{out}");
}

#[test]
fn the_child_handle_reaps_once_and_kills_only_its_own() {
    let (a, b) = lanes();
    let mut cmd = LaneCommand::new("/bin/sh");
    cmd.args(["-c", "exit 0"]);
    let mut done = spawn_with_two_fds(cmd, a, b).expect("spawn sh");
    let first = done.wait().expect("wait");
    assert!(first.success());
    assert_eq!(
        done.wait().expect("second wait"),
        first,
        "the status is kept"
    );
    assert_eq!(done.try_wait().expect("try_wait"), Some(first));
    done.kill()
        .expect("kill after the reap is Ok and signals nothing");

    let (held, stdin) = UnixStream::pair().expect("socketpair");
    let (a, b) = lanes();
    let mut cmd = LaneCommand::new("/bin/sh");
    cmd.args(["-c", "read line"])
        .stdin(ChildStdio::Fd(stdin.into()));
    let mut running = spawn_with_two_fds(cmd, a, b).expect("spawn sh");
    assert!(running.id() > 0);
    assert_eq!(running.try_wait().expect("try_wait"), None, "still running");
    running.kill().expect("kill");
    let status = running.wait().expect("wait");
    drop(held);
    assert_eq!(status.signal(), Some(SIGKILL), "{status:?}");
}

#[test]
fn a_bare_name_is_searched_on_the_childs_path_and_the_environment_is_exact() {
    let (near_verb, far_verb) = UnixStream::pair().expect("socketpair");
    let (mut near_push, far_push) = UnixStream::pair().expect("socketpair");
    test_support::bound_read(&near_push).expect("bound the read");
    drop(near_verb);
    let mut cmd = LaneCommand::new("sh");
    cmd.arg("-c")
        .arg("echo \"$ONLY:${HOME-unset}\" >&4")
        .env_exactly([("PATH", "/nonexistent:/bin"), ("ONLY", "this")])
        .stdin(ChildStdio::Null);
    let mut child = spawn_with_two_fds(cmd, far_verb.into(), far_push.into()).expect("spawn sh");
    let mut got = String::new();
    near_push.read_to_string(&mut got).expect("read fd 4");
    assert!(child.wait().expect("wait").success());
    assert_eq!(got.trim_end(), "this:unset");

    let (a, b) = lanes();
    let mut nowhere = LaneCommand::new("sh");
    nowhere.env_exactly([("PATH", "/nonexistent")]);
    let err = spawn_with_two_fds(nowhere, a, b).expect_err("no sh on that PATH");
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
}

/// Metadata denial does not prove absence or turn a directory into an executable.
/// The real exec authority decides both the candidate to skip and the image to run.
#[cfg(target_vendor = "apple")]
#[test]
fn a_bare_path_search_uses_exec_authority_when_metadata_is_denied() {
    const CHILD: &str = "ATERM_UDS_METADATA_SEARCH_CHILD";
    const SEARCH: &str = "ATERM_UDS_METADATA_SEARCH_PATH";
    const RECEIPT: &str = "metadata-denied-path-executed-echo";
    if let Ok(stage) = std::env::var(CHILD) {
        let search = std::env::var_os(SEARCH).unwrap();
        let aliases: Vec<_> = std::env::split_paths(&search)
            .map(|dir| dir.join("echo"))
            .collect();
        assert_eq!(aliases.len(), 2, "the directory and executable aliases");
        for path in aliases {
            assert_eq!(
                std::fs::metadata(&path).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied,
                "the sandbox denies metadata on {}",
                path.display()
            );
        }
        let denied = stage.starts_with("deny-");
        let prepared = stage.ends_with("prepared");
        assert!(
            matches!(
                stage.as_str(),
                "spawn" | "prepared" | "deny-spawn" | "deny-prepared"
            ),
            "unexpected staged role: {stage}"
        );
        println!("STAGED {stage} metadata-denial-observed");
        std::io::stdout().flush().unwrap();
        if prepared {
            let mut exec = transparent("echo");
            exec.arg(RECEIPT).env_exactly([("PATH", &search)]);
            let error = match exec.prepare_exec() {
                Ok(prepared) => {
                    // The prepared native image owns the original search and arguments
                    // while its source command can change before dispatch is granted.
                    exec.env("PATH", "/nonexistent").arg("too-late-argument");
                    prepared.exec_until(u64::MAX)
                }
                Err(error) => error,
            };
            assert!(denied, "the permitted in-place exec returned: {error}");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        } else {
            let result = command("echo").arg(RECEIPT).env("PATH", &search).output();
            if denied {
                assert_eq!(
                    result
                        .expect_err("the sandbox still denies execution")
                        .kind(),
                    io::ErrorKind::PermissionDenied
                );
            } else {
                let output = result.expect("spawn through denied metadata");
                assert!(output.status.success(), "echo failed: {output:?}");
                assert_eq!(output.stdout, format!("{RECEIPT}\n").as_bytes());
                print!("{}", String::from_utf8_lossy(&output.stdout));
            }
        }
        println!("STAGED {stage} completed");
        return;
    }
    let root = aterm_tempfile::tempdir().expect("private PATH directory fixture");
    let directory = root.path().canonicalize().unwrap().join("directory");
    let first = directory.parent().unwrap().join("first/echo");
    let second = directory.parent().unwrap().join("second/echo");
    for path in [
        directory.as_path(),
        first.parent().unwrap(),
        second.parent().unwrap(),
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::os::unix::fs::symlink(&directory, &first).unwrap();
    std::os::unix::fs::symlink("/bin/echo", &second).unwrap();
    let search = std::env::join_paths([first.parent().unwrap(), second.parent().unwrap()]).unwrap();
    let name = format!(
        "{}::a_bare_path_search_uses_exec_authority_when_metadata_is_denied",
        module_path!().split_once("::").expect("a crate's module").1
    );
    let literal = directory
        .to_str()
        .unwrap()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    for stage in ["spawn", "prepared", "deny-spawn", "deny-prepared"] {
        let deny_exec = if stage.starts_with("deny-") {
            "(deny process-exec (literal \"/bin/echo\"))"
        } else {
            ""
        };
        let policy = format!(
            "(version 1)(allow default)\
             (deny file-read-metadata (literal \"{literal}\") (literal \"/bin/echo\")){deny_exec}"
        );
        let output = crate::deadline::ends_within(
            command("/usr/bin/sandbox-exec")
                .args(["-p", &policy])
                .arg(std::env::current_exe().expect("the original test image"))
                .args(["--exact", &name, "--nocapture", "--test-threads=1"])
                .env(CHILD, stage)
                .env(SEARCH, &search),
            crate::deadline::ECHO_HANG,
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success()
                && stdout.contains(&format!("STAGED {stage} metadata-denial-observed")),
            "{stage}: {:?}\nstdout: {stdout}\nstderr: {stderr}",
            output.status
        );
        assert_eq!(
            stdout.lines().filter(|line| *line == RECEIPT).count(),
            usize::from(!stage.starts_with("deny-")),
            "{stage}: exactly the permitted image ran\nstdout: {stdout}\nstderr: {stderr}"
        );
        if stage != "prepared" {
            assert!(
                stdout.contains(&format!("STAGED {stage} completed")),
                "{stdout}"
            );
        }
    }
}

/// Descriptor exhaustion is an operational failure of the directory probe, never evidence
/// that a PATH candidate is absent or a regular executable.
#[cfg(target_vendor = "apple")]
#[test]
fn a_prepared_path_search_preserves_directory_probe_descriptor_exhaustion() {
    const STAGE: &str = "path-directory-probe-emfile";
    const RECEIPT: &str = "STAGED path-directory-probe preserved EMFILE";
    if std::env::var(STAGE_ENV).as_deref() != Ok(STAGE) {
        let name = format!(
            "{}::a_prepared_path_search_preserves_directory_probe_descriptor_exhaustion",
            module_path!().split_once("::").expect("a crate's module").1
        );
        let output = run_staged(&name, STAGE);
        assert!(output.contains(RECEIPT), "the exact child ran: {output}");
        return;
    }
    let root = aterm_tempfile::tempdir().expect("private directory probe fixture");
    let directory = root.path().join("first/echo");
    std::fs::create_dir_all(&directory).unwrap();
    let search =
        std::env::join_paths([directory.parent().unwrap(), std::path::Path::new("/bin")]).unwrap();
    let original = soft_limit();
    assert_eq!(
        set_soft_limit(64),
        64,
        "the isolated child's descriptor limit"
    );
    let prepared = transparent("echo")
        .env_exactly([("PATH", &search)])
        .prepare_exec()
        .expect("skip the directory with descriptors available");
    assert_eq!(prepared.captured().1.to_bytes(), b"/bin/echo");
    drop(prepared);

    let mut held = Vec::new();
    let mut cloexec = true;
    let exhausted = loop {
        match std::fs::File::open("/dev/null") {
            Ok(file) => {
                cloexec &= flags(file.as_raw_fd()) & FD_CLOEXEC != 0;
                held.push(file);
            }
            Err(error) => break error,
        }
    };
    let refused = transparent("echo")
        .env_exactly([("PATH", &search)])
        .prepare_exec();
    // Restore room before diagnostics or libtest's own completion can open another file.
    drop(held);
    set_soft_limit(original.cur);
    assert!(cloexec, "the fixture held only close-on-exec descriptors");
    assert_eq!(exhausted.raw_os_error(), Some(libc::EMFILE));
    let error = refused
        .err()
        .expect("a directory probe with no free descriptor is refused");
    assert_eq!(
        error.raw_os_error(),
        Some(libc::EMFILE),
        "the first directory probe's operational failure survives PATH search"
    );
    println!("{RECEIPT}");
}

#[test]
fn a_key_named_twice_is_its_last_value_for_the_search_and_the_child_alike() {
    let (mut out, child_out) = UnixStream::pair().expect("socketpair");
    test_support::bound_read(&out).expect("bound the read");
    let mut cmd = LaneCommand::new("env");
    cmd.env_exactly([
        ("PATH", "/nonexistent"),
        ("ONLY", "first"),
        ("PATH", "/usr/bin"),
        ("ONLY", "last"),
    ])
    .stdin(ChildStdio::Null)
    .stdout(ChildStdio::Fd(child_out.into()));
    let mut child = spawn(cmd).expect("spawn env");
    let mut got = String::new();
    out.read_to_string(&mut got).expect("read stdout");
    assert!(child.wait().expect("wait").success());
    let mut block: Vec<&str> = got.lines().collect();
    block.sort_unstable();
    assert_eq!(block, ["ONLY=last", "PATH=/usr/bin"]);
}

#[test]
fn the_search_passes_over_what_it_may_not_run_and_a_script_without_a_shebang_runs() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("spawnfd-execvp-{}", std::process::id()));
    let (denied, script) = (dir.join("denied"), dir.join("script"));
    std::fs::create_dir_all(&denied).expect("scratch dir");
    std::fs::create_dir_all(&script).expect("scratch dir");
    std::fs::write(denied.join("tool"), b"#!/bin/sh\nexit 9\n").expect("write");
    std::fs::set_permissions(denied.join("tool"), std::fs::Permissions::from_mode(0o474))
        .expect("chmod");
    std::fs::write(script.join("tool"), b"echo \"ran:$1\" >&4\n").expect("write");
    std::fs::set_permissions(script.join("tool"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    let path = format!("{}:{}", denied.display(), script.display());

    let (near_verb, far_verb) = UnixStream::pair().expect("socketpair");
    let (mut near_push, far_push) = UnixStream::pair().expect("socketpair");
    test_support::bound_read(&near_push).expect("bound the read");
    drop(near_verb);
    let mut cmd = LaneCommand::new("tool");
    cmd.arg("x")
        .env_exactly([("PATH", path.as_str())])
        .stdin(ChildStdio::Null);
    let spawned = spawn_with_two_fds(cmd, far_verb.into(), far_push.into());
    let mut got = String::new();
    near_push.read_to_string(&mut got).expect("read fd 4");
    let status = spawned.map(|mut child| child.wait().expect("wait"));

    let (a, b) = lanes();
    let mut alone = LaneCommand::new("tool");
    alone.env_exactly([("PATH", denied.as_os_str())]);
    let refused = spawn_with_two_fds(alone, a, b);
    let _ = std::fs::remove_dir_all(&dir);

    let status = status.expect("the denied candidate is passed over, the script spawned");
    assert!(status.success(), "{status:?}");
    assert_eq!(
        got.trim_end(),
        "ran:x",
        "the shebang-less script ran under /bin/sh"
    );
    assert_eq!(
        refused.as_ref().map_err(io::Error::raw_os_error).err(),
        Some(Some(EACCES)),
        "alone, the candidate it may not run is EACCES: {refused:?}"
    );
}

#[test]
fn a_helper_spawned_without_lanes_holds_its_stdio_and_nothing_else() {
    let held = unflagged_at_or_above(64);
    let fd = held.as_raw_fd();
    assert!(
        sh_holds(fd, false),
        "CONTROL: a leaky spawn carries fd {fd}"
    );
    let (mut out, child_out) = UnixStream::pair().expect("socketpair");
    test_support::bound_read(&out).expect("bound the read");
    let probe = [3, 4, fd]
        .map(|f| format!("{} && printf 'held:{f} '; ", test_support::sh_held(f)))
        .concat();
    let mut cmd = LaneCommand::new("/bin/sh");
    cmd.args(["-c", &(probe + "echo end")])
        .stdin(ChildStdio::Null)
        .stdout(ChildStdio::Fd(child_out.into()));
    let mut child = spawn(cmd).expect("spawn sh");
    let mut got = String::new();
    out.read_to_string(&mut got).expect("read stdout");
    assert!(child.wait().expect("wait").success());
    drop(held);
    assert_eq!(got.trim_end(), "end");
}

const OLD_CAP: RawFd = 65_536;

const HEADROOM: RawFd = 8;

fn above_old_cap() -> Option<RawFd> {
    let fd = 70_000.min(descriptor_bound().saturating_sub(HEADROOM));
    (fd > OLD_CAP).then_some(fd)
}

fn sh_holds(fd: RawFd, lane: bool) -> bool {
    let probe = test_support::held_probe(fd);
    let status = if lane {
        let (a, b) = lanes();
        let mut cmd = LaneCommand::new("/bin/sh");
        cmd.args(["-c", &probe]);
        spawn_with_two_fds(cmd, a, b)
            .expect("spawn")
            .wait()
            .expect("wait")
    } else {
        test_support::leaky_status("/bin/sh", &["-c", &probe]).expect("control spawn")
    };
    held(status)
}

fn held(status: ExitStatus) -> bool {
    match status.code() {
        Some(3) => true,
        Some(0) => false,
        _ => panic!("the probe did not answer: {status:?}"),
    }
}

fn stripped_sh_holds(fd: RawFd) -> bool {
    let probe = test_support::held_probe(fd);
    held(test_support::stripped_status("/bin/sh", &["-c", &probe]).expect("stripped spawn"))
}

fn staged_above_old_cap() {
    let above = above_old_cap().expect("the parent stages only where the host holds one");
    require_soft_limit(u64::try_from(above).unwrap() + 64, above);
    let held = unflagged_at_or_above(above);
    let fd = held.as_raw_fd();
    assert!(fd > OLD_CAP);
    assert!(
        InheritStrip::prepare().bound() > fd,
        "the strip's bound must reach every number this process can hold"
    );
    assert!(
        sh_holds(fd, false),
        "CONTROL: a leaky spawn carries fd {fd}"
    );
    assert!(
        !sh_holds(fd, true),
        "the lane child inherited fd {fd}, above the old 65 536 cap"
    );
    assert!(
        !stripped_sh_holds(fd),
        "the stripped child inherited fd {fd}"
    );
    println!("STAGED above-old-cap ok fd={fd}");
}

#[test]
fn an_unflagged_descriptor_above_the_old_cap_reaches_no_child() {
    const NAME: &str = "spawnfd::tests::an_unflagged_descriptor_above_the_old_cap_reaches_no_child";
    if std::env::var(STAGE_ENV).is_ok() {
        staged_above_old_cap();
        return;
    }
    if above_old_cap().is_none() {
        require_host_holds(OLD_CAP + HEADROOM, "");
    }
    let out = run_staged(NAME, "above-old-cap");
    assert!(out.contains("STAGED above-old-cap ok"), "{out}");
    print!("{out}");
}

const RACE_SPAWNS: usize = 500;
const RACE_CONTROL_SPAWNS: usize = 100;

const RACE_LOW: RawFd = 2000;

fn race(stop: &std::sync::atomic::AtomicBool, high_soft: u64, high: Option<RawFd>) {
    use std::sync::atomic::Ordering;
    while !stop.load(Ordering::Relaxed) {
        set_soft_limit(1024);
        set_soft_limit(high_soft);
        let low = unflagged_at_or_above(RACE_LOW);
        let above = high.map(unflagged_at_or_above);
        let until = std::time::Instant::now() + Duration::from_micros(200);
        while std::time::Instant::now() < until {
            std::hint::spin_loop();
        }
        drop(low);
        drop(above);
    }
}

fn race_probe(high: Option<RawFd>) -> String {
    let above = high.map_or(String::new(), |hi| {
        format!(
            "{} && c=$((c|2)); ",
            test_support::sh_held_any(&[hi, hi + 1])
        )
    });
    format!(
        "c=0; {} && c=$((c|1)); {above}{} && {} || c=$((c|4)); exit $c",
        test_support::sh_held_any(&[RACE_LOW, RACE_LOW + 1, RACE_LOW + 2, RACE_LOW + 3]),
        test_support::sh_held(BRIDGE_VERB_FD),
        test_support::sh_held(BRIDGE_PUSH_FD),
    )
}

fn staged_race() {
    use std::sync::atomic::{AtomicBool, Ordering};
    require_host_holds(OLD_CAP + HEADROOM, "the above-cap half of the race ");
    let high = above_old_cap().expect("the admitted host has a number above the old cap");
    let want = u64::try_from(high).unwrap() + 64;
    require_soft_limit(want, high);
    let high_soft = raise_soft_limit(want);
    let high = Some(high);
    let probe = race_probe(high);
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let racer = {
        let stop = std::sync::Arc::clone(&stop);
        std::thread::spawn(move || race(&stop, high_soft, high))
    };
    let mut tally = [0usize; 8];
    for _ in 0..RACE_SPAWNS {
        let (a, b) = lanes();
        let mut cmd = LaneCommand::new("/bin/sh");
        cmd.args(["-c", &probe]).stdin(ChildStdio::Null);
        let status = spawn_with_two_fds(cmd, a, b)
            .expect("spawn")
            .wait()
            .expect("wait");
        let code = usize::try_from(status.code().unwrap_or(7)).unwrap_or(7) & 7;
        tally[code] += 1;
    }
    let mut control = 0usize;
    for _ in 0..RACE_CONTROL_SPAWNS {
        let status = test_support::leaky_status("/bin/sh", &["-c", &probe]).expect("control spawn");
        if status.code().is_some_and(|c| c & 3 != 0) {
            control += 1;
        }
    }
    stop.store(true, Ordering::Relaxed);
    racer.join().expect("racer");
    let leaks = RACE_SPAWNS - tally[0] - tally[4];
    let lost_lane = tally[4] + tally[5] + tally[6] + tally[7];
    println!(
        "STAGED race spawns={RACE_SPAWNS} leaks={leaks} (low={} above-cap={}) lost-lane={lost_lane} \
         control={control}/{RACE_CONTROL_SPAWNS} above-cap-staged={}",
        tally[1] + tally[3] + tally[5] + tally[7],
        tally[2] + tally[3] + tally[6] + tally[7],
        high.is_some(),
    );
    assert!(
        control > 0,
        "CONTROL: the racer's descriptors never reached a leaky spawn"
    );
    assert_eq!(
        leaks, 0,
        "a racer descriptor reached a lane child: {tally:?}"
    );
    assert_eq!(lost_lane, 0, "a lane child lacked fd 3 or 4: {tally:?}");
}

#[test]
fn no_descriptor_reaches_a_lane_child_while_another_thread_races_the_spawn() {
    const NAME: &str =
        "spawnfd::tests::no_descriptor_reaches_a_lane_child_while_another_thread_races_the_spawn";
    if std::env::var(STAGE_ENV).is_ok() {
        staged_race();
        return;
    }
    let out = run_staged(NAME, "race");
    assert!(out.contains("STAGED race"), "{out}");
    print!("{out}");
}

#[test]
fn a_stripped_child_inherits_nothing_above_stdio() {
    let held = unflagged_at_or_above(64);
    let fd = held.as_raw_fd();
    let plain = sh_holds(fd, false);
    let stripped = stripped_sh_holds(fd);
    drop(held);
    assert!(
        plain,
        "CONTROL: an unflagged descriptor must reach a leaky child"
    );
    assert!(!stripped, "the strip must keep fd {fd} out of the child");
}

/// The number a pipe made now puts its write end at, with every free number below 10 filled
/// first (the files come back to be held until the spawn): the number `std`'s exec-error pipe
/// takes at the next spawn of a command with inherited stdio, which opens nothing before that
/// pipe, and leaves where it is (Trust's `std` moves the pipe's end to 10 or above when hooks
/// run). Only a process where nothing else opens descriptors (a staged child) can name it ahead.
fn next_pipe_write_end() -> (RawFd, Vec<std::fs::File>) {
    let mut low = Vec::new();
    loop {
        let null = std::fs::File::open("/dev/null").expect("open /dev/null");
        if null.as_raw_fd() >= 10 {
            break;
        }
        low.push(null);
    }
    let (_rd, wr) = std::io::pipe().expect("a probe pipe");
    (wr.as_raw_fd(), low)
}

fn staged_pass_at() {
    let child_reads = |pass_before_the_strip: bool, same_number: bool| -> String {
        let (rd, mut wr) = std::io::pipe().expect("a pipe");
        wr.write_all(b"x").expect("write the byte");
        let at = if same_number {
            rd.as_raw_fd()
        } else {
            wr.as_raw_fd()
        };
        drop(wr);
        let rd = OwnedFd::from(rd);
        let source = rd.as_raw_fd();
        #[cfg(target_vendor = "apple")]
        assert_eq!(
            flags(source) & FD_CLOFORK,
            0,
            "the ordinary pipe is unmarked"
        );
        let source_held = test_support::sh_held(source);
        let script = format!(
            "v=$(dd if=/dev/fd/{at} bs=1 count=1 2>/dev/null); \
             [ {source} -ne {at} ] && {source_held} && v=\"$v+{source}\"; printf %s \"$v\""
        );
        let mut cmd = std::process::Command::new("/bin/sh");
        if pass_before_the_strip {
            cmd.pass_at(rd, at).expect("pass the read end");
            cmd.inherit_nothing();
        } else {
            cmd.inherit_nothing();
            cmd.pass_at(rd, at).expect("pass the read end");
        }
        let out = cmd
            .args(["-c", &script])
            .stderr(std::process::Stdio::null())
            .output()
            .expect("run the probe");
        let read = String::from_utf8_lossy(&out.stdout).into_owned();
        format!("{read} at {at} from {source}")
    };
    for same_number in [false, true] {
        let read = child_reads(false, same_number);
        assert!(
            read.starts_with("x at "),
            "the child reads the passed pipe, including when its number is unchanged: {read}"
        );
        let read = child_reads(true, same_number);
        assert!(
            read.starts_with(" at "),
            "CONTROL: a strip added after the pass closes the number at the exec: {read}"
        );
    }

    #[cfg(target_vendor = "apple")]
    for same_number in [true, false] {
        let (lane, mut peer) = crate::kept_out(UnixStream::pair).expect("a kept-out pair");
        // NEGATIVE CONTROL: accepting the same number loses the lane at fork; copying a
        // marked input first instead exposes an unmarked copy to other forks while held.
        let at = if same_number {
            lane.as_raw_fd()
        } else {
            let free = std::fs::File::open("/dev/null").expect("reserve a different number");
            let at = free.as_raw_fd();
            assert_ne!(at, lane.as_raw_fd());
            drop(free);
            at
        };
        let supported = crate::spawn_lock::tests::clofork_supported();
        assert_eq!(
            flags(lane.as_raw_fd()) & FD_CLOFORK,
            if supported { FD_CLOFORK } else { 0 },
            "the input lane agrees with the kernel's supported flags"
        );
        let mut marked = std::process::Command::new("/bin/sh");
        marked.inherit_nothing();
        if !supported {
            peer.write_all(b"x").expect("the byte to pass");
            drop(peer);
            marked
                .pass_at(lane.into(), at)
                .expect("an unmarked input is supported");
            let script = format!("dd if=/dev/fd/{at} bs=1 count=1 2>/dev/null");
            let out = marked
                .args(["-c", &script])
                .output()
                .expect("read the passed input");
            assert!(out.status.success());
            assert_eq!(
                out.stdout, b"x",
                "close-on-exec passing still works without CLOFORK"
            );
            continue;
        }
        let error = marked
            .pass_at(lane.into(), at)
            .map(|_| ())
            .expect_err("a marked input is refused before any copy or pass hook");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported, "{error}");
        assert_eq!(flags(at), -1, "a refusal retains nothing at the target");
        let out = marked
            .args(["-c", "printf refused"])
            .output()
            .expect("a refusal left no pass hook in the command");
        assert!(out.status.success());
        assert_eq!(out.stdout, b"refused");
        assert_ne!(
            flags(peer.as_raw_fd()) & FD_CLOFORK,
            0,
            "the other lane remains kept out while the command lives"
        );
    }

    let mut missing = std::process::Command::new("/nonexistent/aterm-uds-pass-at-no-such-prog");
    missing.inherit_nothing();
    let null = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .expect("open /dev/null");
    let (at, _below_ten) = next_pipe_write_end();
    missing
        .pass_at(OwnedFd::from(null), at)
        .expect("pass /dev/null");
    let started = missing.status();
    assert_eq!(
        started.as_ref().map_err(io::Error::kind).err(),
        Some(io::ErrorKind::NotFound),
        "a failed exec is an Err though the pass names {at}, where std's exec-error pipe would \
         put its write end: {started:?}"
    );

    let mut cmd = std::process::Command::new("/bin/sh");
    let low = std::fs::File::open("/dev/null").expect("open /dev/null");
    let refused = cmd.pass_at(OwnedFd::from(low), 2).map(|_| ()).unwrap_err();
    assert_eq!(
        refused.kind(),
        io::ErrorKind::InvalidInput,
        "a pass onto the child's stdio is refused: {refused}"
    );
    let held = std::fs::File::open("/dev/null").expect("open /dev/null");
    let other = std::fs::File::open("/dev/null").expect("open /dev/null");
    let refused = cmd
        .pass_at(OwnedFd::from(other), held.as_raw_fd())
        .map(|_| ())
        .unwrap_err();
    assert_eq!(
        refused.kind(),
        io::ErrorKind::ResourceBusy,
        "a pass onto a number this process holds is refused: {refused}"
    );
    println!("STAGED pass-at ok");
}

/// A `std` command given a pipe's read end after the strip holds it at the number it names, the
/// same open file (the child reads the byte written before the spawn), and nothing at the number
/// it came from; a strip added after the pass marks that number close-on-exec again and the exec
/// closes it. The command holds the number from the pass on, so a failed exec is an `Err` even
/// when the number is the one `std`'s exec-error pipe would otherwise take, and a number this
/// process already holds is refused. Staged, so those numbers are known ahead.
/// An ordinary descriptor already at its number also reaches the child. On Apple, a
/// close-on-fork input is refused at both the same and a different number before a copy or pass
/// hook is installed, and the command still runs normally after that refusal.
#[test]
fn a_descriptor_passed_after_the_strip_reaches_the_child_at_its_number() {
    const NAME: &str =
        "spawnfd::tests::a_descriptor_passed_after_the_strip_reaches_the_child_at_its_number";
    if std::env::var(STAGE_ENV).is_ok() {
        staged_pass_at();
        return;
    }
    let out = run_staged(NAME, "pass-at");
    assert!(out.contains("STAGED pass-at ok"), "{out}");
}

#[test]
fn the_descriptor_bound_is_the_kernels_per_process_maximum() {
    let (tool, args): (&str, &[&str]) = if cfg!(target_vendor = "apple") {
        ("/usr/sbin/sysctl", &["-n", "kern.maxfilesperproc"])
    } else {
        ("/bin/sh", &["-c", "ulimit -Hn"])
    };
    let out = std::process::Command::new(tool)
        .args(args)
        .output()
        .expect("run the tool");
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim();
    let want = if text == "unlimited" {
        RawFd::MAX
    } else {
        text.parse::<RawFd>()
            .unwrap_or_else(|e| panic!("{tool} answered {text:?}: {e}"))
    };
    assert_eq!(descriptor_bound(), want);
    assert_eq!(InheritStrip::prepare().bound(), want);
}

/// The listing tier, in a staged child (it marks the table it runs in): a
/// table that fits one listing is vouched for whole — an unflagged descriptor
/// is flagged with no number probed — and one that overflows it is vouched for
/// up to its last entry, the rest left to the loop, which reaches the table's
/// end and no further (not `kern.maxfilesperproc`: a spawn with 2,100
/// descriptors open costs 10–13 ms probing that far) and still keeps a
/// descriptor beyond the listing out of a stripped child.
#[cfg(target_vendor = "apple")]
fn staged_listing() {
    let bound = descriptor_bound();
    let low = unflagged_at_or_above(64);
    assert!(
        darwin_strip::mark_listed(bound).is_empty(),
        "a small table is listed whole"
    );
    assert_ne!(
        flags(low.as_raw_fd()) & FD_CLOEXEC,
        0,
        "and what it lists is flagged"
    );

    let more = u64::try_from(darwin_strip::SLOTS).unwrap() + 4096;
    assert!(raise_soft_limit(more) >= more, "a soft limit above {more}");
    let file = std::fs::File::open("/dev/null").expect("open /dev/null");
    let filler: Vec<OwnedFd> = (0..darwin_strip::SLOTS + 64)
        .map(|_| park(file.try_clone().expect("dup").into(), 3))
        .collect();
    let past = filler.iter().map(AsRawFd::as_raw_fd).max().unwrap_or(3) + 1;
    let beyond = unflagged_at_or_above(past);
    let rest = darwin_strip::mark_listed(bound);
    assert!(
        (4..=beyond.as_raw_fd()).contains(&rest.start),
        "a full listing vouches up to its last entry: {rest:?}"
    );
    assert!(
        rest.end > beyond.as_raw_fd() && rest.end < bound,
        "the loop past it reaches the table's end, beyond fd {} and short of the bound {bound}: {rest:?}",
        beyond.as_raw_fd()
    );
    assert_eq!(
        flags(beyond.as_raw_fd()) & FD_CLOEXEC,
        0,
        "PRECONDITION: fd {} lies beyond the listing",
        beyond.as_raw_fd()
    );
    assert!(
        !stripped_sh_holds(beyond.as_raw_fd()),
        "the loop past the listing kept fd {} out",
        beyond.as_raw_fd()
    );
    drop(filler);
    println!(
        "STAGED listing ok rest={rest:?} beyond={}",
        beyond.as_raw_fd()
    );
}

/// One `PROC_PIDLISTFDS` vouches for a table that fits it — avoiding an ~8 ms
/// per-spawn loop to `kern.maxfilesperproc` — and a table past one listing
/// still leaks nothing.
#[cfg(target_vendor = "apple")]
#[test]
fn the_darwin_strip_lists_the_table_and_probes_only_past_a_full_listing() {
    const NAME: &str =
        "spawnfd::tests::the_darwin_strip_lists_the_table_and_probes_only_past_a_full_listing";
    if std::env::var(STAGE_ENV).is_ok() {
        staged_listing();
        return;
    }
    let out = run_staged(NAME, "listing");
    assert!(out.contains("STAGED listing ok"), "{out}");
}

/// libnotify's once-gate, read where libSystem keeps it: `_os_alloc_once_table`
/// (exported by libsystem_kernel, `<os/alloc_once_private.h>`), one
/// `{ once, ptr }` slot per library, libnotify's at index 0 — the `calibrate`
/// stage below checks that index on the running macOS.
#[cfg(target_vendor = "apple")]
fn libnotify_gate() -> isize {
    use std::sync::atomic::{AtomicIsize, Ordering};
    #[repr(C)]
    struct AllocOnce {
        once: AtomicIsize,
        _ptr: *mut core::ffi::c_void,
    }
    unsafe extern "C" {
        static _os_alloc_once_table: [AllocOnce; 1];
    }
    // SAFETY: an exported libSystem table, read in place; `once` is the word `os_once` itself
    // updates atomically, and slot 0 lies within the table.
    unsafe { _os_alloc_once_table[0].once.load(Ordering::Acquire) }
}

/// The staged child: libnotify's gate before and after one step, where
/// nothing else has touched libnotify yet.
#[cfg(target_vendor = "apple")]
fn staged_libnotify_gate(stage: &str) {
    let fresh = libnotify_gate();
    assert_eq!(
        fresh, 0,
        "PRECONDITION: a fresh test process must start with libnotify's gate \
         untouched, else no step here can be seen passing it"
    );
    if stage == "libnotify-calibrate" {
        let _ = descriptor_bound();
        assert_eq!(
            libnotify_gate(),
            0,
            "CALIBRATION: reading the descriptor bound moved slot 0, so the \
             other stage cannot tell the libnotify call from it"
        );
        pass_libnotify_gate();
        assert_ne!(
            libnotify_gate(),
            0,
            "CALIBRATION: a libnotify call left slot 0 at 0 — slot 0 is not \
             libnotify's gate on this macOS, and this test reads the wrong word"
        );
    } else {
        let _ = InheritStrip::prepare();
        assert_ne!(
            libnotify_gate(),
            0,
            "InheritStrip::prepare left libnotify's once-gate cold: a fork after \
             it can still catch another thread inside that gate and lose its \
             child in fork() (`os_once_t is corrupt`)"
        );
    }
    println!("STAGED {stage} ok");
}

/// In a fresh process (a staged child), where nothing has touched libnotify,
/// [`InheritStrip::prepare`] leaves its once-gate passed, so a fork after it
/// finds libnotify's globals built whatever another thread is calling
/// ([`pass_libnotify_gate`]).
#[cfg(target_vendor = "apple")]
#[test]
fn preparing_a_fork_passes_the_libnotify_gate() {
    const NAME: &str = "spawnfd::tests::preparing_a_fork_passes_the_libnotify_gate";
    if let Ok(stage) = std::env::var(STAGE_ENV) {
        staged_libnotify_gate(&stage);
        return;
    }
    for stage in ["libnotify-calibrate", "libnotify-prepare"] {
        let out = run_staged(NAME, stage);
        assert!(out.contains(&format!("STAGED {stage} ok")), "{out}");
    }
}

#[test]
fn adoption_takes_an_exclusively_owned_inherited_socket() {
    let (near, mut far) = UnixStream::pair().expect("socketpair");
    let fd = std::os::fd::IntoRawFd::into_raw_fd(near);
    // SAFETY: this fixture alone holds the unowned raw number; the other end has a different
    // descriptor, and nobody closes/replaces this one.
    assert_eq!(unsafe { fcntl(fd, F_SETFD, 0) }, 0);
    // SAFETY: the owner was given up above; this call consumes it exactly once.
    let adopted = unsafe { adopt_inherited_socket(fd) }.expect("adopt inherited socket");
    assert_ne!(adopted.as_raw_fd(), fd, "the adoption is a copy");
    assert_ne!(flags(adopted.as_raw_fd()) & FD_CLOEXEC, 0);
    let mut adopted = UnixStream::from(adopted);
    let limit = Some(Duration::from_secs(60));
    adopted.set_write_timeout(limit).unwrap();
    far.set_read_timeout(limit).unwrap();
    adopted.write_all(b"x").expect("write through the copy");
    let mut byte = [0u8; 1];
    far.read_exact(&mut byte).expect("the far end reads it");
    assert_eq!(&byte, b"x");
}

/// The number [`an_inherited_socket_is_adopted_while_its_number_is_looked_up_elsewhere`]
/// adopts at, round after round: one no other test holds or looks up.
#[cfg(target_vendor = "apple")]
const CONTENDED: RawFd = 1500;

/// Adopting an inherited socket succeeds while another thread looks its number
/// up on `/dev/fd` (which on Darwin causes a `/dev/fd` path lookup of the same
/// number to fail with `EBADF`; see [`descriptor_metadata`]).
#[cfg(target_vendor = "apple")]
#[test]
fn an_inherited_socket_is_adopted_while_its_number_is_looked_up_elsewhere() {
    const F_DUPFD: i32 = 0;
    let mut refused = Vec::new();
    let rounds = test_support::racing_lookups(CONTENDED, 20, |looker| {
        let (near, _far) = UnixStream::pair().expect("socketpair");
        // SAFETY: `F_DUPFD` returns a fresh, inheritable descriptor or -1.
        let fd = unsafe { fcntl(near.as_raw_fd(), F_DUPFD, CONTENDED) };
        assert_eq!(
            fd, CONTENDED,
            "fd {CONTENDED} is held elsewhere in this process"
        );
        drop(near);
        looker.probe();
        // SAFETY: `fd` was just made here and has no other owner; nothing else in this process
        // names it.
        match unsafe { adopt_inherited_socket(fd) } {
            Ok(adopted) => drop(adopted),
            Err(e) => {
                refused.push(e.to_string());
                // SAFETY: a refusal leaves `fd` open and still ours alone.
                drop(unsafe { OwnedFd::from_raw_fd(fd) });
            }
        }
    });
    assert!(
        refused.is_empty(),
        "{} of {rounds} adoptions refused an open socket: {:?}",
        refused.len(),
        refused.first()
    );
}

/// The numbers [`the_shell_probe_reads_a_contended_number_exactly`] holds in its staged
/// child: one the probe duplicates (below 10) and one it finds in the `/dev/fd` listing.
#[cfg(target_vendor = "apple")]
const SHELL_PROBED: [RawFd; 2] = [7, 1700];

/// [`crate::host::sh_held_any`] reads a contended descriptor number accurately
/// while another thread looks the held number up on `/dev/fd`
/// ([`test_support::racing_lookups`]; see [`descriptor_metadata`] for why
/// `[ -e /dev/fd/N ]` misreads under contention).
#[cfg(target_vendor = "apple")]
#[test]
fn the_shell_probe_reads_a_contended_number_exactly() {
    const NAME: &str = "spawnfd::tests::the_shell_probe_reads_a_contended_number_exactly";
    if std::env::var(STAGE_ENV).is_ok() {
        staged_shell_probe();
        return;
    }
    run_staged(NAME, "shell-probe");
}

/// The staged half of [`the_shell_probe_reads_a_contended_number_exactly`].
#[cfg(target_vendor = "apple")]
fn staged_shell_probe() {
    unsafe extern "C" {
        fn dup2(old: RawFd, new: RawFd) -> RawFd;
    }
    const PER_ROUND: usize = 50;
    for held in SHELL_PROBED {
        let absent = held + 1;
        require_soft_limit(u64::try_from(absent).unwrap() + 1, absent);
        assert_eq!(flags(absent), -1, "fd {absent} is open in the staged child");
        let (near, _far) = UnixStream::pair().expect("socketpair");
        // SAFETY: `dup2` onto a number this staged child does not hold (its table is stdio alone)
        // makes a fresh, inheritable descriptor there.
        assert_eq!(
            unsafe { dup2(near.as_raw_fd(), held) },
            held,
            "dup2 to {held}"
        );
        // SAFETY: just made here; nothing else owns it.
        let _held = unsafe { OwnedFd::from_raw_fd(held) };
        let script = format!(
            "m=0; i=0; while [ $i -lt {PER_ROUND} ]; do {} || m=$((m+1)); {} && m=$((m+1)); \
             i=$((i+1)); done; exit $m",
            crate::host::sh_held(held),
            crate::host::sh_held(absent),
        );
        let mut misread = Vec::new();
        let mut shells = 0;
        let rounds = test_support::racing_lookups(held, 20, |looker| {
            let mut contended = false;
            for _ in 0..PER_ROUND {
                contended |= looker.probe();
            }
            // Spend the shell work on observed contention, not on unsuccessful
            // calibration rounds that consume the same hang detector.
            if !contended {
                return;
            }
            shells += 1;
            let status =
                test_support::leaky_status("/bin/sh", &["-c", &script]).expect("run the probe");
            if status.code() != Some(0) {
                misread.push(status.code());
            }
        });
        assert!(
            misread.is_empty(),
            "fd {held}: {} of {shells} shells misread in {rounds} calibration rounds \
             (exit: how many of their \
             2 × {PER_ROUND} reads were wrong): {misread:?}",
            misread.len()
        );
    }
}

#[cfg(target_vendor = "apple")]
unsafe extern "C" {
    fn change_fdguard_np(
        fd: RawFd,
        guard: *const u64,
        guardflags: u32,
        nguard: *const u64,
        nguardflags: u32,
        fdflagsp: *mut i32,
    ) -> i32;
}

/// Darwin kills a process that duplicates a descriptor guarded with `GUARD_DUP`
/// (`EXC_GUARD`: `SIGKILL`), and a guard forces close-on-exec.
#[cfg(target_vendor = "apple")]
#[test]
fn a_descriptor_guarded_against_copies_is_read_never_copied() {
    const NAME: &str = "spawnfd::tests::a_descriptor_guarded_against_copies_is_read_never_copied";
    if std::env::var(STAGE_ENV).is_ok() {
        staged_guarded();
        return;
    }
    let out = run_staged(NAME, "guarded");
    assert!(out.contains("STAGED guarded ok"), "{out}");
}

#[cfg(target_vendor = "apple")]
fn staged_guarded() {
    const GUARD_CLOSE: u32 = 1 << 0;
    const GUARD_DUP: u32 = 1 << 1;
    const GUARD_ID: u64 = 0x5350_4157_4e46_4401;
    let (near, _far) = UnixStream::pair().expect("socketpair");
    let guarded = std::os::fd::IntoRawFd::into_raw_fd(near);
    let mut fdflags = 0;
    // SAFETY: guards a descriptor this stage made and alone names (its owner given up above, so
    // nothing closes it); every pointer is a live local or NULL.
    let rc = unsafe {
        change_fdguard_np(
            guarded,
            std::ptr::null(),
            0,
            &GUARD_ID,
            GUARD_CLOSE | GUARD_DUP,
            &mut fdflags,
        )
    };
    assert_eq!(rc, 0, "guard fd {guarded}: {}", io::Error::last_os_error());
    // SAFETY: `guarded` is uniquely owned here, and refusal leaves it open.
    let refused = unsafe { adopt_inherited_socket(guarded) }
        .expect_err("a guarded socket is close-on-exec, never an inherited lane");
    assert_eq!(refused.kind(), io::ErrorKind::InvalidInput, "{refused}");

    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-uds-guarded.")
        .tempdir()
        .expect("scratch");
    let (lifeline, far) = crate::lifeline::Lifeline::open(scratch.path()).expect("a lifeline");
    drop(lifeline);
    let far = OwnedFd::from(far);
    let copy = std::os::fd::IntoRawFd::into_raw_fd(far.try_clone().expect("copy"));
    let closed = crate::lifeline::close_inherited_copies(&far).expect("sweep");
    assert_eq!(closed, [copy], "the sweep closes the lifeline's copy alone");
    assert_ne!(flags(guarded), -1, "the guarded socket is still held");
    println!("STAGED guarded ok");
}

/// The lane spawn retries a program still open for writing, as [`Command`]
/// does: Linux refuses the exec (`ETXTBSY`) while anything holds the file open
/// for writing, the lane arm's child reports that on its status pipe, and the
/// spawn pauses and retries once the writer lets go.
#[cfg(target_os = "linux")]
#[test]
fn a_lane_spawn_refused_as_text_busy_starts_once_the_writer_lets_go() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let path = dir.path().join("busy");
    let mut writer = std::fs::File::create(&path).expect("create");
    writer.write_all(b"#!/bin/sh\nexit 7\n").expect("write");
    writer
        .set_permissions(std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    let tried_again = std::sync::Arc::new(AtomicBool::new(false));
    let seen = std::sync::Arc::clone(&tried_again);
    TEXT_BUSY_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            seen.store(true, Ordering::SeqCst);
            drop(writer);
        }));
    });
    let mut child = spawn(LaneCommand::new(&path)).expect("started once the writer let go");
    assert_eq!(child.wait().expect("reaped").code(), Some(7));
    assert!(
        tried_again.load(Ordering::SeqCst),
        "the first try was refused and the spawn paused"
    );
    let _stays = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("a writer that stays");
    let refused = spawn(LaneCommand::new(&path)).expect_err("refused once the window passed");
    assert_eq!(
        refused.kind(),
        io::ErrorKind::ExecutableFileBusy,
        "{refused}"
    );
}

/// A DETACHED child leads a session of its own, so it leads its own process group too and no
/// job's signal reaches it, while it holds the two lanes at 3 and 4 and stdio on `/dev/null`:
/// it says its pid on fd 4 and waits on fd 3, and its session and group, read while it waits,
/// are its own pid. NEGATIVE CONTROL: the same child by [`spawn_with_two_fds`] stays in this
/// process's session and group.
#[cfg(target_vendor = "apple")]
#[test]
fn a_detached_child_leads_a_session_of_its_own() {
    let run = |detached: bool| {
        let (mut near_verb, far_verb) = UnixStream::pair().expect("socketpair");
        let (near_push, far_push) = UnixStream::pair().expect("socketpair");
        test_support::bound_read(&near_push).expect("bound the read");
        let mut cmd = LaneCommand::new("/bin/sh");
        cmd.args(["-c", "echo $$ >&4; read line <&3"])
            .stdin(ChildStdio::Null)
            .stdout(ChildStdio::Null)
            .stderr(ChildStdio::Null);
        let mut child = if detached {
            detached_with_two_fds(cmd, far_verb.into(), far_push.into())
        } else {
            spawn_with_two_fds(cmd, far_verb.into(), far_push.into())
        }
        .expect("spawn sh");
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(&near_push), &mut line)
            .expect("its pid on fd 4");
        let pid: i32 = line.trim().parse().expect("a pid");
        assert_eq!(u32::try_from(pid).ok(), Some(child.id()));
        // SAFETY: two integer reads of a live child's ids.
        let (sid, pgid) = unsafe { (libc::getsid(pid), libc::getpgid(pid)) };
        near_verb.write_all(b"done\n").expect("write fd 3");
        assert!(child.wait().expect("reap").success());
        (pid, sid, pgid)
    };
    let (pid, sid, pgid) = run(true);
    assert_eq!(
        (sid, pgid),
        (pid, pid),
        "detached: its own session and group"
    );
    // SAFETY: integer reads of this process's own ids.
    let (my_sid, my_pgid) = unsafe { (libc::getsid(0), libc::getpgrp()) };
    let (_, sid, pgid) = run(false);
    assert_eq!(
        (sid, pgid),
        (my_sid, my_pgid),
        "a lane spawn stays in this job"
    );
}
