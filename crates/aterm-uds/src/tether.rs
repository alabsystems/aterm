// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Parent-death ties for helpers started through [`spawnfd::Tether`](crate::spawnfd::Tether),
//! including when the parent is killed outright or crashes. The platform's conditions below
//! determine whether the helper ends.
//!
//! Linux has the parent-death signal: the child is spawned with `PR_SET_PDEATHSIG` set to
//! `SIGTERM`, and a child that ignores it runs on. Darwin has none, so a tethered spawn there runs
//! this process's own executable as [`VERB`] first, under [`NAME`], and that tether leaves a
//! watcher behind as it execs the child in its own place: the parent's handle names the child
//! itself, its pid, its signals and its exit status. The watcher is a grandchild of the tether,
//! adopted by launchd, holding a kqueue `NOTE_EXIT` watch on the parent and on the child. It leads
//! a session of its own, made before it arms, so a listing of the child's group (a stage sweep's),
//! a kill of that group and a hangup of the child's terminal never count or reach it; it still ends
//! with the child by its exit watch. When the parent exits, the watcher attempts to end the child
//! through [`end_proven`] ([`end_watched`]): `SIGTERM`, then `SIGKILL` to one still there
//! [`END_GRACE`](crate::process::END_GRACE) later, each signal only while the pid still reads the
//! birth and kernel unique id the tether read of itself before it forked the watcher ([`Watched`]),
//! and its one thread has run ([`Proof`]): a child macOS's program check holds before its first
//! instruction gets no signal. The watcher exits after that attempt, even if the proof did not read
//! or the thread had not run, including during an exec; such a child can run on. It also exits when
//! the child exits first. The tether execs the child only once the watcher is armed and the parent
//! is still its own parent. A kill by the parent's name passes the watcher by.

#[cfg(unix)]
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;
use std::time::Duration;

use crate::process::{Birth, EndControl, EndRefusal, Ended, Identity, Lineage, end_proven};

/// The verb a tethered spawn runs this executable with: `<exe> __tether <parent pid> <program>
/// <argv0> [args…]`.
pub const VERB: &str = "__tether";

/// The `argv[0]` a tether runs under on Darwin, and so its watcher, a fork that never execs: not
/// the parent's. macOS's `killall` and `pkill` name a process by its `argv[0]`'s last part, not by
/// the kernel's name, which stays the executable's (measured on macOS 27.0), so a kill by the
/// window's name (`killall -9 aterm`, `pkill -9 aterm`) ends the window and leaves the watcher to
/// end the child. `aterm` is no part of it, as `pkill` matches a part.
pub const NAME: &str = "tether";

/// A tether's exit code when it started nothing: the pid it was given is not its parent (that
/// parent died, or never was), or its watcher did not arm (`env`'s code for its own failure).
pub const UNTETHERED: u8 = 125;

/// A tether's exit code when the child's exec failed (the shell's).
pub const NOT_RUN: u8 = 127;

/// A tied child as its watcher knows it: its pid, its birth and its kernel unique id, read before
/// the watcher was forked. On Darwin the tether reads itself, the process the child is exec'd into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Watched {
    /// Its pid.
    pub pid: i32,
    /// Its birth, which tells it from the next process given its pid.
    pub birth: Birth,
    /// Its kernel unique id ([`Lineage::uniqueid`]: `p_uniqueid` on Apple).
    pub uniqueid: u64,
}

impl Watched {
    /// The process `lineage` reads.
    #[must_use]
    pub const fn of(lineage: &Lineage) -> Self {
        Self {
            pid: lineage.identity.pid,
            birth: lineage.identity.birth,
            uniqueid: lineage.uniqueid,
        }
    }

    /// What [`end_watched`] re-proves before each signal: the pid and the birth, with no pid
    /// version, since the tether's exec of the child in its own place moves the version.
    #[must_use]
    pub const fn token(&self) -> Identity {
        Identity {
            pid: self.pid,
            birth: self.birth,
            idversion: None,
        }
    }
}

/// What [`end_watched`] reads of the child's pid before each signal, besides its identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Proof {
    /// Its kernel unique id ([`Lineage::uniqueid`]), which must still be the one read at arming.
    pub uniqueid: u64,
    /// Whether its one thread has run ([`thread_ran`](crate::process::thread_ran)), which it
    /// must have: no process whose thread has not run is signalled, a program macOS's program
    /// check holds before its first instruction included.
    pub ran: bool,
}

