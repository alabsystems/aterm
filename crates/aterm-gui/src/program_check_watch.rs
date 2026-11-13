// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Background watcher for macOS `syspolicyd` worker pool saturation and its dev stand-ins.
//!
//! A look runs one setuid `ps` of the daemon's pool, so the watch looks only while there is
//! something to judge ([`Cadence`]): once a tick after the window starts (the first answer, which
//! ends a stuck alert a restart cleared), every tick while the pool reads full (the stuck verdict
//! times the full run from its first full reading, and only a look sees it end), and at a ring
//! ([`want`]): the jobs pass rings each pass while a program of this user waits at the check
//! ([`last_read_for`]), and an ask for the board (`status`, `program-check`) rings when its last
//! look is a tick old or more ([`ask`]). A ring never brings looks closer than a tick apart. At
//! rest (the pool below its limit and nothing waiting) the thread parks with no timeout: a full
//! pool strands only a program that starts, and such a start is a held row in the jobs pass.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use aterm_uds::program_check::Look;
use aterm_vitals::program_check::{Daemon, Judge, Reading, State};

use crate::harness_host::HostNote;

/// What the watch knows: the judge and its last reading.
#[derive(Debug, Default)]
pub(crate) struct Board {
    judge: Judge,
    last: Option<Reading>,
    /// When the last look read [`Self::last`], on [`now_ms`]'s clock: `None` once a later look
    /// read nothing or found no `syspolicyd` running.
    read_ms: Option<u64>,
    /// The daemon the looks have read since none or another ran, and when the first of them read
    /// it, on [`now_ms`]'s clock, which stops while the Mac sleeps.
    seen: Option<(Daemon, u64)>,
    /// Whether the window has been notified that the check is not stuck.
    told_unstuck: bool,
    /// `(workers, limit)` reported at the stuck edge and repeated while stuck.
    told_stuck: Option<(u64, Option<u64>)>,
    /// When the last look was taken, whatever it read, on [`now_ms`]'s clock: what an ask
    /// judges stale ([`stale`]).
    looked_ms: Option<u64>,
}

