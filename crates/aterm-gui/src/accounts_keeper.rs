// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WINDOW'S ACCOUNTS' KEEPER: the one thread that makes the window's accounts' upkeep and
//! tells what the accounts say, each time something it read moved, and only then.
//!
//! A turn of the keeper ([`Keep::turn`]) makes two parts, each under a trace scope of its own
//! (`aterm_uds::trace::scope`), and keeps what each read and compared:
//!
//! - the UPKEEP ([`Duties::upkeep`]): the window's upkeep pass, each duty of it made through
//!   its kept verdict (`aterm_agent::harness::accounts_upkeep_memo`, which this thread arms by
//!   being the keeper, `aterm_uds::trace::keeper`), then the turn after it
//!   ([`Duties::upkept`], outside the scope);
//! - the TOLD part ([`Duties::told`]): the news of the accounts, then the host's look, told once
//!   per change.
//!
//! A part runs where it never ran, where its last run's trace no longer stands
//! (`Trace::stands`: a file it read shows another stamp, a listing other names, a watched
//! generation moved, the wall clock left the interval its comparisons answer the same over, a
//! monotonic deadline came), and, for a run not kept (it did something, did not finish, or used
//! what no trace replays), at the rerun that run owes (`Trace::horizon`), or sooner where what
//! it read moved (`Trace::moved`): the told part at once, the upkeep no sooner than
//! [`RERUN_FIRST`] after its last run, so a pass whose own writes move what it reads runs again
//! on the backoff, never in a loop. The told part also runs after every upkeep that ran, and
//! both run at a forced turn ([`Keeper::force`]: a changed configuration, a resume, a word told
//! again). A re-tell a reader asks for ([`Keeper::look_again_at`]) makes the told part, telling
//! each standing word again, at the instant asked.
//!
//! Between turns the thread waits for the first of: a file or folder either trace read changing
//! (a [`FileWatch`] over the paths the traces name, at most [`watch_cap`] of them), the process's
//! trace bell (a watched value or generation moved, `aterm_uds::trace::bell`), the wall clock
//! moving under the monotonic one as the system announces it (a step of the clock, a wake from
//! sleep: a [`WallWatch`] heard on the thread `<name>-wall`), a deadline either part's trace
//! gives, an asked re-tell, a forced turn, or the stop. A wall-clock deadline is waited for whole
//! where both a step of the clock and a sleep are heard; where either is not (no wall watch runs,
//! the system announces no step, `aterm_uds::wall_watch::STEPS_ANNOUNCED`, or the watch hears no
//! sleep, [`WallWatch::hears_sleep`]), it is waited for at most [`LOOK_EVERY`] at a time, so a
//! clock that jumps, or a sleep the monotonic clock stood still across, is caught within that.
//! With no file watch running, or a path past its cap, every wait is that short. A turn that runs
//! neither part is counted as unmoved ([`KeeperCounts::unmoved`]); it replays stamps and opens no
//! file.
//!
//! What the keeper did is counted ([`counts`]): each wake by its cause and each unmoved turn.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aterm_uds::file_watch::FileWatch;
use aterm_uds::trace::{self, Now, RERUN_FIRST, Trace};
use aterm_uds::wall_watch::WallWatch;

/// The longest the keeper waits for a wall-clock deadline at a time where the wall's moves are not
/// all heard (no step of the clock, or no sleep, is: [`hears_wall`]), and every wait while no file
/// watch runs or a path is past its cap ([`watch_cap`]): a clock that jumped unheard, a sleep the
/// monotonic clock stood still across, or a file no watch hears, is looked at within it.
pub(crate) const LOOK_EVERY: Duration = Duration::from_secs(60);

/// The least a turn at a deadline that ran nothing waits for the next deadline: a guard that a
/// trace whose deadline does not make it fall turns once a second, never in a spin.
const DEADLINE_GUARD: Duration = Duration::from_secs(1);

/// One second, in nanoseconds.
const SEC_NS: i128 = 1_000_000_000;

