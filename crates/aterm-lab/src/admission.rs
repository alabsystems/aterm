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
        .try_clone_fd()
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
    let checkout = std::env::current_dir().map_err(|why| fail(&why, None))?;
    let me = named_lock::Holder::me(&format!("lab {verb}: native windows"), &checkout);
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
        held: held.into_close_only(),
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
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::time::Instant;

    use aterm_capacity::{Fixed, Reading};
    use aterm_spec::wait::{MUST_HAPPEN, until_within};

    fn functional_need() -> Need {
        Need::new("functional fixture")
            .mem_gib(2)
            .margin(FUNCTIONAL_MARGIN)
    }

    #[test]
    fn functional_claim_reserves_memory_without_timing_or_quiet_evidence() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(scratch.path().join("ledger"));
        let mut reading = Reading::of_gib(48, 40, 14);
        reading.load1 = Some(200.0);
        let fixed = Fixed::new(reading);
        let mut claim = functional_in(
            Some(&ledger),
            &fixed,
            functional_need(),
            "functional fixture",
            Admit::Wait,
            Pace::default(),
            &mut |line| panic!("unexpected wait: {line}"),
        )
        .unwrap();
        let records = ledger.list().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].need.mem, 2 * aterm_capacity::floor::GIB);
        assert_eq!(records[0].need.margin, FUNCTIONAL_MARGIN);
        assert!(records[0].need.quiet.is_none());
        assert!(!records[0].need.covers);
        assert!(!records[0].held.unwrap().alone);
        assert_eq!(
            claim.timing_admission(),
            Err(aterm_capacity::TimingIssue::UnknownAdmission)
        );
        assert_eq!(
            claim.measurement_entry(),
            Err(aterm_capacity::TimingIssue::UnknownAdmission)
        );
        assert!(fixed.cpu_point().is_none());
        drop(claim);
        assert!(ledger.list().unwrap().is_empty());
    }

    #[test]
    fn functional_claim_refuses_now_zero_missing_and_unknown_before_work() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(scratch.path().join("ledger"));
        let healthy = Fixed::new(Reading::of_gib(48, 40, 14));
        for (ledger, need, mode) in [
            (Some(&ledger), functional_need(), Admit::Now),
            (
                Some(&ledger),
                Need::new("zero").margin(FUNCTIONAL_MARGIN),
                Admit::Wait,
            ),
            (None, functional_need(), Admit::Wait),
        ] {
            let error = functional_in(
                ledger,
                &healthy,
                need,
                "functional fixture",
                mode,
                Pace::default(),
                &mut |line| panic!("unexpected wait: {line}"),
            )
            .unwrap_err();
            assert_eq!(error.code, 3);
            assert!(error.msg.unwrap().contains("started nothing"));
        }
        for reading in [
            Reading::default(),
            Reading {
                available: None,
                ..Reading::of_gib(48, 40, 14)
            },
            Reading {
                physical: None,
                ..Reading::of_gib(48, 40, 14)
            },
        ] {
            let error = functional_in(
                Some(&ledger),
                &Fixed::new(reading),
                functional_need(),
                "functional fixture",
                Admit::Wait,
                Pace::default(),
                &mut |line| panic!("unknown memory must not wait or admit: {line}"),
            )
            .unwrap_err();
            assert_eq!(error.code, 3);
            assert!(error.msg.unwrap().contains("memory observation"));
        }
        assert!(ledger.list().unwrap().is_empty());
    }

    #[test]
    fn functional_shortage_waits_for_actual_room_and_never_uses_alone() {
        struct RoomAfterLine {
            ready: Cell<bool>,
            start: Instant,
        }
        impl Machine for RoomAfterLine {
            fn read(&self, _: Option<&Path>) -> Reading {
                assert!(self.start.elapsed() < MUST_HAPPEN, "admission hung");
                Reading::of_gib(48, if self.ready.get() { 40 } else { 0 }, 14)
            }
            fn procs(&self) -> Option<aterm_capacity::Procs> {
                None
            }
        }
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(scratch.path().join("ledger"));
        let machine = RoomAfterLine {
            ready: Cell::new(false),
            start: Instant::now(),
        };
        let claim = functional_in(
            Some(&ledger),
            &machine,
            functional_need(),
            "functional fixture",
            Admit::Wait,
            Pace {
                poll: std::time::Duration::ZERO,
                ..Pace::default()
            },
            &mut |line| {
                if machine.ready.get() {
                    assert!(line.contains("admitted"), "{line}");
                } else {
                    assert!(line.contains("waits") && line.contains("memory"), "{line}");
                    machine.ready.set(true);
                }
            },
        )
        .unwrap();
        assert!(
            machine.ready.get(),
            "a zero-room ALONE admission would skip the actual wait"
        );
        assert!(!ledger.list().unwrap()[0].held.unwrap().alone);
        drop(claim);
        assert!(ledger.list().unwrap().is_empty());
    }

    #[test]
    fn functional_memory_must_remain_known_during_admission() {
        struct Lost {
            reads: Cell<u32>,
        }
        impl Machine for Lost {
            fn read(&self, _: Option<&Path>) -> Reading {
                let reads = self.reads.get();
                self.reads.set(reads + 1);
                if reads == 0 {
                    Reading::of_gib(48, 40, 14)
                } else {
                    Reading::default()
                }
            }
            fn procs(&self) -> Option<aterm_capacity::Procs> {
                None
            }
        }
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(scratch.path().join("ledger"));
        let machine = Lost {
            reads: Cell::new(0),
        };
        let error = functional_in(
            Some(&ledger),
            &machine,
            functional_need(),
            "functional fixture",
            Admit::Wait,
            Pace {
                poll: std::time::Duration::ZERO,
                ..Pace::default()
            },
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(error.code, 3);
        assert!(error.msg.unwrap().contains("changed to unknown"));
        assert!(machine.reads.get() > 1);
        assert!(ledger.list().unwrap().is_empty());
    }

    #[test]
    fn window_lane_reuses_original_run_but_blocks_an_independent_run() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(scratch.path().join("ledger"));
        let original = Mutex::new(None);
        let other = Mutex::new(None);
        for _ in 0..2 {
            window_lane_in(
                &original,
                Some(&ledger),
                "functional fixture",
                Wait::No,
                &mut |line| panic!("same run must not wait: {line}"),
                &|| false,
            )
            .unwrap();
        }
        assert_eq!(
            window_lane_in(
                &other,
                Some(&ledger),
                "timing fixture",
                Wait::No,
                &mut |_| {},
                &|| false
            )
            .unwrap_err()
            .code,
            3
        );
        assert!(other.lock().unwrap().is_none());
        assert!(
            ledger.list().unwrap().is_empty(),
            "lane is not a memory or Timing claim"
        );
        drop(original);
        window_lane_in(
            &other,
            Some(&ledger),
            "timing fixture",
            Wait::No,
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    }

    #[test]
    fn window_lane_fails_closed_on_missing_storage_replacement_and_stopping() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(scratch.path().join("ledger"));
        let run = Mutex::new(None);
        for (ledger, stopped) in [(None, false), (Some(&ledger), true)] {
            assert_eq!(
                window_lane_in(&run, ledger, "fixture", Wait::No, &mut |_| {}, &|| stopped)
                    .unwrap_err()
                    .code,
                3
            );
            assert!(run.lock().unwrap().is_none());
        }
        window_lane_in(
            &run,
            Some(&ledger),
            "fixture",
            Wait::No,
            &mut |_| {},
            &|| false,
        )
        .unwrap();
        let path = ledger.dir().join(WINDOW_LANE);
        std::fs::rename(&path, ledger.dir().join("original-window.lock")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        assert_eq!(
            window_lane_in(
                &run,
                Some(&ledger),
                "fixture",
                Wait::No,
                &mut |_| {},
                &|| false
            )
            .unwrap_err()
            .code,
            3
        );
    }

    #[test]
    fn occupied_window_lane_wait_can_be_interrupted_without_starting_work() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(scratch.path().join("ledger"));
        let holder = Mutex::new(None);
        window_lane_in(
            &holder,
            Some(&ledger),
            "holder",
            Wait::No,
            &mut |_| {},
            &|| false,
        )
        .unwrap();
        let waiter = Mutex::new(None);
        let stop = Cell::new(false);
        let start = Instant::now();
        let error = window_lane_in(
            &waiter,
            Some(&ledger),
            "waiter",
            Wait::Block,
            &mut |line| {
                assert!(line.contains("account window lane"));
                stop.set(true);
            },
            &|| {
                assert!(start.elapsed() < MUST_HAPPEN, "lane wait hung");
                stop.get()
            },
        )
        .unwrap_err();
        assert_eq!(error.code, 3);
        assert!(stop.get());
        assert!(waiter.lock().unwrap().is_none());
        assert!(holder.lock().unwrap().is_some());
    }

    /// Set, the test binary is [`standin_waits_for_the_quiet_machine`], its ledger under this
    /// directory.
    const STANDIN: &str = "ATERM_LAB_QUIET_INTERRUPT_STANDIN";

    /// The machine both sides read: idle, so only the quiet claim the test holds keeps the
    /// stand-in waiting.
    struct Measured {
        fixed: Fixed,
        origin: Instant,
        points: std::cell::Cell<u64>,
    }
    impl Machine for Measured {
        fn read(&self, path: Option<&std::path::Path>) -> Reading {
            self.fixed.read(path)
        }
        fn procs(&self) -> Option<aterm_capacity::Procs> {
            self.fixed.procs()
        }
        fn cpu_point(&self) -> Option<aterm_capacity::CpuPoint> {
            let n = self.points.get() + 1;
            self.points.set(n);
            Some(aterm_capacity::CpuPoint {
                at: self.origin + std::time::Duration::from_secs(n),
                unix_ms: aterm_time::unix_millis()?,
                ticks: vec![aterm_sysprobe::CoreTicks {
                    user: n,
                    idle: n * 99,
                    ..Default::default()
                }],
            })
        }
    }
    fn machine() -> Measured {
        Measured {
            fixed: Fixed::new(Reading::of_gib(48, 40, 14)),
            origin: Instant::now(),
            points: std::cell::Cell::new(0),
        }
    }

    #[test]
    fn requested_timing_refuses_unknown_now_and_missing_ledger_before_work() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(scratch.path().join("ledger"));
        let fixed = Fixed::new(Reading::of_gib(48, 40, 14));
        for (ledger, mode) in [
            (Some(&ledger), Admit::Wait),
            (Some(&ledger), Admit::Now),
            (None, Admit::Wait),
        ] {
            let refused = quiet_in(
                ledger,
                &fixed,
                Need::timing("refusal control"),
                "refusal control",
                mode,
                Pace::default(),
                &mut |_| {},
            )
            .unwrap_err();
            assert_eq!(refused.code, 3);
            assert!(refused.msg.unwrap().contains("started nothing"));
        }
        let claim = quiet_in(
            Some(&ledger),
            &machine(),
            Need::timing("positive measured fixture"),
            "positive measured fixture",
            Admit::Wait,
            Pace {
                poll: std::time::Duration::ZERO,
                ..Pace::default()
            },
            &mut |_| {},
        )
        .unwrap();
        assert!(claim.timing_admission().is_ok());
        // The positive fixture remains explicit; Fixed itself still supplies no CPU proof.
        assert!(fixed.cpu_point().is_none());
    }

    /// The stand-in's quiet claim for the x86_64 GUI rows, looking every two minutes (twice the
    /// parent's hang detector, so only an interrupt ends the wait in time).
    fn gui_rows(dir: &std::path::Path, say: &mut dyn FnMut(&str)) -> Result<Claim, Fail> {
        quiet_in(
            Some(&Ledger::at(dir.join("ledger"))),
            &machine(),
            Need::timing("lab proof: x86_64 GUI rows"),
            "x86_64 GUI rows",
            Admit::Wait,
            Pace {
                poll: MUST_HAPPEN * 2,
                ..Pace::default()
            },
            say,
        )
    }

    /// Run by [`an_interrupt_ends_the_runs_quiet_claims`] in a process of its own, its
    /// interrupts the lab's own ([`crate::stop::install`]) with SIGINT at its default action, as
    /// a foreground run has it: waits for the quiet machine, writing `waiting` at its first
    /// wait line; once an interrupt ends that wait, a later quiet claim of the run is refused
    /// without a wait, and it exits as the lab's main would ([`crate::stop::status`]), saying
    /// what a FAIL found before, and a refusal, would have exited.
    #[test]
    fn standin_waits_for_the_quiet_machine() {
        let Some(dir) = std::env::var_os(STANDIN).map(PathBuf::from) else {
            return;
        };
        crate::sys::start_signal(libc::SIGINT, false);
        crate::stop::install();
        let waiting = dir.join("waiting");
        let first = gui_rows(&dir, &mut |line| {
            if !waiting.exists() {
                std::fs::write(&waiting, line).expect("said waiting");
            }
        })
        .expect_err("an interrupt ended the wait");
        assert!(
            crate::stop::ended_a_wait(),
            "the wait handed its interrupt on"
        );
        let sig = first.stopped.expect("the interrupt's signal");
        assert_eq!(crate::stop::check(), Err(crate::stop::Stopped(sig)));
        let again = gui_rows(&dir, &mut |line| panic!("a second wait: {line}"))
            .expect_err("no quiet claim once the run is ending");
        println!("{}", first.msg.unwrap_or_default());
        println!("{}", again.msg.unwrap_or_default());
        println!(
            "a FAIL found before exits {}, a refusal {}",
            crate::stop::status(1, true),
            crate::stop::status(1, false)
        );
        std::process::exit(crate::stop::status(first.code, false));
    }

    /// A stand-in, killed when it is dropped still running (a test that failed before it
    /// ended), so none outlives its test.
    struct Standin(aterm_uds::spawnfd::Child);

    impl Drop for Standin {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(None)) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    /// The lab's quiet claim (a windowed instance's, the Linux lane's GUI rows') ends on SIGTERM,
    /// or SIGINT, as NOT RUN (exit 3) well inside the one-minute hang detector (the time is
    /// reported), naming the signal; the interrupt becomes the run's, so its next quiet claim is
    /// refused without a wait, and a FAIL the verb found before stays exit 1 (a refusal's exit 1
    /// does not). Only the test's claim is left in the ledger.
    #[test]
    fn an_interrupt_ends_the_runs_quiet_claims() {
        for (sig, word) in [(libc::SIGTERM, "SIGTERM"), (libc::SIGINT, "SIGINT")] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("lab-quiet-int-")
                .tempdir()
                .expect("a scratch ledger");
            let ledger = Ledger::at(dir.path().join("ledger"));
            let held = aterm_capacity::admit_with(
                &ledger,
                &machine(),
                Need::new("holder").quiet(28.0),
                Admit::Wait,
                Pace::default(),
                &mut |_| {},
            )
            .expect("the holder's claim");
            let mut child = Standin(
                aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
                    .args([
                        "--exact",
                        "admission::tests::standin_waits_for_the_quiet_machine",
                        "--test-threads=1",
                        "--nocapture",
                    ])
                    .env(STANDIN, dir.path())
                    .stdin(aterm_uds::spawnfd::Stdio::null())
                    .stdout(aterm_uds::spawnfd::Stdio::piped())
                    .stderr(aterm_uds::spawnfd::Stdio::null())
                    .spawn()
                    .expect("the stand-in"),
            );
            until_within(MUST_HAPPEN, "the stand-in's wait", || {
                std::fs::metadata(dir.path().join("waiting")).ok()
            });
            let pid = i32::try_from(child.0.id()).expect("a pid");
            let sent = Instant::now();
            crate::sys::kill(pid, sig).expect("signalled the stand-in");
            let status = until_within(MUST_HAPPEN, "the stand-in's end", || {
                child.0.try_wait().ok().flatten()
            });
            let took = sent.elapsed();
            let mut out = String::new();
            if let Some(mut o) = child.0.stdout.take() {
                let _ = std::io::Read::read_to_string(&mut o, &mut out);
            }
            assert_eq!(status.code(), Some(3), "{word}: {status}\n{out}");
            assert!(
                out.contains(&format!(
                    "NOT RUN: x86_64 GUI rows: its quiet claim was interrupted ({word}) while it \
                     waited"
                )),
                "{word}: {out}"
            );
            assert!(
                out.contains("NOT RUN: x86_64 GUI rows: its quiet claim was not asked for"),
                "{word}: {out}"
            );
            assert!(
                out.contains("a FAIL found before exits 1, a refusal 3"),
                "{word}: {out}"
            );
            let left: Vec<String> = ledger
                .dir()
                .read_dir()
                .expect("the ledger")
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".claim") || n.ends_with(".wait"))
                .collect();
            assert_eq!(left.len(), 1, "{word}: only the holder's claim: {left:?}");
            eprintln!("{word} ended the lab's quiet claim in {took:?}");
            drop(held);
        }
    }

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|w| (*w).to_owned()).collect()
    }

    /// `--admit` is taken out wherever it stands before a `--`, and a word after `--` belongs to
    /// the program the verb runs.
    #[test]
    fn admit_is_taken_out_before_the_verb_reads_its_arguments() {
        let mut a = words(&["--window", "--admit", "now", "--trials", "3"]);
        assert_eq!(strip(&mut a).ok(), Some(Admit::Now));
        assert_eq!(a, words(&["--window", "--trials", "3"]));
        let mut a = words(&["--window"]);
        assert_eq!(strip(&mut a).ok(), Some(Admit::Wait));
        assert_eq!(a, words(&["--window"]));
        let mut a = words(&["--", "prog", "--admit", "now"]);
        assert_eq!(strip(&mut a).ok(), Some(Admit::Wait));
        assert_eq!(a.len(), 4, "the program's own words stay");
        let mut a = words(&["--admit=now", "proof"]);
        assert_eq!(strip(&mut a).ok(), Some(Admit::Now));
        assert_eq!(a, words(&["proof"]));
        assert!(strip(&mut words(&["--admit=soon"])).is_err());
        assert!(strip(&mut words(&["--admit", "soon"])).is_err());
        assert!(strip(&mut words(&["--admit"])).is_err());
        assert!(strip(&mut words(&["--admit", "--", "now"])).is_err());
    }
}
