// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE QUIET MACHINE FOR THE LAB'S TIMED WORK (`docs/DESIGN-adaptive-build-2026-10-24.md`,
//! QUIET-ADMIT). A windowed instance ([`crate::instance::Instance::start`]) and the Linux lane's
//! GUI rows each hold a quiet claim in the per-user capacity ledger while they run: alone among
//! quiet claims, admitted once the 1-minute load is low, saying once a minute what it waits for.
//! That is admission, not a retry. Requested Timing work also requires an actual sampled CPU
//! interval; unavailable, forced and `--admit now` admission cannot qualify it and start no
//! window. This is admission evidence, not continuous quiet or a postcompile performance
//! qualification. The same flag is the run's for every claim it makes: the
//! Linux and Windows lanes' boots take it for their disk claims ([`mode`]).
//! Functional windows reserve memory without timing evidence. Both window kinds also share
//! one account-level lane for this original lab process; a scratch World never chooses it.
//! Native handoffs retain the original lane in keepers and guarded development GUIs, so a
//! surviving window remains serialized after its controller dies. Other desktop apps do
//! not participate. Keepers retain their existing lifeline teardown and never reacquire it.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use aterm_capacity::{Admit, Claim, Ledger, Machine, Need, Pace};
use aterm_toolchain::{
    named_lock::{self, Refused, Wait},
    record_lock::CloseOnlyLock,
};

use crate::Fail;

/// The run's `--admit` mode and its verb.
static RUN: OnceLock<(Admit, String)> = OnceLock::new();
static WINDOW_PROCESS: OnceLock<u32> = OnceLock::new();

/// A positive headroom requirement keeps functional work out of the ledger's auto-ALONE
/// policy: its requested peak must fit alongside this unreserved margin.
const FUNCTIONAL_MARGIN: u64 = 128 * 1024 * 1024;
const WINDOW_LANE: &str = aterm_types::lab_window_guard::LOCK_NAME;

struct WindowLane {
    path: PathBuf,
    held: CloseOnlyLock,
}

/// One original lab run retains its lane even between windows. Never pass a path or an
/// environment marker to a keeper as a substitute for this actual held description.
static WINDOWS: Mutex<Option<WindowLane>> = Mutex::new(None);

fn refused(what: &str, kind: &str, why: &dyn std::fmt::Display, signal: Option<i32>) -> Fail {
    Fail {
        stopped: signal
            .inspect(|sig| crate::stop::note(*sig))
            .or_else(|| crate::stop::check().err().map(|s| s.0)),
        ..crate::linux::not_run(format!("{what}: its {kind} was {why}; it started nothing"))
    }
}

/// Hold the shared native-window lane once for this original lab run. Every Timing and
/// Functional window calls this before its resource claim; repeated calls do not relock.
/// The account database selects the capacity directory, independently of HOME/XDG/LAB_STATE.
/// # Errors
/// NOT RUN for missing/unusable account storage, a changed lane identity, unchecked `now`
/// admission, or interruption before or while waiting. This grants no timing evidence.
pub fn window_lane() -> Result<(), Fail> {
    let pid = std::process::id();
    if *WINDOW_PROCESS.get_or_init(|| pid) != pid {
        // Check before touching WINDOWS: an unexecuted fork could have copied that mutex
        // while another thread held it. An inherited label or descriptor is no run handoff.
        return Err(refused(
            "native window",
            "window lane",
            &"unavailable: this is not the original lab process",
            None,
        ));
    }
    if mode() == Admit::Now {
        return Err(refused(
            "native window",
            "window lane",
            &"not asked for: windows require --admit wait",
            None,
        ));
    }
    let verb = RUN.get().map_or("lab", |(_, verb)| verb.as_str());
    window_lane_in(
        &WINDOWS,
        Ledger::user().as_ref(),
        verb,
        Wait::Block,
        &mut |line| eprintln!("{line}"),
        &|| crate::stop::check().is_err(),
    )
}