/// Ends `watched` as [`end_proven`] ends its [`Watched::token`]: `SIGTERM`, then `SIGKILL` to one
/// still there `grace` later, each signal only while `proof(pid)` reads the recorded unique id
/// and a thread that ran, between two identity reads that name the token. `control`'s identity
/// reads are judged with their pid version dropped, as the token has none; its signals and naps
/// are its own.
///
/// # Errors
/// [`end_proven`]'s: [`EndRefusal::Stale`] when the pid no longer reads the child's birth and
/// [`EndRefusal::NotProven`] when its unique id moved, its thread has not run, or its proof does
/// not read, each before any signal, or [`EndRefusal::SignalFailed`] when the kernel refused the
/// `SIGTERM` to a child still there. Once a `SIGTERM` went out the answer is `Ok`.
pub fn end_watched(
    watched: Watched,
    grace: Duration,
    control: &mut impl EndControl,
    mut proof: impl FnMut(i32) -> Option<Proof>,
) -> Result<Ended, EndRefusal> {
    let armed = Proof {
        uniqueid: watched.uniqueid,
        ran: true,
    };
    end_proven(watched.token(), grace, &mut Versionless(control), |token| {
        proof(token.pid) == Some(armed)
    })
}

/// An [`EndControl`] whose identity reads drop their pid version, for a token that has none. Its
/// signals get that token as it is, with no pid version: an inner arm whose signal re-reads the
/// identity must judge it as these reads do.
struct Versionless<'a, C>(&'a mut C);

impl<C: EndControl> EndControl for Versionless<'_, C> {
    fn identity(&mut self, pid: i32) -> Option<Identity> {
        self.0.identity(pid).map(|read| Identity {
            idversion: None,
            ..read
        })
    }
    fn signal(&mut self, token: &Identity, sig: i32) -> std::io::Result<()> {
        self.0.signal(token, sig)
    }
    fn nap(&mut self, pause: Duration) {
        self.0.nap(pause);
    }
}

/// The executable this process's tethered spawns run through ([`entry`], [`through`]).
static THROUGH: OnceLock<Option<Through>> = OnceLock::new();

/// An executable recorded to run through, with its file as it was recorded.
struct Through {
    /// Its path.
    exe: PathBuf,
    /// Its file's device and inode then; `None` when they did not read.
    file: Option<(u64, u64)>,
}

impl Through {
    /// `exe`, with the file at its path now.
    #[cfg(any(unix, test, debug_assertions))]
    fn of(exe: PathBuf) -> Self {
        let file = file_at(&exe);
        Self { exe, file }
    }

    /// Its path, while the file there is still the one recorded.
    fn unchanged(&self) -> Option<&Path> {
        (self.file.is_some() && file_at(&self.exe) == self.file).then_some(self.exe.as_path())
    }
}

/// The device and inode of the file at `path`.
fn file_at(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Called in an executable's `main` before anything that starts a thread. Asked for [`VERB`], it
/// serves it and answers the exit code. Otherwise it records this executable as the one this
/// process's tethered spawns run through on Darwin, and answers `None`.
#[must_use = "return a handled tether command's exit status to the caller"]
pub fn entry() -> Option<ExitCode> {
    #[cfg(unix)]
    {
        let mut args = std::env::args_os().skip(1);
        if args.next().is_some_and(|first| first == VERB) {
            return Some(serve(&args.collect::<Vec<_>>()));
        }
        let _ = THROUGH.get_or_init(|| std::env::current_exe().ok().map(Through::of));
    }
    None
}

/// The executable a tethered spawn runs through: this process's own once its `main` called
/// [`entry`], while the file at its path is still the one recorded then. `None` in a process
/// whose `main` never did (a test binary's), and once an update or a rebuild put another file at
/// that path, which may be a build from before [`VERB`] that reads the tether's words as a
/// window's command line: tethered spawns then start the child untethered on Darwin.
#[must_use]
pub fn executable() -> Option<&'static Path> {
    THROUGH.get()?.as_ref()?.unchanged()
}

/// TEST SEAM: this process's tethered spawns run through `exe`, an executable whose `main` calls
/// [`entry`] first, for a test binary whose own does not. The first record stands.
#[cfg(any(test, debug_assertions))]
pub fn through(exe: PathBuf) {
    let _ = THROUGH.set(Some(Through::of(exe)));
}

