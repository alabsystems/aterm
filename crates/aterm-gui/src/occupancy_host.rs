// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OCCUPANCY FACTS BOARD (`docs/DESIGN-harness-occupancy-2026-10-20.md`,).
//! Under the same lock it keeps the pace and reservation table (design §4.2, [`Pace`]),
//! which a loop under `occupancy = "prompt"` takes before it books its intent ([`pace_for`]).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use aterm_agent::harness::disk::Level;
use aterm_agent::supervise::memory::Pressure as MemoryPressure;
use aterm_agent::supervise::occupancy::rules::{HOLD_ROOM_MACHINE, HOLD_ROOM_SLOTS};
use aterm_agent::supervise::occupancy::{OccupancyReading, PaceGrant, RuleId, SlotsSeen};
use aterm_sysprobe::slots::{SlotsRead, Unread};
use aterm_uds::process::ProcessStart;
use aterm_vitals::model::{Pressure, ProcSample, SlotsUnread, SlotsView, Snapshot, TabRoot};
use aterm_vitals::sys::predicate::{AwaitSys, Latch, Parsed, evaluate};

/// A board whose snapshot is older than this (two disk looks) is stale.
pub(crate) const STALE_AFTER: Duration =
    Duration::from_millis(aterm_agent::supervise::occupancy::reading::STALE_AFTER_MS as u64);

/// How long the board remembers that a session's tab held a build slot.
pub(crate) const HELD_KEEP: Duration = Duration::from_secs(48 * 3600);

/// `d` in whole milliseconds, saturating.
fn whole_ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// The occupancy facts at one look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FactsBoard {
    /// When the board was taken (unix ms).
    pub(crate) taken_unix_ms: i64,
    /// When the snapshot it projects landed (unix ms); 0 for none.
    pub(crate) as_of_unix_ms: i64,
    /// Logical cores.
    pub(crate) cores: Option<u16>,
    /// The 1-minute load average, times 100.
    pub(crate) load1_x100: Option<u32>,
    /// The 5-minute load average, times 100.
    pub(crate) load5_x100: Option<u32>,
    /// Memory available without paging (`build-slot`'s figure), MiB.
    pub(crate) avail_mib: Option<u64>,
    /// The kernel's memory pressure; `None` where unread.
    pub(crate) pressure: Option<Pressure>,
    /// The home volume's level on the disk watch; `None` where the watch has not judged it.
    pub(crate) disk: Option<Level>,
    /// The build slots, as the slot reader last proved them.
    pub(crate) slots: Option<SlotsView>,
    /// The snapshot is older than [`STALE_AFTER`], or there is none.
    pub(crate) stale: bool,
}

/// A load average times 100, rounded; `None` for a value no load is.
fn x100(load: f64) -> Option<u32> {
    (load.is_finite() && load >= 0.0)
        .then(|| (load * 100.0).round().min(f64::from(u32::MAX)) as u32)
}

/// The board of `snap`, which landed at `snap_unix_ms` (`None`.
pub(crate) fn board_of(
    snap: &Snapshot,
    snap_unix_ms: Option<i64>,
    disk: Option<Level>,
    now_unix_ms: i64,
) -> FactsBoard {
    let stale_ms = i64::try_from(STALE_AFTER.as_millis()).unwrap_or(i64::MAX);
    FactsBoard {
        taken_unix_ms: now_unix_ms,
        as_of_unix_ms: snap_unix_ms.unwrap_or(0),
        cores: snap.total_cores().and_then(|c| u16::try_from(c).ok()),
        load1_x100: snap.load.and_then(|l| x100(l[0])),
        load5_x100: snap.load.and_then(|l| x100(l[1])),
        avail_mib: snap.avail.map(|b| b >> 20),
        pressure: snap
            .mem
            .as_ref()
            .map(|m| m.sample.pressure)
            .filter(|p| *p != Pressure::Unknown),
        disk: disk.filter(|l| *l != Level::Unknown),
        slots: snap.slots,
        stale: snap_unix_ms.is_none_or(|at| now_unix_ms.saturating_sub(at) > stale_ms),
    }
}