/// Duplicate the original run's actual window description for an explicit
/// native handoff. This does not create admission, reacquire a lane or turn a
/// descriptor number into authority.
pub fn window_descriptor() -> Result<std::os::fd::OwnedFd, Fail> {
    window_custody()?
        .try_clone_fd()
        .map_err(|why| refused("native window", "window lane", &why, None))
}

/// Retain the original description for checking a later controlled transfer.
/// This neither reopens its path nor admits another native window.
pub(crate) fn window_custody() -> Result<CloseOnlyLock, Fail> {
    window_lane()?;
    let run = WINDOWS.lock().map_err(|_| {
        refused(
            "native window",
            "window lane",
            &"original run state was poisoned",
            None,
        )
    })?;
    run.as_ref()
        .ok_or_else(|| {
            refused(
                "native window",
                "window lane",
                &"original description is absent",
                None,
            )
        })?
        .held
        .try_clone()
        .map_err(|why| refused("native window", "window lane", &why, None))
}

/// The canonical physical-account lane expected by a keeper receiving actual
/// inherited custody. No scratch HOME, command-line path or PID selects it.
pub fn window_path() -> Result<PathBuf, String> {
    let ledger = Ledger::user().ok_or("no account capacity directory")?;
    aterm_uds::private_file::check_private_dir(ledger.dir()).map_err(|why| why.to_string())?;
    ledger
        .dir()
        .canonicalize()
        .map(|dir| dir.join(WINDOW_LANE))
        .map_err(|why| why.to_string())
}

fn window_lane_in(
    run: &Mutex<Option<WindowLane>>,
    ledger: Option<&Ledger>,
    verb: &str,
    wait: Wait,
    say: &mut dyn FnMut(&str),
    stopped: &dyn Fn() -> bool,
) -> Result<(), Fail> {
    let fail =
        |why: &dyn std::fmt::Display, signal| refused("native window", "window lane", why, signal);
    if stopped() {
        return Err(fail(&"not asked for: the run was interrupted", None));
    }
    let ledger = ledger.ok_or_else(|| fail(&"unavailable: no account capacity ledger", None))?;
    let path = ledger.dir().join(WINDOW_LANE);
    let mut run = run
        .lock()
        .map_err(|_| fail(&"unavailable: original run state was poisoned", None))?;
    if stopped() {
        return Err(fail(&"not asked for: the run was interrupted", None));
    }
    let check = |held: &WindowLane| -> std::io::Result<()> {
        use std::os::unix::fs::MetadataExt as _;
        aterm_uds::private_file::check_private_dir(ledger.dir())?;
        let named = std::fs::symlink_metadata(&path)?;
        let original = held.held.metadata()?;
        if held.path != path
            || !named.is_file()
            || named.dev() != original.dev()
            || named.ino() != original.ino()
        {
            return Err(std::io::Error::other(
                "the original window lane was replaced",
            ));
        }
        Ok(())
    };
    if let Some(held) = run.as_ref() {
        return check(held).map_err(|why| fail(&why, None));
    }
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(ledger.dir())
            .and_then(|()| aterm_uds::private_file::check_private_dir(ledger.dir()))
            .map_err(|why| fail(&why, None))?;
    }
    let me = named_lock::Holder::me(
        &format!("lab {verb}: native windows"),
        &crate::paths::paths().repo,
    );
    let held = named_lock::take(
        &path,
        &me,
        wait,
        &mut |holder, waited| {
            let who = named_lock::name(holder, |_| 0, aterm_uds::process::pid_alive);
            say(&format!(
                "native window: waiting for {who}, which holds the account window lane ({} s)",
                waited.as_secs()
            ));
        },
        stopped,
    )
    .map_err(|why| match why {
        Refused::Stopped { signal, .. } => fail(&"interrupted while waiting", signal),
        Refused::Held(_) => fail(&"held by another lab run", None),
        Refused::Io(error) => fail(&error, None),
    })?;
    let held = WindowLane {
        path: path.clone(),
        held: held.into_sole_close_only(),
    };
    check(&held).map_err(|why| fail(&why, None))?;
    if stopped() {
        return Err(fail(&"not asked for: the run was interrupted", None));
    }
    *run = Some(held);
    Ok(())
}