/// The tether's argv for `program` started from this process, after its `argv[0]` ([`NAME`]):
/// [`VERB`], this pid, the program, its `argv[0]`, its arguments. `None` when this process records
/// no executable to run through. The spawn judges the program's search first
/// (`spawnfd::platform::findable`), since the tether runs it only after the spawn returned.
#[cfg(target_vendor = "apple")]
pub(crate) fn route(
    program: &std::ffi::OsStr,
    arg0: Option<&std::ffi::OsStr>,
    args: &[OsString],
) -> Option<(PathBuf, Vec<OsString>)> {
    let exe = executable()?;
    let mut routed = Vec::with_capacity(args.len() + 4);
    routed.push(OsString::from(VERB));
    routed.push(OsString::from(std::process::id().to_string()));
    routed.push(program.to_owned());
    routed.push(arg0.unwrap_or(program).to_owned());
    routed.extend(args.iter().cloned());
    Some((exe.to_path_buf(), routed))
}

/// [`VERB`]: tie this process to `parent`, then exec the program in its place.
#[cfg(unix)]
fn serve(rest: &[OsString]) -> ExitCode {
    let [parent, program, argv0, args @ ..] = rest else {
        eprintln!("aterm: {VERB} <parent pid> <program> <argv0> [args…]");
        return ExitCode::from(UNTETHERED);
    };
    let Some(parent) = parent_pid(parent) else {
        eprintln!("aterm: {VERB}: {parent:?} is not a parent pid");
        return ExitCode::from(UNTETHERED);
    };
    if let Err(why) = hold(parent) {
        eprintln!(
            "aterm: {} was not started: {why}",
            Path::new(program).display()
        );
        return ExitCode::from(UNTETHERED);
    }
    // The spawn judged the search by the `PATH` it gave this tether, passing over an entry too
    // long to join to the name, as the search of a `PATH` a spawn sets does
    // (`spawnfd::platform::findable`): the exec searches it as that `PATH`, not as this process's
    // own, whose search ends at such an entry.
    let mut command = crate::spawnfd::command(program);
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    let error = command.arg0(argv0).args(args).exec();
    eprintln!("aterm: {}: {error}", Path::new(program).display());
    ExitCode::from(NOT_RUN)
}

/// Parse an unsigned decimal parent pid greater than one.
#[cfg(unix)]
fn parent_pid(arg: &std::ffi::OsStr) -> Option<i32> {
    arg.to_str()
        .filter(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|pid| pid.parse::<i32>().ok())
        .filter(|&pid| pid > 1)
}

/// Words for a pid that is not this process's parent: that parent died, or never was.
#[cfg(unix)]
fn not_parent(parent: i32) -> String {
    format!("pid {parent} is not its parent")
}

/// Whether `parent` is still this process's parent.
#[cfg(unix)]
fn parent_is(parent: i32) -> bool {
    // SAFETY: `getppid` takes nothing and cannot fail.
    unsafe { libc::getppid() == parent }
}

/// Tie this process to `parent` before its exec: Linux's parent-death signal, which the exec
/// keeps.
#[cfg(target_os = "linux")]
fn hold(parent: i32) -> Result<(), String> {
    bind(parent).map_err(|e| match e.raw_os_error() {
        Some(libc::ESRCH) => not_parent(parent),
        _ => format!("PR_SET_PDEATHSIG: {e}"),
    })
}

/// No death tie here: only a check that `parent` is still this process's parent.
#[cfg(all(unix, not(target_os = "linux"), not(target_vendor = "apple")))]
fn hold(parent: i32) -> Result<(), String> {
    if parent_is(parent) {
        Ok(())
    } else {
        Err(not_parent(parent))
    }
}