/// The slot reader's reading as a snapshot carries it.
pub(crate) fn slots_view(r: &SlotsRead) -> SlotsView {
    match r {
        SlotsRead::Unread(why) => SlotsView::Unread(match why {
            Unread::Unsupported => SlotsUnread::Unsupported,
            Unread::NotSampled => SlotsUnread::NotSampled,
            Unread::Refused => SlotsUnread::Refused,
            Unread::UnreadFormat => SlotsUnread::UnreadFormat,
        }),
        SlotsRead::Read { counts, .. } => SlotsView::Read {
            n: counts.n,
            held: counts.held,
            free: counts.free,
            pending: counts.pending,
            unknown: counts.unknown,
            discounted: counts.discounted,
        },
    }
}

/// THE TABS THAT HOLD A BUILD SLOT (their keys, each once, in order).
pub(crate) fn slot_tabs(
    read: &SlotsRead,
    procs: Option<&[ProcSample]>,
    self_pid: u32,
    tabs: &[TabRoot],
    current_start: &mut dyn FnMut(u32) -> Option<ProcessStart>,
) -> Option<Vec<u64>> {
    let SlotsRead::Read { holders, .. } = read else {
        return None;
    };
    let procs = procs?;
    if holders.is_empty() {
        return Some(Vec::new());
    }
    let members = aterm_vitals::model::attribute(procs, self_pid, tabs);
    let mut keys: Vec<u64> = holders
        .iter()
        .filter_map(|h| {
            let at = procs.iter().position(|p| p.pid == h.pid)?;
            let tab = members.get(at)?.tab?;
            let key = tabs.get(tab)?.key;
            let swept_start = procs.get(at)?.start?;
            let current = current_start(h.pid)?;
            let wall = current.wall?;
            let birth_us = wall
                .seconds
                .checked_mul(1_000_000)?
                .checked_add(wall.microseconds)?;
            (current.boot.0 == swept_start && birth_us == h.birth_us).then_some(key)
        })
        .collect();
    keys.sort_unstable();
    keys.dedup();
    Some(keys)
}

/// One of `hold-room`'s waits (`text`, its leading `sys` included, as
/// `await sys` hands the grammar its words), parsed: a machine wait, never a
/// volume's.
fn machine_wait(text: &str) -> Option<AwaitSys> {
    match AwaitSys::parse(text) {
        Ok(Parsed::Machine(wait)) => Some(wait),
        _ => None,
    }
}

/// The facts of `snap` `hold-room` judges, at THE BOARD'S CADENCE: the slot
/// reader reads once a look at rest, so a slots reading no older than
/// [`STALE_AFTER`] speaks for the look (`await sys`'s
/// [`aterm_vitals::sys::predicate::SLOTS_FRESH_MS`] is a parked wait's, which
/// arms the reader to seconds); an older one keeps its age and reads as
/// warming.
fn judged_snapshot(snap: &Snapshot) -> Snapshot {
    let stale_ms = u64::try_from(STALE_AFTER.as_millis()).unwrap_or(u64::MAX);
    Snapshot {
        at_ns: snap.at_ns,
        avail: snap.avail,
        mem: snap.mem.clone(),
        slots: snap.slots,
        slots_age_ms: snap.slots_age_ms.filter(|age| *age > stale_ms),
        ..Snapshot::default()
    }
}

/// THE BOARD AND WHAT IT JUDGED ACROSS ITS LOOKS ([module docs](self)).
#[derive(Debug, Default)]
pub(crate) struct Judged {
    /// The board as last taken.
    board: Option<FactsBoard>,
    /// `hold-room`'s slot term over the looks.
    latch: Latch,
    /// How long that term has held, ms, once it spans its `for=`.
    slots_free_ms: Option<u64>,
    /// Whether `hold-room`'s machine terms held at the last look; `None`
    /// where they could not be judged.
    room: Option<bool>,
    /// Each supervised session's last look at which a build slot's holder
    /// lay in its tab (the snapshot's unix ms), kept [`HELD_KEEP`].
    held: BTreeMap<String, i64>,
    /// The machine's pace and the slots reserved ([`Pace`]).
    pace: Pace,
}

