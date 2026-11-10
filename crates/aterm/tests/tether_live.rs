// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A helper a window tethers ends when the window is killed outright: it gets `SIGTERM` first,
//! and on macOS one deaf to that gets `SIGKILL` after the grace, from a watcher that leads a
//! session of its own, outside the helper's group, runs under a name a kill by the window's name
//! passes by, and ends after its helper. A helper at the head of a group of its own has its
//! group ended so, on Linux too, at the window's kill or exec (a Linux exec from Linux 6.9, whose
//! thread pidfds let the watcher see it; before, the window's exit alone). One started plainly is
//! adopted and runs on. A tethered spawn of a program that cannot run fails as a plain one does. The window is a stand-in: this
//! test binary, re-run as [`PARENT`] or [`PARITY`], which spawns through `aterm_uds::spawnfd`
//! with the front door as its tether executable, as a window's helpers run through the window's
//! own.

use std::os::fd::{AsRawFd as _, OwnedFd};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aterm_spec::wait::MUST_HAPPEN;
use aterm_uds::spawnfd::{Stdio, Tether as _};

use crate::{launch_isolation, standin};

/// The stand-in window's [`standin::ENV`] word: it starts a tethered and a plain helper, the
/// tethered one running the script [`TETHERED`] names.
const PARENT: &str = "tether-parent";

/// The variable that names [`PARENT`]'s tethered helper: [`TERMED`], [`DEAF`], [`SESSION`] or
/// [`GROUP`].
const TETHERED: &str = "TETHER_HELPER";

/// A tethered helper that ends at `SIGTERM`: it says it ran, by its pid, and naps a tenth of a
/// second at a time for ten minutes (a bound on a leftover); at `SIGTERM` it says `termed <pid>`
/// and exits, so nothing of its own is left. It naps in the foreground because the shell runs a
/// trap between commands and never loses it there. With a background sleep and `wait`, a `SIGTERM`
/// just before the `wait` left the trap pending for the whole sleep, and one the trap passed on
/// before the sleep's exec landed in its inherited trap, which the exec wiped.
const TERMED: &str = "termed";

/// A tethered helper deaf to `SIGTERM`: it ignores it, says it ran, by its pid, and sleeps ten
/// minutes in its own place, `SIGTERM` still ignored.
const DEAF: &str = "deaf";

/// A tethered helper in a session of its own (`setsid`), so it leads its own group: it says it
/// ran, by its pid, and sleeps ten minutes in its own place, which ends at `SIGTERM`.
const SESSION: &str = "session";

/// A tethered helper spawned at the head of a group of its own (`process_group(0)`), a pipeline's
/// root: it starts a `/bin/sleep` in its group, says it ran, by its pid, and sleeps ten minutes in
/// its own place, each of the two ending at `SIGTERM`.
const GROUP: &str = "group";

/// The variable that has [`PARENT`] exec in its own place once its helpers ran and a byte came on
/// its stdin: this test binary again, as [`PARKED`].
const THEN_EXEC: &str = "TETHER_THEN_EXEC";

/// The stand-in a window that execs in its own place becomes: it waits for its own kill.
const PARKED: &str = "tether-parked";

/// The stand-in that spawns each of [`CASES`] plainly, tethered, and tethered at the head of a
/// group of its own, and says how each went.
const PARITY: &str = "tether-parity";

/// The folder [`PARITY`]'s cases search, from the test.
const PARITY_DIR: &str = "TETHER_PARITY_DIR";

/// The kept signed executable [`PARITY`]'s successful searches must find.
const PARITY_TOOL: &str = "TETHER_PARITY_TOOL";

/// The plain helper: it says it ran, by its pid, then sleeps ten minutes (a bound on a leftover).
const HELPER: [&str; 2] = ["-c", "echo ran $$; exec /bin/sleep 600"];

/// The tethered helper's script for each of [`TERMED`], [`DEAF`] and [`SESSION`].
fn tethered_script(word: &str) -> Option<[&'static str; 2]> {
    match word {
        TERMED => Some([
            "-c",
            "trap 'echo termed $$; exit 0' TERM; echo ran $$; i=0; while [ $i -lt 6000 ]; do \
             /bin/sleep 0.1; i=$((i+1)); done",
        ]),
        DEAF => Some(["-c", "trap '' TERM; echo ran $$; exec /bin/sleep 600"]),
        SESSION => Some(HELPER),
        GROUP => Some(["-c", "/bin/sleep 600 & echo ran $$; exec /bin/sleep 600"]),
        _ => None,
    }
}

/// [`PARITY`]'s cases: a name, the program (None names [`PARITY_TOOL`]'s basename), and its
/// `PATH`. Each runs in [`PARITY_DIR`], which holds only the non-executable `plain` file.
const CASES: [(&str, Option<&str>, Search); 6] = [
    ("missing", Some("no-such-program"), Search::Dir),
    ("not-runnable", Some("plain"), Search::Dir),
    ("found", None, Search::Tool),
    ("found-past-long", None, Search::PastLong),
    ("missing-path", Some("./no-such-program"), Search::Dir),
    ("not-runnable-path", Some("./plain"), Search::Dir),
];

/// A [`CASES`] program's `PATH`, which its spawn sets.
#[derive(Clone, Copy)]
enum Search {
    /// [`PARITY_DIR`].
    Dir,
    /// [`PARITY_TOOL`]'s folder.
    Tool,
    /// The tool's folder after one too long to join to a name (longer than Linux's `PATH_MAX`,
    /// and so Darwin's): the search of a `PATH` its spawn set passes over it.
    PastLong,
}

