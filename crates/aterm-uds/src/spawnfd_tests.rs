// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `spawnfd`'s tests.

use super::*;
use crate::host::could_not_run;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::time::Duration;

/// Present in the environment means "you ARE the staged child".
pub(super) const STAGE_ENV: &str = "ATERM_UDS_SPAWNFD_STAGE";

/// The kernel's name for [`descriptor_bound`], for a skip line.
const BOUND_NAME: &str = if cfg!(target_vendor = "apple") {
    "kern.maxfilesperproc"
} else {
    "the hard RLIMIT_NOFILE"
};

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

/// Raise the soft descriptor limit toward `request`, and end the calling test COULD NOT RUN when it cannot pass `floor`.
#[track_caller]
pub(super) fn require_soft_limit(request: u64, floor: RawFd) {
    if raise_soft_limit(request) <= u64::try_from(floor).unwrap_or(0) {
        could_not_run(format!("the hard descriptor limit is below {floor}"));
    }
}

/// The binary translator this process runs under ([`crate::process::translator`]).
pub(crate) use crate::process::translator;

/// Why `std` is no oracle for a spawn that fails under Rosetta on Linux, for the line a test prints instead of the comparison ([`translator`]).
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

/// A child reading fd 3 and echoing it to fd 4, with what it finds at `probe`.
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

/// Spawn [`echo_command`] on the two lanes and return what came back on fd 4.
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

/// The numbers a stage that must find them free is started with held: 3 and 4,
/// where the lanes land, and 5 to 8, where `std`'s exec-status pipe would.
const HELD: std::ops::RangeInclusive<RawFd> = 3..=8;

/// Present in a staged child's environment when its launcher holds [`HELD`].
const HELD_ENV: &str = "ATERM_UDS_SPAWNFD_HELD";

/// Re-run this test binary as the staged child for `test` / `stage`, through [`InheritStrip`] so it inherits stdio alone, and return its stdout.
pub(super) fn run_staged(test: &str, stage: &str) -> String {
    let mut launch = std::process::Command::new(std::env::current_exe().expect("the test binary"));
    launch.args(["--exact", test, "--nocapture", "--test-threads=1"]);
    launch_staged(launch, stage)
}

/// [`run_staged`] with `SIGHUP` ignored across the exec, as `nohup` (or any launcher that ignores it) leaves a test run.
pub(super) fn run_staged_hup_ignored(test: &str, stage: &str) -> String {
    let mut launch = std::process::Command::new("/bin/sh");
    launch
        .arg("-c")
        .arg("trap '' HUP; exec \"$0\" \"$@\"")
        .arg(std::env::current_exe().expect("the test binary"))
        .args(["--exact", test, "--nocapture", "--test-threads=1"]);
    launch_staged(launch, stage)
}

/// [`run_staged`] for a stage that needs [`HELD`] free: `/bin/sh` opens
/// `/dev/null` on each of them and `exec`s this test binary with them, and the
/// staged child calls [`release_held`].
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

/// Free [`HELD`] in a staged child whose launcher held them: each must still be
/// the launcher's `/dev/null`, and is closed.
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
        // SAFETY: `fd` is open (just checked); the `File` is never dropped, so
        // it only borrows the descriptor for one `fstat`.
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

