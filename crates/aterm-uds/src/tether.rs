// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Parent-death ties for helpers started through [`spawnfd::Tether`](crate::spawnfd::Tether),
//! including when the parent is killed outright or crashes. The platform's conditions below
//! determine whether the helper ends.
//!
//! Linux has the parent-death signal: the child is spawned with `PR_SET_PDEATHSIG` set to
//! `SIGTERM`, and a child that ignores it runs on. The kernel sends it when the thread that forked
//! the child exits, so every tethered spawn forks on a spawner, a thread that lives as long as the
//! process (`spawned`): the child ends with the process, not with the thread that asked for it,
//! which may end while the child runs on (its wait handed to the reaper). An exec in the process's
//! place ends every other thread, the spawners among them, so it ends tethered children too. A
//! process serving [`VERB`] on Linux sets the signal on itself, so it is tied to whichever thread
//! forked it. Darwin has none, so a tethered spawn there runs this process's own executable as
//! [`VERB`] first, under [`NAME`], and that tether leaves a watcher behind as it execs the child in
//! its own place: the parent's handle names the child itself, its pid, its signals and its exit
//! status. A Linux spawn whose child leads its own process group (below) runs so too, its signal
//! set as well, through the running image itself (`/proc/self/exe`), which no file put at the
//! executable's path since replaces (`route`). The watcher is a grandchild of the tether,
//! adopted by launchd (on Linux by init, or the nearest subreaper), watching the parent's end and
//! the child's exit: on Darwin by a kqueue watch on the parent's exit and exec (`NOTE_EXIT`,
//! `NOTE_EXEC`) and on the child's exit; on Linux by pidfds (Linux 5.3), one of the thread that
//! forked the tether, which ends at the parent's exit or exec as the signal's thread does
//! (`THREAD_VAR`; `PIDFD_THREAD`, Linux 6.9; an older kernel watches the parent's own, readable
//! at its exit alone), and one of the child. A Linux kernel with no pidfd, or a seccomp filter
//! that refuses one, arms the watcher with nothing to watch, and the child is tied by its signal
//! alone. A Linux `/proc` mounted with `hidepid` lists no group whole, so a watcher there could
//! never tell its group had emptied: a spawn there routes nothing (`route`), nor does one of a
//! process whose pid is 1, whose orphans it adopts itself. The watcher leads a session of its
//! own, made before it arms, so a listing of the child's group (a stage sweep's), a kill of that
//! group and a hangup of the child's terminal never count or reach it (a group leader's group
//! holds the watcher's anchor, a zombie, as below, which Darwin's listing shows and Linux's passes
//! by); it still ends with the child by its exit watch. When the parent exits, or execs in its own
//! place (an update's cold lane) as Linux's spawners end at, the watcher ends the child through
//! [`end_proven`] ([`end_watched`]): `SIGTERM`, then `SIGKILL` to one still there
//! [`END_GRACE`](crate::process::END_GRACE) later, each signal only while the pid still reads the
//! birth and kernel unique id the tether read of itself before it forked the watcher
//! ([`Watched`]), and its one thread has run ([`Proof`]). A child macOS's program check holds
//! before its first instruction, the tether's own exec of it included, gets no signal while it is
//! held: the watcher reads it again every [`HELD_RECHECK`] until its thread has run, then ends
//! it, or until it exits (`end_once_run`). The watcher exits after that end, which signals
//! nothing when the proof does not hold (a moved unique id, or one that does not read), so such a
//! child can run on. A child that leads its own process group when the tether forks the watcher
//! (a spawn's `process_group(0)` or session) is a pipeline's root, whose descendants in that group
//! would run on, adopted, once it alone ended. Its watcher holds the group by an anchor, a child
//! of its own forked before it leaves the group, which exits at once and is never reaped, so the
//! group id names the child's group and no other while the watcher lives (`group`). When the
//! parent exits or execs, the watcher ends the whole group, `SIGTERM` then `SIGKILL` rounds
//! (`group::end_group`), then the child alone, as above, should it have left the group. When the
//! child exits first, the watcher of a group leader stays while the group holds a live member,
//! woken by each member's exit, and ends the group should the parent go first (`group::linger`);
//! the watcher of any other child exits with it. Linux leaves a watcher for a group leader alone:
//! any other tethered child there is tied by its signal alone. The tether execs the child only
//! once the watcher is armed and the parent is still its own parent. An exec of the parent before
//! the watcher armed ends nothing on Darwin; on Linux a forking thread gone by then leaves the
//! watcher unarmed and the tether starts nothing, and a child with no watcher is tied from the
//! moment it set its signal. A kill by the parent's name passes the watcher by.

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

/// The `argv[0]` a tether runs under, and so its watcher, a fork that never execs: not the
/// parent's. macOS's `killall` and `pkill` name a process by its `argv[0]`'s last part, not by the
/// kernel's name, which stays the executable's (measured on macOS 27.0), so a kill by the window's
/// name (`killall -9 aterm`, `pkill -9 aterm`) ends the window and leaves the watcher to end the
/// child. Linux's `pkill` names a process by the kernel's name, so a Linux watcher takes this one
/// as that name too (`KERNEL_NAME`). `aterm` is no part of it, as `pkill` matches a part.
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

/// How long a watcher waits on its child's exit before it reads a held child again.
pub const HELD_RECHECK: Duration = Duration::from_millis(250);

/// The watcher's end of its child once the parent is gone, by its exit or by an exec in its
/// place. While the child is held before its first instruction (its pid still reads its birth,
/// and `proof` reads the unique id it was armed with and a thread that has not run) it gets no
/// signal, and `exited` waits on it up to [`HELD_RECHECK`], answering `true` once it has exited;
/// once anything else reads, it is ended as [`end_watched`] ends it. `None` when it exited while
/// held, with no signal sent.
#[cfg(any(target_vendor = "apple", target_os = "linux", test))]
fn end_once_run(
    watched: Watched,
    grace: Duration,
    control: &mut impl EndControl,
    mut proof: impl FnMut(i32) -> Option<Proof>,
    exited: impl FnMut() -> bool,
) -> Option<Result<Ended, EndRefusal>> {
    if held_until_exit(watched, control, &mut proof, exited) {
        return None;
    }
    Some(end_watched(watched, grace, control, proof))
}