impl Search {
    /// The `PATH`, with `dir` as [`PARITY_DIR`] and `tool_dir` holding [`PARITY_TOOL`].
    fn path(self, dir: &std::ffi::OsStr, tool_dir: &std::ffi::OsStr) -> std::ffi::OsString {
        match self {
            Self::Dir => dir.to_owned(),
            Self::Tool => tool_dir.to_owned(),
            Self::PastLong => {
                let mut path = std::ffi::OsString::from(format!("/{}:", "l".repeat(5_000)));
                path.push(tool_dir);
                path
            }
        }
    }
}

/// Play the stand-in asked for, or return to the test.
fn play_if_asked() {
    let Some(spec) = standin::spec() else { return };
    aterm_uds::tether::through(launch_isolation::aterm_executable());
    match spec.as_str() {
        PARENT => play_parent(),
        PARITY => play_parity(),
        PARKED => loop {
            std::thread::park();
        },
        _ => {
            standin::put(format!("unknown stand-in: {spec}\n").as_bytes());
            standin::exit(2);
        }
    }
}

/// The window: a tethered and a plain helper, their pids said, then a wait for its own kill, or
/// with [`THEN_EXEC`], once a byte came on its stdin, an exec in its own place, as [`PARKED`].
fn play_parent() -> ! {
    let word = std::env::var(TETHERED).unwrap_or_default();
    let Some(script) = tethered_script(&word) else {
        standin::put(format!("unknown tethered helper: {word:?}\n").as_bytes());
        standin::exit(2)
    };
    let spawn = |tethered: bool| {
        let mut helper = aterm_uds::spawnfd::command("/bin/sh");
        helper
            .args(if tethered { script } else { HELPER })
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        if tethered {
            helper.tether().setsid(word == SESSION);
            if word == GROUP {
                helper.process_group(0);
            }
        }
        helper.spawn().unwrap_or_else(|e| {
            standin::put(format!("spawn: {e}\n").as_bytes());
            standin::exit(3)
        })
    };
    let tethered = spawn(true);
    let plain = spawn(false);
    standin::put(format!("tethered {}\nplain {}\n", tethered.id(), plain.id()).as_bytes());
    if std::env::var_os(THEN_EXEC).is_some() {
        use std::io::Read as _;
        let _ = std::io::stdin().read(&mut [0_u8; 1]);
        let error = aterm_uds::spawnfd::command(standin::exe())
            .args(std::env::args_os().skip(1))
            .env(standin::ENV, PARKED)
            .env_remove(THEN_EXEC)
            .exec();
        standin::put(format!("exec: {error}\n").as_bytes());
        standin::exit(4)
    }
    loop {
        std::thread::park();
    }
}

/// Each case run plainly, tethered, then tethered at the head of a group of its own (which on
/// Linux too runs through its tether): a line `<case> <plain> <tethered> <leader>`, each
/// `exit <code>` or `refused <errno>`.
fn play_parity() -> ! {
    let dir = std::env::var_os(PARITY_DIR).expect("the cases' folder");
    let tool = std::path::PathBuf::from(std::env::var_os(PARITY_TOOL).expect("the kept tool"));
    let tool_dir = tool.parent().expect("the tool's folder").as_os_str();
    let run = |program: Option<&str>, search: Search, tethered: bool, leads: bool| {
        let program = program.map_or_else(
            || tool.file_name().expect("the tool's basename"),
            std::ffi::OsStr::new,
        );
        let mut cmd = aterm_uds::spawnfd::command(program);
        cmd.current_dir(&dir)
            .env("PATH", search.path(&dir, tool_dir));
        if tethered {
            cmd.tether();
        }
        if leads {
            cmd.process_group(0);
        }
        match aterm_uds::deadline::output_until(&mut cmd, Instant::now() + MUST_HAPPEN) {
            Ok(out) => format!("exit {}", out.status.code().unwrap_or(-1)),
            Err(aterm_uds::deadline::RunError::Spawn(e)) => {
                format!("refused {}", e.raw_os_error().unwrap_or(-1))
            }
            Err(e) => format!("failed {e}"),
        }
        .replace(' ', "_")
    };
    for (case, program, search) in CASES {
        let line = format!(
            "{case} {} {} {}\n",
            run(program, search, false, false),
            run(program, search, true, false),
            run(program, search, true, true)
        );
        standin::put(line.as_bytes());
    }
    standin::exit(0)
}

/// Every process of a group the stand-in, or a helper in a session of its own, leads: `SIGKILL`ed
/// on drop, so a failing run leaves nothing it started. A group is signalled only while it holds
/// a member and was never seen empty: once its last member is gone its number is free, and the
/// next process given that pid may lead a group that is not this test's.
struct Group {
    pgid: i32,
    emptied: std::cell::Cell<bool>,
}

impl Group {
    /// The group `pgid`, which holds its leader as it is made.
    fn of(pgid: i32) -> Self {
        Self {
            pgid,
            emptied: std::cell::Cell::new(false),
        }
    }

    /// Whether any process is left in the group; once it answers no, it always does.
    fn holds_any(&self) -> bool {
        // SAFETY: signal zero to a group of this test's only asks whether a member exists.
        let any = !self.emptied.get() && unsafe { libc::kill(-self.pgid, 0) == 0 };
        self.emptied.set(!any);
        any
    }

