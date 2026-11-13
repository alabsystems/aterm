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
//! executable's path since replaces (`route`; not for a process started through its dynamic
//! loader, whose `/proc/self/exe` is the loader). On both, a routed spawn's tether reports the
//! program's exec error back to the spawn (`STATUS_VAR`). The watcher is a grandchild of the tether,
//! adopted by launchd (on Linux by init, or the nearest subreaper), watching the parent's end and
//! the child's exit: on Darwin by a kqueue watch on the parent's exit and exec (`NOTE_EXIT`,
//! `NOTE_EXEC`) and on the child's exit; on Linux by pidfds (Linux 5.3), one of the thread that
//! forked the tether, which ends at the parent's exit or exec as the signal's thread does
//! (`THREAD_VAR`; `PIDFD_THREAD`, Linux 6.9; an older kernel watches the parent's own, readable
//! at its exit alone), and one of the child. A Linux kernel with no pidfd, or a seccomp filter
//! that refuses one, arms the watcher with nothing to watch, and the child is tied by its signal
//! alone. The tether and its watcher read themselves and the parent by their pids in `/proc`, so
//! where `/proc` is another pid namespace's a spawn routes nothing (`route`); before Linux 6.9 a
//! watcher judges its group empty by listings of `/proc`, so nor does one where `/proc` lists no
//! group whole (`hidepid`, no `ns_last_pid`), nor one of a process whose pid is 1, whose orphans
//! it adopts itself. The watcher leads a session of its own, made before it arms, so a listing of the
//! child's group (a stage sweep's), a kill of that group and a hangup of the child's terminal never
//! count or reach it (a group leader's group holds the watcher's anchor where it has one, a zombie,
//! as below, which Darwin's listing shows and Linux's passes by); it still ends with the child by
//! its exit watch. When the parent exits, or execs in its own
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
//! group id names the child's group and no other while the watcher lives (`group`); from Linux 6.9
//! a Linux watcher holds it by the child's pidfd instead, by which the kernel signals and counts
//! the group itself, never another group that reuses its number (`group::GroupPidfd`). When the
//! parent exits or execs, the watcher ends the whole group, `SIGTERM` then `SIGKILL` rounds
//! (`group::end_group`), then the child alone, as above, should it have left the group. When the
//! child exits first, the watcher of a group leader stays while the group holds a live member,
//! woken by each member's exit, and ends the group should the parent go first (`group::linger`);
//! the watcher of any other child exits with it. Linux leaves a watcher for a group leader alone:
//! any other tethered child there is tied by its signal alone. The tether execs the child only
//! once the watcher is armed and the parent is still its own parent. An exec of the parent before
//! the watcher armed ends nothing on Darwin; on Linux from 6.9 a forking thread gone by then leaves
//! the watcher unarmed and the tether starts nothing (before 6.9 the watcher arms on the parent's
//! own pidfd and sees that exec only at the new image's exit), and a child with no watcher is tied
//! from the moment it set its signal. A kill by the parent's name passes the watcher by.

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

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "tether_image.rs"]
mod image;

#[cfg(target_os = "macos")]
#[path = "tether_selected.rs"]
mod selected;
#[cfg(target_os = "macos")]
pub use selected::SelectedExecutable;

/// An executable recorded to run through, with its file as it was recorded.
struct Through {
    /// Its path.
    exe: PathBuf,
    /// Its file's device and inode then; `None` when they did not read.
    file: Option<(u64, u64)>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    held: Option<std::fs::File>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    source: Option<image::Source>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    retained: OnceLock<image::Retained>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    ready: OnceLock<()>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    creating: std::sync::Mutex<()>,
    /// Whether it is this process's own image, as [`entry`] records it; a test's [`through`]
    /// names another.
    #[cfg(target_os = "linux")]
    own: bool,
}

