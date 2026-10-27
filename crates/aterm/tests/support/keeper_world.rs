// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keeper's scratch world on real processes: this build's `aterm keeper serve` and the
//! headless instances that crash and recover beside it, shared by the crash-restore live lane
//! (`keeper_restore_live_headless`) and the restore measurement (`benches/restore_bench.rs`).

#![allow(dead_code)]

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use crate::live_ctl::Ctl;
use crate::{child_output, headless_boot, launch_isolation};

/// A wait for something that must happen is a hang detector (AGENTS.md).
pub const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;
/// How long macOS may hold the first exec of a new program: the bound on the keeper showing
/// it runs from its fresh copy of its code ([`keeper_reached`]).
pub const FIRST_EXEC_HANG: Duration = aterm_uds::deadline::FIRST_EXEC_HANG;
/// How often a wait on a status line asks again.
pub const GAP: Duration = Duration::from_millis(50);

/// The recovery census's seam (`recovery_census::LEDGER_SEAM`): every instance writes the
/// row the installed app's cold start would into the world's own ledger, so the test reads
/// the `keeper=` and `restored=` words the soak judges (`aterm_update::restore_soak`).
pub const LEDGER_SEAM: &str = "ATERM_DEBUG_RECOVERY_LEDGER";

/// One scratch world the keeper and both instances share: one `$HOME`, one config with
/// the keeper on, one `$XDG_RUNTIME_DIR` (where the keeper's socket lives, `/tmp` being no
/// private `$TMPDIR`), in a directory made for it ([`world`]); removed on drop, after the
/// keeper and every instance (declared after it, so dropped first), past the hung-up shell's
/// last history write, unless it holds a copy of the keeper's code that never showed it ran
/// ([`unrun_copies`]), or that check could not finish.
pub struct World(pub PathBuf);

impl Drop for World {
    fn drop(&mut self) {
        match unrun_copies(&self.0) {
            Ok(unrun) if unrun.is_empty() => headless_boot::remove_world(&self.0),
            Ok(unrun) => eprintln!(
                "KEPT {}: the keeper's copy {unrun:?} never showed it ran, so macOS may still be \
                 assessing its first exec; remove it once no process runs from it",
                self.0.display()
            ),
            Err(error) => eprintln!(
                "KEPT {}: cannot establish whether every keeper copy ran: {error}",
                self.0.display()
            ),
        }
    }
}

/// Where the world's keepers keep the copies of their code they run from.
pub fn keeper_code(root: &Path) -> PathBuf {
    aterm_keeper::code::code_dir_in(&root.join("home"))
}