    /// `SIGKILL` every member while the group holds one; whether it signalled.
    fn end(&self) -> bool {
        let any = self.holds_any();
        if any {
            // SAFETY: a group of this test's, never seen empty, so its number is still the one
            // the stand-in or its helper made; every member is this test's.
            unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
        }
        any
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.end();
    }
}

/// A group this test leads, a `/bin/sleep` its only member, ends at its [`Group::end`] by
/// `SIGKILL`. A group seen empty is never signalled again, even while a group of its number holds
/// a member: a live group stands in for one a reused pid leads.
#[test]
fn a_group_seen_empty_is_never_signalled_again() {
    use std::os::unix::process::ExitStatusExt as _;

    let start = || {
        aterm_uds::spawnfd::command("/bin/sleep")
            .arg("600")
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("start /bin/sleep in a group of its own")
    };
    let mut ended = start();
    let group = Group::of(i32::try_from(ended.id()).expect("a pid"));
    assert!(group.end(), "a group that holds its leader is signalled");
    let status = ended.wait().expect("reap the leader");
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "the group's end killed its leader: {status:?}"
    );
    assert!(!group.holds_any(), "the reaped leader's group is empty");
    assert!(!group.end(), "an empty group is not signalled");

    let mut alive = start();
    let reused = Group::of(i32::try_from(alive.id()).expect("a pid"));
    reused.emptied.set(true);
    let signalled = reused.end();
    let still = alive.try_wait().expect("look at the member");
    alive.kill().expect("SIGKILL this test's own sleep");
    alive.wait().expect("reap it");
    assert!(
        !signalled && still.is_none(),
        "a group seen empty is never signalled again: signalled {signalled}, member {still:?}"
    );
}