impl Through {
    /// Record `exe` only when its opened regular file matches this process's loaded image.
    #[cfg(unix)]
    fn own(exe: PathBuf) -> Self {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
        let opened = (|| {
            let image = crate::process::LoadedImage::read(i32::try_from(std::process::id()).ok()?)?;
            // Bind the opened description to the kernel. Linux opens its own proc image;
            // Darwin accepts a launch link only when its target is the loaded file.
            // Nonblocking opens refuse a FIFO without waiting for a writer.
            #[cfg(target_os = "linux")]
            let source = Path::new("/proc/self/exe");
            #[cfg(not(target_os = "linux"))]
            let source = exe.as_path();
            let held = crate::kept_out(|| {
                std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOCTTY)
                    .open(source)
            })
            .ok()?;
            let meta = held.metadata().ok()?;
            image
                .matches_metadata(&meta)
                .then_some((held, (meta.dev(), meta.ino())))
        })();
        let file = opened.as_ref().map(|(_, identity)| *identity);
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let source = opened
            .as_ref()
            .and_then(|(held, _)| image::Source::read(held).ok());
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let retained = {
            let retained = OnceLock::new();
            if let (Some(source), Some((held, _)), Some(name)) = (&source, &opened, exe.file_name())
                && source.bundled()
                && let Ok(copy) = source.retain(held, name)
            {
                let _ = retained.set(copy);
            }
            retained
        };
        Self {
            exe,
            file,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            held: opened.map(|(held, _)| held),
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            source,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            retained,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            ready: OnceLock::new(),
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            creating: std::sync::Mutex::new(()),
            #[cfg(target_os = "linux")]
            own: true,
        }
    }

    /// `exe`, another process's image, with the file at its path now.
    #[cfg(any(test, debug_assertions))]
    fn of(exe: PathBuf) -> Self {
        let file = file_at(&exe);
        Self {
            exe,
            file,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            held: None,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            source: None,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            retained: OnceLock::new(),
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            ready: OnceLock::new(),
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            creating: std::sync::Mutex::new(()),
            #[cfg(target_os = "linux")]
            own: false,
        }
    }

    /// Its path, while the file there is still the one recorded.
    fn unchanged(&self) -> Option<&Path> {
        (self.file.is_some() && file_at(&self.exe) == self.file).then_some(self.exe.as_path())
    }

    fn available(&self) -> Option<&Path> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if let Some(retained) = self.retained.get() {
            return retained.path();
        }
        // An owned image must be retained before a helper is prepared. Returning
        // the original name would reintroduce replacement between lookup and spawn.
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let original_usable = self.held.is_none();
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let original_usable = true;
        if original_usable && let Some(path) = self.unchanged() {
            return Some(path);
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let _guard = self.creating.lock().ok()?;
            if let Some(retained) = self.retained.get() {
                return retained.path();
            }
            let held = self.held.as_ref()?;
            let retained = self
                .source
                .as_ref()?
                .retain(held, self.exe.file_name()?)
                .ok()?;
            self.retained.set(retained).ok()?;
            self.retained.get()?.path()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            None
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn prepared(&self) -> Result<Option<&Path>, String> {
        let Some(path) = self.available() else {
            return Ok(None);
        };
        // Explicit test hosts supply their own protocol. Owned frontends retain a
        // code-matched copy; its loader and native assessment precede a bounded probe.
        if self.held.is_none() || self.ready.get().is_some() {
            return Ok(Some(path));
        }
        let _guard = self
            .creating
            .lock()
            .map_err(|_| "tether helper preparation lock poisoned".to_string())?;
        if self.ready.get().is_none() {
            let mut command = crate::spawnfd::command(path);
            command.arg(READY_VERB).process_group(0);
            let output =
                crate::deadline::output_within(&mut command, std::time::Duration::from_secs(60))
                    .map_err(|error| format!("tether helper did not prepare: {error}"))?;
            if !output.status.success() || output.stdout != READY_LINE.as_bytes() {
                return Err("tether helper returned no execution receipt".into());
            }
            let _ = self.ready.set(());
        }
        Ok(Some(path))
    }
}

