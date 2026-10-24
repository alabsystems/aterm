// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Background watcher for macOS `syspolicyd` worker pool saturation and its dev stand-ins.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use aterm_uds::program_check::Look;
use aterm_vitals::program_check::{Judge, Reading, State};

use crate::harness_host::HostNote;

/// What the watch knows: the judge and its last reading.
#[derive(Debug, Default)]
pub(crate) struct Board {
    judge: Judge,
    last: Option<Reading>,
    /// Whether the window has been notified that the check is not stuck.
    told_unstuck: bool,
    /// `(workers, limit)` reported at the stuck edge and repeated while stuck.
    told_stuck: Option<(u64, Option<u64>)>,
}

impl Board {
    /// A board nothing has read: not stuck.
    pub(crate) const fn new() -> Self {
        Self {
            judge: Judge::new(),
            last: None,
            told_unstuck: false,
            told_stuck: None,
        }
    }
}

/// Process-wide `syspolicyd` check board.
pub(crate) static BOARD: Mutex<Board> = Mutex::new(Board::new());

/// Whether development stand-ins are enabled in this build.
const STAND_IN_ENABLED: bool = cfg!(any(debug_assertions, feature = "dev-seams"));

/// Whether the background watcher thread is running.
static WATCHING: AtomicBool = AtomicBool::new(false);

/// Whether `program-check full` overrides daemon readings as saturated.
static FULL_STAND_IN: AtomicBool = AtomicBool::new(false);

/// PID stood in by `program-check held <pid>`, or 0 for none.
static HELD_STAND_IN: AtomicU32 = AtomicU32::new(0);

const USAGE: &str = "usage: program-check [full|live|held <pid>|held -]";

/// Returns the PID stood in as held at macOS's program check, if any.
pub(crate) fn held_stand_in() -> Option<u32> {
    if !STAND_IN_ENABLED {
        return None;
    }
    match HELD_STAND_IN.load(Ordering::SeqCst) {
        0 => None,
        pid => Some(pid),
    }
}

/// Whether macOS's program check holds `pid` now: the development build's stand-in first, as the
/// jobs pass reads it, then the kernel's own reading ([`aterm_uds::process::at_program_check`]).
pub(crate) fn held_now(pid: u32) -> bool {
    held_stand_in() == Some(pid)
        || i32::try_from(pid)
            .ok()
            .and_then(aterm_uds::process::at_program_check)
            == Some(true)
}

/// The development build's held stand-in set to a pid until it drops, for a test of what reads it.
#[cfg(all(test, target_os = "macos"))]
pub(crate) struct HeldStandIn;

#[cfg(all(test, target_os = "macos"))]
impl HeldStandIn {
    pub(crate) fn of(pid: u32) -> Self {
        HELD_STAND_IN.store(pid, Ordering::SeqCst);
        Self
    }
}

#[cfg(all(test, target_os = "macos"))]
impl Drop for HeldStandIn {
    fn drop(&mut self) {
        HELD_STAND_IN.store(0, Ordering::SeqCst);
    }
}

/// Milliseconds elapsed since the watcher's first reading.
fn now_ms() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    u64::try_from(EPOCH.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Folds `look` at `at_ms` into `board` and returns any [`HostNote`] owed to the window.
pub(crate) fn step(board: &mut Board, at_ms: u64, look: Look) -> Option<HostNote> {
    let was_stuck = matches!(board.judge.state(), State::Stuck { .. });
    let now = match look {
        Look::Read(reading) => {
            board.last = Some(reading);
            board.judge.absorb(at_ms, Some(&reading))
        }
        Look::NoneRunning => {
            board.last = None;
            board.judge.absorb_none_running()
        }
        Look::Unread => board.judge.absorb(at_ms, None),
    };
    match now {
        State::Stuck { since_ms } => {
            if was_stuck {
                return board
                    .told_stuck
                    .map(|(workers, limit)| HostNote::ProgramCheckStuck { workers, limit });
            }
            board.told_unstuck = false;
            let last = board.last?;
            board.told_stuck = Some((last.workers, last.limit));
            aterm_log::error!(
                "program check: syspolicyd (pid {}) has been at its workqueue limit for {} s \
                 ({} threads, constrained limit {}); no new program can start until the Mac \
                 restarts",
                last.daemon.pid,
                at_ms.saturating_sub(since_ms) / 1000,
                last.workers,
                last.limit.map_or_else(|| "-".to_owned(), |l| l.to_string()),
            );
            Some(HostNote::ProgramCheckStuck {
                workers: last.workers,
                limit: last.limit,
            })
        }
        _ if was_stuck || (now == State::Answering && !board.told_unstuck) => {
            board.told_unstuck = true;
            board.told_stuck = None;
            if was_stuck {
                aterm_log::info!("program check: no longer stuck ({})", now.as_str());
            }
            Some(HostNote::ProgramCheckUnstuck)
        }
        _ => None,
    }
}

/// Overrides a live daemon reading as at-limit when `full` is active.
fn stood_in(look: Look, full: bool) -> Look {
    match look {
        Look::Read(reading) if full => Look::Read(Reading {
            at_limit: true,
            workers: reading
                .limit
                .map_or(reading.workers, |limit| reading.workers.max(limit)),
            ..reading
        }),
        other => other,
    }
}

/// What the verb asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    /// Read the board.
    Read,
    /// `full` (true) or `live` (false): the pool's stand-in.
    Pool(bool),
    /// `held <pid>` or `held -` (`None`): the held program's stand-in.
    Held(Option<u32>),
}