/// Whether `exit` (an [`aterm_uds::exitwatch::exit_fd`]) reports its process's exit within `hang`.
fn exits_within(exit: &OwnedFd, hang: Duration) -> bool {
    let mut fd = libc::pollfd {
        fd: exit.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let deadline = Instant::now() + hang;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let ms = i32::try_from(left.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: one live `pollfd`, which `poll` reads and writes and does not retain.
        match unsafe { libc::poll(&mut fd, 1, ms) } {
            1.. => return true,
            0 => return false,
            _ if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => {}
            _ => panic!("poll: {}", std::io::Error::last_os_error()),
        }
    }
}

/// A stand-in window ([`PARENT`]) whose two helpers said they ran.
struct Started {
    parent: aterm_uds::spawnfd::Child,
    group: Group,
    tethered: u32,
    plain: u32,
    tethered_exit: OwnedFd,
    plain_exit: OwnedFd,
    /// The lines its stdout says from here on, its helpers' included.
    said: mpsc::Receiver<String>,
    /// Its tethered helper's watcher; Linux leaves one for a helper at the head of its own group
    /// alone ([`SESSION`], [`GROUP`]).
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    watcher: Option<Watcher>,
}

/// The tether's watcher of a stand-in window's tethered helper, found as it runs: an orphan
/// (launchd's child on macOS; on Linux, adopted by init or the nearest subreaper, not the
/// window's), born after the window, the front door's image with its thread run, serving the
/// tether's verb for that window (`argv[1]`, `argv[2]`). Only such an image's arguments are read.
/// On drop it is `SIGKILL`ed by pid while its pid still reads the unique id it was found with, so
/// a failing run leaves no watcher behind.
#[cfg(any(target_os = "macos", target_os = "linux"))]
struct Watcher {
    pid: i32,
    /// Its kernel unique id as found, which no later process reads.
    uniqueid: u64,
    /// A descriptor readable once it has exited.
    exit: OwnedFd,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Watcher {
    /// The one watcher serving `window`. It leads a session of its own, with no terminal, and
    /// runs under a name a kill by the front door's name (`pkill aterm`, which matches the last
    /// part of `argv[0]` on macOS and the kernel's name on Linux) passes by.
    fn of(window: u32) -> Self {
        use std::os::unix::fs::MetadataExt as _;

        use aterm_uds::process;

        let born_after = process::read_lineage(i32::try_from(window).expect("a pid"))
            .expect("read the stand-in window")
            .uniqueid;
        let aterm = launch_isolation::aterm_executable();
        let image = std::fs::metadata(&aterm).expect("the front-door image");
        let image = (image.dev(), image.ino());
        let window_word = std::ffi::OsString::from(window.to_string());
        let found = aterm_spec::wait::until_within(
            MUST_HAPPEN,
            "the tether's watcher of the stand-in window is listed",
            || {
                let watchers: Vec<_> = aterm_uds::holders::uid_pids()
                    .ok()?
                    .into_iter()
                    .filter_map(|pid| {
                        let pid = i32::try_from(pid).ok()?;
                        let lineage = process::read_lineage(pid)?;
                        let front_door = || {
                            process::read_process_path(pid)
                                .and_then(|path| std::fs::metadata(path).ok())
                                .is_some_and(|meta| (meta.dev(), meta.ino()) == image)
                        };
                        // Linux's unique id is the start in clock ticks, which the window and
                        // the watcher may share.
                        let born_before = if cfg!(target_os = "linux") {
                            lineage.uniqueid < born_after
                        } else {
                            lineage.uniqueid <= born_after
                        };
                        let adopted = if cfg!(target_os = "linux") {
                            lineage.parent != window
                        } else {
                            lineage.parent == 1
                        };
                        if !adopted
                            || born_before
                            || !front_door()
                            || process::thread_ran(pid) != Some(true)
                        {
                            return None;
                        }
                        let argv = process::read_process_argv(pid)?;
                        (argv
                            .get(1)
                            .is_some_and(|verb| verb == aterm_uds::tether::VERB)
                            && argv.get(2) == Some(&window_word))
                        .then_some((lineage, argv))
                    })
                    .collect();
                (!watchers.is_empty()).then_some(watchers)
            },
        );
        let [(lineage, argv)] = <[_; 1]>::try_from(found)
            .unwrap_or_else(|found| panic!("one watcher serves window {window}: {found:?}"));
        let pid = lineage.identity.pid;
        // SAFETY: `getsid` only reads the session id of a process this test found.
        let session = unsafe { libc::getsid(pid) };
        assert!(
            session == pid && lineage.tty.is_none(),
            "the tether's watcher (pid {pid}) leads a session of its own, with no terminal: \
             session {session}, {lineage:?}"
        );
        let name = aterm.file_name().expect("the front door's name");
        let named = std::path::Path::new(&argv[0])
            .file_name()
            .map(|last| last.to_string_lossy().into_owned())
            .unwrap_or_default();
        assert!(
            !named.contains(&*name.to_string_lossy()),
            "the tether's watcher (pid {pid}) runs as {named:?}, which a kill by the name \
             {name:?} matches: {argv:?}"
        );
        #[cfg(target_os = "linux")]
        {
            let kernel = std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .expect("the watcher's kernel name");
            assert!(
                !kernel.contains(&*name.to_string_lossy()),
                "the tether's watcher (pid {pid}) has the kernel name {kernel:?}, which Linux's \
                 kill by the name {name:?} matches"
            );
        }
        Self {
            pid,
            uniqueid: lineage.uniqueid,
            exit: aterm_uds::exitwatch::exit_fd(pid.unsigned_abs()).expect("watch the watcher"),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for Watcher {
    fn drop(&mut self) {
        if aterm_uds::process::read_lineage(self.pid).is_some_and(|l| l.uniqueid == self.uniqueid) {
            // SAFETY: the watcher this test found, its unique id read again just now.
            unsafe { libc::kill(self.pid, libc::SIGKILL) };
        }
    }
}

impl Started {
    /// Its tethered helper's watcher.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn watcher(&self) -> &Watcher {
        self.watcher
            .as_ref()
            .expect("the tethered helper's watcher")
    }

    /// Whether `line` is said within `hang`.
    fn says_within(&self, line: &str, hang: Duration) -> bool {
        let deadline = Instant::now() + hang;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.said.recv_timeout(left) {
                Ok(said) if said == line => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
    }
}

/// Start the stand-in window as the test `entry`, its tethered helper `helper` ([`TERMED`],
/// [`DEAF`], [`SESSION`] or [`GROUP`]), and wait for both its helpers to say they ran.
fn start_parent(entry: &str, helper: &str) -> Started {
    start_parent_then(entry, helper, false)
}

/// [`start_parent`], the window exec'ing in its own place when `then_exec` ([`THEN_EXEC`]), once a
/// byte comes on its piped stdin.
fn start_parent_then(entry: &str, helper: &str, then_exec: bool) -> Started {
    let aterm = launch_isolation::aterm_executable();
    aterm_spec::wait::first_exec(&aterm, &["--help"]);
    let mut parent = aterm_uds::spawnfd::command(standin::exe());
    parent
        .args(standin::argv(entry))
        .env(standin::ENV, PARENT)
        .env(TETHERED, helper);
    if then_exec {
        parent.env(THEN_EXEC, "1");
    }
    let mut parent = parent
        .stdin(if then_exec {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .process_group(0)
        .spawn()
        .expect("start the stand-in window");
    let group = Group::of(i32::try_from(parent.id()).expect("a pid"));
    let out = parent.stdout.take().expect("its piped stdout");
    let (lines, said) = mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead as _;
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let (mut tethered, mut plain, mut ran) = (None, None, Vec::new());
    let deadline = Instant::now() + aterm_spec::wait::FIRST_EXEC;
    while tethered.is_none_or(|pid| !ran.contains(&pid))
        || plain.is_none_or(|pid| !ran.contains(&pid))
    {
        let left = deadline.saturating_duration_since(Instant::now());
        let line: String = said
            .recv_timeout(left)
            .expect("both helpers said they ran, by pid");
        let mut words = line.split_whitespace();
        let (Some(word), Some(pid)) = (
            words.next(),
            words.next().and_then(|p| p.parse::<u32>().ok()),
        ) else {
            continue;
        };
        match word {
            "tethered" => tethered = Some(pid),
            "plain" => plain = Some(pid),
            "ran" => ran.push(pid),
            _ => {}
        }
    }
    let (tethered, plain) = (tethered.expect("its pid"), plain.expect("its pid"));
    #[cfg(target_os = "macos")]
    let watcher = Some(Watcher::of(parent.id()));
    #[cfg(target_os = "linux")]
    let watcher = [SESSION, GROUP]
        .contains(&helper)
        .then(|| Watcher::of(parent.id()));
    Started {
        parent,
        group,
        tethered,
        plain,
        tethered_exit: aterm_uds::exitwatch::exit_fd(tethered).expect("watch the tethered helper"),
        plain_exit: aterm_uds::exitwatch::exit_fd(plain).expect("watch the plain helper"),
        said,
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        watcher,
    }
}

/// Once the tethered helper has ended: its watcher, where it has one, ends too, the plain helper,
/// the negative control, was adopted and runs on; end it, and see nothing the stand-in started is
/// left in its group. The watcher is in none of it: it leads a session of its own and ends with
/// its helper by its exit watch.
fn plain_ran_on(started: Started) {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if let Some(watcher) = &started.watcher {
        assert!(
            exits_within(&watcher.exit, MUST_HAPPEN),
            "the tether's watcher (pid {}) outlived its helper by {MUST_HAPPEN:?}",
            watcher.pid
        );
    }
    let Started {
        group,
        plain,
        plain_exit,
        ..
    } = started;
    assert_eq!(
        aterm_uds::process::process_presence(i32::try_from(plain).expect("a pid")),
        aterm_uds::process::ProcessPresence::Present,
        "the plain helper (pid {plain}), the negative control, was adopted and runs on"
    );
    // SAFETY: the plain helper is this test's own, alive as just read.
    unsafe { libc::kill(i32::try_from(plain).expect("a pid"), libc::SIGKILL) };
    assert!(
        exits_within(&plain_exit, MUST_HAPPEN),
        "the plain helper ended"
    );
    aterm_spec::wait::until_within(
        MUST_HAPPEN,
        "nothing the stand-in started is left in its group",
        || (!group.holds_any()).then_some(()),
    );
}

/// The stand-in window is `SIGKILL`ed: its tethered helper gets `SIGTERM`, says so and ends, and
/// the plain one runs on.
#[test]
fn a_tethered_helper_ends_with_its_sigkilled_starter() {
    play_if_asked();
    let mut started = start_parent(
        "tether_live::a_tethered_helper_ends_with_its_sigkilled_starter",
        TERMED,
    );
    let tethered = started.tethered;
    let killed = Instant::now();
    started.parent.kill().expect("SIGKILL the stand-in window");
    started.parent.wait().expect("reap it");
    assert!(
        exits_within(&started.tethered_exit, MUST_HAPPEN),
        "the tethered helper (pid {tethered}) outlived its killed parent by {MUST_HAPPEN:?}"
    );
    eprintln!(
        "the tethered helper (pid {tethered}) ended {:?} after its parent's SIGKILL",
        killed.elapsed()
    );
    let termed = format!("termed {tethered}");
    assert!(
        started.says_within(&termed, MUST_HAPPEN),
        "the tethered helper (pid {tethered}) never said `{termed}`: no SIGTERM came first"
    );
    plain_ran_on(started);
}

/// The stand-in window is `SIGKILL`ed with a tethered helper that ignores `SIGTERM`: it lives
/// through the grace, the watcher's `SIGKILL` after it ends it, and the plain one runs on. The
/// grace starts after the parent's kill and the `SIGKILL` waits it out, so an end sooner than the
/// grace (a bound below, not a budget) is a `SIGKILL` that came early or a helper that was not
/// deaf. The window is killed only once the helper runs its last image with its thread run: it
/// says it ran before it execs `/bin/sleep`, and the watcher signals no child whose thread has not
/// run, so a kill in between would leave it no signal at all. Linux leaves a watcher for a helper
/// at the head of its own group alone: there such a helper runs on.
#[cfg(target_os = "macos")]
#[test]
fn a_tethered_helper_deaf_to_sigterm_ends_after_the_grace() {
    play_if_asked();
    let mut started = start_parent(
        "tether_live::a_tethered_helper_deaf_to_sigterm_ends_after_the_grace",
        DEAF,
    );
    let tethered = started.tethered;
    runs_sleep_with_its_thread_run(tethered);
    let grace = aterm_uds::process::END_GRACE;
    let killed = Instant::now();
    started.parent.kill().expect("SIGKILL the stand-in window");
    started.parent.wait().expect("reap it");
    assert!(
        exits_within(&started.tethered_exit, MUST_HAPPEN),
        "the tethered helper deaf to SIGTERM (pid {tethered}) outlived its killed parent by \
         {MUST_HAPPEN:?}: no SIGKILL came after the grace"
    );
    let ended = killed.elapsed();
    eprintln!(
        "the tethered helper deaf to SIGTERM (pid {tethered}) ended {ended:?} after its parent's \
         SIGKILL; the grace is {grace:?}"
    );
    assert!(
        ended >= grace,
        "the tethered helper deaf to SIGTERM (pid {tethered}) ended {ended:?} after its parent's \
         SIGKILL, inside the grace of {grace:?}: its SIGKILL came early"
    );
    plain_ran_on(started);
}

/// Waits until `pid` runs `/bin/sleep`, the deaf helper's last image, and its thread has run
/// ([`aterm_uds::process::thread_ran`]), as the watcher's proof reads it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn runs_sleep_with_its_thread_run(pid: u32) {
    use std::os::unix::fs::MetadataExt as _;

    let pid = i32::try_from(pid).expect("a pid");
    let sleep = std::fs::metadata("/bin/sleep").expect("/bin/sleep");
    let sleep = (sleep.dev(), sleep.ino());
    aterm_spec::wait::until_within(
        MUST_HAPPEN,
        "the deaf helper runs /bin/sleep, its thread run",
        || {
            let image = aterm_uds::process::read_process_path(pid)
                .and_then(|path| std::fs::metadata(path).ok())?;
            ((image.dev(), image.ino()) == sleep
                && aterm_uds::process::thread_ran(pid) == Some(true))
            .then_some(())
        },
    );
}

/// A tethered helper that leads a session and group of its own holds that group with its
/// watcher's anchor alone, a zombie that holds the group's number while the watcher lives: the
/// watcher leads a session of its own ([`Watcher::of`] reads it), so a listing of the helper's
/// group (a stage sweep's), a kill of it or a hangup of its terminal never counts or reaches the
/// watcher. macOS lists the anchor; Linux's listing passes a zombie by, so there the anchor is
/// found as the watcher's exited child in the helper's group ([`zombies_of`]). NEGATIVE CONTROL:
/// a watcher with no anchor has none. The watcher still ends the helper when the stand-in window
/// is `SIGKILL`ed, then ends itself, and the plain helper runs on. The window is killed only once
/// the helper runs its last image with its thread run, as the watcher's proof reads it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_watcher_leaves_its_helpers_group_and_session() {
    play_if_asked();
    let mut started = start_parent(
        "tether_live::the_watcher_leaves_its_helpers_group_and_session",
        SESSION,
    );
    let tethered = started.tethered;
    let helper = i32::try_from(tethered).expect("a pid");
    let helpers_group = Group::of(helper);
    let members = aterm_uds::process::group_members(tethered).expect("list the helper's group");
    assert!(
        members.complete,
        "the listing of the tethered helper's group (pid {tethered}) is whole: {members:?}"
    );
    let others: Vec<i32> = members
        .pids
        .iter()
        .copied()
        .filter(|&p| p != helper)
        .collect();
    let watcher = started.watcher().pid;
    #[cfg(target_os = "macos")]
    assert!(
        members.pids.contains(&helper)
            && !members.pids.contains(&watcher)
            && matches!(
                others[..],
                [anchor] if aterm_uds::process::process_presence(anchor)
                    == aterm_uds::process::ProcessPresence::Exited
            ),
        "the tethered helper's group (pid {tethered}) holds the helper and its watcher's anchor, \
         an exited process, and not the watcher, which leads a session of its own: {members:?}"
    );
    #[cfg(target_os = "linux")]
    {
        assert!(
            members.pids == [helper] && others.is_empty(),
            "the tethered helper's group (pid {tethered}) lists the helper alone, not the \
             watcher (pid {watcher}), which leads a session of its own: {members:?}"
        );
        let anchors = zombies_of(watcher, helper);
        assert!(
            matches!(anchors[..], [_]),
            "the watcher (pid {watcher}) holds the helper's group (pid {tethered}) by one anchor, \
             an exited child of its own in that group: {anchors:?}"
        );
    }
    runs_sleep_with_its_thread_run(tethered);
    started.parent.kill().expect("SIGKILL the stand-in window");
    started.parent.wait().expect("reap it");
    assert!(
        exits_within(&started.tethered_exit, MUST_HAPPEN),
        "the tethered helper in a session of its own (pid {tethered}) outlived its killed parent \
         by {MUST_HAPPEN:?}"
    );
    aterm_spec::wait::until_within(
        MUST_HAPPEN,
        "nothing is left in the tethered helper's group",
        || (!helpers_group.holds_any()).then_some(()),
    );
    plain_ran_on(started);
}

/// The pids of `parent`'s exited children in group `pgid`, read from every `/proc/<pid>/stat`
/// (state `Z`, its parent and its group): a Linux group listing passes them by.
#[cfg(target_os = "linux")]
fn zombies_of(parent: i32, pgid: i32) -> Vec<i32> {
    std::fs::read_dir("/proc")
        .expect("list /proc")
        .filter_map(|entry| {
            let pid = entry.ok()?.file_name().to_str()?.parse::<i32>().ok()?;
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let (_, rest) = stat.rsplit_once(')')?;
            let mut fields = rest.split_whitespace();
            let state = fields.next()?;
            let ppid = fields.next()?.parse::<i32>().ok()?;
            let pgrp = fields.next()?.parse::<i32>().ok()?;
            (state == "Z" && ppid == parent && pgrp == pgid).then_some(pid)
        })
        .collect()
}

/// A tethered helper at the head of a group of its own ends with its group: when the stand-in
/// window is `SIGKILL`ed, its watcher signals the group the helper leads, so the `/bin/sleep` the
/// helper started in it ends too, where a signal to the helper alone would leave that sleep
/// adopted and running (Git's `fetch` leaves `remote-http`, `fetch-pack` and `index-pack` so;
/// Linux's parent-death signal reaches the helper alone). The window is killed only once both run
/// `/bin/sleep` with their threads run. The plain helper, in the window's group, runs on.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_tethered_group_leaders_group_ends_with_its_sigkilled_starter() {
    play_if_asked();
    let mut started = start_parent(
        "tether_live::a_tethered_group_leaders_group_ends_with_its_sigkilled_starter",
        GROUP,
    );
    let (helpers_group, kid) = group_runs_sleep(&started);
    started.parent.kill().expect("SIGKILL the stand-in window");
    started.parent.wait().expect("reap it");
    group_ended(&started, &helpers_group, kid);
    plain_ran_on(started);
}

/// A tethered helper at the head of a group of its own ends with its group when the stand-in
/// window execs in its own place, as at its exit: on macOS the watcher's kqueue notes the exec,
/// and on Linux 6.9 and later the exec ends the thread that forked the tether, whose pidfd the
/// watcher holds (the window's own pidfd stays unread across an exec). An older Linux kernel
/// opens no thread's pidfd, so its watcher holds the window's own: the helper ends at its
/// parent-death signal and the sleep it left runs on in its group until the window's exit, which
/// ends it. The window execs this test binary again, parked ([`PARKED`]), once both run
/// `/bin/sleep`, and is killed once the sleep has ended (on an older Linux, once the helper has);
/// the helper, which the parked window never reaps, holds its group until then. The window's
/// `SIGKILL` status shows it execed and ran on, parked: a failed exec exits it. The plain helper
/// runs on. NEGATIVE CONTROL: a watcher on the window's own pidfd on Linux 6.9 sees only the
/// helper's exit at its parent-death signal, and stays for the sleep it left (the hang detector
/// fires).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_tethered_group_leaders_group_ends_at_its_starters_exec() {
    play_if_asked();
    let mut started = start_parent_then(
        "tether_live::a_tethered_group_leaders_group_ends_at_its_starters_exec",
        GROUP,
        true,
    );
    let (helpers_group, kid) = group_runs_sleep(&started);
    {
        use std::io::Write as _;
        let mut go = started
            .parent
            .stdin
            .take()
            .expect("the stand-in window's piped stdin");
        go.write_all(b"exec\n")
            .expect("tell the stand-in window to exec");
    }
    let tethered = started.tethered;
    assert!(
        exits_within(&started.tethered_exit, MUST_HAPPEN),
        "the tethered group leader (pid {tethered}) outlived its parent's exec by {MUST_HAPPEN:?}"
    );
    let in_group = || aterm_uds::process::read_lineage(kid).is_some_and(|l| l.pgid == tethered);
    if exec_seen() {
        aterm_spec::wait::until_within(
            MUST_HAPPEN,
            &format!(
                "the sleep the tethered group leader started in its group (pid {kid}) ended at \
                 its parent's exec"
            ),
            || (!in_group()).then_some(()),
        );
    } else {
        assert!(
            in_group(),
            "the sleep (pid {kid}) ran on in its group past the exec, which an older Linux \
             kernel's watcher does not see"
        );
    }
    started
        .parent
        .kill()
        .expect("SIGKILL the parked stand-in window");
    let status = started.parent.wait().expect("reap it");
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(libc::SIGKILL),
        "the stand-in window execed in its own place and ran on, parked, until its kill: {status:?}"
    );
    group_ended(&started, &helpers_group, kid);
    plain_ran_on(started);
}

/// Whether a tether's watcher sees its parent's exec: on macOS by its kqueue, on Linux where the
/// kernel opens a thread's pidfd (`PIDFD_THREAD`, Linux 6.9; `EINVAL` before).
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn exec_seen() -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::FromRawFd as _;
        // SAFETY: `gettid` takes nothing; `pidfd_open` takes a thread id and flags and touches no
        // memory of ours.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_pidfd_open,
                libc::syscall(libc::SYS_gettid),
                libc::O_EXCL,
            )
        };
        let Ok(fd) = i32::try_from(fd) else {
            return false;
        };
        if fd < 0 {
            return false;
        }
        // SAFETY: the descriptor the call just made, which nothing else owns.
        drop(unsafe { OwnedFd::from_raw_fd(fd) });
        true
    }
    #[cfg(target_os = "macos")]
    {
        true
    }
}