/// The fewest and the most paths the keeper's file watch watches ([`watch_cap`]).
const WATCH_MIN: usize = 8;
const WATCH_MAX: usize = 128;

/// THE KEEPER'S WATCH CAP: the most paths its file watch watches at once, a thirty-second of the
/// process's soft descriptor limit, 8 to 128 (8 where the limit is not read). A path takes at
/// most two descriptors of its own there (the macOS arm listens on its folder, or the nearest one
/// above that is there, and on the path itself: `aterm_uds::file_watch`), so its own listeners
/// take at most twice the cap; the folders above those, each open once for the listeners of
/// every watch whose paths run through it, are not counted. A path past the cap is not watched,
/// and every wait is then [`LOOK_EVERY`] at most, as with no file watch running.
pub(crate) fn watch_cap(soft_limit: Option<u64>) -> usize {
    soft_limit.map_or(WATCH_MIN, |soft| {
        usize::try_from(soft / 32)
            .unwrap_or(usize::MAX)
            .clamp(WATCH_MIN, WATCH_MAX)
    })
}

/// What the keeper makes, in each turn's order.
pub(crate) trait Duties: Send {
    /// Whether there is an upkeep to make at all: an inert one's turns make the told part alone.
    fn upkeeps(&self) -> bool;
    /// The upkeep's pass at `now`, made under the part's trace scope.
    fn upkeep(&mut self, now: Now);
    /// What follows a pass that ran, at `now`, outside its scope.
    fn upkept(&mut self, now: Now);
    /// The news of the accounts, then the look as of the turn's reading `now`, told once per
    /// change; with `again`, each standing word is told again. Made under the part's trace scope.
    fn told(&mut self, now: Now, again: bool);
    /// What the keeper's stop does at once, on the thread that stops it ([`Keeper::stop`]), taken
    /// once as the keeper starts: the duties themselves drop only after the turn in hand, which
    /// the process may never see end. None by default.
    fn on_stop(&mut self) -> Option<Box<dyn FnOnce() + Send>> {
        None
    }
}

/// Why the keeper's thread made a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Woke {
    /// Its first turn.
    Start,
    /// A file or folder a trace read changed ([`FileWatch`]).
    File,
    /// The trace bell rang (`aterm_uds::trace::bell`).
    Bell,
    /// A deadline a part's trace gave came.
    Deadline,
    /// The wall clock moved under the monotonic one, as the system announced it (a step, a wake
    /// from sleep); or, where the wall's moves are not all heard ([`hears_wall`]), a wall-clock
    /// deadline stood and [`LOOK_EVERY`] passed waiting for it.
    Wall,
    /// A re-tell asked for came ([`Keeper::look_again_at`]).
    Asked,
    /// A turn was forced ([`Keeper::force`]).
    Forced,
    /// No file watch runs, or a path a trace read is past its cap ([`watch_cap`]), and
    /// [`LOOK_EVERY`] passed.
    Unwatched,
}

impl Woke {
    /// Every cause a wake is counted under, in [`KeeperCounts`]' order.
    const COUNTED: [Self; 7] = [
        Self::File,
        Self::Bell,
        Self::Deadline,
        Self::Wall,
        Self::Asked,
        Self::Forced,
        Self::Unwatched,
    ];
}

/// The keeper's readings for one turn: the wall clock (Unix nanoseconds) and the monotonic one.
#[derive(Clone, Copy, Debug)]
pub(crate) struct At {
    /// The wall clock, in Unix nanoseconds.
    pub(crate) wall_ns: i128,
    /// The monotonic clock.
    pub(crate) mono: Instant,
}