/// Start a staged child (`launch`, the stage named in [`STAGE_ENV`]) through
/// [`InheritStrip`] and return its stdout, failing on its failure.
fn launch_staged(mut launch: std::process::Command, stage: &str) -> String {
    launch.env(STAGE_ENV, stage);
    let strip = InheritStrip::prepare();
    // SAFETY: the closure runs between fork and exec and calls only `InheritStrip::apply`, which is async-signal-safe.
    unsafe {
        launch.pre_exec(move || {
            strip.apply();
            Ok(())
        });
    }
    let out = launch.output().expect("start the staged child");
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

/// Move `fd` to the lowest free number at or above `floor`, close-on-exec (as `UnixStream::pair()` hands its ends out), the original closed.
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

/// Put `fd` at exactly `number` (asserted free), close-on-exec.
fn place(fd: OwnedFd, number: RawFd) -> OwnedFd {
    unsafe extern "C" {
        fn dup2(old: RawFd, new: RawFd) -> RawFd;
    }
    assert_eq!(
        flags(number),
        -1,
        "fd {number} is already open in the staged child"
    );
    // SAFETY: `fd` is live, `number` free; `from_raw_fd` takes exactly the
    // descriptor `dup2` created, after the original is dropped.
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

/// Set this process's soft `RLIMIT_NOFILE` to `soft` (capped by the hard
/// limit, which is kept); the soft limit it ends at.
fn set_soft_limit(soft: u64) -> u64 {
    let mut limit = Rl { cur: 0, max: 0 };
    // SAFETY: a live out-parameter.
    assert_eq!(unsafe { getrlimit(NOFILE, &mut limit) }, 0, "getrlimit");
    limit.cur = soft.min(limit.max);
    // SAFETY: a live, initialised limit that keeps the hard one; moving the
    // soft limit below the hard one needs no privilege.
    assert_eq!(unsafe { setrlimit(NOFILE, &limit) }, 0, "setrlimit");
    limit.cur
}

/// Raise the soft limit to at least `want` (capped by the hard limit).
pub(super) fn raise_soft_limit(want: u64) -> u64 {
    let mut limit = Rl { cur: 0, max: 0 };
    // SAFETY: a live out-parameter.
    assert_eq!(unsafe { getrlimit(NOFILE, &mut limit) }, 0, "getrlimit");
    if limit.cur >= want {
        limit.cur
    } else {
        set_soft_limit(want)
    }
}

/// The lane child inherits its two lanes and nothing else above stdio. An
/// unflagged descriptor of this process (from 64 up) must not reach it, while
/// fds 3 and 4 still round-trip.
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

/// Where the high stage puts the verb source: Darwin's `OPEN_MAX`, the first
/// number a file action may not name.
const HIGH_VERB: RawFd = 10_240;

/// One placement scenario, run INSIDE the staged child.
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

/// Every source reaches its number, wherever it started: on its own target (3 or
/// 4), on each other's targets, and at 10,240 and 20,000 (where Darwin's libc
/// refuses a file action) — each in a fresh table where those numbers are
/// held across the exec ([`HELD`]).
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

/// `off_the_targets` keeps a source already in range, and moves one at or
/// above the file-action limit to a close-on-exec copy below it, closing the
/// original.
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

/// One exec-failure scenario in the staged child: fds 3..=8 ([`HELD`]) FREE at the spawn,
/// which is where a `Command`'s own exec-status pipe would land — and where a
/// `pre_exec` `dup2` onto 3 and 4 would clobber it, turning the failure into
/// `Ok` (refuted by name by the descriptor-protocol model).
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

/// A failed exec is an error, never a child — with the numbers a status pipe
/// would take free, where a `pre_exec` `dup2` onto 3 and 4 would clobber it.
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

/// The handle reaps exactly once: a second `wait` answers from the first, and
/// `kill` after the reap signals nobody.
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

/// A bare name is searched on the CHILD's `PATH`, and the environment is
/// exactly what `env_exactly` gave: nothing of this process's leaks in.
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

/// A key named twice is set once, to its last value — what the `PATH` search
/// used and what the child's block holds agree.
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

/// `execvp`'s semantics, which `Command` (through `posix_spawnp`) has: a `PATH`
/// entry holding a file this process may NOT execute — execute bits, none of
/// them the owner's — is passed over for a later one, and is the error
/// (`EACCES`, not `ENOENT`) only when it is the only match; and a script with
/// no `#!` line (`ENOEXEC`) runs under `/bin/sh` with its arguments.
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

/// [`spawn`]: no lanes, and still nothing of this process's above stdio — an
/// unflagged descriptor (the control: a leaky spawn carries it) reaches
/// neither 3, 4 nor its own number in the child, while stdout arrives.
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

/// A 65,536 ceiling below Darwin's `kern.maxfilesperproc`.
const OLD_CAP: RawFd = 65_536;

/// Free numbers kept between a high stand-in and the host's maximum.
const HEADROOM: RawFd = 8;

/// The number the high stand-ins sit at: 70,000 (above [`OLD_CAP`], below
/// 92,160, one Mac's `kern.maxfilesperproc`), or just under this host's
/// maximum when that is lower.
fn above_old_cap() -> Option<RawFd> {
    let fd = 70_000.min(descriptor_bound().saturating_sub(HEADROOM));
    (fd > OLD_CAP).then_some(fd)
}

/// What `/bin/sh` finds at `fd`, started by `spawn` (`true`: the lane spawn;
/// `false`: a deliberately leaky raw spawn — the control,
/// [`test_support::leaky_status`]; not `std`'s `Command`, whose child Trust's
/// std hands nothing above stdio since SP-1).
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

/// A [`test_support::held_probe`]'s answer: exit 3 is "held", exit 0 is "clear", and
/// anything else is a probe that never ran, which must not read as either.
fn held(status: ExitStatus) -> bool {
    match status.code() {
        Some(3) => true,
        Some(0) => false,
        _ => panic!("the probe did not answer: {status:?}"),
    }
}

/// Same, for a raw `fork` whose child runs [`InheritStrip::apply`] before its
/// `execve` — the PTY child's mechanism, with no `std` spawn around it
/// ([`test_support::stripped_status`]): Trust's std strips a `pre_exec`
/// child itself since SP-1, which would keep the descriptor out with or
/// without this strip.
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

/// An unflagged descriptor above 65,536 reaches neither child. Darwin allocates
/// up to `kern.maxfilesperproc`; a stand-in at 70,000 must reach a leaky spawn
/// (the control) and neither the lane spawn nor an [`InheritStrip`]ped one.
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

/// How many spawns the race runs, and how many the control runs.
const RACE_SPAWNS: usize = 500;
const RACE_CONTROL_SPAWNS: usize = 100;

/// The low descriptor number the racing thread opens unflagged.
const RACE_LOW: RawFd = 2000;

/// Lower the soft limit, raise it, open unflagged descriptors at [`RACE_LOW`]
/// and `high` (an [`above_old_cap`] number, when staged), hold them briefly,
/// and close them until `stop`.
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

/// The shell probe a raced child runs: bit 1 for a racer descriptor in the low
/// range, bit 2 at `high` (above the old cap, when staged), bit 4 for a missing
/// lane, each read from the child's own table ([`test_support::sh_held_any`]).
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

/// Zero leaks under a racing thread over 500 spawns. A thread lowers and raises
/// the soft limit and opens unflagged descriptors (at 2000 and above 65,536)
/// while this one spawns.
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

/// A stripped child inherits nothing above stdio. A descriptor this process
/// holds without close-on-exec rides a leaky spawn (the control) and not a
/// fork that runs the strip, while the child's stdio still arrives.
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

/// The bound is the kernel's per-process maximum — `kern.maxfilesperproc` on
/// Darwin, the hard `RLIMIT_NOFILE` on Linux — pinned against the system's own
/// tools, which read the same numbers by name.
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
    // SAFETY: an exported libSystem table, read in place; `once` is the word
    // `os_once` itself updates atomically, and slot 0 lies within the table.
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

/// The raw boundary's valid caller gives up its only owner before admission.
#[test]
fn adoption_takes_an_exclusively_owned_inherited_socket() {
    let (near, mut far) = UnixStream::pair().expect("socketpair");
    let fd = std::os::fd::IntoRawFd::into_raw_fd(near);
    // SAFETY: this fixture alone holds the unowned raw number; the other end
    // has a different descriptor, and nobody closes/replaces this one.
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
        // SAFETY: `fd` was just made here and has no other owner; nothing else
        // in this process names it.
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
        // SAFETY: `dup2` onto a number this staged child does not hold (its table is stdio alone) makes a fresh, inheritable descriptor there.
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
        let rounds = test_support::racing_lookups(held, 20, |looker| {
            for _ in 0..PER_ROUND {
                looker.probe();
            }
            let status =
                test_support::leaky_status("/bin/sh", &["-c", &script]).expect("run the probe");
            if status.code() != Some(0) {
                misread.push(status.code());
            }
        });
        assert!(
            misread.is_empty(),
            "fd {held}: {} of {rounds} shells misread (exit: how many of their \
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
    // SAFETY: guards a descriptor this stage made and alone names (its owner given up
    // above, so nothing closes it); every pointer is a live local or NULL.
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