/// `PR_SET_PDEATHSIG` to `SIGTERM`, then the check that `parent` did not die before it was set:
/// the kernel signals when the THREAD that forked this process exits, and a child that ignores
/// `SIGTERM` runs on. Async-signal-safe (two system calls), for a spawn's hook between fork and
/// exec.
///
/// # Errors
/// `prctl`'s, or `ESRCH` when `parent` is no longer this process's parent.
#[cfg(target_os = "linux")]
pub(crate) fn bind(parent: i32) -> std::io::Result<()> {
    const PR_SET_PDEATHSIG: i32 = 1;
    unsafe extern "C" {
        fn prctl(option: i32, ...) -> i32;
    }
    // SAFETY: `PR_SET_PDEATHSIG` takes one integer, the signal; it touches no memory of ours.
    if unsafe {
        prctl(
            PR_SET_PDEATHSIG,
            libc::c_ulong::from(libc::SIGTERM.unsigned_abs()),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    if parent_is(parent) {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(libc::ESRCH))
    }
}

/// Leave the watcher behind: read this process, the child it will exec into, as the watcher will
/// know it ([`Watched`]; one that does not read arms nothing); fork, and fork again so launchd
/// adopts the watcher (the child this process becomes has no child of its own to wait for); wait
/// for it to arm, then check `parent` is still this process's parent, so the watch it armed is on
/// the right process.
#[cfg(target_vendor = "apple")]
fn hold(parent: i32) -> Result<(), String> {
    use std::io::Read as _;
    use std::os::fd::AsRawFd as _;
    if !parent_is(parent) {
        return Err(not_parent(parent));
    }
    let null = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .map_err(|e| format!("/dev/null: {e}"))?;
    let (mut armed, arming) = std::io::pipe().map_err(|e| format!("a pipe: {e}"))?;
    // SAFETY: `getpid` takes nothing and cannot fail.
    let child = unsafe { libc::getpid() };
    let Some(watched) = crate::process::read_lineage(child).map(|lineage| Watched::of(&lineage))
    else {
        return Err("its watcher did not arm: this process did not read itself".to_string());
    };
    // SAFETY: `fork` takes nothing. This process has one thread, since its `main` calls `entry`
    // before anything that starts one, so no other thread holds a lock across the fork and the
    // forked branch may read, keep time and sleep as `end_proven` does. That branch never
    // returns.
    let first = unsafe { libc::fork() };
    if first < 0 {
        return Err(format!("fork: {}", std::io::Error::last_os_error()));
    }
    if first == 0 {
        // SAFETY: the forked child, as above, with one thread: `fork`, then the watcher (a fork
        // of that one thread, with `null`, `arming` and `armed` open) or `_exit`.
        unsafe {
            let second = libc::fork();
            if second == 0 {
                watch(
                    parent,
                    watched,
                    null.as_raw_fd(),
                    arming.as_raw_fd(),
                    armed.as_raw_fd(),
                );
            }
            libc::_exit(i32::from(second < 0));
        }
    }
    let mut status = 0;
    // SAFETY: `first` is this process's own child; `status` is a live local.
    while unsafe { libc::waitpid(first, &mut status, 0) } < 0
        && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
    {}
    drop(arming);
    drop(null);
    let mut byte = [0_u8; 1];
    let said = loop {
        match armed.read(&mut byte) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            said => break said,
        }
    };
    if !parent_is(parent) {
        return Err(not_parent(parent));
    }
    match said {
        Ok(1) => Ok(()),
        _ => Err("its watcher did not arm".to_string()),
    }
}