#[cfg(unix)]
const READY_VERB: &str = "__tether_ready";
#[cfg(unix)]
const READY_LINE: &str = "aterm tether helper executed\n";

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
/// `None`. The launch path must open the kernel's loaded image; an unavailable or mismatched
/// file is recorded without an executable path to run through. On Linux it records none when
/// `/proc/self/exe` is not the file this code runs from ([`runs_its_own_image`]): under its
/// dynamic loader (`ld.so <program>`) the kernel's loaded image is the loader, which would read
/// [`VERB`] as a program to load.
#[must_use = "return a handled tether command's exit status to the caller"]
pub fn entry() -> Option<ExitCode> {
    #[cfg(unix)]
    {
        let mut args = std::env::args_os().skip(1);
        if args.next().is_some_and(|first| first == VERB) {
            return Some(serve(&args.collect::<Vec<_>>()));
        }
        if std::env::args_os()
            .nth(1)
            .is_some_and(|arg| arg == READY_VERB)
        {
            if std::env::args_os().len() != 2 {
                return Some(ExitCode::FAILURE);
            }
            print!("{READY_LINE}");
            return Some(ExitCode::SUCCESS);
        }
        let _ = THROUGH.get_or_init(|| {
            #[cfg(target_os = "linux")]
            if !runs_its_own_image() {
                return None;
            }
            std::env::current_exe().ok().map(Through::own)
        });
    }
    None
}

/// Whether `/proc/self/exe`, the image an exec of it runs, is the image this code runs from: the
/// file mapped where the kernel put the program headers of the image it exec'd (`AT_PHDR` as the
/// kernel saved it at the exec, `/proc/self/auxv`, which no loader rewrites) is the file mapped
/// where this function runs. Both are read off `/proc/self/maps`, which names every mapping's file
/// alike on any file system, where a `stat` of `/proc/self/exe` need not (btrfs gives each
/// subvolume a device of its own, and an overlay before Linux 6.8 maps its lower file). Not so
/// under an explicit loader (`ld.so <program>`): the kernel exec'd the loader, which mapped the
/// program itself. `false` when either does not read.
#[cfg(target_os = "linux")]
fn runs_its_own_image() -> bool {
    /// `AT_PHDR`: the program headers of the image the kernel exec'd.
    const AT_PHDR: usize = 3;
    let Some(headers) = kernel_aux(AT_PHDR) else {
        return false;
    };
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
        return false;
    };
    let file_at = |address| maps.lines().find_map(|line| maps_file_at(line, address));
    let here = (runs_its_own_image as *const ()).addr();
    matches!(
        (file_at(headers), file_at(here)),
        (Some(exec), Some(code)) if exec == code && exec.1 != 0
    )
}

/// The value of entry `kind` of the auxiliary vector the kernel gave this process at its exec, as
/// it saved it (`/proc/self/auxv`: pairs of native words).
#[cfg(target_os = "linux")]
fn kernel_aux(kind: usize) -> Option<usize> {
    const WORD: usize = size_of::<usize>();
    let auxv = std::fs::read("/proc/self/auxv").ok()?;
    let (words, _) = auxv.as_chunks::<WORD>();
    let (pairs, _) = words.as_chunks::<2>();
    pairs.iter().find_map(|[key, value]| {
        (usize::from_ne_bytes(*key) == kind).then(|| usize::from_ne_bytes(*value))
    })
}

/// The file one `/proc/<pid>/maps` `line` maps, its device as the line names it and its inode
/// (`0` for no file), when the line's range holds `address`.
#[cfg(any(target_os = "linux", all(test, unix)))]
fn maps_file_at(line: &str, address: usize) -> Option<(&str, u64)> {
    let mut fields = line.split_ascii_whitespace();
    let (low, high) = fields.next()?.split_once('-')?;
    let range = usize::from_str_radix(low, 16).ok()?..usize::from_str_radix(high, 16).ok()?;
    if !range.contains(&address) {
        return None;
    }
    let device = fields.nth(2)?;
    Some((device, fields.next()?.parse().ok()?))
}

/// Tie this running process to its immediate parent, including its own group when it leads one.
/// This arms the same parent-death watch as [`entry`] without requiring a preceding exec.
/// Use [`hold_parent_of`] when the original parent was captured before spawning, so an
/// adopter cannot replace it before this entry runs.
/// The same platform conditions apply; on Linux the signal follows the thread that forked
/// this process, so that thread must live while it owns this process.
/// # Errors
/// The parent has already exited, or the platform cannot arm its watch.
/// # Safety
/// Call before starting any other thread: the watcher's fork must inherit no locks held by
/// another thread.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
pub unsafe fn hold_parent() -> Result<(), String> {
    // SAFETY: getppid takes nothing and cannot fail. hold validates it again after arming.
    let parent = unsafe { libc::getppid() };
    // SAFETY: this function carries the same single-threaded entry obligation.
    unsafe { hold_parent_of(parent) }
}