impl Judged {
    /// THE LOOK that took `board` from `snap`, with the supervised `sessions`.
    pub(crate) fn step(
        &mut self,
        board: FactsBoard,
        snap: Option<&Snapshot>,
        sessions: &[(u64, String)],
    ) {
        match snap.filter(|_| !board.stale) {
            None => {
                self.latch = Latch::default();
                self.slots_free_ms = None;
                self.room = None;
            }
            Some(snap) => {
                let judged = judged_snapshot(snap);
                self.slots_free_ms = machine_wait(HOLD_ROOM_SLOTS).and_then(|wait| {
                    let o = evaluate(&wait, &judged, &[]);
                    let holds = o.holds && o.refused.is_none();
                    self.latch.step(holds, snap.at_ns, wait.for_ms)
                });
                self.room = machine_wait(HOLD_ROOM_MACHINE).and_then(|wait| {
                    let o = evaluate(&wait, &judged, &[]);
                    o.refused.is_none().then_some(o.holds)
                });
                if let Some(tabs) = &snap.slot_tabs {
                    for (key, sid) in sessions {
                        if tabs.contains(key) {
                            self.held.insert(sid.clone(), board.as_of_unix_ms);
                        }
                    }
                }
            }
        }
        let taken = board.taken_unix_ms;
        self.held
            .retain(|_, at| taken.saturating_sub(*at) <= whole_ms(HELD_KEEP));
        // A reservation ends where its session's tab was seen holding a slot
        // since: the build it was for took one.
        let held = &self.held;
        self.pace
            .reserved
            .retain(|r| held.get(&r.sid).is_none_or(|at| *at < r.since_ms));
        self.board = Some(board);
    }

    /// The board as session `sid`'s host reads it at `now_unix_ms`.
    pub(crate) fn reading(&self, sid: &str, now_unix_ms: i64) -> OccupancyReading {
        let Some(b) = &self.board else {
            return OccupancyReading::default();
        };
        OccupancyReading {
            taken_ms: b.taken_unix_ms,
            stale: b.stale,
            avail_mib: b.avail_mib,
            pressure: b.pressure.and_then(memory_pressure),
            disk: b.disk,
            slots: match b.slots {
                Some(SlotsView::Read {
                    n,
                    free,
                    discounted,
                    ..
                }) => Some(SlotsSeen {
                    usable: free.saturating_sub(discounted),
                    n,
                }),
                _ => None,
            },
            slots_free_ms: self.slots_free_ms,
            room: self.room,
            held_slot_last_ms: self.held.get(sid).copied(),
        }
        .aged_at(Some(now_unix_ms))
    }

    /// THE PACE FOR A LINE of `rule` in session `sid`'s hold begun at
    /// `hold_since_ms`, asked at `now_unix_ms` ([`Pace::ask`]): a slot rule
    /// counts the slots the board proves usable now, none on a board that no
    /// longer speaks for now.
    pub(crate) fn pace(
        &mut self,
        sid: &str,
        rule: RuleId,
        hold_since_ms: i64,
        now_unix_ms: i64,
    ) -> PaceGrant {
        let slot = match rule {
            RuleId::HoldRoom => true,
            RuleId::WaitNudge => false,
        };
        let reading = self.reading(sid, now_unix_ms);
        let usable = reading
            .slots
            .filter(|_| reading.fresh())
            .map_or(0, |s| s.usable);
        let ask = Ask {
            sid,
            slot,
            hold_since_ms,
            usable,
        };
        self.pace.ask(&ask, now_unix_ms)
    }
}

/// THE MACHINE'S PACE: at most this many fact lines across the machine in
/// any hour (design §4.2).
pub(crate) const MACHINE_LINES_PER_HOUR: usize = 6;

/// The machine pace's window, ms.
const HOUR_MS: i64 = 60 * 60_000;

/// A slot rule's line reserves one free build slot this long (design §4.2).
pub(crate) const RESERVE_MS: i64 = 10 * 60_000;

/// A hold refused for want of a slot stands first in line this long after
/// its last ask: a hold that stopped asking (its own guard holds it) no
/// longer keeps a younger one waiting.
pub(crate) const ASK_KEEP_MS: i64 = aterm_agent::supervise::occupancy::reading::STALE_AFTER_MS;

/// One ask of the table.
struct Ask<'a> {
    sid: &'a str,
    /// The rule's line needs a free build slot.
    slot: bool,
    /// When the asking session's hold began, unix ms: the oldest first.
    hold_since_ms: i64,
    /// The build slots the board proves usable now.
    usable: u16,
}

/// A build slot reserved for a session's line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reservation {
    sid: String,
    /// When it was taken, unix ms.
    since_ms: i64,
}

/// A hold refused for want of a slot, waiting.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Waiting {
    sid: String,
    hold_since_ms: i64,
    /// Its last ask, unix ms.
    asked_ms: i64,
}