impl At {
    /// Both clocks now.
    pub(crate) fn now() -> Self {
        let wall_ns = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(since) => i128::try_from(since.as_nanos()).unwrap_or(i128::MAX),
            Err(before) => -i128::try_from(before.duration().as_nanos()).unwrap_or(i128::MAX),
        };
        Self {
            wall_ns,
            mono: Instant::now(),
        }
    }

    /// The wall reading a run compares through: whole seconds, floored.
    pub(crate) fn secs(self) -> Now {
        Now::at(i64::try_from(self.wall_ns.div_euclid(SEC_NS)).unwrap_or(i64::MAX))
    }

    /// How long from this reading until the wall clock reads `wall_ns`; none where it has.
    fn wall_until(self, wall_ns: i128) -> Duration {
        let left = wall_ns.saturating_sub(self.wall_ns).max(0);
        let secs = u64::try_from(left / SEC_NS).unwrap_or(u64::MAX);
        let nanos = u32::try_from(left % SEC_NS).unwrap_or(0);
        Duration::new(secs, nanos)
    }
}

/// When the keeper looks next with nothing heard: the earliest monotonic and wall-clock
/// deadlines that may bring a part due.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Next {
    /// The earliest monotonic deadline.
    pub(crate) mono: Option<Instant>,
    /// The earliest wall-clock deadline, in Unix nanoseconds.
    pub(crate) wall_ns: Option<i128>,
}

impl Next {
    fn mono(&mut self, at: Instant) {
        self.mono = Some(self.mono.map_or(at, |was| was.min(at)));
    }

    fn wall(&mut self, at: i128) {
        self.wall_ns = Some(self.wall_ns.map_or(at, |was| was.min(at)));
    }
}

/// One part's last run: what it read and compared, when the turn that made it began, and how
/// many of its runs in a row, this one among them, were not kept.
struct Ran {
    trace: Trace,
    at: At,
    unkept_in_a_row: u32,
}

/// One part of the keeper's turn, and its last run.
#[derive(Default)]
struct Part {
    last: Option<Ran>,
}

impl Part {
    /// Whether the part runs at `at`. A run not kept owes its rerun; before it, a moved input
    /// brings it due only `floor` after that run began. The look of the `AccountUpkeepVerdict`
    /// model (`aterm_models_harness::account_upkeep_verdict_model`): due only for a cause.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountUpkeepVerdict",
            action = "Look",
            project = "aterm_gui::accounts_keeper::tests::look_transition"
        )
    )]
    fn due(&self, at: At, floor: Duration) -> bool {
        let Some(ran) = &self.last else {
            return true;
        };
        let now = at.secs();
        if ran.trace.unkept().is_none() {
            return ran.trace.stands(now, at.mono).is_err();
        }
        let horizon = ran.trace.horizon();
        let owed = match horizon.rerun {
            Some(rerun) => at.mono >= ran.at.mono + rerun.after(ran.unkept_in_a_row),
            None => horizon.wall_ns.is_some_and(|settled| at.wall_ns >= settled),
        };
        owed || (at.mono >= ran.at.mono + floor && ran.trace.moved(now, at.mono).is_err())
    }

    /// Keep `trace` as the part's last run, made by a turn that began at `at`.
    fn ran(&mut self, trace: Trace, at: At) {
        let unkept_in_a_row = match (&self.last, trace.unkept()) {
            (_, None) => 0,
            (Some(last), Some(_)) => last.unkept_in_a_row.saturating_add(1),
            (None, Some(_)) => 1,
        };
        self.last = Some(Ran {
            trace,
            at,
            unkept_in_a_row,
        });
    }

    /// Add to `next` the deadlines at which the part may come due with nothing heard, as seen
    /// at `at`, with `floor` as in [`Self::due`].
    fn next(&self, at: At, floor: Duration, next: &mut Next) {
        let Some(ran) = &self.last else {
            next.mono(at.mono);
            return;
        };
        let horizon = ran.trace.horizon();
        if ran.trace.unkept().is_none() {
            if let Some(mono) = horizon.mono {
                next.mono(mono);
            }
            if let Some(wall) = horizon.wall_ns {
                next.wall(wall);
            }
            return;
        }
        let floor_at = ran.at.mono + floor;
        match horizon.rerun {
            Some(rerun) => next.mono(ran.at.mono + rerun.after(ran.unkept_in_a_row)),
            None => {
                if let Some(settled) = horizon.wall_ns {
                    next.wall(settled);
                }
            }
        }
        if let Some(mono) = horizon.mono {
            next.mono(mono.max(floor_at));
        }
        if let Some(wall) = horizon.wall_ns {
            if wall > at.wall_ns {
                next.wall(wall);
            } else if floor_at > at.mono {
                next.mono(floor_at);
            }
        }
    }

    /// Every path the part's last run read: each file and owner's standing (`false`), and each
    /// folder listed (`true`).
    fn paths(&self, into: &mut BTreeSet<(PathBuf, bool)>) {
        let Some(ran) = &self.last else {
            return;
        };
        for (path, _) in ran.trace.files() {
            into.insert((path.to_path_buf(), false));
        }
        for path in ran.trace.standings() {
            into.insert((path.to_path_buf(), false));
        }
        for path in ran.trace.listings() {
            into.insert((path.to_path_buf(), true));
        }
    }
}