/// What `held <pid>` answers for a pid no live process of this user holds: a start is read only for those (another user's thread list is not read, a zombie has none), so the stand-in would count nowhere.
const NOT_OURS: &str = "program-check held <pid>: no live process of this user has that pid, and a start is read \
     only for those";

/// Whether `pid` is a live process of this user ([`NOT_OURS`]).
fn ours(pid: u32) -> bool {
    #[cfg(target_vendor = "apple")]
    {
        i32::try_from(pid)
            .ok()
            .and_then(aterm_uds::process::read_short_info)
            .is_some_and(|i| i.pbsi_uid == aterm_uds::peer::our_uid())
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = pid;
        false
    }
}

/// Parses `program-check` arguments and validates build/platform gates, then that a `held <pid>` names a live process of this user (`ours`).
fn parse(
    rest: &str,
    injection: bool,
    watching: bool,
    ours: impl Fn(u32) -> bool,
) -> Result<Ask, &'static str> {
    let ask = match rest.split_whitespace().collect::<Vec<_>>().as_slice() {
        [] => return Ok(Ask::Read),
        ["full"] => Ask::Pool(true),
        ["live"] => Ask::Pool(false),
        ["held", "-"] => Ask::Held(None),
        ["held", pid] => match pid.parse::<u32>() {
            Ok(pid) if pid > 0 => Ask::Held(Some(pid)),
            _ => return Err(USAGE),
        },
        _ => return Err(USAGE),
    };
    if !injection {
        return Err("program-check full|live|held needs a development build (dev-seams)");
    }
    if !watching {
        return Err("program-check full|live|held: this instance runs no stuck check (not macOS)");
    }
    if let Ask::Held(Some(pid)) = ask
        && !ours(pid)
    {
        return Err(NOT_OURS);
    }
    Ok(ask)
}

/// Handles the `program-check [full|live|held <pid>|held -]` control verb.
pub(crate) fn cmd(rest: &str) -> String {
    match parse(
        rest,
        STAND_IN_ENABLED,
        WATCHING.load(Ordering::SeqCst),
        ours,
    ) {
        Ok(Ask::Pool(full)) => FULL_STAND_IN.store(full, Ordering::SeqCst),
        Ok(Ask::Held(pid)) => HELD_STAND_IN.store(pid.unwrap_or(0), Ordering::SeqCst),
        Ok(Ask::Read) => {}
        Err(refusal) => return format!("ERR {refusal}\n"),
    }
    let reading = if FULL_STAND_IN.load(Ordering::SeqCst) {
        "full"
    } else {
        "live"
    };
    let held = held_stand_in().map_or_else(|| "-".to_owned(), |pid| pid.to_string());
    format!("OK reading={reading} held={held} {}\n", status_fields())
}

/// Returns whether `board` currently judges `syspolicyd` stuck.
pub(crate) fn stuck(board: &Mutex<Board>) -> bool {
    matches!(
        board
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .judge
            .state(),
        State::Stuck { .. }
    )
}

/// Formats the `syspolicyd=*` fields for `status`.
pub(crate) fn status_fields() -> String {
    let b = BOARD.lock().unwrap_or_else(PoisonError::into_inner);
    let state = b.judge.state();
    let workers = b.last.map_or_else(
        || "-".to_owned(),
        |r| {
            format!(
                "{}/{}",
                r.workers,
                r.limit.map_or_else(|| "-".to_owned(), |l| l.to_string())
            )
        },
    );
    let full_ms = state.since_ms().map_or_else(
        || "-".to_owned(),
        |since| now_ms().saturating_sub(since).to_string(),
    );
    format!(
        "syspolicyd={} syspolicyd_workers={workers} syspolicyd_full_ms={full_ms}",
        state.as_str()
    )
}