/// [`end_once_run`]'s wait: while the child is held before its first instruction, wait on its
/// exit; `true` once it exited while held, `false` once anything else reads.
#[cfg(any(target_vendor = "apple", target_os = "linux", test))]
fn held_until_exit(
    watched: Watched,
    control: &mut impl EndControl,
    mut proof: impl FnMut(i32) -> Option<Proof>,
    mut exited: impl FnMut() -> bool,
) -> bool {
    let held = Proof {
        uniqueid: watched.uniqueid,
        ran: false,
    };
    while Versionless(&mut *control).identity(watched.pid) == Some(watched.token())
        && proof(watched.pid) == Some(held)
    {
        if exited() {
            return true;
        }
    }
    false
}

/// A query's positive original-child exit is distinct from a failed watch read.
#[cfg(any(target_vendor = "apple", target_os = "linux", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChildExitNotice {
    Pending,
    Exited,
    Unavailable,
}

/// Positive point evidence, not permission retained across later signals.
#[cfg(any(target_vendor = "apple", target_os = "linux", test))]
fn group_readiness(
    watched: Watched,
    control: &mut impl EndControl,
    proof: &mut impl FnMut(i32) -> Option<Proof>,
    exited: &mut bool,
    exit_notice: &mut impl FnMut(Duration) -> ChildExitNotice,
) -> group::Readiness {
    if *exited {
        return group::Readiness::Exited;
    }
    let token = watched.token();
    let before = Versionless(&mut *control).identity(watched.pid);
    let observed = proof(watched.pid);
    let after = Versionless(&mut *control).identity(watched.pid);
    let current = if before == Some(token)
        && after == Some(token)
        && observed.is_some_and(|read| read.uniqueid == watched.uniqueid)
    {
        match observed {
            Some(Proof { ran: true, .. }) => group::Readiness::Started,
            _ => group::Readiness::Held,
        }
    } else {
        group::Readiness::Unavailable
    };
    if current == group::Readiness::Started {
        return current;
    }
    match exit_notice(Duration::ZERO) {
        ChildExitNotice::Exited => {
            *exited = true;
            group::Readiness::Exited
        }
        ChildExitNotice::Pending => current,
        ChildExitNotice::Unavailable => group::Readiness::Unavailable,
    }
}