/// What one turn ran.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Turned {
    /// The upkeep ran.
    pub(crate) upkept: bool,
    /// The told part ran.
    pub(crate) told: bool,
}

/// The keeper's two parts between turns.
#[derive(Default)]
pub(crate) struct Keep {
    upkeep: Part,
    told: Part,
}

impl Keep {
    /// Make the turn `woke` brought at `at`: each part that is due, the told part after an upkeep
    /// that ran, and both where the turn is forced or the first. With `again` (a re-tell asked
    /// for) or a forced turn, the told part runs and tells each standing word again.
    pub(crate) fn turn(
        &mut self,
        woke: Woke,
        again: bool,
        at: At,
        duties: &mut dyn Duties,
    ) -> Turned {
        let forced = matches!(woke, Woke::Start | Woke::Forced);
        let upkept = duties.upkeeps() && (forced || self.upkeep.due(at, RERUN_FIRST));
        if upkept {
            let ((), trace) = trace::scope(|| duties.upkeep(at.secs()));
            self.upkeep.ran(trace, at);
            duties.upkept(at.secs());
        }
        let again = again || woke == Woke::Forced;
        let told = forced || again || upkept || self.told.due(at, Duration::ZERO);
        if told {
            let ((), trace) = trace::scope(|| duties.told(at.secs(), again));
            self.told.ran(trace, at);
        }
        Turned { upkept, told }
    }

    /// When the keeper looks next with nothing heard, as seen at `at` (`upkeeps`: whether the
    /// upkeep part is made at all).
    pub(crate) fn next(&self, at: At, upkeeps: bool) -> Next {
        let mut next = Next::default();
        if upkeeps {
            self.upkeep.next(at, RERUN_FIRST, &mut next);
        }
        self.told.next(at, Duration::ZERO, &mut next);
        next
    }

    /// Every path either part's last run read ([`Part::paths`]).
    pub(crate) fn paths(&self) -> BTreeSet<(PathBuf, bool)> {
        let mut paths = BTreeSet::new();
        self.upkeep.paths(&mut paths);
        self.told.paths(&mut paths);
        paths
    }
}

/// What the keeper's thread is asked, under its door's lock.
#[derive(Default)]
struct Asks {
    stop: bool,
    forced: bool,
    file: bool,
    bell: bool,
    /// The wall clock moved, as its hearing heard ([`WallHearing`]).
    wall: bool,
    /// The wall clock's hearing ended by itself (its wait failed): no move of the wall is heard
    /// from then.
    deaf: bool,
    /// The re-tells asked for, in Unix milliseconds.
    again_at: BTreeSet<u64>,
}

/// The door to the keeper's thread. Its lock is held only to read or set what is asked, never
/// over a wait for anything else, so the trace bell's listener, called with a watched value's
/// lock held, may take it.
#[derive(Default)]
struct Door {
    asks: Mutex<Asks>,
    rung: Condvar,
}

impl Door {
    fn ask(&self, ask: impl FnOnce(&mut Asks)) {
        ask(&mut self.asks.lock().unwrap_or_else(PoisonError::into_inner));
        self.rung.notify_all();
    }