/// The copies of the keeper's code in the world that never showed they ran (no
/// [`aterm_keeper::code::RAN`] mark): macOS may still be assessing the first exec of one, and
/// a file unlinked mid-assessment can strand every exec waiting on macOS's program check, so
/// it stays until a child showed it ran from it (AGENTS.md), however old its exec stamp is.
/// A half-made `.partial` copy was never run, and an entry no keeper named is no copy.
///
/// # Errors
/// Enumerating the code directory or an entry, or reading a copy's execution marker failed.
/// Only an absent directory before enumeration means no copies exist.
pub fn unrun_copies(root: &Path) -> io::Result<Vec<PathBuf>> {
    scan_unrun_copies(
        std::fs::read_dir(keeper_code(root))
            .map(|entries| entries.map(|entry| entry.map(|entry| entry.path()))),
        |copy| match std::fs::symlink_metadata(copy.join(aterm_keeper::code::RAN)) {
            Ok(marker) if marker.file_type().is_file() => Ok(true),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the keeper's execution marker is not a regular file",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        },
    )
}

/// The same cleanup decision with injected enumeration and marker observations.
pub(crate) fn scan_unrun_copies(
    entries: io::Result<impl IntoIterator<Item = io::Result<PathBuf>>>,
    mut ran: impl FnMut(&Path) -> io::Result<bool>,
) -> io::Result<Vec<PathBuf>> {
    let entries = match entries {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut unrun = Vec::new();
    for entry in entries {
        let copy = entry?;
        let Some(name) = copy.file_name() else {
            continue;
        };
        let name = name.to_string_lossy();
        if !name.ends_with(".partial")
            && aterm_keeper::code::owner_of(&name).is_some()
            && !ran(&copy)?
        {
            unrun.push(copy);
        }
    }
    Ok(unrun)
}

/// One child process, SIGKILLed and reaped on drop.
pub struct Proc {
    pub child: Child,
    /// The instant just before it was spawned.
    pub spawned: Instant,
    _lifeline: Option<aterm_uds::lifeline::Lifeline>,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A new world in a directory made for it (`/tmp/.ubd/atkr-<tag>-…`), never one an earlier run
/// kept ([`World`]) and never removed to make room.
pub fn world(tag: &str) -> World {
    let made = aterm_tempfile::Builder::new()
        .prefix(&format!("atkr-{tag}-"))
        .tempdir_in("/tmp");
    let world = match made {
        Ok(dir) => World(dir.keep()),
        Err(e) => aterm_spec::host::could_not_run(format!("no scratch world in /tmp ({e})")),
    };
    let root = &world.0;
    if !launch_isolation::sock_fits(&root.join("run/aterm/keeper-dev-00000000.sock"))
        || launch_isolation::prepare(root).is_err()
    {
        aterm_spec::host::could_not_run("no scratch world with a socket path short enough");
    }
    let config = root.join("cfg/aterm/aterm.toml");
    let mut text = std::fs::read_to_string(&config).expect("the scratch config");
    text.push_str("[keeper]\nenabled = true\n");
    std::fs::write(&config, text).expect("turn the keeper on");
    world
}

/// `aterm keeper <args…>` in the world's environment, read to its end.
pub fn keeper_cli(root: &Path, args: &[&str]) -> String {
    #[expect(
        clippy::disallowed_types,
        reason = "the shared std-typed launch helper closes non-stdio descriptors before spawn"
    )]
    let mut cmd = std::process::Command::new(launch_isolation::aterm_executable());
    launch_isolation::apply(&mut cmd, root);
    cmd.arg("keeper")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("spawn aterm keeper");
    let out = child_output::wait_and_read(child, std::time::Instant::now() + HANG * 2)
        .unwrap_or_else(|| panic!("aterm keeper {args:?} did not exit in time"));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The keeper's `status` text, `key=value` words across its lines.
pub fn keeper_status(root: &Path) -> String {
    keeper_cli(root, &["status"])
}

/// Wait until the keeper's status holds `word`.
#[track_caller]
pub fn await_keeper(root: &Path, word: &str) {
    let mut last = String::new();
    let seen = aterm_spec::wait::within(HANG, GAP, || {
        last = keeper_status(root);
        last.split_whitespace().any(|w| w == word).then_some(())
    });
    assert!(
        seen.is_some(),
        "the keeper never said `{word}`; its status:\n{last}"
    );
}

/// This build's keeper, in the foreground (`serve --no-relaunch`: no window is brought
/// back, the next launch recovers). It runs again from a fresh copy of its code
/// (`aterm_keeper::code`), a new program whose first exec macOS holds for as long as its
/// queue of new programs makes it (over a minute behind a build's), so the one-minute wait for
/// its answer starts once it showed it runs ([`await_start`]).
pub fn keeper(root: &Path) -> Proc {
    let log = root.join("keeper.log");
    let file = std::fs::File::create(&log).expect("the keeper's log");
    #[expect(
        clippy::disallowed_types,
        reason = "the shared std-typed launch helper closes non-stdio descriptors before spawn"
    )]
    let mut cmd = std::process::Command::new(launch_isolation::aterm_executable());
    launch_isolation::apply(&mut cmd, root);
    cmd.args(["keeper", "serve", "--no-relaunch"])
        .stdin(Stdio::null())
        .stdout(file.try_clone().expect("log"))
        .stderr(file);
    let spawned = Instant::now();
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => aterm_spec::host::could_not_run(format!("cannot launch the keeper ({e})")),
    };
    let mut keeper = Proc {
        child,
        spawned,
        _lifeline: None,
    };
    let pid = keeper.child.id();
    let started = await_start(root, pid, HANG, FIRST_EXEC_HANG, &mut || {
        !matches!(keeper.child.try_wait(), Ok(None))
    });
    match started {
        Start::Ran | Start::Ended => {}
        Start::NoCopy => {
            aterm_uds::deadline::require_trust_service();
            panic!(
                "the keeper (pid {pid}) made no copy of its code to run from in {HANG:?}: no \
                 exec stamp in one, nothing in its log"
            );
        }
        Start::NotRun => {
            let held = aterm_uds::process::at_program_check(i32::try_from(pid).unwrap_or(0));
            if let Some(words) = aterm_uds::deadline::held_words(pid, FIRST_EXEC_HANG, held) {
                aterm_spec::host::could_not_run(words);
            }
            panic!(
                "the keeper (pid {pid}) never showed it runs: no mark in its copy of its code, \
                 nothing in its log"
            );
        }
    }
    if let Ok(Some(status)) = keeper.child.try_wait() {
        panic!(
            "the keeper exited ({status}) before it answered; its log:\n{}",
            headless_boot::log_tail(&log)
        );
    }
    let mut last = String::new();
    let answered = aterm_spec::wait::within(HANG, GAP, || {
        last = keeper_status(root);
        last.lines()
            .any(|l| l.starts_with("masters="))
            .then_some(())
    });
    assert!(
        answered.is_some(),
        "the keeper never answered; its status:\n{last}\nits log:\n{}",
        headless_boot::log_tail(&log)
    );
    keeper
}