/// Tie this running process to the immediate parent captured by its caller before spawn.
/// An adopter cannot replace that parent if the caller died before this entry ran.
/// The relationship is checked before and after arming the native watch.
/// # Errors
/// `parent` is at most 1, is not this process's parent, has exited, or its watch cannot be armed.
/// # Safety
/// Call before starting any other thread: the watcher's fork must inherit no locks held by
/// another thread.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
pub unsafe fn hold_parent_of(parent: i32) -> Result<(), String> {
    if parent <= 1 || !parent_is(parent) {
        return Err(not_parent(parent));
    }
    hold(parent)
}

/// The executable a tethered spawn runs through, once its `main` called [`entry`]. A macOS
/// bundle-bound image uses a shared complete clone retained at startup, including before an
/// update can replace the original name between lookup and spawn. A standalone image clones
/// its held file before returning a helper path, even while the original is unchanged.
/// A private image's CodeDirectory matches
/// the kernel's loaded cdhash. Its distinct inode is helper code custody,
/// not the running process's original file identity. The copy stays for this boot, including
/// after parent death while macOS may still assess a child. Linux retains a byte-matched ELF
/// from its kernel-bound description; its loaded inode is never a reusable parent proc path.
/// `None` without a recorded image or
/// when its retained copy cannot be provided; a Darwin spawn then starts untethered.
#[must_use]
pub fn executable() -> Option<&'static Path> {
    THROUGH.get()?.as_ref()?.available()
}

/// Prepare the helper before constructing a probe with its own execution budget.
/// macOS and Linux retained copies must execute their early entry once in this process's
/// preparation, proving their loader context before a separate queried-program budget.
/// The one-minute preparation hang detector preserves an unproved copy on failure.
/// Other platforms use [`executable`] without an extra execution.
///
/// # Errors
/// Helper preparation's lock, spawn or I/O fails, it hangs, or the child returns no
/// successful execution receipt.
pub fn prepared_executable() -> Result<Option<&'static Path>, String> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        THROUGH
            .get()
            .and_then(Option::as_ref)
            .map(Through::prepared)
            .unwrap_or(Ok(None))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Ok(executable())
    }
}

/// Whether this process's tethered spawns have lost their tie: its `main` served the tether
/// ([`entry`]), or a test named an executable ([`through`]), but no unchanged executable is
/// available now (neither its original file nor a code-matched retained image can be provided),
/// so on Darwin a tethered spawn starts its child untethered. `false` where nothing
/// was recorded (a test binary's own spawns), and off Darwin, whose tie needs no executable: the
/// parent-death signal, and a Linux group leader's tether run through the running image itself
/// (`route`).
#[must_use]
pub fn lost() -> bool {
    cfg!(target_vendor = "apple") && lost_of(THROUGH.get())
}

/// [`lost`] for what was `recorded`, on Darwin.
fn lost_of(recorded: Option<&Option<Through>>) -> bool {
    recorded.is_some_and(|recorded| recorded.as_ref().and_then(Through::available).is_none())
}

/// TEST SEAM: choose the private retained-image cache of a Cargo-named native fixture.
/// # Errors
/// This is not a test binary, the path is relative, or a cache was already selected.
#[cfg(all(
    any(target_os = "macos", target_os = "linux"),
    any(test, debug_assertions)
))]
pub fn image_cache_for_tests(root: PathBuf) -> std::io::Result<()> {
    image::test_root(root)
}