    /// Wait for what wakes the next turn, as [`Keep::next`] says at `next`, with a file watch
    /// hearing every path the traces name or not (`watched`) and the wall clock's moves heard or
    /// not (`wall_heard`, [`hears_wall`]); a deadline no earlier than `guard`. `None` at the stop.
    /// Every turn reads both clocks afresh, so whatever wakes it answers a move of the wall heard
    /// before it.
    fn wait(
        &self,
        next: Next,
        watched: bool,
        wall_heard: bool,
        guard: Option<Instant>,
    ) -> Option<(Woke, bool)> {
        let mut asks = self.asks.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if asks.stop {
                return None;
            }
            let at = At::now();
            let woke = if std::mem::take(&mut asks.forced) {
                asks.file = false;
                asks.bell = false;
                Some((Woke::Forced, take_due(&mut asks.again_at, at)))
            } else if take_due(&mut asks.again_at, at) {
                Some((Woke::Asked, true))
            } else if std::mem::take(&mut asks.file) {
                asks.bell = false;
                Some((Woke::File, false))
            } else if std::mem::take(&mut asks.bell) {
                Some((Woke::Bell, false))
            } else if asks.wall {
                Some((Woke::Wall, false))
            } else {
                None
            };
            if woke.is_some() {
                asks.wall = false;
                return woke;
            }
            let again_ns = asks.again_at.first().map(|ms| i128::from(*ms) * 1_000_000);
            let heard = wall_heard && !asks.deaf;
            let Some((wait, capped)) = span(at, next, again_ns, watched, heard, guard) else {
                return Some((Woke::Deadline, false));
            };
            asks = match wait {
                Some(wait) => {
                    let (asks, out) = self
                        .rung
                        .wait_timeout(asks, wait)
                        .unwrap_or_else(PoisonError::into_inner);
                    if out.timed_out()
                        && let Some(capped) = capped
                    {
                        return Some((capped, false));
                    }
                    asks
                }
                None => self.rung.wait(asks).unwrap_or_else(PoisonError::into_inner),
            };
        }
    }
}

/// How long a wait begun at `at` lasts with nothing heard, for the deadlines `next` gives and the
/// first re-tell asked (`again_ns`, Unix nanoseconds), none earlier than `guard`, and what wakes
/// the turn where it runs out (`None`: a deadline, read again). `None` where a deadline has come.
/// A wall-clock deadline is waited for whole where the wall's moves are `heard` (a running hearing
/// that hears both a step of the clock and a sleep, [`hears_wall`]), and at most [`LOOK_EVERY`]
/// where they are not ([`Woke::Wall`]); where no file watch hears every path the traces name
/// (`watched`), no wait is longer ([`Woke::Unwatched`]).
fn span(
    at: At,
    next: Next,
    again_ns: Option<i128>,
    watched: bool,
    heard: bool,
    guard: Option<Instant>,
) -> Option<(Option<Duration>, Option<Woke>)> {
    let mono = next
        .mono
        .map(|mono| guard.map_or(mono, |guard| mono.max(guard)));
    let wall = [next.wall_ns, again_ns].into_iter().flatten().min();
    let mut left = mono.map(|mono| mono.saturating_duration_since(at.mono));
    if let Some(wall) = wall {
        let until = at.wall_until(wall);
        let until = guard.map_or(until, |guard| {
            until.max(guard.saturating_duration_since(at.mono))
        });
        left = Some(left.map_or(until, |left| left.min(until)));
    }
    if left == Some(Duration::ZERO) {
        return None;
    }
    let mut capped = None;
    if wall.is_some() && !heard && left.is_none_or(|left| left > LOOK_EVERY) {
        capped = Some(Woke::Wall);
    }
    if !watched && left.is_none_or(|left| left > LOOK_EVERY) {
        capped = Some(Woke::Unwatched);
    }
    let wait = if capped.is_some() {
        Some(LOOK_EVERY)
    } else {
        left
    };
    Some((wait, capped))
}

/// Take every re-tell asked for at or before `at`: whether there was one.
fn take_due(again_at: &mut BTreeSet<u64>, at: At) -> bool {
    let mut due = false;
    while let Some(first) = again_at.first().copied() {
        if i128::from(first) * 1_000_000 > at.wall_ns {
            break;
        }
        again_at.remove(&first);
        due = true;
    }
    due
}