/// Reserve the declared peak for a functional native window without asking for a quiet
/// machine or issuing Timing evidence. Unknown memory and unchecked admission start no work.
/// # Errors
/// NOT RUN for zero memory, `--admit now`, unavailable memory/ledger, capacity refusal or an
/// interrupted wait. Known insufficient memory waits for room with positive spare headroom.
pub fn functional(what: &str, memory_bytes: u64) -> Result<Claim, Fail> {
    let (mode, verb) = RUN
        .get()
        .cloned()
        .unwrap_or_else(|| (Admit::Wait, String::new()));
    functional_in(
        Ledger::user().as_ref(),
        &aterm_capacity::Live,
        Need::new(format!("lab {verb}: {what}"))
            .mem(memory_bytes)
            .margin(FUNCTIONAL_MARGIN),
        what,
        mode,
        Pace::default(),
        &mut |line| eprintln!("{line}"),
    )
}

struct FunctionalMachine<'a> {
    inner: &'a dyn Machine,
    unknown: Cell<bool>,
}

impl Machine for FunctionalMachine<'_> {
    fn read(&self, path: Option<&Path>) -> aterm_capacity::Reading {
        let mut reading = self.inner.read(path);
        if !matches!((reading.available, reading.physical), (Some(available), Some(physical)) if physical > 0 && available <= physical)
        {
            self.unknown.set(true);
            // Keep this look blocked until the admission loop observes the unknown stop.
            // A positive margin prevents the ordinary ledger's auto-ALONE rule here.
            reading.available = Some(0);
        }
        reading
    }
    fn procs(&self) -> Option<aterm_capacity::Procs> {
        self.inner.procs()
    }
    fn lineage(&self, pid: u32) -> Option<aterm_capacity::Procs> {
        self.inner.lineage(pid)
    }
}

fn functional_in(
    ledger: Option<&Ledger>,
    machine: &dyn Machine,
    need: Need,
    what: &str,
    mode: Admit,
    pace: Pace,
    say: &mut dyn FnMut(&str),
) -> Result<Claim, Fail> {
    let fail =
        |why: &dyn std::fmt::Display, signal| refused(what, "functional memory claim", why, signal);
    if crate::stop::check().is_err() {
        return Err(fail(&"not asked for: the run was interrupted", None));
    }
    if mode == Admit::Now {
        return Err(fail(
            &"not asked for: functional windows require --admit wait",
            None,
        ));
    }
    if need.mem == 0 || need.margin == 0 || need.quiet.is_some() || need.alone {
        return Err(fail(
            &"invalid: a positive peak and spare memory are required without quiet or ALONE",
            None,
        ));
    }
    let ledger = ledger.ok_or_else(|| {
        fail(
            &"unavailable: no home to keep the capacity ledger under",
            None,
        )
    })?;
    let machine = FunctionalMachine {
        inner: machine,
        unknown: Cell::new(false),
    };
    let _ = machine.read(None);
    if machine.unknown.get() {
        return Err(fail(&"unavailable: usable memory observation", None));
    }
    let result = aterm_capacity::admit_until(ledger, &machine, need, mode, pace, say, &|| {
        machine.unknown.get() || crate::stop::check().is_err()
    });
    if machine.unknown.get() {
        return Err(fail(
            &"unavailable: memory observation changed to unknown",
            None,
        ));
    }
    match result {
        Ok(claim) => Ok(claim),
        Err(error @ aterm_capacity::Error::Stopped { signal, .. }) => Err(fail(&error, signal)),
        Err(error) => Err(fail(&error, None)),
    }
}

/// Take `--admit now|wait` (or `--admit=now|wait`) out of `args` (any place before a `--`) for
/// the run of `verb`.
/// # Errors
/// A value that is neither, or none.
pub fn take(verb: &str, args: &mut Vec<String>) -> Result<(), Fail> {
    let mode = strip(args)?;
    // The normal entry calls take before dispatch; capture the original process before
    // any window or worker can start, even when the first window will be much later.
    let _ = WINDOW_PROCESS.set(std::process::id());
    let _ = RUN.set((mode, verb.to_owned()));
    Ok(())
}