/// One `EVFILT_PROC` `NOTE_EXIT` change for `pid`, with a receipt.
#[cfg(target_vendor = "apple")]
const fn exit_of(pid: i32) -> libc::kevent {
    libc::kevent {
        ident: pid.unsigned_abs() as usize,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_RECEIPT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

/// Whether `events` hold `pid`'s exit.
#[cfg(target_vendor = "apple")]
fn exited(events: &[libc::kevent], pid: i32) -> bool {
    events
        .iter()
        .any(|e| e.ident == pid.unsigned_abs() as usize && e.fflags & libc::NOTE_EXIT != 0)
}

/// The watcher: a session of its own (`setsid`; one it cannot lead exits, unarmed, and the tether
/// starts nothing), stdio on `/dev/null`, a kqueue exit watch on `parent` and on the `watched`
/// child, the arming byte, then a wait. The child's exit ends it; the parent's ends the child
/// first, as [`end_watched`] ends one, with the live reads and `kill(2)` ([`EndControlLive`]), the
/// child's [`Proof`] read by [`read_lineage`] and [`thread_ran`]. A child whose exit came first
/// reads as gone there (a zombie reads nothing, a reused pid another birth) and gets no signal, as
/// does one whose thread has not run.
///
/// [`EndControlLive`]: crate::process::EndControlLive
/// [`read_lineage`]: crate::process::read_lineage
/// [`thread_ran`]: crate::process::thread_ran
///
/// # Safety
/// Only in a forked child of the tether, with `null`, `arming` and `armed` open, forked while the
/// tether had one thread, so no lock is held that the reads, the clock and the sleep would wait
/// on.
#[cfg(target_vendor = "apple")]
unsafe fn watch(parent: i32, watched: Watched, null: i32, arming: i32, armed: i32) -> ! {
    const BLANK: libc::kevent = exit_of(0);
    let child = watched.pid;
    // SAFETY (whole body): the function's contract.
    unsafe {
        // First, before it arms: out of the child's group and session. The watcher is the
        // tether's second fork, never a group leader, so `setsid` has no cause to refuse; if it
        // does, the watcher exits unarmed and the tether starts nothing.
        if libc::setsid() < 0 {
            libc::_exit(1);
        }
        libc::close(armed);
        for fd in 0..3 {
            libc::dup2(null, fd);
        }
        if null > 2 {
            libc::close(null);
        }
        let kq = libc::kqueue();
        if kq < 0 {
            libc::_exit(1);
        }
        let changes = [exit_of(parent), exit_of(child)];
        let mut receipts = [BLANK; 2];
        let got = libc::kevent(
            kq,
            changes.as_ptr(),
            2,
            receipts.as_mut_ptr(),
            2,
            std::ptr::null(),
        );
        if got != 2
            || receipts
                .iter()
                .any(|r| r.flags & libc::EV_ERROR != 0 && r.data != 0)
        {
            libc::_exit(1);
        }
        if libc::write(arming, [1_u8].as_ptr().cast(), 1) != 1 {
            libc::_exit(1);
        }
        libc::close(arming);
        loop {
            let mut events = [BLANK; 2];
            let got = libc::kevent(
                kq,
                std::ptr::null(),
                0,
                events.as_mut_ptr(),
                2,
                std::ptr::null(),
            );
            let Ok(got) = usize::try_from(got) else {
                if *libc::__error() == libc::EINTR {
                    continue;
                }
                libc::_exit(1);
            };
            let events = &events[..got.min(2)];
            if exited(events, child) {
                libc::_exit(0);
            }
            if exited(events, parent) {
                let _ended = end_watched(
                    watched,
                    crate::process::END_GRACE,
                    &mut crate::process::EndControlLive,
                    |pid| {
                        Some(Proof {
                            uniqueid: crate::process::read_lineage(pid)?.uniqueid,
                            ran: crate::process::thread_ran(pid)?,
                        })
                    },
                );
                libc::_exit(0);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{Through, parent_pid};
    use std::ffi::OsStr;

    #[test]
    fn parent_pid_accepts_only_unsigned_pids_above_init() {
        assert_eq!(parent_pid(OsStr::new("42")), Some(42));
        assert_eq!(parent_pid(OsStr::new("1")), None);
        assert_eq!(parent_pid(OsStr::new("0")), None);
        assert_eq!(parent_pid(OsStr::new("+42")), None);
        assert_eq!(parent_pid(OsStr::new("-42")), None);
    }

    #[test]
    fn a_file_put_in_place_of_the_recorded_executable_is_not_run_through() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let exe = dir.path().join("aterm");
        std::fs::write(&exe, b"the build this process started from").expect("write");
        let through = Through::of(exe.clone());
        assert_eq!(
            through.unchanged(),
            Some(exe.as_path()),
            "the recorded file"
        );
        let next = dir.path().join("aterm.next");
        std::fs::write(&next, b"another build").expect("write");
        std::fs::rename(&next, &exe).expect("rename over");
        assert_eq!(through.unchanged(), None, "another file at its path");
        std::fs::remove_file(&exe).expect("remove");
        assert_eq!(through.unchanged(), None, "no file at its path");
        assert_eq!(
            Through::of(exe.clone()).unchanged(),
            None,
            "nothing recorded"
        );
    }
}

#[cfg(test)]
mod watcher_tests {
    use super::{Proof, Watched, end_watched};
    use crate::process::{Birth, EndControl, EndRefusal, Ended, Identity, Lineage};
    use std::collections::VecDeque;
    use std::time::Duration;

    const SIGTERM: i32 = 15;
    const SIGKILL: i32 = 9;
    const PID: i32 = 4242;
    const BIRTH: Birth = Birth(1_792_000_000_000_042);
    const UNIQUE: u64 = 51_832_760;

    /// The child as the tether read itself before it forked its watcher: on Apple its pid
    /// version is the tether's own, which the exec of the child moves.
    fn armed() -> Watched {
        Watched::of(&Lineage {
            identity: Identity {
                pid: PID,
                birth: BIRTH,
                idversion: Some(7),
            },
            uniqueid: UNIQUE,
            parent: 1,
            pgid: 4200,
            launcher_version: Some(3),
            tty: None,
        })
    }

    /// A read of the pid: its birth, and its pid version where the kernel keeps one.
    const fn read(birth: Birth, idversion: Option<i32>) -> Option<Identity> {
        Some(Identity {
            pid: PID,
            birth,
            idversion,
        })
    }

    /// An `EndControl` whose identity reads follow a script (`None` past it) and that logs each
    /// signal; its naps take no time.
    struct Script {
        reads: VecDeque<Option<Identity>>,
        signals: Vec<(i32, i32)>,
    }

    impl EndControl for Script {
        fn identity(&mut self, _pid: i32) -> Option<Identity> {
            self.reads.pop_front().flatten()
        }
        fn signal(&mut self, token: &Identity, sig: i32) -> std::io::Result<()> {
            self.signals.push((token.pid, sig));
            Ok(())
        }
        fn nap(&mut self, _pause: Duration) {}
    }

    /// One case: its name, the identity reads, the proof reads (`None` past the script), the
    /// answer and the signals sent.
    type Case = (
        &'static str,
        Vec<Option<Identity>>,
        Vec<Option<Proof>>,
        Result<Ended, EndRefusal>,
        Vec<i32>,
    );

    const fn ended(killed: bool, gone: bool) -> Result<Ended, EndRefusal> {
        Ok(Ended {
            termed: true,
            killed,
            gone,
        })
    }

    #[test]
    fn the_watcher_ends_by_proof() {
        let watched = armed();
        assert_eq!(
            watched.token(),
            Identity {
                pid: PID,
                birth: BIRTH,
                idversion: None
            },
            "the token is the pid and birth with no pid version"
        );
        let same = read(BIRTH, None);
        let exec_d = read(BIRTH, Some(8));
        let reborn = read(Birth(BIRTH.0 + 1), None);
        let proven = Some(Proof {
            uniqueid: UNIQUE,
            ran: true,
        });
        let moved = Some(Proof {
            uniqueid: UNIQUE + 1,
            ran: true,
        });
        let held = Some(Proof {
            uniqueid: UNIQUE,
            ran: false,
        });
        let cases: [Case; 8] = [
            (
                "gone after the SIGTERM",
                vec![same, same, None],
                vec![proven],
                ended(false, true),
                vec![SIGTERM],
            ),
            (
                "still there past the grace",
                vec![same, same, same, same, same, None],
                vec![proven, proven],
                ended(true, true),
                vec![SIGTERM, SIGKILL],
            ),
            (
                "exec'd in place since the arming, its pid version moved",
                vec![exec_d, exec_d, None],
                vec![proven],
                ended(false, true),
                vec![SIGTERM],
            ),
            (
                "its unique id moved before the SIGTERM (the control)",
                vec![same, same],
                vec![moved],
                Err(EndRefusal::NotProven),
                vec![],
            ),
            (
                "its pid reused, another birth",
                vec![reborn, reborn],
                vec![proven],
                Err(EndRefusal::Stale),
                vec![],
            ),
            (
                "held at macOS's program check, its thread not yet run",
                vec![same, same],
                vec![held],
                Err(EndRefusal::NotProven),
                vec![],
            ),
            (
                "its proof does not read",
                vec![same, same],
                vec![None],
                Err(EndRefusal::NotProven),
                vec![],
            ),
            (
                "its unique id moved before the SIGKILL",
                vec![same, same, same, same],
                vec![proven, moved],
                ended(false, false),
                vec![SIGTERM],
            ),
        ];
        for (name, reads, proofs, want, signals) in cases {
            let mut control = Script {
                reads: reads.into(),
                signals: Vec::new(),
            };
            let mut proofs = proofs.into_iter();
            let out = end_watched(watched, Duration::ZERO, &mut control, |pid| {
                assert_eq!(pid, PID, "{name}: the proof read is the child's");
                proofs.next().flatten()
            });
            let sent: Vec<i32> = control.signals.iter().map(|&(_, sig)| sig).collect();
            if signals.is_empty() {
                assert!(sent.is_empty(), "{name}: no signal, sent {sent:?}");
            }
            assert_eq!(out, want, "{name}: sent {sent:?}");
            assert_eq!(sent, signals, "{name}");
            assert!(
                control.signals.iter().all(|&(pid, _)| pid == PID),
                "{name}: every signal went to the child's pid alone: {:?}",
                control.signals
            );
        }
    }
}