/// What the keeper's stop does at once ([`Duties::on_stop`]).
type OnStop = Box<dyn FnOnce() + Send>;

/// The window's accounts' keeper: the door to its thread, and what its stop does at once.
pub(crate) struct Keeper {
    door: Arc<Door>,
    on_stop: Mutex<Option<OnStop>>,
}

impl Keeper {
    /// Start the keeper on a thread named `name`, making `duties`; its file watch's thread is
    /// `<name>-watch`, and its hearing of the wall clock's moves `<name>-wall`. A file watch that
    /// cannot start leaves every wait [`LOOK_EVERY`] long, and a wall clock unheard
    /// ([`WallWatch::start`] answering `None`) every wait for a wall-clock deadline.
    ///
    /// # Errors
    /// Where the thread cannot start.
    pub(crate) fn spawn(name: &str, duties: Box<dyn Duties>) -> std::io::Result<Self> {
        Self::spawn_hearing(name, duties, WallWatch::start().map(Arc::new))
    }

    /// [`Self::spawn`], hearing the wall clock's moves from `wall` (`None`: unheard).
    fn spawn_hearing(
        name: &str,
        mut duties: Box<dyn Duties>,
        wall: Option<Arc<WallWatch>>,
    ) -> std::io::Result<Self> {
        let door = Arc::new(Door::default());
        let on_stop = Mutex::new(duties.on_stop());
        let names = (format!("{name}-watch"), format!("{name}-wall"));
        let keeper_door = Arc::clone(&door);
        std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                trace::keeper(|| keep(&keeper_door, &names, wall, duties));
            })?;
        Ok(Self { door, on_stop })
    }

    /// [`Self::spawn`] hearing the wall clock's moves from `wall`, which a test tells by hand.
    #[cfg(test)]
    pub(crate) fn spawn_for_test(
        name: &str,
        duties: Box<dyn Duties>,
        wall: Option<Arc<WallWatch>>,
    ) -> std::io::Result<Self> {
        Self::spawn_hearing(name, duties, wall)
    }

    /// A turn now that makes both parts, telling each standing word again.
    pub(crate) fn force(&self) {
        self.door.ask(|asks| asks.forced = true);
    }

    /// A re-tell at `unix_ms`, or now where it has come: the told part runs then, telling each
    /// standing word again.
    pub(crate) fn look_again_at(&self, unix_ms: u64) {
        self.door.ask(|asks| {
            asks.again_at.insert(unix_ms);
        });
    }

    /// Stop the keeper: what its duties do at a stop ([`Duties::on_stop`]) is done at once, on
    /// this thread, the first time; its thread ends after the turn in hand, which nothing waits
    /// for (a pass may be fetching), and its duties drop with it.
    pub(crate) fn stop(&self) {
        self.door.ask(|asks| asks.stop = true);
        let on_stop = self
            .on_stop
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(on_stop) = on_stop {
            on_stop();
        }
    }
}

/// The keeper's hearing of the wall clock's moves ([`WallWatch`]) on a thread of its own, which
/// tells the door at each move, and at its own end where its wait failed; the thread ends as
/// this drops.
struct WallHearing {
    stop: Arc<aterm_uds::stop::Stop>,
}

impl WallHearing {
    /// Hear `watch` on a thread named `name`, telling `door`: `None` where the stop's bell or the
    /// thread could not be made, or off unix.
    fn start(name: &str, watch: Arc<WallWatch>, door: &Arc<Door>) -> Option<Self> {
        #[cfg(unix)]
        {
            let stop = Arc::new(aterm_uds::stop::Stop::new());
            stop.bell().ok()?;
            let held = Arc::clone(&stop);
            let door = Arc::clone(door);
            let spawned = std::thread::Builder::new()
                .name(name.to_owned())
                .spawn(move || {
                    loop {
                        let waited = held.wait_ready(&watch.fds(), None);
                        if held.is_set() {
                            return;
                        }
                        if waited.is_err() {
                            door.ask(|asks| asks.deaf = true);
                            return;
                        }
                        if watch.take().any() {
                            door.ask(|asks| asks.wall = true);
                        }
                    }
                });
            spawned.ok().map(|_| Self { stop })
        }
        #[cfg(not(unix))]
        {
            let _ = (name, watch, door);
            None
        }
    }
}