/// The production parent-exit path: retain the initial held wait, then renew
/// positive point readiness immediately before each owned-group signal.
#[cfg(any(target_vendor = "apple", target_os = "linux", test))]
fn end_group_once_ready(
    watched: Watched,
    mut exited: bool,
    control: &mut impl EndControl,
    mut proof: impl FnMut(i32) -> Option<Proof>,
    mut exit_notice: impl FnMut(Duration) -> ChildExitNotice,
    group: &mut impl group::GroupControl,
) -> Option<group::GroupEnded> {
    loop {
        match group_readiness(watched, control, &mut proof, &mut exited, &mut exit_notice) {
            group::Readiness::Started | group::Readiness::Exited => break,
            group::Readiness::Unavailable => return None,
            group::Readiness::Held => match exit_notice(HELD_RECHECK) {
                ChildExitNotice::Pending => {}
                ChildExitNotice::Exited => exited = true,
                ChildExitNotice::Unavailable => return None,
            },
        }
    }
    group::end_group_guarded(watched.pid, crate::process::END_GRACE, group, || {
        group_readiness(watched, control, &mut proof, &mut exited, &mut exit_notice)
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
    /// Whether it is this process's own image, as [`entry`] records it; a test's [`through`]
    /// names another.
    #[cfg(target_os = "linux")]
    own: bool,
}

impl Through {
    /// `exe`, another process's image, with the file at its path now.
    #[cfg(any(unix, test, debug_assertions))]
    fn of(exe: PathBuf) -> Self {
        let file = file_at(&exe);
        Self {
            exe,
            file,
            #[cfg(target_os = "linux")]
            own: false,
        }
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
/// process's tethered spawns run through (on Linux, a group leader's: `route`), and answers
/// `None`.
#[must_use = "return a handled tether command's exit status to the caller"]
pub fn entry() -> Option<ExitCode> {
    #[cfg(unix)]
    {
        let mut args = std::env::args_os().skip(1);
        if args.next().is_some_and(|first| first == VERB) {
            return Some(serve(&args.collect::<Vec<_>>()));
        }
        let _ = THROUGH.get_or_init(|| {
            std::env::current_exe().ok().map(|exe| Through {
                #[cfg(target_os = "linux")]
                own: true,
                ..Through::of(exe)
            })
        });
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

/// Whether this process's tethered spawns have lost their tie: its `main` served the tether
/// ([`entry`]), or a test named an executable ([`through`]), but no unchanged executable is
/// recorded now (an update or a rebuild put another file at its path, or none read when it was
/// recorded), so on Darwin a tethered spawn starts its child untethered. `false` where nothing
/// was recorded (a test binary's own spawns), and off Darwin, whose tie needs no executable: the
/// parent-death signal, and a Linux group leader's tether run through the running image itself
/// (`route`).
#[must_use]
pub fn lost() -> bool {
    cfg!(target_vendor = "apple") && lost_of(THROUGH.get())
}

/// [`lost`] for what was `recorded`, on Darwin.
fn lost_of(recorded: Option<&Option<Through>>) -> bool {
    recorded.is_some_and(|recorded| recorded.as_ref().and_then(Through::unchanged).is_none())
}

/// TEST SEAM: this process's tethered spawns run through `exe`, an executable whose `main` calls
/// [`entry`] first, for a test binary whose own does not. The first record stands.
#[cfg(any(test, debug_assertions))]
pub fn through(exe: PathBuf) {
    let _ = THROUGH.set(Some(Through::of(exe)));
}

/// The tether's image and argv for `program` started from this process, after its `argv[0]`
/// ([`NAME`]): [`VERB`], this pid, the program, its `argv[0]`, its arguments. The image is
/// [`executable`]; on Linux, once this process's `main` served [`entry`], its running image itself
/// (`/proc/self/exe`), which an update's or a rebuild's file at the executable's path does not
/// replace, so such a tie is never lost there. `None` when this process records no executable to
/// run through, on Linux where `/proc` hides processes (`hidepid`), and in a process whose pid is
/// 1 (a container's first process, or a PID namespace's), a parent no tether takes
/// ([`parent_pid`]), since its orphans are adopted by it: the child is then tied by Linux's signal
/// alone, and starts untethered on Darwin. The spawn judges the program's search first
/// (`spawnfd::program_search::findable`), since the tether runs it only after the spawn returned.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
pub(crate) fn route(
    program: &std::ffi::OsStr,
    arg0: Option<&std::ffi::OsStr>,
    args: &[OsString],
) -> Option<(PathBuf, Vec<OsString>)> {
    if std::process::id() == 1 {
        return None;
    }
    #[cfg(target_os = "linux")]
    let exe = if THROUGH.get()?.as_ref()?.own {
        PathBuf::from("/proc/self/exe")
    } else {
        executable()?.to_path_buf()
    };
    // A `/proc` that hides processes lists no group whole, so the watcher could never tell the
    // group had emptied (`group::linger`).
    #[cfg(target_os = "linux")]
    if !crate::process::group_listing_can_be_whole() {
        return None;
    }
    #[cfg(target_vendor = "apple")]
    let exe = executable()?.to_path_buf();
    let mut routed = Vec::with_capacity(args.len() + 4);
    routed.push(OsString::from(VERB));
    routed.push(OsString::from(std::process::id().to_string()));
    routed.push(program.to_owned());
    routed.push(arg0.unwrap_or(program).to_owned());
    routed.extend(args.iter().cloned());
    Some((exe, routed))
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
    // The spawn judged the search by the `PATH` it gave this tether
    // (`spawnfd::program_search::findable`), so the exec searches it as a `PATH` a spawn sets: on
    // Darwin not as this process's own, whose search ends at an entry too long to join to the name.
    // This tether holds the descriptors its spawn handed it and no others (every one it inherits
    // for a transparent spawn, stdio alone otherwise), so the exec hands them on as they are.
    let mut command = crate::spawnfd::transparent(program).into_command();
    // The forking thread's id is the watcher's to read ([`THREAD_VAR`]), never the program's.
    #[cfg(target_os = "linux")]
    command.env_remove(THREAD_VAR);
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    command.arg0(argv0).args(args);
    // An image still open for writing (`ETXTBSY`) is tried again as a spawn tries it.
    let error = match crate::spawnfd::spawn_past_text_busy(|| {
        Err::<std::convert::Infallible, _>(command.exec())
    }) {
        Ok(never) => match never {},
        Err(error) => error,
    };
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
/// keeps, and for a process at the head of its own group (a spawn's `process_group(0)`, or a
/// session) a watcher left behind ([`leave_watcher`]), which ends that group when the parent exits
/// or execs (an exec from Linux 6.9, [`parent_end`]).
#[cfg(target_os = "linux")]
fn hold(parent: i32) -> Result<(), String> {
    bind(parent).map_err(|e| match e.raw_os_error() {
        Some(libc::ESRCH) => not_parent(parent),
        _ => format!("PR_SET_PDEATHSIG: {e}"),
    })?;
    // SAFETY: `getpgrp` and `getpid` take nothing and cannot fail.
    if unsafe { libc::getpgrp() != libc::getpid() } {
        return Ok(());
    }
    leave_watcher(parent)
}

/// The variable a Linux tethered spawn that runs through its tether ([`route`]) names the thread
/// that forked it by, in decimal: that thread ends at this process's exit or exec, as the one the
/// parent-death signal follows does ([`bind`]), so the tether's watcher watches it
/// ([`parent_end`]). The tether takes it out of the program's environment.
#[cfg(target_os = "linux")]
pub(crate) const THREAD_VAR: &str = "ATERM_TETHER_THREAD";

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
/// the kernel signals when the thread that forked this process exits, which for a tethered spawn
/// is a spawner ([`spawned`]), alive as long as the process, and for a process serving [`VERB`]
/// whichever thread forked it; a child that ignores `SIGTERM` runs on. Async-signal-safe (two
/// system calls), for a spawn's hook between fork and exec.
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

/// The name of each thread a tethered spawn forks on, a spawner ([`spawned`]).
#[cfg(any(target_os = "linux", test))]
pub(crate) const SPAWNER: &str = "aterm-tether-spawner";

/// One fork for a spawner. It is handed the spawner's way to list itself free again, which it
/// calls once its fork is done and before it answers, so a caller that has its answer finds
/// that spawner free.
#[cfg(any(target_os = "linux", test))]
type Job = Box<dyn FnOnce(&dyn Fn()) + Send>;

/// How a spawner's thread starts: `std::thread::Builder::spawn` of the body, its handle dropped,
/// since a spawner is never joined.
#[cfg(any(target_os = "linux", test))]
type Start = fn(std::thread::Builder, Box<dyn FnOnce() + Send>) -> std::io::Result<()>;

/// The threads tethered spawns fork on ([`SPAWNER`]): each free one by the sender of its own
/// queue. A spawner keeps a sender of its own queue too, so its thread waits for its next job
/// for as long as the process lives.
#[cfg(any(target_os = "linux", test))]
struct Spawners {
    /// The spawners no job holds; the one freed last is taken first.
    free: std::sync::Mutex<Vec<std::sync::mpsc::Sender<Job>>>,
    /// How a new spawner's thread starts ([`start_thread`], but in a test).
    start: Start,
}

/// This process's spawners.
#[cfg(any(target_os = "linux", test))]
static SPAWNERS: Spawners = Spawners::new(start_thread);

/// `std::thread::Builder::spawn` of `body`, its handle dropped.
#[cfg(any(target_os = "linux", test))]
fn start_thread(
    builder: std::thread::Builder,
    body: Box<dyn FnOnce() + Send>,
) -> std::io::Result<()> {
    builder.spawn(body).map(drop)
}

/// Runs `spawn` on a spawner ([`SPAWNER`]), a thread that lives as long as the process, and
/// answers what `spawn` answered. Linux sends a tethered child's parent-death signal when the
/// thread that forked it exits ([`bind`]), so a child forked here is tied to this process, not to
/// the calling thread. The call waits for no other call's fork: it takes a free spawner, or
/// starts one when none is free. `spawn` runs under `catch_unwind`: one that panics answers an
/// error, and its spawner runs on.
///
/// # Errors
/// `spawn`'s, or an error when it panicked. When no spawner is free and a new one's thread cannot
/// start, that error, with nothing run and nothing started untethered; the next call tries again.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn spawned<T: Send + 'static>(
    spawn: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    SPAWNERS.run(spawn)
}

#[cfg(any(target_os = "linux", test))]
impl Spawners {
    /// No spawner yet; a new one's thread starts by `start`.
    const fn new(start: Start) -> Self {
        Self {
            free: std::sync::Mutex::new(Vec::new()),
            start,
        }
    }

    /// [`spawned`] over these spawners.
    fn run<T: Send + 'static>(
        &'static self,
        spawn: impl FnOnce() -> std::io::Result<T> + Send + 'static,
    ) -> std::io::Result<T> {
        let (answer, answered) = std::sync::mpsc::sync_channel(1);
        let job: Job = Box::new(move |freed: &dyn Fn()| {
            let said = std::panic::catch_unwind(std::panic::AssertUnwindSafe(spawn))
                .unwrap_or_else(|_| Err(std::io::Error::other("a tethered spawn panicked")));
            freed();
            let _ = answer.send(said);
        });
        let queue = match self.take_free() {
            Some(queue) => queue,
            None => self.started()?,
        };
        if queue.send(job).is_err() {
            return Err(std::io::Error::other("a tether spawner has ended"));
        }
        answered
            .recv()
            .unwrap_or_else(|_| Err(std::io::Error::other("a tether spawner dropped a spawn")))
    }

    /// A new spawner's queue, once its thread started: the thread runs each job sent to it, then
    /// lists itself free.
    fn started(&'static self) -> std::io::Result<std::sync::mpsc::Sender<Job>> {
        let (queue, waiting) = std::sync::mpsc::channel::<Job>();
        let own = queue.clone();
        (self.start)(
            std::thread::Builder::new().name(SPAWNER.to_owned()),
            Box::new(move || {
                for job in waiting {
                    job(&|| self.list_free(own.clone()));
                }
            }),
        )?;
        Ok(queue)
    }

    /// The spawner freed last, taken off the free list; `None` when none is free.
    fn take_free(&self) -> Option<std::sync::mpsc::Sender<Job>> {
        self.free
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop()
    }

    /// List a spawner, by its queue, as free.
    fn list_free(&self, queue: std::sync::mpsc::Sender<Job>) {
        self.free
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(queue);
    }
}

/// Tie this process to `parent` before its exec: the check that `parent` is still this process's
/// parent, then a watcher left behind ([`leave_watcher`]).
#[cfg(target_vendor = "apple")]
fn hold(parent: i32) -> Result<(), String> {
    if !parent_is(parent) {
        return Err(not_parent(parent));
    }
    leave_watcher(parent)
}

/// Leave the watcher behind: read this process, the child it will exec into, as the watcher will
/// know it ([`Watched`]; one that does not read arms nothing); fork, and fork again so launchd (on
/// Linux, init or the nearest subreaper) adopts the watcher (the child this process becomes has
/// no child of its own to wait for); wait for it to arm, then check `parent` is still this
/// process's parent, so the watch it armed is on the right process.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn leave_watcher(parent: i32) -> Result<(), String> {
    use std::io::Read as _;
    use std::os::fd::AsRawFd as _;
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
    // SAFETY: `getpgrp` takes nothing and cannot fail. The spawn put this process at the head of
    // its own group (`process_group(0)`, or a session) before it ran, and the exec keeps it.
    let leads = unsafe { libc::getpgrp() } == child;
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
                    leads,
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

/// One `EVFILT_PROC` change for `pid`'s `notes`, with a receipt.
#[cfg(target_vendor = "apple")]
const fn watch_of(pid: i32, notes: u32) -> libc::kevent {
    libc::kevent {
        ident: pid.unsigned_abs() as usize,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_RECEIPT,
        fflags: notes,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

/// Whether `events` hold one of `notes` for `pid`.
#[cfg(target_vendor = "apple")]
fn noted(events: &[libc::kevent], pid: i32, notes: u32) -> bool {
    events
        .iter()
        .any(|e| e.ident == pid.unsigned_abs() as usize && e.fflags & notes != 0)
}

/// Waits on `kq` up to [`HELD_RECHECK`] for `child`'s exit: `true` once it has exited, or when
/// the wait itself fails (the watcher then gives up its watch, as it gives up an unread proof).
///
/// # Safety
/// As [`watch`]'s, with `kq` the watcher's queue.
#[cfg(target_vendor = "apple")]
unsafe fn child_exited_within(kq: i32, child: i32) -> bool {
    !matches!(
        // SAFETY: this is the same owned watch and live kqueue as this function's contract.
        unsafe { child_exit_notice(kq, child, HELD_RECHECK) },
        ChildExitNotice::Pending
    )
}

/// Typed read of the armed original child's exit. Errors refuse execution
/// evidence; they must never grant a group's positive Exited state.
///
/// # Safety
/// As [`watch`]'s, with `kq` the watcher's queue.
#[cfg(target_vendor = "apple")]
unsafe fn child_exit_notice(kq: i32, child: i32, patience: Duration) -> ChildExitNotice {
    const BLANK: libc::kevent = watch_of(0, 0);
    let pause = libc::timespec {
        tv_sec: libc::time_t::try_from(patience.as_secs()).unwrap_or(libc::time_t::MAX),
        tv_nsec: libc::c_long::from(patience.subsec_nanos()),
    };
    let mut events = [BLANK; 2];
    // SAFETY: `events` is a live array of two kevents the call fills; nothing is changed.
    let got = unsafe { libc::kevent(kq, std::ptr::null(), 0, events.as_mut_ptr(), 2, &pause) };
    match usize::try_from(got) {
        Ok(got) if noted(&events[..got.min(2)], child, libc::NOTE_EXIT) => ChildExitNotice::Exited,
        Ok(_) => ChildExitNotice::Pending,
        Err(_) if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => {
            ChildExitNotice::Pending
        }
        Err(_) => ChildExitNotice::Unavailable,
    }
}

/// The watcher: a session of its own (`setsid`; one it cannot lead exits, unarmed, and the tether
/// starts nothing), stdio on `/dev/null`, a kqueue watch on `parent`'s exit and exec and on the
/// `watched` child's exit, the arming byte, then a wait. The child's exit ends it; the parent's
/// exit or exec ends the child first, as [`end_once_run`] ends one, with the live reads and
/// `kill(2)` ([`EndControlLive`]), the child's [`Proof`] read by [`read_lineage`] and
/// [`thread_ran`]. A child whose exit came first reads as gone there (a zombie reads nothing, a
/// reused pid another birth) and gets no signal; one whose thread has not run gets none until it
/// has.
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
unsafe fn watch(
    parent: i32,
    watched: Watched,
    leads: bool,
    null: i32,
    arming: i32,
    armed: i32,
) -> ! {
    const BLANK: libc::kevent = watch_of(0, 0);
    let child = watched.pid;
    // SAFETY (whole body): the function's contract.
    unsafe {
        // A group leader's group is held first, while the watcher is still in it: the anchor
        // exits at once and stays a zombie until the watcher exits, since `SIGCHLD` is reset to
        // its default (no automatic reap) and nothing here waits on it. `waitid`'s `WNOWAIT` sees
        // it exited and leaves it unreaped. One that does not arm so exits the watcher unarmed.
        let anchor = if leads {
            match anchored() {
                Some(anchor) => Some(anchor),
                None => libc::_exit(1),
            }
        } else {
            None
        };
        // Then, before it arms: out of the child's group and session. The watcher is the
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
        let changes = [
            watch_of(parent, libc::NOTE_EXIT | libc::NOTE_EXEC),
            watch_of(child, libc::NOTE_EXIT),
        ];
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
            // The parent first: a child that died of its parent's end (a pipe the parent held)
            // can be noted beside it, and its group still has to end.
            if noted(events, parent, libc::NOTE_EXIT | libc::NOTE_EXEC) {
                let proof = |pid| {
                    Some(Proof {
                        uniqueid: crate::process::read_lineage(pid)?.uniqueid,
                        ran: crate::process::thread_ran(pid)?,
                    })
                };
                let mut control = crate::process::EndControlLive;
                if let Some(anchor) = &anchor {
                    // A child held before its first instruction gets no signal, by the group or
                    // alone; one that exited so started nothing the group could hold.
                    let _ended = end_group_once_ready(
                        watched,
                        noted(events, child, libc::NOTE_EXIT),
                        &mut control,
                        proof,
                        |patience| child_exit_notice(kq, child, patience),
                        &mut group::GroupLive { kq, parent, anchor },
                    );
                }
                // The child alone, by its proof: any child of a watcher that did not lead its
                // group, and a leader that left its group. One the group end ended reads as gone
                // and gets nothing.
                let _ended = end_once_run(
                    watched,
                    crate::process::END_GRACE,
                    &mut control,
                    proof,
                    || child_exited_within(kq, child),
                );
                libc::_exit(0);
            }
            if noted(events, child, libc::NOTE_EXIT) {
                if let Some(anchor) = &anchor
                    && group::linger(child, &mut group::GroupLive { kq, parent, anchor })
                {
                    let _ended = group::end_group_guarded(
                        child,
                        crate::process::END_GRACE,
                        &mut group::GroupLive { kq, parent, anchor },
                        || group::Readiness::Exited,
                    );
                }
                libc::_exit(0);
            }
        }
    }
}

/// [`NAME`] as the kernel keeps a process's name (`PR_SET_NAME`): at most 15 bytes, then NUL.
#[cfg(target_os = "linux")]
const KERNEL_NAME: [u8; 16] = {
    let mut name = [0_u8; 16];
    let mut at = 0;
    while at < NAME.len() && at < 15 {
        name[at] = NAME.as_bytes()[at];
        at += 1;
    }
    name
};

/// `pidfd_open(id, flags)`: a descriptor `poll(2)` reports readable once the process `id` names
/// has exited (with `PIDFD_THREAD`, once that thread has), close-on-exec as every pidfd is.
///
/// # Errors
/// The kernel's: `ESRCH` for no such process or thread, `EINVAL` for flags it does not know
/// (`PIDFD_THREAD` before Linux 6.9), `ENOSYS` before Linux 5.3.
#[cfg(target_os = "linux")]
fn pidfd_open(id: i32, flags: u32) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd as _;
    // SAFETY: `pidfd_open` takes a pid and flags and touches no memory of ours.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, id, flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let fd = i32::try_from(fd).map_err(std::io::Error::other)?;
    // SAFETY: a descriptor the call just made, which nothing else owns.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

/// A descriptor readable once `parent` exits or execs in its own place: a pidfd of the thread
/// [`THREAD_VAR`] names, which ends at either, as the parent-death signal's thread does, held
/// once that thread reads as one of `parent`'s (`/proc/<parent>/task/<thread>`) with its pidfd
/// open, so the descriptor names that thread. Where no thread is named, or the kernel opens no
/// thread's pidfd (`EINVAL`, before Linux 6.9), the parent's own pidfd, readable at its exit
/// alone.
///
/// # Errors
/// [`pidfd_open`]'s, and `ESRCH` for a named thread that is not one of `parent`'s.
#[cfg(target_os = "linux")]
fn parent_end(parent: i32) -> std::io::Result<std::os::fd::OwnedFd> {
    const PIDFD_THREAD: u32 = libc::O_EXCL.unsigned_abs();
    if let Some(thread) = std::env::var_os(THREAD_VAR).as_deref().and_then(parent_pid) {
        match pidfd_open(thread, PIDFD_THREAD) {
            Ok(fd) if Path::new(&format!("/proc/{parent}/task/{thread}")).exists() => {
                return Ok(fd);
            }
            Ok(_) => return Err(std::io::Error::from_raw_os_error(libc::ESRCH)),
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {}
            Err(e) => return Err(e),
        }
    }
    pidfd_open(parent, 0)
}

/// Waits on `exit`, the child's pidfd, up to [`HELD_RECHECK`] for its exit: `true` once it has
/// exited, or when the wait itself fails (the watcher then gives up its watch, as on Darwin).
#[cfg(target_os = "linux")]
fn child_exited_within(exit: &std::os::fd::OwnedFd) -> bool {
    !matches!(
        child_exit_notice(exit, HELD_RECHECK),
        ChildExitNotice::Pending
    )
}

/// Typed read of the armed original child's exit on its pidfd `exit`, waiting up to `patience`:
/// `Exited` once it reads readable, `Pending` at the timeout or an interrupted wait. A failed
/// wait is `Unavailable`, which refuses execution evidence and never grants a group's positive
/// Exited state.
#[cfg(target_os = "linux")]
fn child_exit_notice(exit: &std::os::fd::OwnedFd, patience: Duration) -> ChildExitNotice {
    use std::os::fd::AsRawFd as _;
    let mut fd = libc::pollfd {
        fd: exit.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = i32::try_from(patience.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: one live `pollfd`, which `poll` reads and writes and does not retain.
    match unsafe { libc::poll(&mut fd, 1, ms) } {
        0 => ChildExitNotice::Pending,
        1.. => ChildExitNotice::Exited,
        _ if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => {
            ChildExitNotice::Pending
        }
        _ => ChildExitNotice::Unavailable,
    }
}

/// The Linux watcher: Darwin's, pidfds in place of its kqueue. It holds a group leader's group by
/// its anchor ([`anchored`]), leads a session of its own, takes [`NAME`] as its kernel name
/// ([`KERNEL_NAME`]; Linux's `pkill` matches that name), puts stdio on `/dev/null`, opens a
/// descriptor of the parent's end ([`parent_end`]) and a pidfd of the `watched` child, writes the
/// arming byte, then waits. The parent's exit or exec ends a leader's group once the child reads
/// started or exited ([`end_group_once_ready`]), then the child alone by its proof
/// ([`end_once_run`]), with the live reads and `kill(2)`; the child's exit first leaves a
/// leader's group watched while it holds a live member, and ends it should the parent end first
/// (`group::linger`). A kernel with no pidfd (`ENOSYS`), or a seccomp filter that refuses one
/// (`EPERM`), arms it with nothing to watch: it writes the byte and exits, and the child is tied
/// by its parent-death signal alone. A forking thread gone by then, a parent already reaped, or a
/// child whose pidfd does not open otherwise, leaves it unarmed. A parent that exited unreaped,
/// watched by its own pidfd, arms it with its end already readable, so it ends the tether's group
/// at once. Either way the tether starts nothing, as its parent check fails.
///
/// # Safety
/// Only in a forked child of the tether, with `null`, `arming` and `armed` open, forked while the
/// tether had one thread, so no lock is held that the reads, the clock and the sleep would wait
/// on.
#[cfg(target_os = "linux")]
unsafe fn watch(
    parent: i32,
    watched: Watched,
    leads: bool,
    null: i32,
    arming: i32,
    armed: i32,
) -> ! {
    use std::os::fd::AsRawFd as _;
    const PR_SET_NAME: i32 = 15;
    unsafe extern "C" {
        fn prctl(option: i32, ...) -> i32;
    }
    let child = watched.pid;
    // SAFETY (whole body): the function's contract; every pointer is to a live local or static.
    unsafe {
        // As on Darwin: the group held first, while the watcher is still in it, then out of the
        // child's group and session before it arms.
        let anchor = if leads {
            match anchored() {
                Some(anchor) => Some(anchor),
                None => libc::_exit(1),
            }
        } else {
            None
        };
        if libc::setsid() < 0 {
            leave(anchor.as_ref(), 1);
        }
        prctl(PR_SET_NAME, KERNEL_NAME.as_ptr());
        libc::close(armed);
        for fd in 0..3 {
            libc::dup2(null, fd);
        }
        if null > 2 {
            libc::close(null);
        }
        let (gone, exit) = match (parent_end(parent), pidfd_open(child, 0)) {
            (Ok(gone), Ok(exit)) => (gone, exit),
            (Err(e), _) | (_, Err(e))
                if matches!(e.raw_os_error(), Some(libc::ENOSYS | libc::EPERM)) =>
            {
                let armed = libc::write(arming, [1_u8].as_ptr().cast(), 1) == 1;
                leave(anchor.as_ref(), i32::from(!armed));
            }
            _ => leave(anchor.as_ref(), 1),
        };
        if libc::write(arming, [1_u8].as_ptr().cast(), 1) != 1 {
            leave(anchor.as_ref(), 1);
        }
        libc::close(arming);
        let live = |anchor| group::GroupLive {
            parent: gone.as_raw_fd(),
            pgid: child,
            anchor,
        };
        loop {
            let mut fds = [gone.as_raw_fd(), exit.as_raw_fd()].map(|fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            });
            if libc::poll(fds.as_mut_ptr(), 2, -1) < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                leave(anchor.as_ref(), 1);
            }
            // The parent first, as on Darwin.
            if fds[0].revents != 0 {
                let proof = |pid| {
                    Some(Proof {
                        uniqueid: crate::process::read_lineage(pid)?.uniqueid,
                        ran: crate::process::thread_ran(pid)?,
                    })
                };
                let mut control = crate::process::EndControlLive;
                if let Some(anchor) = &anchor {
                    let _ended = end_group_once_ready(
                        watched,
                        fds[1].revents != 0,
                        &mut control,
                        proof,
                        |patience| child_exit_notice(&exit, patience),
                        &mut live(anchor),
                    );
                }
                let _ended = end_once_run(
                    watched,
                    crate::process::END_GRACE,
                    &mut control,
                    proof,
                    || child_exited_within(&exit),
                );
                leave(anchor.as_ref(), 0);
            }
            if fds[1].revents != 0 {
                if let Some(anchor) = &anchor
                    && group::linger(child, &mut live(anchor))
                {
                    let _ended = group::end_group_guarded(
                        child,
                        crate::process::END_GRACE,
                        &mut live(anchor),
                        || group::Readiness::Exited,
                    );
                }
                leave(anchor.as_ref(), 0);
            }
        }
    }
}

/// Ends the Linux watcher with `code`, its anchor reaped first: the watcher signals nothing
/// after, and an anchor left a zombie would pass to the namespace's init with the watcher, which
/// need not reap it (a container's `sleep` as its pid 1). The watcher itself is left to that
/// init, as any daemon is.
#[cfg(target_os = "linux")]
fn leave(anchor: Option<&Anchor>, code: i32) -> ! {
    if let Some(anchor) = anchor {
        let mut status = 0;
        // SAFETY: the watcher's own exited child, unreaped until this wait, which nothing else
        // waits on; a live local status word.
        unsafe { libc::waitpid(anchor.pid, &mut status, libc::WNOHANG) };
    }
    // SAFETY: `_exit` takes a status and returns to nothing.
    unsafe { libc::_exit(code) }
}

/// The watcher's anchor in the group its child leads: a fork that exits at once, seen exited by
/// `waitid`'s `WNOWAIT` and left unreaped, with `SIGCHLD` reset to its default so the kernel does
/// not reap it either. `None` when it could not be made so.
///
/// # Safety
/// As [`watch`]'s, before the watcher leaves the child's group.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
unsafe fn anchored() -> Option<Anchor> {
    // SAFETY (whole body): the function's contract; every pointer is to a live local.
    unsafe {
        let mut default: libc::sigaction = std::mem::zeroed();
        default.sa_sigaction = libc::SIG_DFL;
        if libc::sigaction(libc::SIGCHLD, &default, std::ptr::null_mut()) != 0 {
            return None;
        }
        let anchor = libc::fork();
        if anchor == 0 {
            libc::_exit(0);
        }
        if anchor < 0 {
            return None;
        }
        let Ok(id) = libc::id_t::try_from(anchor) else {
            return None;
        };
        loop {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            if libc::waitid(libc::P_PID, id, &mut info, libc::WEXITED | libc::WNOWAIT) == 0 {
                #[cfg(target_vendor = "apple")]
                let pid = info.si_pid;
                #[cfg(target_os = "linux")]
                let pid = info.si_pid();
                if terminal_anchor_wait(anchor, info.si_signo, pid, info.si_code) {
                    return Some(Anchor { pid: anchor });
                }
                // Darwin can report a stop despite WEXITED. This is still our
                // actual, unreaped fork, but it is not an exited anchor. Refuse
                // arming and clean up that owned child, never a listed pid.
                if info.si_signo == libc::SIGCHLD && pid == anchor {
                    retire_unarmed_anchor(anchor);
                }
                return None;
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return None;
            }
        }
    }
}

/// A matching pid alone is insufficient: WEXITED may report a stopped Darwin child.
#[cfg(any(target_vendor = "apple", target_os = "linux", all(test, unix)))]
fn terminal_anchor_wait(expected: i32, signo: i32, pid: i32, code: i32) -> bool {
    signo == libc::SIGCHLD
        && pid == expected
        && matches!(code, libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED)
}

/// End and reap only the actual fork whose nonterminal SIGCHLD was just observed,
/// while that unreaped child still pins its number. A kernel-blocked child is left
/// to the kernel after the existing group cleanup bound; the watcher stays unarmed.
///
/// # Safety
/// The caller is the sole waiter of this actual, unreaped fork, as in `anchored()`.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
unsafe fn retire_unarmed_anchor(anchor: i32) {
    // SAFETY (whole body): that actual child remains unreaped until this sole
    // wait returns its pid. No signal is sent after the wait, even on error.
    unsafe {
        libc::kill(anchor, libc::SIGKILL);
        let until =
            crate::deadline::saturating_after(std::time::Instant::now(), group::KILL_ROUNDS);
        loop {
            let mut status = 0;
            let got = libc::waitpid(anchor, &mut status, libc::WNOHANG);
            if got == anchor && (libc::WIFEXITED(status) || libc::WIFSIGNALED(status))
                || got < 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                return;
            }
            let left = until.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return;
            }
            std::thread::sleep(left.min(Duration::from_millis(20)));
        }
    }
}

/// The watcher's actual exited child, never reaped while this native watch runs.
/// Only `anchored()` constructs it after its own fork and successful WNOWAIT.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
pub(super) struct Anchor {
    pid: i32,
}

#[cfg(any(target_vendor = "apple", target_os = "linux", test))]
#[path = "tether_group.rs"]
mod group;

#[cfg(all(test, unix))]
mod tests {
    use super::{Through, lost_of, parent_pid};
    use std::ffi::OsStr;

    #[test]
    #[cfg(unix)]
    fn an_anchor_requires_a_terminal_sigchld_for_the_actual_child() {
        for code in [libc::CLD_EXITED, libc::CLD_KILLED, libc::CLD_DUMPED] {
            assert!(super::terminal_anchor_wait(42, libc::SIGCHLD, 42, code));
        }
        for code in [libc::CLD_STOPPED, libc::CLD_TRAPPED, libc::CLD_CONTINUED] {
            assert!(!super::terminal_anchor_wait(42, libc::SIGCHLD, 42, code));
        }
        assert!(!super::terminal_anchor_wait(
            42,
            libc::SIGCHLD,
            43,
            libc::CLD_EXITED
        ));
        assert!(!super::terminal_anchor_wait(42, 0, 42, libc::CLD_EXITED));
    }

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

    /// The tie is lost once the file recorded to run through was replaced, or none read when it
    /// was recorded, and never where nothing was recorded: a test binary's own spawns, which were
    /// never tied there.
    #[test]
    fn a_tie_is_lost_only_once_its_recorded_executable_was_replaced() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let exe = dir.path().join("aterm");
        std::fs::write(&exe, b"the build this process started from").expect("write");
        assert!(!lost_of(None), "nothing recorded");
        let recorded = Some(Through::of(exe.clone()));
        assert!(!lost_of(Some(&recorded)), "the recorded file");
        let next = dir.path().join("aterm.next");
        std::fs::write(&next, b"an update's build").expect("write");
        std::fs::rename(&next, &exe).expect("rename over");
        assert!(lost_of(Some(&recorded)), "another file at its path");
        assert!(lost_of(Some(&None)), "nothing read when it was recorded");
    }
}

#[cfg(test)]
mod watcher_tests {
    use super::{Proof, Watched, end_once_run, end_watched};
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

    /// Once its parent is gone, a child held before its first instruction gets no signal and is
    /// waited on until its thread has run, then ended by proof; one that exits while held gets
    /// none at all, and one whose pid reads another birth is not waited on.
    #[test]
    fn a_held_child_is_ended_once_its_thread_has_run() {
        let same = read(BIRTH, None);
        let reborn = read(Birth(BIRTH.0 + 1), None);
        let proven = Some(Proof {
            uniqueid: UNIQUE,
            ran: true,
        });
        let held = Some(Proof {
            uniqueid: UNIQUE,
            ran: false,
        });
        /// One run of `end_once_run` against a scripted child.
        struct Case {
            name: &'static str,
            /// What each identity read of the child returns, in turn.
            reads: Vec<Option<Identity>>,
            /// What each proof read of the child returns, in turn.
            proofs: Vec<Option<Proof>>,
            /// The wait at which the child is seen to have exited.
            exits_at_wait: usize,
            /// What `end_once_run` returns.
            want: Option<Result<Ended, EndRefusal>>,
            /// The signals the child is sent.
            signals: Vec<i32>,
        }
        let cases = [
            Case {
                name: "held for two reads, then run",
                reads: vec![same, same, same, same, same, None],
                proofs: vec![held, held, proven, proven],
                exits_at_wait: usize::MAX,
                want: Some(ended(false, true)),
                signals: vec![SIGTERM],
            },
            Case {
                name: "held, then exited",
                reads: vec![same],
                proofs: vec![held],
                exits_at_wait: 1,
                want: None,
                signals: vec![],
            },
            Case {
                name: "its pid reused, another birth",
                reads: vec![reborn, reborn],
                proofs: vec![],
                exits_at_wait: usize::MAX,
                want: Some(Err(EndRefusal::Stale)),
                signals: vec![],
            },
        ];
        for Case {
            name,
            reads,
            proofs,
            exits_at_wait,
            want,
            signals,
        } in cases
        {
            let mut control = Script {
                reads: reads.into(),
                signals: Vec::new(),
            };
            let mut proofs = proofs.into_iter();
            let mut waits = 0;
            let out = end_once_run(
                armed(),
                Duration::ZERO,
                &mut control,
                |pid| {
                    assert_eq!(pid, PID, "{name}: the proof read is the child's");
                    proofs.next().flatten()
                },
                || {
                    waits += 1;
                    waits >= exits_at_wait
                },
            );
            let sent: Vec<i32> = control.signals.iter().map(|&(_, sig)| sig).collect();
            assert_eq!(out, want, "{name}: sent {sent:?} after {waits} waits");
            assert_eq!(sent, signals, "{name}");
        }
    }
}

#[cfg(test)]
mod spawner_tests {
    use super::{SPAWNER, Spawners, spawned, start_thread};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::thread::{self, ThreadId};
    use std::time::Duration;

    /// The hang detector for a wait on what must happen.
    const HANG: Duration = Duration::from_secs(60);

    /// The thread a job ran on, and its name.
    fn here() -> std::io::Result<(ThreadId, Option<String>)> {
        let this = thread::current();
        Ok((this.id(), this.name().map(str::to_owned)))
    }

    /// A spawn runs on a spawner, never on its caller. Calls made one after another, each from a
    /// thread that ended before the next began, run on one spawner, which still runs a job after
    /// every caller ended and after a job that panicked.
    #[test]
    fn a_spawner_outlives_its_callers_and_serves_the_next_call() {
        static ONE_AFTER_ANOTHER: Spawners = Spawners::new(start_thread);
        let ran: Vec<_> = (0..4)
            .map(|_| {
                thread::spawn(|| {
                    let ran = ONE_AFTER_ANOTHER.run(here).expect("a spawner runs the job");
                    assert_ne!(
                        ran.0,
                        thread::current().id(),
                        "the job ran on a spawner, not on its caller"
                    );
                    ran
                })
                .join()
                .expect("a caller")
            })
            .collect();
        let panicked = ONE_AFTER_ANOTHER.run::<()>(|| panic!("a job that panics"));
        assert!(panicked.is_err(), "a job that panicked answers an error");
        let later = ONE_AFTER_ANOTHER
            .run(here)
            .expect("a spawner runs a job after its callers ended");
        assert_eq!(later.1.as_deref(), Some(SPAWNER), "a spawner's name");
        assert!(
            ran.iter().all(|job| job.0 == later.0),
            "one spawner for calls one after another, still running after every caller ended: \
             {ran:?}, then {later:?}"
        );
        let process = spawned(here).expect("this process's spawners run a job");
        assert_eq!(
            process.1.as_deref(),
            Some(SPAWNER),
            "this process's spawner's name"
        );
    }

    /// A call waits for no other call's fork: while one job holds the spawner an earlier call
    /// left free, another call runs on a spawner of its own and answers.
    #[test]
    fn a_spawn_waits_for_no_other_spawns_fork() {
        static BUSY: Spawners = Spawners::new(start_thread);
        let earlier = BUSY.run(here).expect("an earlier job ran");
        let (began, beginning) = mpsc::channel();
        let (release, held) = mpsc::channel::<()>();
        let first = thread::spawn(move || {
            BUSY.run(move || {
                let _ = began.send(());
                let _ = held.recv();
                here()
            })
        });
        beginning.recv_timeout(HANG).expect("the first job began");
        let (said, answer) = mpsc::channel();
        thread::spawn(move || {
            let _ = said.send(BUSY.run(here));
        });
        let second = answer
            .recv_timeout(HANG)
            .expect("a spawn waits for no other spawn's fork")
            .expect("the second job ran");
        drop(release);
        let first = first
            .join()
            .expect("the first caller")
            .expect("the first job ran");
        assert_eq!(first.0, earlier.0, "the first took the spawner left free");
        assert_ne!(first.0, second.0, "the second ran on a spawner of its own");
        assert_eq!(second.1.as_deref(), Some(SPAWNER), "a spawner's name");
    }

    /// When a spawner's thread cannot start, the call answers that error, runs nothing and lists
    /// no spawner, and the next call starts one.
    #[test]
    fn a_spawner_that_cannot_start_runs_nothing_and_the_next_call_starts_one() {
        static REFUSED: AtomicBool = AtomicBool::new(false);
        fn refuse_once(
            builder: thread::Builder,
            body: Box<dyn FnOnce() + Send>,
        ) -> std::io::Result<()> {
            if REFUSED.swap(true, Ordering::SeqCst) {
                start_thread(builder, body)
            } else {
                Err(std::io::Error::other("no thread"))
            }
        }
        static REFUSING: Spawners = Spawners::new(refuse_once);
        let ran = Arc::new(AtomicBool::new(false));
        let job_ran = Arc::clone(&ran);
        let refused = REFUSING
            .run(move || {
                job_ran.store(true, Ordering::SeqCst);
                here()
            })
            .expect_err("a spawn whose spawner cannot start answers an error");
        assert_eq!(refused.to_string(), "no thread", "the start's own error");
        assert!(!ran.load(Ordering::SeqCst), "its job did not run");
        assert!(REFUSING.take_free().is_none(), "no spawner is listed free");
        let next = REFUSING.run(here).expect("the next call starts a spawner");
        assert_eq!(next.1.as_deref(), Some(SPAWNER), "a spawner's name");
    }
}