/// Waits until the stand-in window's tethered helper ([`GROUP`]) and the `/bin/sleep` it started
/// in its group both run `/bin/sleep` with their threads run, and answers the helper's group, held
/// from while it holds the helper, and the sleep's pid.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn group_runs_sleep(started: &Started) -> (Group, i32) {
    let tethered = started.tethered;
    let helper = i32::try_from(tethered).expect("a pid");
    let helpers_group = Group::of(helper);
    runs_sleep_with_its_thread_run(tethered);
    let kid = aterm_spec::wait::until_within(
        MUST_HAPPEN,
        "the helper's group holds the helper and the sleep it started",
        || {
            let members = aterm_uds::process::group_members(tethered)?;
            let [kid] = members
                .pids
                .iter()
                .copied()
                .filter(|&p| {
                    // The watcher's anchor, a zombie, reads nothing and is passed by.
                    p != helper && aterm_uds::process::read_lineage(p).is_some()
                })
                .collect::<Vec<_>>()[..]
            else {
                return None;
            };
            (members.complete && members.pids.contains(&helper)).then_some(kid)
        },
    );
    runs_sleep_with_its_thread_run(kid.unsigned_abs());
    (helpers_group, kid)
}

/// Waits until the stand-in window's tethered helper ([`GROUP`]) and the sleep `kid` it started in
/// its group have ended, its group `helpers_group` empty.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn group_ended(started: &Started, helpers_group: &Group, kid: i32) {
    let tethered = started.tethered;
    assert!(
        exits_within(&started.tethered_exit, MUST_HAPPEN),
        "the tethered group leader (pid {tethered}) outlived its parent's end by {MUST_HAPPEN:?}"
    );
    aterm_spec::wait::until_within(
        MUST_HAPPEN,
        &format!(
            "the sleep the tethered group leader started in its group (pid {kid}) ended with it"
        ),
        || (!helpers_group.holds_any()).then_some(()),
    );
}