/// How far a keeper's start got ([`await_start`]).
#[derive(Debug, PartialEq, Eq)]
pub enum Start {
    /// It showed it runs.
    Ran,
    /// It exited.
    Ended,
    /// No copy of its code was stamped for its exec, nor did it write a line.
    NoCopy,
    /// Its copy was stamped for its exec and never showed it runs.
    NotRun,
}

/// Wait for keeper `pid` to show it runs, `ended` saying whether it exited, in two waits. First
/// up to `copy_hang` for its copy's exec stamp ([`aterm_uds::deadline::EXEC_STAMP`], written
/// just before it runs again from the copy): the work before is its own, its first exec (this
/// build's) warmed by [`launch_isolation::apply`]. Then up to `run_hang` for the copy's mark
/// ([`aterm_keeper::code::RAN`]): that wait is macOS's first-exec check of the copy. A line
/// in its log answers either wait ([`keeper_reached`]).
pub fn await_start(
    root: &Path,
    pid: u32,
    copy_hang: Duration,
    run_hang: Duration,
    ended: &mut dyn FnMut() -> bool,
) -> Start {
    let mut wait = |hang, mark| {
        aterm_spec::wait::within(hang, GAP, || {
            if ended() {
                Some(Start::Ended)
            } else {
                keeper_reached(root, pid, mark).then_some(Start::Ran)
            }
        })
    };
    match wait(copy_hang, aterm_uds::deadline::EXEC_STAMP) {
        None => Start::NoCopy,
        Some(Start::Ran) => wait(run_hang, aterm_keeper::code::RAN).unwrap_or(Start::NotRun),
        Some(other) => other,
    }
}

/// Whether keeper `pid`'s copy of its code in the world ([`keeper_code`], `<pid>-<start>`,
/// never a half-made `.partial` one) holds the file `mark`, or the keeper wrote to its log,
/// which before it runs from its copy it does only where its start went another way: it
/// exits, serves from this build, or could not stamp its copy (a world made for it holds no
/// earlier keeper's copy for its sweep to speak of).
pub fn keeper_reached(root: &Path, pid: u32, mark: &str) -> bool {
    let said = std::fs::metadata(root.join("keeper.log")).is_ok_and(|meta| meta.len() > 0);
    said || std::fs::read_dir(keeper_code(root))
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            !name.ends_with(".partial")
                && aterm_keeper::code::owner_of(&name).is_some_and(|(owner, _)| owner == pid)
                && entry.path().join(mark).exists()
        })
}