impl Drop for WallHearing {
    fn drop(&mut self) {
        self.stop.set();
    }
}

/// Whether the keeper may wait a wall-clock deadline whole, hearing the wall's moves from
/// `watch` on a hearing that runs where `hearing`: the system announces a step of the clock
/// (`aterm_uds::wall_watch::STEPS_ANNOUNCED`) and the watch hears a sleep
/// ([`WallWatch::hears_sleep`]), across which the monotonic clock stands still while the wall
/// moves on. A watch whose power half was refused hears steps alone, and a sleep the clock was
/// not set across says nothing to it.
fn hears_wall(watch: &WallWatch, hearing: bool) -> bool {
    hearing && aterm_uds::wall_watch::STEPS_ANNOUNCED && watch.hears_sleep()
}

/// The keeper's thread: turns until the stop. `names` are its file watch's thread's and its
/// wall hearing's.
fn keep(
    door: &Arc<Door>,
    names: &(String, String),
    wall: Option<Arc<WallWatch>>,
    mut duties: Box<dyn Duties>,
) {
    let bell_door = Arc::clone(door);
    let _bell = trace::listen(move || bell_door.ask(|asks| asks.bell = true));
    let (_hearing, wall_heard) = match wall {
        Some(wall) => {
            let hearing = WallHearing::start(&names.1, Arc::clone(&wall), door);
            let heard = hears_wall(&wall, hearing.is_some());
            (hearing, heard)
        }
        None => (None, false),
    };
    let file_door = Arc::clone(door);
    let watch = match FileWatch::platform(
        &names.0,
        Box::new(move |_| file_door.ask(|asks| asks.file = true)),
    ) {
        Ok(watch) => Some(watch),
        Err(e) => {
            aterm_log::warn!(
                "harness: accounts-keeper: no file watch ({e}); what the accounts read is looked \
                 at every {}s",
                LOOK_EVERY.as_secs()
            );
            None
        }
    };
    let mut watching = Watching::default();
    let mut keep = Keep::default();
    let (mut woke, mut again) = (Woke::Start, false);
    let mut past = 0;
    loop {
        let at = At::now();
        let turned = keep.turn(woke, again, at, &mut *duties);
        count(woke, turned);
        if let Some(watch) = &watch {
            let cap = watch_cap(crate::control::soft_descriptor_limit());
            let unwatched = watching.set(watch, keep.paths(), cap);
            if unwatched > 0 && past == 0 {
                aterm_log::info!(
                    "harness: accounts-keeper: {unwatched} of the paths read are past the file \
                     watch's cap of {cap}, so no wait is longer than {}s",
                    LOOK_EVERY.as_secs()
                );
            }
            past = unwatched;
        }
        let guard = (woke == Woke::Deadline && turned == Turned::default())
            .then(|| at.mono + DEADLINE_GUARD);
        let next = keep.next(At::now(), duties.upkeeps());
        let heard = watch.is_some() && past == 0;
        let Some(next) = door.wait(next, heard, wall_heard, guard) else {
            break;
        };
        (woke, again) = next;
    }
}

/// The paths the keeper's file watch hears, by the key each is watched as.
#[derive(Default)]
struct Watching {
    keys: BTreeMap<(PathBuf, bool), u64>,
    next: u64,
}