/// TEST SEAM: run retained-image cleanup before a fixture's first execution receipt.
/// # Errors
/// No test cache was selected, the boot identity is unavailable, or cleanup failed.
#[cfg(all(
    any(target_os = "macos", target_os = "linux"),
    any(test, debug_assertions)
))]
pub fn image_cleanup_for_tests() -> std::io::Result<()> {
    image::test_sweep()
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
/// run through (its `main` never called [`entry`], or ran under an explicit loader), in a process
/// whose pid is 1 (a container's first process, or a PID namespace's), a parent no tether takes
/// ([`parent_pid`]), since its orphans are adopted by it, and on Linux where `/proc` is another
/// pid namespace's (`unshare --pid` with no `--mount-proc`), whose numbers the tether's reads of
/// itself and of the parent would take for strangers', or where the watcher could never tell its
/// group had emptied: a kernel that signals no group by pidfd (before 6.9,
/// [`signals_groups_by_pidfd`]) whose listing of a group cannot be whole
/// (`process::group_listing_can_be_whole`: a `/proc` that hides processes, no `ns_last_pid`). The child is then tied by Linux's signal alone, and starts
/// untethered on Darwin. The tether reports its exec's error back on the spawn's exec status
/// ([`STATUS_VAR`]), since it runs the program only after the spawn started it.
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
    // The tether and its watcher read themselves, the parent and its forking thread by their
    // pids in `/proc`, which must be this pid namespace's on every kernel; a watcher with no
    // pidfd scope for its group judges it empty by whole listings alone (`group::linger`).
    #[cfg(target_os = "linux")]
    if !crate::process::proc_is_this_namespaces()
        || (!signals_groups_by_pidfd() && !crate::process::group_listing_can_be_whole())
    {
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
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    let status = exec_status();
    #[cfg(target_vendor = "apple")]
    let own_path = std::env::var_os(OWN_PATH_VAR).is_some();
    if let Err(why) = hold(parent) {
        eprintln!(
            "aterm: {} was not started: {why}",
            Path::new(program).display()
        );
        return ExitCode::from(UNTETHERED);
    }
    // The exec searches the `PATH` the spawn gave this tether as its spawn would have searched
    // it: as a `PATH` a spawn sets, which on Darwin passes over an entry too long to join to the
    // name, unless the spawn left the child this process's own (Darwin's [`OWN_PATH_VAR`]), whose
    // search ends there. This tether holds the descriptors its spawn handed it and no others
    // (every one it inherits for a transparent spawn, stdio alone otherwise, and its exec status,
    // close-on-exec), so the exec hands them on as they are.
    let mut command = crate::spawnfd::transparent(program).into_command();
    // The forking thread's id, the exec status and the search's rule are the tether's to read
    // ([`THREAD_VAR`], [`STATUS_VAR`], [`OWN_PATH_VAR`]), never the program's, as the test's
    // listing seam is.
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    command.env_remove(STATUS_VAR);
    #[cfg(target_os = "linux")]
    command.env_remove(THREAD_VAR);
    #[cfg(target_vendor = "apple")]
    command.env_remove(OWN_PATH_VAR);
    #[cfg(all(target_os = "linux", any(test, debug_assertions)))]
    command.env_remove(BY_LISTING_VAR);
    #[cfg(all(target_vendor = "apple", any(test, debug_assertions)))]
    command.env_remove(NO_LISTING_VAR);
    #[cfg(target_vendor = "apple")]
    let set_path = !own_path;
    #[cfg(not(target_vendor = "apple"))]
    let set_path = true;
    if set_path && let Some(path) = std::env::var_os("PATH") {
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
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    if let Some(status) = status
        && let Some(errno) = error.raw_os_error()
    {
        // The spawn returns this error and reaps this tether, which says nothing more.
        let _ = std::io::Write::write_all(&mut std::fs::File::from(status), &errno.to_ne_bytes());
        return ExitCode::from(NOT_RUN);
    }
    eprintln!("aterm: {}: {error}", Path::new(program).display());
    ExitCode::from(NOT_RUN)
}

/// The variable a tethered spawn that runs through its tether ([`route`]) names its exec
/// status by: the number of a pipe's write end the tether holds, close-on-exec once the tether
/// took it ([`exec_status`]), so the program's exec closes it; an exec that fails writes its
/// `errno` there (four bytes, native order) first. The spawn reads it to its end and fails with
/// that error, as a spawn of the program itself would fail with its own exec's, before it returns
/// ([`spawnfd::Command`](crate::spawnfd::Command)'s spawn).
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
pub(crate) const STATUS_VAR: &str = "ATERM_TETHER_STATUS";

/// The variable a Darwin routed spawn sets (to `1`) when its child's `PATH` is this process's own,
/// which the spawn did not set, clear or remove: Darwin's search of that `PATH` ends at an entry
/// too long to join to the name (`posix_spawnp`'s rule) where one a spawn set passes over it, so
/// the tether's exec searches it so too ([`serve`]).
#[cfg(target_vendor = "apple")]
pub(crate) const OWN_PATH_VAR: &str = "ATERM_TETHER_OWN_PATH";

/// The exec status [`STATUS_VAR`] names, made close-on-exec: `None` when it names none, a number
/// below 3 (the spawn places it above stdio), or a descriptor this process does not hold as a
/// pipe.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn exec_status() -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd as _;
    let fd: i32 = std::env::var(STATUS_VAR).ok()?.parse().ok()?;
    if fd < 3 {
        return None;
    }
    // SAFETY: `fstat` writes one live, zeroed stat; `fcntl` takes integers.
    unsafe {
        let mut stat: libc::stat = std::mem::zeroed();
        if libc::fstat(fd, &mut stat) != 0
            || stat.st_mode & libc::S_IFMT != libc::S_IFIFO
            || libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) != 0
        {
            return None;
        }
        // A pipe this process holds at that number, which nothing else of it owns: its spawn
        // handed it over for this alone.
        Some(std::os::fd::OwnedFd::from_raw_fd(fd))
    }
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
/// starts nothing), stdio on `/dev/null` and every other descriptor the tether held closed but
/// `arming` ([`hold_nothing_but`]: a transparent spawn's, and the spawn's exec status), a kqueue watch on `parent`'s exit and exec and on the
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
    // Closed with every other descriptor but `arming` ([`hold_nothing_but`]).
    let _ = armed;
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
        for fd in 0..3 {
            libc::dup2(null, fd);
        }
        // `null` and `armed` among them, and the spawn's exec status, whose end must come at the
        // tether's exec.
        hold_nothing_but(arming);
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

/// The Linux watcher: Darwin's, pidfds in place of its kqueue. It puts stdio on `/dev/null` and
/// closes every other descriptor the tether held but `arming` ([`hold_nothing_but`]: a
/// transparent spawn's, and the spawn's exec status, whose end must come at the tether's exec),
/// takes [`NAME`] as its kernel name ([`KERNEL_NAME`]; Linux's `pkill` matches that name), and
/// opens a pidfd of the `watched` child while it still shares the child's group, so the number
/// names the child or opens nothing: a group's number is not reused while a member, here the
/// watcher, holds it, and the child waits on this arming. It holds a group leader's group by that
/// pidfd where the kernel signals a group by one (Linux 6.9, `group::GroupPidfd`), else by its
/// anchor ([`anchored`], `group::GroupLive`); then it leads a session of its own, opens a
/// descriptor of the parent's end ([`parent_end`]), writes the arming byte, and waits. The
/// parent's exit or exec ends a leader's group once the child reads started or exited
/// ([`end_group_once_ready`]), then the child alone by its proof ([`end_once_run`]), with the live
/// reads and `kill(2)`; the child's exit first leaves a leader's group watched while it holds a
/// live member, and ends it should the parent end first (`group::linger`). A kernel with no pidfd
/// (`ENOSYS`), or a seccomp filter that refuses one (`EPERM`), arms it with nothing to watch: it
/// writes the byte and exits, and the child is tied by its parent-death signal alone. A child
/// whose pidfd does not open otherwise, a parent already reaped, or from Linux 6.9 a forking
/// thread gone by then, leaves it unarmed; before 6.9 it watches the parent's own pidfd, so an
/// exec of the parent before it armed is seen only at the new image's exit. A parent that exited
/// unreaped arms it with its end already readable, so it ends the tether's group at once. Either
/// way the tether starts nothing, as its parent check fails.
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
    let _ = armed;
    let child = watched.pid;
    // SAFETY (whole body): the function's contract; every pointer is to a live local or static.
    unsafe {
        for fd in 0..3 {
            libc::dup2(null, fd);
        }
        hold_nothing_but(arming);
        prctl(PR_SET_NAME, KERNEL_NAME.as_ptr());
        let exit = match pidfd_open(child, 0) {
            Ok(exit) => exit,
            Err(e) if matches!(e.raw_os_error(), Some(libc::ENOSYS | libc::EPERM)) => {
                let armed = libc::write(arming, [1_u8].as_ptr().cast(), 1) == 1;
                leave(None, i32::from(!armed));
            }
            Err(_) => leave(None, 1),
        };
        // As on Darwin: the group held first, while the watcher is still in it, then out of the
        // child's group and session before it arms.
        let by_pidfd =
            leads && !listing_forced() && group::signal_by_pidfd(exit.as_raw_fd(), 0).is_ok();
        // Whether a listing can show the group left with zombies alone (`group::GroupPidfd`).
        let whole = by_pidfd && crate::process::group_listing_can_be_whole();
        let anchor = if leads && !by_pidfd {
            match anchored() {
                Some(anchor) => Some(anchor),
                None => leave(None, 1),
            }
        } else {
            None
        };
        if libc::setsid() < 0 {
            leave(anchor.as_ref(), 1);
        }
        let gone = match parent_end(parent) {
            Ok(gone) => gone,
            Err(e) if matches!(e.raw_os_error(), Some(libc::ENOSYS | libc::EPERM)) => {
                let armed = libc::write(arming, [1_u8].as_ptr().cast(), 1) == 1;
                leave(anchor.as_ref(), i32::from(!armed));
            }
            Err(_) => leave(anchor.as_ref(), 1),
        };
        if libc::write(arming, [1_u8].as_ptr().cast(), 1) != 1 {
            leave(anchor.as_ref(), 1);
        }
        libc::close(arming);
        // The leader's group, as this watcher holds it; `None` for any other child.
        let held = || {
            if by_pidfd {
                Some(group::Holding::Pidfd(group::GroupPidfd {
                    parent: gone.as_raw_fd(),
                    leader: exit.as_raw_fd(),
                    pgid: child,
                    whole,
                }))
            } else {
                anchor.as_ref().map(|anchor| {
                    group::Holding::Anchor(group::GroupLive {
                        parent: gone.as_raw_fd(),
                        pgid: child,
                        anchor,
                    })
                })
            }
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
                if let Some(mut group) = held() {
                    let _ended = end_group_once_ready(
                        watched,
                        fds[1].revents != 0,
                        &mut control,
                        proof,
                        |patience| child_exit_notice(&exit, patience),
                        &mut group,
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
                if let Some(mut group) = held()
                    && group::linger(child, &mut group)
                {
                    let _ended = group::end_group_guarded(
                        child,
                        crate::process::END_GRACE,
                        &mut group,
                        || group::Readiness::Exited,
                    );
                }
                leave(anchor.as_ref(), 0);
            }
        }
    }
}

/// TEST SEAM: the variable that has a Linux tether's watcher hold its group by its anchor and
/// listings even where the kernel signals a group by pidfd, so a test on such a kernel runs the
/// older kernels' mode too. The tether takes it out of the program's environment.
#[cfg(all(target_os = "linux", any(test, debug_assertions)))]
pub const BY_LISTING_VAR: &str = "ATERM_TETHER_BY_LISTING";

/// Whether [`BY_LISTING_VAR`] asks for the anchor and listings; never in a release build.
#[cfg(target_os = "linux")]
fn listing_forced() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        std::env::var_os(BY_LISTING_VAR).is_some()
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// Whether this kernel signals a process group by a pidfd (`PIDFD_SIGNAL_PROCESS_GROUP`, Linux
/// 6.9), so a watcher holds its group by its child's pidfd with no listing: the scope known when
/// signal `0` by this process's own pidfd answers, `0` for a group leader and `ESRCH` for any
/// other process (no process's group is this one's pid), where an older kernel answers `EINVAL`.
/// Read once.
#[cfg(target_os = "linux")]
#[must_use]
pub fn signals_groups_by_pidfd() -> bool {
    use std::os::fd::AsRawFd as _;
    static KNOWN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *KNOWN.get_or_init(|| {
        let own = i32::try_from(std::process::id())
            .ok()
            .map(|pid| pidfd_open(pid, 0));
        let Some(Ok(own)) = own else {
            return false;
        };
        match group::signal_by_pidfd(own.as_raw_fd(), 0) {
            Ok(()) => true,
            Err(e) => e.raw_os_error() == Some(libc::ESRCH),
        }
    })
}

/// Closes every descriptor from 3 up but `keep`: each one `proc_pidinfo(PROC_PIDLISTFDS)` lists,
/// else, when the listing fails, every number below the bound on them
/// ([`crate::spawnfd::descriptor_bound`], `kern.maxfilesperproc`), so the
/// watcher never holds what it inherited (its spawn's exec status above all) for want of a list.
/// For a forked child with one thread, which nothing else opens descriptors in.
#[cfg(target_vendor = "apple")]
fn hold_nothing_but(keep: i32) {
    if let Some(listed) = listed_descriptors() {
        for fd in listed {
            if fd >= 3 && fd != keep {
                // SAFETY: a number this process listed, which nothing else of this one-thread
                // process owns.
                unsafe { libc::close(fd) };
            }
        }
        return;
    }
    let size = crate::spawnfd::descriptor_bound();
    for fd in (3..size).filter(|&fd| fd != keep) {
        // SAFETY: a number of this one-thread process's table; one not open is a harmless `EBADF`.
        unsafe { libc::close(fd) };
    }
}

/// Every descriptor number this process holds, as `proc_pidinfo(PROC_PIDLISTFDS)` lists them:
/// `None` when either call fails. Its seam forces the failure in a test and debug build
/// ([`NO_LISTING_VAR`]).
#[cfg(target_vendor = "apple")]
fn listed_descriptors() -> Option<Vec<i32>> {
    const ENTRY: usize = std::mem::size_of::<libc::proc_fdinfo>();
    #[cfg(any(test, debug_assertions))]
    if std::env::var_os(NO_LISTING_VAR).is_some() {
        return None;
    }
    // SAFETY: `getpid` takes nothing and cannot fail.
    let me = unsafe { libc::getpid() };
    // SAFETY: a null buffer of no size asks for the size the listing needs, and touches no memory.
    let need = unsafe { libc::proc_pidinfo(me, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    let need = usize::try_from(need).ok().filter(|&need| need > 0)?;
    // Room for what the call said and more: nothing opens a descriptor here meanwhile.
    let mut listed = vec![
        libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0,
        };
        need / ENTRY + 8
    ];
    let room = i32::try_from(listed.len() * ENTRY).unwrap_or(i32::MAX);
    // SAFETY: `listed` is a live buffer of `room` bytes the call fills with whole entries.
    let got = unsafe {
        libc::proc_pidinfo(
            me,
            libc::PROC_PIDLISTFDS,
            0,
            listed.as_mut_ptr().cast(),
            room,
        )
    };
    let got = usize::try_from(got).ok().filter(|&got| got > 0)?;
    Some(
        listed[..(got / ENTRY).min(listed.len())]
            .iter()
            .map(|entry| entry.proc_fd)
            .collect(),
    )
}

/// TEST SEAM: the variable that has a test or debug build's Darwin watcher find its descriptor
/// listing failed ([`listed_descriptors`]), so it closes every number below the bound instead. The tether
/// takes it out of the program's environment.
#[cfg(all(target_vendor = "apple", any(test, debug_assertions)))]
pub const NO_LISTING_VAR: &str = "ATERM_TETHER_NO_FD_LISTING";

/// Closes every descriptor from 3 up but `keep`: `close_range(2)` (Linux 5.9) over the two spans
/// either side of it, else each number `/proc/self/fd` lists. For a forked child with one thread,
/// which nothing else opens descriptors in.
#[cfg(target_os = "linux")]
fn hold_nothing_but(keep: i32) {
    // The asm-generic, x86 and Arm number (MIPS offsets its tables; there a call that answers
    // `ENOSYS` falls back to the listing below).
    const SYS_CLOSE_RANGE: libc::c_long = 436;
    let close_range = |low: i32, high: u32| {
        // SAFETY: three integers; the kernel touches no memory of ours.
        unsafe { libc::syscall(SYS_CLOSE_RANGE, low.unsigned_abs(), high, 0_u32) == 0 }
    };
    let below = keep <= 3 || close_range(3, keep.unsigned_abs() - 1);
    if below && close_range(keep.saturating_add(1), u32::MAX) {
        return;
    }
    let Ok(listed) = std::fs::read_dir("/proc/self/fd") else {
        return;
    };
    let fds: Vec<i32> = listed
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .collect();
    for fd in fds {
        if fd >= 3 && fd != keep {
            // SAFETY: a number this process listed; one closed meanwhile (the listing's own) is a
            // harmless `EBADF`.
            unsafe { libc::close(fd) };
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
#[path = "tether_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tether_watcher_tests.rs"]
mod watcher_tests;

#[cfg(test)]
#[path = "tether_spawner_tests.rs"]
mod spawner_tests;