/// One headless instance on its own socket in the world, hearing no system's memory pressure
/// ([`launch_isolation::SYSTEM_PRESSURE`]), with each debug seam in `seams` set to its value.
pub fn boot(root: &Path, tag: &str, seams: &[(&str, &str)]) -> (Proc, String) {
    let sock = root.join(format!("run/aterm/{tag}.sock"));
    let log_path = root.join(format!("{tag}.log"));
    let (out, err) = match std::fs::File::create(&log_path).and_then(|f| Ok((f.try_clone()?, f))) {
        Ok(pair) => pair,
        Err(e) => aterm_spec::host::could_not_run(format!("cannot open the instance log ({e})")),
    };
    #[expect(
        clippy::disallowed_types,
        reason = "the shared std-typed launch and ready helpers close non-stdio descriptors before spawn"
    )]
    let mut cmd = std::process::Command::new(launch_isolation::aterm_executable());
    launch_isolation::apply(&mut cmd, root);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .arg("--control-sock")
        .arg(&sock)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    for (seam, value) in seams {
        cmd.env(seam, value);
    }
    let ledger = ledger_dir(root);
    if let Err(e) = std::fs::create_dir_all(&ledger) {
        aterm_spec::host::could_not_run(format!("cannot make the census's folder ({e})"));
    }
    cmd.env(LEDGER_SEAM, &ledger);
    let lifeline = launch_isolation::lifeline(&mut cmd, root);
    let mut ready = headless_boot::arm_ready(&mut cmd, root);
    let spawned = Instant::now();
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => aterm_spec::host::could_not_run(format!("cannot launch aterm --headless ({e})")),
    };
    let mut instance = Proc {
        child,
        spawned,
        _lifeline: Some(lifeline),
    };
    headless_boot::await_ready(
        &mut instance,
        |p| &mut p.child,
        &mut ready,
        &log_path,
        |_| true,
    );
    (instance, sock.to_string_lossy().into_owned())
}

/// The world's recovery ledger folder, every instance's census row in it.
pub fn ledger_dir(root: &Path) -> PathBuf {
    root.join("ledger")
}

/// What every request to a recovered instance answers while an old journal the keeper
/// may still offer has no accepted successor (`keeper_link::RecoveryCountAdmission`):
/// the instance serves nothing and closes the connection until the keeper confirms it
/// read each adopted session's new journal (its SYNC answer behind the COMPANION).
pub const FENCED: &str = "ERR recovery-count-custody unconfirmed";

/// A connection to the instance at `sock` once its recovery-count fence has lifted
/// ([`FENCED`]): a must-happen wait, each refused connection dropped and a new one asked
/// every [`GAP`].
#[track_caller]
pub fn admitted(sock: &str) -> Ctl {
    admitted_every(sock, GAP).ctl
}

/// A connection past the recovery-count fence ([`admitted_every`]), and when its probes ran:
/// the fence lifted after `last_refused` (or before the first probe) and before `answered`.
pub struct Admitted {
    pub ctl: Ctl,
    /// Just before the last probe the fence refused was sent; `None` when the first was served.
    pub last_refused: Option<Instant>,
    /// When the served probe's answer arrived.
    pub answered: Instant,
}

/// [`admitted`], asking again every `gap`: a refused probe only reads the fence's flag, so how
/// often the bench asks does not move when it lifts.
#[track_caller]
pub fn admitted_every(sock: &str, gap: Duration) -> Admitted {
    let mut last = String::new();
    let mut last_refused = None;
    let admitted = aterm_spec::wait::within(HANG, gap, || {
        let sent = Instant::now();
        let mut ctl = Ctl::connect(sock);
        last = ctl.line("status");
        if last == FENCED {
            last_refused = Some(sent);
            return None;
        }
        Some(Admitted {
            ctl,
            last_refused,
            answered: Instant::now(),
        })
    });
    admitted.unwrap_or_else(|| panic!("the recovery-count fence never lifted: {last}"))
}