/// The run's `--admit` mode, as [`take`] read it: [`Admit::Wait`] where none was given.
#[must_use]
pub fn mode() -> Admit {
    RUN.get().map_or(Admit::Wait, |r| r.0)
}

/// [`take`]'s parse: the mode, with the flag and its value removed from `args`.
pub(crate) fn strip(args: &mut Vec<String>) -> Result<Admit, Fail> {
    let end = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let joined = format!("{}=", Admit::FLAG);
    let Some(at) = args[..end]
        .iter()
        .position(|a| a == Admit::FLAG || a.starts_with(&joined))
    else {
        return Ok(Admit::Wait);
    };
    let (word, taken) = match args[at].strip_prefix(&joined) {
        Some(word) => (Some(word.to_owned()), 1),
        None => (args.get(at + 1).filter(|_| at + 1 < end).cloned(), 2),
    };
    let mode = word.as_deref().and_then(Admit::parse).ok_or_else(|| {
        Fail::usage(format!(
            "--admit takes now or wait, not `{}`",
            word.unwrap_or_default()
        ))
    })?;
    args.drain(at..at + taken);
    Ok(mode)
}

/// A measured Timing claim for `what`, part of this run's verb, as [`take`] admits it.
/// An unavailable ledger or unqualified admission starts no work.
/// # Errors
/// NOT RUN when an interrupt ended the wait, or came before it ([`quiet_in`]).
pub fn quiet(what: &str) -> Result<Claim, Fail> {
    let (mode, verb) = RUN
        .get()
        .cloned()
        .unwrap_or_else(|| (Admit::Wait, String::new()));
    quiet_in(
        Ledger::user().as_ref(),
        &aterm_capacity::Live,
        Need::timing(format!("lab {verb}: {what}")),
        what,
        mode,
        Pace::default(),
        &mut |line| eprintln!("{line}"),
    )
}

/// [`quiet`]'s admission of `need` for `what` in `ledger` on `machine`, at `pace`, its wait
/// lines said through `say`. Missing/failed ledger or unknown/forced admission is NOT RUN,
/// before any work or window starts.
/// # Errors
/// NOT RUN, carrying the signal, when an interrupt ended the wait: `what` started nothing, and
/// the interrupt is the run's ([`crate::stop::note`]), so it ends what it started its own way and
/// ends NOT RUN. An interrupt the run's own handling took before the wait began
/// ([`crate::stop::check`]) ends it too, before or during its first look: once a run is ending,
/// none of its work waits anew.
pub(crate) fn quiet_in(
    ledger: Option<&Ledger>,
    machine: &dyn Machine,
    need: Need,
    what: &str,
    mode: Admit,
    pace: Pace,
    say: &mut dyn FnMut(&str),
) -> Result<Claim, Fail> {
    let stopped = |e: &dyn std::fmt::Display, signal: Option<i32>| Fail {
        stopped: signal
            .inspect(|sig| crate::stop::note(*sig))
            .or_else(|| crate::stop::check().err().map(|s| s.0)),
        ..crate::linux::not_run(format!(
            "{what}: its quiet claim was {e}; it started nothing"
        ))
    };
    if crate::stop::check().is_err() {
        return Err(stopped(&"not asked for: the run was interrupted", None));
    }
    let Some(ledger) = ledger else {
        return Err(stopped(
            &"unavailable: no home to keep the capacity ledger under",
            None,
        ));
    };
    match aterm_capacity::admit_until(ledger, machine, need, mode, pace, say, &|| {
        crate::stop::check().is_err()
    }) {
        Ok(claim) => {
            claim
                .timing_admission()
                .map_err(|why| stopped(&why, None))?;
            Ok(claim)
        }
        Err(e @ aterm_capacity::Error::Stopped { signal, .. }) => Err(stopped(&e, signal)),
        Err(e) => Err(stopped(&e, None)),
    }
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