/// A tethered helper at the head of a group of its own that ends first leaves its group watched:
/// the helper is `SIGKILL`ed by the test (as a Git `fetch` ends of a pipe its killed parent held),
/// the `/bin/sleep` it started in its group runs on while the stand-in window lives, and ends when
/// the window is `SIGKILL`ed, by the watcher that stayed for it. NEGATIVE CONTROL: a watcher that
/// exits with its helper leaves the sleep adopted and running (the hang detector below fires).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_tethered_leaders_group_left_behind_ends_with_its_sigkilled_starter() {
    play_if_asked();
    let mut started = start_parent(
        "tether_live::a_tethered_leaders_group_left_behind_ends_with_its_sigkilled_starter",
        GROUP,
    );
    let tethered = started.tethered;
    let helper = i32::try_from(tethered).expect("a pid");
    let helpers_group = Group::of(helper);
    runs_sleep_with_its_thread_run(tethered);
    let kid = aterm_spec::wait::until_within(
        MUST_HAPPEN,
        "the helper's group holds the sleep it started",
        || {
            aterm_uds::process::group_members(tethered)?
                .pids
                .into_iter()
                .find(|&p| {
                    p != helper
                        && aterm_uds::process::read_lineage(p)
                            .is_some_and(|l| l.pgid == tethered && l.parent == tethered)
                })
        },
    );
    runs_sleep_with_its_thread_run(kid.unsigned_abs());
    // SAFETY: the tethered helper is this test's own, its exit watch not yet fired (it sleeps ten
    // minutes), so its pid is still its own.
    unsafe { libc::kill(helper, libc::SIGKILL) };
    assert!(
        exits_within(&started.tethered_exit, MUST_HAPPEN),
        "the tethered helper (pid {tethered}) ended at its SIGKILL"
    );
    let kid_exit = aterm_uds::exitwatch::exit_fd(kid.unsigned_abs()).expect("watch the sleep");
    assert_eq!(
        aterm_uds::process::process_presence(started.watcher().pid),
        aterm_uds::process::ProcessPresence::Present,
        "the watcher (pid {}) stays while its helper's group holds the sleep",
        started.watcher().pid
    );
    started.parent.kill().expect("SIGKILL the stand-in window");
    started.parent.wait().expect("reap it");
    assert!(
        exits_within(&kid_exit, MUST_HAPPEN),
        "the sleep the tethered helper left in its group (pid {kid}) outlived the killed window \
         by {MUST_HAPPEN:?}"
    );
    aterm_spec::wait::until_within(
        MUST_HAPPEN,
        "nothing is left in the tethered helper's group",
        || (!helpers_group.holds_any()).then_some(()),
    );
    plain_ran_on(started);
}