/// THE PACE AND RESERVATION TABLE ([module docs](self)).
#[derive(Debug, Default)]
pub(crate) struct Pace {
    /// When each line of the last hour was granted, unix ms.
    lines: Vec<i64>,
    /// The slots reserved, each for [`RESERVE_MS`] from its taking.
    reserved: Vec<Reservation>,
    /// The holds that asked for a slot and found none, by their age.
    waiting: Vec<Waiting>,
}

impl Pace {
    /// THE ASK at `now_ms`, answered with the machine's pace and the slot's
    /// reservation taken together, or neither: [`PaceGrant::Machine`] where
    /// the hour's lines are spent; for a slot rule [`PaceGrant::Reserved`]
    /// where no usable slot is left past those other sessions reserved and
    /// those older holds wait for (the asker then waits in line); else
    /// [`PaceGrant::Granted`], the line counted and, for a slot rule, its
    /// slot reserved.
    fn ask(&mut self, ask: &Ask<'_>, now_ms: i64) -> PaceGrant {
        self.lines.retain(|at| now_ms.saturating_sub(*at) < HOUR_MS);
        self.reserved
            .retain(|r| now_ms.saturating_sub(r.since_ms) < RESERVE_MS);
        self.waiting
            .retain(|w| now_ms.saturating_sub(w.asked_ms) <= ASK_KEEP_MS);
        if self.lines.len() >= MACHINE_LINES_PER_HOUR {
            return PaceGrant::Machine;
        }
        if ask.slot {
            let mine = (ask.hold_since_ms, ask.sid);
            let others = self.reserved.iter().filter(|r| r.sid != ask.sid).count();
            let older = self
                .waiting
                .iter()
                .filter(|w| w.sid != ask.sid && (w.hold_since_ms, w.sid.as_str()) < mine)
                .count();
            if usize::from(ask.usable) <= others + older {
                self.waiting.retain(|w| w.sid != ask.sid);
                self.waiting.push(Waiting {
                    sid: ask.sid.to_string(),
                    hold_since_ms: ask.hold_since_ms,
                    asked_ms: now_ms,
                });
                return PaceGrant::Reserved;
            }
            self.waiting.retain(|w| w.sid != ask.sid);
            self.reserved.retain(|r| r.sid != ask.sid);
            self.reserved.push(Reservation {
                sid: ask.sid.to_string(),
                since_ms: now_ms,
            });
        }
        self.lines.push(now_ms);
        PaceGrant::Granted
    }
}

/// The kernel's memory pressure as the supervisor names it; `None` unread.
fn memory_pressure(p: Pressure) -> Option<MemoryPressure> {
    match p {
        Pressure::Normal => Some(MemoryPressure::Normal),
        Pressure::Warn => Some(MemoryPressure::Warn),
        Pressure::Critical => Some(MemoryPressure::Critical),
        Pressure::Unknown => None,
    }
}

/// The words a change of the board is logged with: its slots and its disk
/// level, which are what the log follows.
fn change_words(b: &FactsBoard) -> String {
    let slots = match b.slots {
        None => "slots=unread why=not_sampled".to_string(),
        Some(SlotsView::Unread(why)) => format!("slots=unread why={}", why.word()),
        Some(SlotsView::Read {
            n,
            held,
            free,
            pending,
            unknown,
            discounted,
        }) => format!(
            "slots={}/{n} held={held} free={free} pending={pending} unknown={unknown} \
             discounted={discounted}",
            free.saturating_sub(discounted)
        ),
    };
    let disk = b.disk.map_or("unknown", Level::as_str);
    format!("{slots} disk={disk}")
}

/// How many waits name a reader now ([`Arming::arm`]): the build slots'
/// ([`SLOTS_ARMED`]), or the split's (`crate::vitals_host::SPLIT_ARMED`).
#[derive(Debug, Default)]
pub(crate) struct Arming {
    armed: AtomicUsize,
    /// The lines logged when the first is armed and when the last is gone,
    /// where the reader's cadence changes then; `None` logs nothing.
    lines: Option<(&'static str, &'static str)>,
}

/// One armed wait; dropped, it is no longer counted.
#[derive(Debug)]
pub(crate) struct Armed<'a>(&'a Arming);

impl Arming {
    /// None armed, and nothing logged.
    pub(crate) const fn new() -> Self {
        Self {
            armed: AtomicUsize::new(0),
            lines: None,
        }
    }