/// Uses the production disk tick regardless of `ATERM_DEV_FAST_TIMERS` to avoid `ps` storms.
const fn every() -> std::time::Duration {
    crate::harness_host::DISK_TICK
}

/// Starts the background `syspolicyd` watcher thread on macOS.
pub(crate) fn start(note: Arc<dyn Fn(HostNote) + Send + Sync>) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name(format!(
            "{}program-check",
            crate::harness_host::THREAD_PREFIX
        ))
        .spawn(move || {
            loop {
                std::thread::sleep(every());
                let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let look = stood_in(
                        aterm_uds::program_check::look(aterm_uds::program_check::PS_HANG),
                        FULL_STAND_IN.load(Ordering::SeqCst),
                    );
                    step(
                        &mut BOARD.lock().unwrap_or_else(PoisonError::into_inner),
                        now_ms(),
                        look,
                    )
                }));
                match read {
                    Ok(Some(owed)) => note(owed),
                    Ok(None) => {}
                    Err(_) => aterm_log::warn!(
                        "program check: a read stopped after an internal panic; the next is \
                         taken at its time"
                    ),
                }
            }
        });
    match spawned {
        Ok(_) => WATCHING.store(true, Ordering::SeqCst),
        Err(e) => aterm_log::warn!("program check: could not start its watch: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_vitals::program_check::{Daemon, STUCK_AFTER_MS};

    const D: Daemon = Daemon {
        pid: 690,
        start_s: 1_791_000_000,
        start_us: 1,
    };
    const MIN: u64 = 60_000;

    fn on(daemon: Daemon, workers: u64, at_limit: bool) -> Look {
        Look::Read(Reading {
            daemon,
            workers,
            at_limit,
            limit: Some(90),
        })
    }

    fn reading(workers: u64, at_limit: bool) -> Look {
        on(D, workers, at_limit)
    }

    fn word(note: Option<&HostNote>) -> &'static str {
        match note {
            None => "-",
            Some(HostNote::ProgramCheckStuck { .. }) => "stuck",
            Some(HostNote::ProgramCheckUnstuck) => "unstuck",
            Some(_) => "other",
        }
    }

    fn words(looks: &[(u64, Look)]) -> Vec<&'static str> {
        let mut board = Board::default();
        looks
            .iter()
            .map(|(at, look)| word(step(&mut board, *at, *look).as_ref()))
            .collect()
    }

    #[test]
    fn the_window_hears_the_first_answer_the_stuck_edge_and_the_end() {
        let mut board = Board::default();
        let notes: Vec<Option<HostNote>> = [
            (0, reading(6, false)),
            (MIN, reading(6, false)),
            (2 * MIN, reading(95, true)),
            (2 * MIN + STUCK_AFTER_MS, reading(95, true)),
            (3 * MIN + STUCK_AFTER_MS, reading(95, true)),
            (4 * MIN + STUCK_AFTER_MS, Look::Unread),
            (5 * MIN + STUCK_AFTER_MS, reading(4, false)),
            (6 * MIN + STUCK_AFTER_MS, reading(4, false)),
        ]
        .into_iter()
        .map(|(at, look)| step(&mut board, at, look))
        .collect();
        let said: Vec<&str> = notes.iter().map(Option::as_ref).map(word).collect();
        assert_eq!(
            said,
            [
                "unstuck", "-", "-", "stuck", "stuck", "stuck", "unstuck", "-"
            ]
        );
        for told in &notes[3..6] {
            assert_eq!(
                *told,
                Some(HostNote::ProgramCheckStuck {
                    workers: 95,
                    limit: Some(90),
                })
            );
        }
        assert_eq!(
            words(&[
                (0, reading(95, true)),
                (STUCK_AFTER_MS - 1, reading(95, true)),
                (STUCK_AFTER_MS, reading(5, false)),
            ]),
            ["-", "-", "unstuck"]
        );
    }

    /// Replacing or stopping a stuck daemon clears the alert.
    #[test]
    fn the_alert_ends_when_the_stuck_daemon_does() {
        let again = Daemon {
            pid: 489,
            start_s: 1_792_597_850,
            start_us: 7,
        };
        assert_eq!(
            words(&[
                (0, reading(95, true)),
                (STUCK_AFTER_MS, reading(95, true)),
                (STUCK_AFTER_MS + MIN, on(again, 95, true)),
                (2 * STUCK_AFTER_MS + MIN, on(again, 95, true)),
                (2 * STUCK_AFTER_MS + 2 * MIN, Look::NoneRunning),
                (2 * STUCK_AFTER_MS + 3 * MIN, Look::NoneRunning),
            ]),
            ["-", "stuck", "unstuck", "stuck", "unstuck", "-"]
        );
    }

    #[test]
    fn the_status_fields_say_dash_until_read() {
        let b = Board::default();
        assert_eq!(b.judge.state().as_str(), "-");
        let s = status_fields();
        assert!(s.starts_with("syspolicyd="), "{s}");
        assert!(s.contains(" syspolicyd_workers="), "{s}");
        assert!(s.contains(" syspolicyd_full_ms="), "{s}");
    }

    /// The `full` stand-in drives a healthy reading to stuck, and `live` clears it.
    #[test]
    fn the_full_stand_in_drives_the_alert_and_live_ends_it() {
        let healthy = reading(2, false);
        assert_eq!(
            stood_in(healthy, true),
            on(D, 90, true),
            "the daemon kept, the flag set, its threads raised to the limit"
        );
        assert_eq!(stood_in(reading(95, true), true), reading(95, true));
        assert_eq!(
            stood_in(healthy, false),
            healthy,
            "CONTROL: live reads as read"
        );
        let no_limit = Look::Read(Reading {
            daemon: D,
            workers: 3,
            at_limit: false,
            limit: None,
        });
        assert_eq!(
            stood_in(no_limit, true),
            Look::Read(Reading {
                daemon: D,
                workers: 3,
                at_limit: true,
                limit: None,
            })
        );
        assert_eq!(stood_in(Look::NoneRunning, true), Look::NoneRunning);
        assert_eq!(stood_in(Look::Unread, true), Look::Unread);
        assert_eq!(
            words(&[
                (0, stood_in(healthy, false)),
                (MIN, stood_in(healthy, true)),
                (MIN + STUCK_AFTER_MS, stood_in(healthy, true)),
                (2 * MIN + STUCK_AFTER_MS, stood_in(healthy, false)),
            ]),
            ["unstuck", "-", "stuck", "unstuck"]
        );
    }

    #[test]
    fn the_verb_reads_bare_and_stands_in_only_where_a_look_reads() {
        let any = |_| true;
        assert_eq!(parse("", false, false, any), Ok(Ask::Read));
        assert_eq!(parse("  ", true, true, any), Ok(Ask::Read));
        assert_eq!(parse("full", true, true, any), Ok(Ask::Pool(true)));
        assert_eq!(parse(" live ", true, true, any), Ok(Ask::Pool(false)));
        assert_eq!(parse("stuck", true, true, any), Err(USAGE));
        assert_eq!(parse("full live", true, true, any), Err(USAGE));
        assert!(parse("full", false, true, any).is_err_and(|e| e.contains("development build")));
        assert!(parse("live", true, false, any).is_err_and(|e| e.contains("runs no stuck check")));
    }

    /// `held_now` holds no program the kernel reads as past its start (this process, which runs
    /// this test) and none left unread (pid 0, which is never read; pid 1, launchd, root's).
    #[test]
    fn held_now_holds_no_program_the_kernel_reads_started_or_cannot_read() {
        assert!(
            !held_now(std::process::id()),
            "this test runs, so it is past its start"
        );
        for unread in [0, 1] {
            assert!(
                !held_now(unread),
                "pid {unread}: no process of this user's, so unread and not held"
            );
        }
    }

    /// The held stand-in needs a development build and a pid of a live process of this user
    /// (the only kind a start is read for, so any other would count nowhere); `held -` needs no
    /// pid. This process is one of this user's; pid 1 (launchd, root's) is not.
    #[test]
    fn the_held_stand_in_needs_a_development_build_and_a_pid() {
        let any = |_| true;
        let only = |mine: u32| move |pid| pid == mine;
        assert_eq!(
            parse("held 25311", true, true, only(25311)),
            Ok(Ask::Held(Some(25311)))
        );
        assert_eq!(
            parse(" held  - ", true, true, |_| false),
            Ok(Ask::Held(None))
        );
        for refused in ["held", "held gh", "held 0", "held -1", "held 7 8"] {
            assert_eq!(parse(refused, true, true, any), Err(USAGE), "{refused:?}");
        }
        assert!(
            parse("held 25311", false, true, any).is_err_and(|e| e.contains("development build"))
        );
        assert!(
            parse("held -", true, false, any).is_err_and(|e| e.contains("runs no stuck check"))
        );
        assert_eq!(
            parse("held 25311", true, true, only(7)),
            Err(NOT_OURS),
            "a pid no live process of this user holds"
        );
        #[cfg(target_vendor = "apple")]
        {
            assert!(ours(std::process::id()), "this process is this user's");
            assert!(
                aterm_uds::peer::our_uid() == 0 || !ours(1),
                "launchd is root's"
            );
        }
    }
}