/// A tethered spawn of a program that cannot run fails as the plain spawn does, with its error
/// (`ENOENT`, `EACCES`), a bare name searched on the child's `PATH` and a relative path in its
/// folder included: on macOS the tether execs the program only after the spawn returned, and it
/// once exited 127 for a bare name the search could not find, and for one it found past a `PATH`
/// entry too long to search, which its own search of that `PATH` ended at. Only a program that
/// runs runs.
#[test]
fn a_tethered_spawn_that_cannot_run_fails_as_a_plain_one_does() {
    play_if_asked();
    let aterm = launch_isolation::aterm_executable();
    aterm_spec::wait::first_exec(&aterm, &["--help"]);
    let dir = aterm_tempfile::tempdir().expect("scratch");
    std::fs::write(dir.path().join("plain"), b"#!/bin/sh\nexit 0\n").expect("write plain");
    let tool = aterm_uds::deadline::stand_in(std::path::Path::new("/usr/bin/true"));
    let out = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command(standin::exe())
            .args(standin::argv(
                "tether_live::a_tethered_spawn_that_cannot_run_fails_as_a_plain_one_does",
            ))
            .env(standin::ENV, PARITY)
            .env(PARITY_DIR, dir.path())
            .env(PARITY_TOOL, &tool),
        Instant::now() + MUST_HAPPEN,
    )
    .expect("run the stand-in");
    assert_eq!(out.status.code(), Some(0), "the stand-in: {out:?}");
    let said = String::from_utf8_lossy(&out.stdout);
    let expected = [
        ("missing", "refused_2"),
        ("not-runnable", "refused_13"),
        ("found", "exit_0"),
        ("found-past-long", "exit_0"),
        ("missing-path", "refused_2"),
        ("not-runnable-path", "refused_13"),
    ];
    for (case, plain) in expected {
        let line = said
            .lines()
            .find(|line| line.split_whitespace().next() == Some(case))
            .unwrap_or_else(|| panic!("{case}: no line in {said:?}"));
        assert_eq!(
            line.split_whitespace().skip(1).collect::<Vec<_>>(),
            [plain, plain, plain],
            "{case}, plainly, tethered, then tethered at the head of its group"
        );
    }
}

/// A shell forks the tether and tells it a pid that is not its parent. Its arguments refuse `1`;
/// this test's own pid is alive and passes them, so only the tether's parent check keeps the
/// program, which would print `ran`, from running.
#[test]
fn a_tether_whose_parent_is_not_its_own_starts_nothing() {
    let aterm = launch_isolation::aterm_executable();
    aterm_spec::wait::first_exec(&aterm, &["--help"]);
    for stranger in ["1".to_string(), std::process::id().to_string()] {
        let out = aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command("/bin/sh")
                .args(["-c", "\"$@\"; exit $?", "sh"])
                .arg(&aterm)
                .args([aterm_uds::tether::VERB, stranger.as_str()])
                .args(["/bin/sh", "/bin/sh", "-c", "echo ran"]),
            Instant::now() + MUST_HAPPEN,
        )
        .expect("run the front door's tether verb from a shell");
        assert_eq!(
            out.status.code(),
            Some(i32::from(aterm_uds::tether::UNTETHERED)),
            "told pid {stranger}: {out:?}"
        );
        assert!(
            out.stdout.is_empty(),
            "told pid {stranger}, the program never ran: {out:?}"
        );
    }
}