impl Board {
    /// A board nothing has read: not stuck.
    pub(crate) const fn new() -> Self {
        Self {
            judge: Judge::new(),
            last: None,
            read_ms: None,
            seen: None,
            told_unstuck: false,
            told_stuck: None,
            looked_ms: None,
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
/// The stand-in is one pid, so its holders take [`STAND_IN_TESTS`] for their whole life: a second
/// test waits for the first to drop rather than replacing its pid, and no drop empties a stand-in
/// another test still holds.
#[cfg(all(test, target_os = "macos"))]
pub(crate) struct HeldStandIn {
    _serial: std::sync::MutexGuard<'static, ()>,
}

/// Serialises every [`HeldStandIn`].
#[cfg(all(test, target_os = "macos"))]
static STAND_IN_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(all(test, target_os = "macos"))]
impl HeldStandIn {
    pub(crate) fn of(pid: u32) -> Self {
        let serial = STAND_IN_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        HELD_STAND_IN.store(pid, Ordering::SeqCst);
        Self { _serial: serial }
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
    board.looked_ms = Some(at_ms);
    let now = match look {
        Look::Read(reading) => {
            board.last = Some(reading);
            board.read_ms = Some(at_ms);
            if board.seen.is_none_or(|(d, _)| d != reading.daemon) {
                board.seen = Some((reading.daemon, at_ms));
            }
            board.judge.absorb(at_ms, Some(&reading))
        }
        Look::NoneRunning => {
            board.last = None;
            board.read_ms = None;
            board.seen = None;
            board.judge.absorb_none_running()
        }
        Look::Unread => {
            board.read_ms = None;
            board.judge.absorb(at_ms, None)
        }
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
pub(crate) fn stood_in(look: Look, full: bool) -> Look {
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
        ["held", pid] => match pid
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| pid.parse::<u32>().ok())
            .flatten()
        {
            Some(pid) if pid > 0 => Ask::Held(Some(pid)),
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
        Ok(Ask::Pool(full)) => {
            FULL_STAND_IN.store(full, Ordering::SeqCst);
            want();
        }
        Ok(Ask::Held(pid)) => HELD_STAND_IN.store(pid.unwrap_or(0), Ordering::SeqCst),
        Ok(Ask::Read) => ask(),
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

/// Whether `board` holds new starts back: macOS's program check reads full or stuck, so a start
/// aterm can put off waits for it. Unread (off macOS, or before the first reading) and answering
/// hold nothing.
pub(crate) fn starts_held(board: &Mutex<Board>) -> bool {
    matches!(
        board
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .judge
            .state(),
        State::Full { .. } | State::Stuck { .. }
    )
}

/// Formats the `syspolicyd=*` fields for `status`: the board as the last look left it, and a
/// ring for a fresh look when that look is a tick old or more ([`ask`]), so a caller that asks
/// again reads one.
pub(crate) fn status_fields() -> String {
    ask();
    let b = BOARD.lock().unwrap_or_else(PoisonError::into_inner);
    fields(&b, now_ms(), aterm_sysprobe::jobs::now_us())
}

/// `status`'s `syspolicyd=*` fields from `b` at `now_ms` ([`now_ms`]'s clock) and `wall_us`
/// (µs since the epoch). The daemon's age is `-` while the last look read nothing, since no
/// look says it still runs. `syspolicyd_age_ms=` is how long ago the last look was taken
/// (`-` before one): at rest the board is that look's, however old.
fn fields(b: &Board, now_ms: u64, wall_us: u64) -> String {
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
        |since| now_ms.saturating_sub(since).to_string(),
    );
    let daemon = b.last.map(|r| r.daemon);
    let pid = daemon.map_or_else(|| "-".to_owned(), |d| d.pid.to_string());
    let up_ms = daemon
        .filter(|_| b.read_ms.is_some())
        .and_then(|d| wall_us.checked_sub(d.started_us()))
        .map_or_else(|| "-".to_owned(), |up| (up / 1000).to_string());
    let age_ms = b.looked_ms.map_or_else(
        || "-".to_owned(),
        |at| now_ms.saturating_sub(at).to_string(),
    );
    format!(
        "syspolicyd={} syspolicyd_workers={workers} syspolicyd_full_ms={full_ms} \
         syspolicyd_pid={pid} syspolicyd_up_ms={up_ms} syspolicyd_age_ms={age_ms}",
        state.as_str()
    )
}

/// The `syspolicyd` a fresh look read: the daemon, whether its pool read full, and how long the
/// looks have read it awake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RunningCheck {
    /// The daemon the look read.
    pub(crate) daemon: Daemon,
    /// Whether its pool read full ([`Reading::full`]).
    pub(crate) pool_full: bool,
    /// How long since the first look that read it, ms on a clock that stops while the Mac
    /// sleeps: a lower bound of its awake age, where its start (a wall clock) counts a sleep.
    pub(crate) awake_ms: u64,
}

/// The check `b`'s last look read, while that look read it and is at most two looks old at
/// `now_ms`.
fn check_of(b: &Board, now_ms: u64) -> Option<RunningCheck> {
    let read_ms = b.read_ms?;
    if u128::from(now_ms.saturating_sub(read_ms)) > 2 * every().as_millis() {
        return None;
    }
    let r = b.last?;
    Some(RunningCheck {
        daemon: r.daemon,
        pool_full: r.full(),
        awake_ms: b
            .seen
            .filter(|(d, _)| *d == r.daemon)
            .map_or(0, |(_, since)| now_ms.saturating_sub(since)),
    })
}

/// The check `b`'s last look read, whatever its age: what stands while nothing waits at the
/// check, for a pass with no held row to judge (its daemon and whether its pool read full).
fn standing_of(b: &Board, now_ms: u64) -> Option<RunningCheck> {
    b.read_ms?;
    let r = b.last?;
    Some(RunningCheck {
        daemon: r.daemon,
        pool_full: r.full(),
        awake_ms: b
            .seen
            .filter(|(d, _)| *d == r.daemon)
            .map_or(0, |(_, since)| now_ms.saturating_sub(since)),
    })
}

/// The process-wide board for the jobs pass, which says whether a program of this user waits at
/// the check (`waiting`): while one does, the watch is rung ([`want`]) and only a fresh look is
/// handed ([`check_of`]), since its rows are judged against it; while none does, the look that
/// stands ([`standing_of`]), against which nothing is judged.
pub(crate) fn last_read_for(waiting: bool) -> Option<RunningCheck> {
    read_for(&BOARD, &BELL, now_ms(), waiting)
}

/// [`last_read_for`] over `board` and `bell` at `now_ms`.
fn read_for(board: &Mutex<Board>, bell: &Bell, now_ms: u64, waiting: bool) -> Option<RunningCheck> {
    if waiting {
        bell.ring();
    }
    let b = board.lock().unwrap_or_else(PoisonError::into_inner);
    if waiting {
        check_of(&b, now_ms)
    } else {
        standing_of(&b, now_ms)
    }
}

/// Whether `b`'s last look is a tick old or more at `now_ms`, or none was taken: an ask then
/// rings for a fresh one ([`ask`]).
fn stale(b: &Board, now_ms: u64) -> bool {
    b.looked_ms
        .is_none_or(|at| u128::from(now_ms.saturating_sub(at)) >= every().as_millis())
}

/// An ask for the board (`status`, `program-check`): rings the watch when its last look is stale
/// ([`stale`]).
fn ask() {
    ask_on(&BOARD, &BELL, now_ms());
}

/// [`ask`] over `board` and `bell` at `now_ms`.
fn ask_on(board: &Mutex<Board>, bell: &Bell, now_ms: u64) {
    let stale = stale(
        &board.lock().unwrap_or_else(PoisonError::into_inner),
        now_ms,
    );
    if stale {
        bell.ring();
    }
}

/// Rings the watch for a look ([`Cadence::rung`]): at once when its last look is a tick old or
/// more, else a tick after that look.
pub(crate) fn want() {
    BELL.ring();
}

/// When the watch looks, on [`now_ms`]'s clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cadence {
    /// The next look; `None` at rest, where only a ring brings one.
    next_ms: Option<u64>,
    /// When the last look was taken.
    last_ms: Option<u64>,
}

impl Cadence {
    /// A watch started at `at_ms`: its first look a tick later.
    pub(crate) fn started(at_ms: u64) -> Self {
        Self {
            next_ms: Some(at_ms.saturating_add(tick_ms())),
            last_ms: None,
        }
    }

    /// A ring at `now_ms`: a look at once when the last is a tick old or more (or none was
    /// taken), else a tick after the last, and never later than a look already due.
    pub(crate) fn rung(&mut self, now_ms: u64) {
        let at = self
            .last_ms
            .map_or(now_ms, |last| last.saturating_add(tick_ms()).max(now_ms));
        self.next_ms = Some(self.next_ms.map_or(at, |next| next.min(at)));
    }

    /// A look taken at `at_ms` left the judge at `state`: the next a tick later while the pool
    /// reads full or stuck, else none until a ring.
    pub(crate) fn looked(&mut self, at_ms: u64, state: State) {
        self.last_ms = Some(at_ms);
        self.next_ms = matches!(state, State::Full { .. } | State::Stuck { .. })
            .then(|| at_ms.saturating_add(tick_ms()));
    }

    /// How long from `now_ms` until the next look: `Some(0)` when due, `None` at rest.
    pub(crate) fn wait_ms(&self, now_ms: u64) -> Option<u64> {
        self.next_ms.map(|next| next.saturating_sub(now_ms))
    }
}

/// [`every`] in ms.
fn tick_ms() -> u64 {
    u64::try_from(every().as_millis()).unwrap_or(u64::MAX)
}

/// What rings the watch from its park ([`want`]).
pub(crate) struct Bell {
    rung: Mutex<bool>,
    cv: Condvar,
    /// How many times it was rung, for a test.
    #[cfg(test)]
    rings: std::sync::atomic::AtomicUsize,
}

impl Bell {
    /// A bell nobody has rung.
    pub(crate) const fn new() -> Self {
        Self {
            rung: Mutex::new(false),
            cv: Condvar::new(),
            #[cfg(test)]
            rings: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Ring it: the parked watch folds the ring into its cadence.
    pub(crate) fn ring(&self) {
        #[cfg(test)]
        self.rings.fetch_add(1, Ordering::SeqCst);
        *self.rung.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.cv.notify_all();
    }

    /// Parks until `cadence` has a look due on `clock`, folding each ring in; with none due
    /// and none rung, the park has no timeout.
    pub(crate) fn until_due(&self, cadence: &mut Cadence, clock: &impl Fn() -> u64) {
        let mut rung = self.rung.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let now = clock();
            if std::mem::take(&mut *rung) {
                cadence.rung(now);
            }
            rung = match cadence.wait_ms(now) {
                Some(0) => return,
                Some(ms) => {
                    self.cv
                        .wait_timeout(rung, Duration::from_millis(ms))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self.cv.wait(rung).unwrap_or_else(PoisonError::into_inner),
            };
        }
    }
}

/// The process-wide watch's bell.
static BELL: Bell = Bell::new();

/// The watch over `board`: parks on `bell` until its [`Cadence`] has a look due on `clock`, takes
/// `look`, folds it in ([`step`]) and hands `note` what the window is owed, forever.
pub(crate) fn watch(
    board: &Mutex<Board>,
    bell: &Bell,
    clock: impl Fn() -> u64,
    mut look: impl FnMut() -> Look,
    note: &(dyn Fn(HostNote) + Send + Sync),
) {
    let mut cadence = Cadence::started(clock());
    loop {
        bell.until_due(&mut cadence, &clock);
        let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let look = look();
            let at = clock();
            let owed = step(
                &mut board.lock().unwrap_or_else(PoisonError::into_inner),
                at,
                look,
            );
            (at, owed)
        }));
        let at = match read {
            Ok((at, owed)) => {
                if let Some(owed) = owed {
                    note(owed);
                }
                at
            }
            Err(_) => {
                aterm_log::warn!(
                    "program check: a read stopped after an internal panic; the next is taken \
                     at its time"
                );
                clock()
            }
        };
        let state = board
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .judge
            .state();
        cadence.looked(at, state);
    }
}

/// Uses the production disk tick regardless of `ATERM_DEV_FAST_TIMERS` to avoid `ps` storms.
const fn every() -> std::time::Duration {
    crate::harness_host::DISK_TICK
}

/// Starts the background `syspolicyd` watcher thread on macOS ([`watch`] over the process-wide
/// board and bell).
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
            watch(
                &BOARD,
                &BELL,
                now_ms,
                || {
                    stood_in(
                        aterm_uds::program_check::look(aterm_uds::program_check::PS_HANG),
                        FULL_STAND_IN.load(Ordering::SeqCst),
                    )
                },
                &*note,
            );
        });
    match spawned {
        Ok(_) => WATCHING.store(true, Ordering::SeqCst),
        Err(e) => aterm_log::warn!("program check: could not start its watch: {e}"),
    }
}

#[cfg(test)]
#[path = "program_check_watch_tests.rs"]
mod tests;