impl Watching {
    /// Watch `paths` (a folder where its flag is set), each kept under its key, at most `cap` of
    /// them ([`watch_cap`]): those watched already first, then the rest, each in order. How many
    /// of `paths` are left unwatched.
    fn set(&mut self, watch: &FileWatch, paths: BTreeSet<(PathBuf, bool)>, cap: usize) -> usize {
        let mut room = cap;
        self.keys.retain(|path, key| {
            let kept = room > 0 && paths.contains(path);
            if kept {
                room -= 1;
            } else {
                watch.unwatch(*key);
            }
            kept
        });
        let mut unwatched = 0;
        for path in paths {
            if self.keys.contains_key(&path) {
                continue;
            }
            if room == 0 {
                unwatched += 1;
                continue;
            }
            room -= 1;
            let key = self.next;
            self.next = self.next.wrapping_add(1);
            if path.1 {
                watch.watch_directory(key, path.0.clone());
            } else {
                watch.watch(key, path.0.clone());
            }
            self.keys.insert(path, key);
        }
        unwatched
    }
}

/// Each counted wake, in [`Woke::COUNTED`]'s order, then each unmoved turn.
static COUNTS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];

#[cfg(test)]
std::thread_local! {
    /// [`COUNTS`] moved on this thread: a test's own.
    static HERE: [std::cell::Cell<u64>; 8] = const { [const { std::cell::Cell::new(0) }; 8] };
}

/// Count `woke` and, where it ran nothing, an unmoved turn.
fn count(woke: Woke, turned: Turned) {
    let bump = |at: usize| {
        COUNTS[at].fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        HERE.with(|here| here[at].set(here[at].get().wrapping_add(1)));
    };
    if let Some(at) = Woke::COUNTED.iter().position(|counted| *counted == woke) {
        bump(at);
    }
    if turned == Turned::default() {
        bump(Woke::COUNTED.len());
    }
}

/// The keeper's counts in this process since it started ([`counts`]): each only rises, and a
/// reader subtracts two readings. They decide nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct KeeperCounts {
    /// Wakes by a file or folder a trace read changing.
    pub(crate) file: u64,
    /// Wakes by the trace bell.
    pub(crate) bell: u64,
    /// Wakes at a deadline a part's trace gave.
    pub(crate) deadline: u64,
    /// Wakes by a move of the wall clock heard, or after [`LOOK_EVERY`] spent waiting for a
    /// wall-clock deadline where the wall's moves are not all heard ([`hears_wall`]).
    pub(crate) wall: u64,
    /// Wakes at an asked re-tell.
    pub(crate) asked: u64,
    /// Forced wakes.
    pub(crate) forced: u64,
    /// Wakes after [`LOOK_EVERY`] with no file watch running, or with a path past its cap
    /// ([`watch_cap`]).
    pub(crate) unwatched: u64,
    /// Turns that ran neither part.
    pub(crate) unmoved: u64,
}

impl KeeperCounts {
    fn of(read: impl Fn(usize) -> u64) -> Self {
        Self {
            file: read(0),
            bell: read(1),
            deadline: read(2),
            wall: read(3),
            asked: read(4),
            forced: read(5),
            unwatched: read(6),
            unmoved: read(7),
        }
    }
}

/// The keeper's counts in this process.
pub(crate) fn counts() -> KeeperCounts {
    KeeperCounts::of(|at| COUNTS[at].load(Ordering::Relaxed))
}

/// The keeper's counts moved on this thread.
#[cfg(test)]
pub(crate) fn counts_here() -> KeeperCounts {
    HERE.with(|here| KeeperCounts::of(|at| here[at].get()))
}

/// Move the process's keeper counts by `by`: a test's own reading of them through `metrics`.
#[cfg(test)]
pub(crate) fn add_for_test(by: KeeperCounts) {
    let by = [
        by.file,
        by.bell,
        by.deadline,
        by.wall,
        by.asked,
        by.forced,
        by.unwatched,
        by.unmoved,
    ];
    for (count, by) in COUNTS.iter().zip(by) {
        count.fetch_add(by, Ordering::Relaxed);
    }
}

/// Count `woke` as the keeper's thread does after a turn that ran `turned`: a test's own turn.
#[cfg(test)]
pub(crate) fn count_for_test(woke: Woke, turned: Turned) {
    count(woke, turned);
}

#[cfg(test)]
#[path = "accounts_keeper_tests.rs"]
mod tests;