    /// None armed; `armed` is logged when the first is, `at_rest` when the
    /// last is gone.
    pub(crate) const fn logged(armed: &'static str, at_rest: &'static str) -> Self {
        Self {
            armed: AtomicUsize::new(0),
            lines: Some((armed, at_rest)),
        }
    }

    /// Count one wait as armed until the guard drops.
    pub(crate) fn arm(&self) -> Armed<'_> {
        if self.armed.fetch_add(1, Ordering::AcqRel) == 0
            && let Some((armed, _)) = self.lines
        {
            aterm_log::info!("{armed}");
        }
        Armed(self)
    }

    /// Whether any wait names the slots now.
    pub(crate) fn armed(&self) -> bool {
        self.armed.load(Ordering::Acquire) > 0
    }
}

impl Drop for Armed<'_> {
    fn drop(&mut self) {
        if self.0.armed.fetch_sub(1, Ordering::AcqRel) == 1
            && let Some((_, at_rest)) = self.0.lines
        {
            aterm_log::info!("{at_rest}");
        }
    }
}

/// THE WAITS ON THE BUILD SLOTS. The two edges are logged: `lab slot-lane`
/// counts them.
pub(crate) static SLOTS_ARMED: Arming = Arming::logged(
    aterm_sysprobe::slots::ARMED_LINE,
    aterm_sysprobe::slots::AT_REST_LINE,
);

/// The board and what it judged, as last taken ([`look`]); `None` before
/// the first look.
static BOARD: Mutex<Option<Judged>> = Mutex::new(None);

/// The home volume's level on the disk board, where the watch judged it.
fn home_level() -> Option<Level> {
    let home = aterm_types::dirs::home_dir()?;
    let board = crate::disk_board::SHELF.board_lock();
    board.row_at(&home).map(|r| r.level)
}

/// TAKE THE BOARD at `now_unix_ms` (the disk look's).
pub(crate) fn look(now_unix_ms: i64, sessions: &[(u64, String)]) {
    let held = crate::sys_host::SYS.held_now();
    let disk = home_level();
    let board = match held.as_ref() {
        Some(h) => board_of(
            &h.snap,
            Some(i64::try_from(h.unix_ms).unwrap_or(i64::MAX)),
            disk,
            now_unix_ms,
        ),
        None => board_of(&Snapshot::default(), None, disk, now_unix_ms),
    };
    let words = change_words(&board);
    let moved = {
        let mut kept = BOARD.lock().unwrap_or_else(PoisonError::into_inner);
        let judged = kept.get_or_insert_with(Judged::default);
        let moved = judged.board.as_ref().map(change_words).as_deref() != Some(words.as_str());
        judged.step(board, held.as_ref().map(|h| &*h.snap), sessions);
        moved
    };
    if moved {
        aterm_log::info!("occupancy board: {words}");
    }
}

/// THE BOARD AS SESSION `sid`'S HOST READS IT NOW (`IdleHost::occupancy`, [`Judged::reading`]).
pub(crate) fn reading_for(sid: &str) -> OccupancyReading {
    let now_unix_ms = aterm_time::unix_millis()
        .and_then(|ms| i64::try_from(ms).ok())
        .unwrap_or(i64::MAX);
    BOARD
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map_or_else(OccupancyReading::default, |j| j.reading(sid, now_unix_ms))
}

/// THE PACE FOR SESSION `sid`'S LINE of `rule`, its hold begun at
/// `hold_since_ms` (`IdleHost::occupancy_pace`, [`Judged::pace`]): taken with
/// the board, under its one lock. A wall clock that cannot be read holds the
/// line at the machine's pace.
pub(crate) fn pace_for(sid: &str, rule: RuleId, hold_since_ms: i64) -> PaceGrant {
    let Some(now_unix_ms) = aterm_time::unix_millis().and_then(|ms| i64::try_from(ms).ok()) else {
        return PaceGrant::Machine;
    };
    let grant = BOARD
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(Judged::default)
        .pace(sid, rule, hold_since_ms, now_unix_ms);
    if grant == PaceGrant::Granted {
        aterm_log::info!("occupancy pace @{sid}: {} granted", rule.name());
    }
    grant
}

#[cfg(test)]
#[path = "occupancy_host_tests.rs"]
mod tests;
