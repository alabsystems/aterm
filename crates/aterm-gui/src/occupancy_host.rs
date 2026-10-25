// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Occupancy facts board, pace and reservation table ([`Pace`], [`pace_for`]), between-look
//! sampler ([`sample_pass`]), and facts ring surfacing ([`cmd_occupancy`]).

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::harness::disk::Level;
use aterm_agent::supervise::Occupancy;
use aterm_agent::supervise::memory::Pressure as MemoryPressure;
use aterm_agent::supervise::occupancy::ring::{self, RingRow, RingSession};
use aterm_agent::supervise::occupancy::rules::{self, HOLD_ROOM_MACHINE, HOLD_ROOM_SLOTS};
use aterm_agent::supervise::occupancy::{
    HoldSeen, OccupancyReading, OccupancySaid, PaceGrant, RuleId, SaidVerdict, SlotsSeen,
};
use aterm_sysprobe::slots::{SlotsRead, Unread};
use aterm_uds::process::ProcessStart;
use aterm_vitals::model::{Pressure, ProcSample, SlotsUnread, SlotsView, Snapshot, TabRoot};
use aterm_vitals::sys::predicate::{AwaitSys, Latch, Parsed, evaluate};

use crate::occupancy_ring::RingFile;

/// A board whose snapshot is older than this (two disk looks) is stale.
pub(crate) const STALE_AFTER: Duration =
    Duration::from_millis(aterm_agent::supervise::occupancy::reading::STALE_AFTER_MS as u64);

/// How long the board remembers that a session's tab held a build slot.
pub(crate) const HELD_KEEP: Duration = Duration::from_secs(48 * 3600);

/// While a session under `prompt` has a `hold-room` line due or pending, the board samples its
/// terms this often.
pub(crate) const SAMPLE_EVERY: Duration = aterm_sysprobe::slots::PERIOD;

/// [`SAMPLE_EVERY`] in whole milliseconds.
const SAMPLE_EVERY_MS: i64 = SAMPLE_EVERY.as_millis() as i64;

/// The oldest the figures a slot rule's line types may be at its write.
const FACT_FRESH_MS: i64 = aterm_agent::supervise::facts::FACT_FRESH_MS;

/// The window's look comes this often: a sampler a look starts starts at most this late.
const LOOK_EVERY_MS: i64 = aterm_agent::supervise::occupancy::reading::LOOK_EVERY_MS;

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
    /// When the slot reader read them (unix ms), or `None` where the snapshot says no age.
    pub(crate) slots_as_of_unix_ms: Option<i64>,
    /// The snapshot is older than [`STALE_AFTER`], or there is none.
    pub(crate) stale: bool,
}

/// A load average times 100, rounded; `None` for a value no load is.
fn x100(load: f64) -> Option<u32> {
    (load.is_finite() && load >= 0.0)
        .then(|| (load * 100.0).round().min(f64::from(u32::MAX)) as u32)
}

/// The board of `snap` (`snap_unix_ms` is `None` before the first snapshot), with the home volume
/// at `disk`, taken at `now_unix_ms`.
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
        slots_as_of_unix_ms: snap_unix_ms
            .zip(snap.slots_age_ms)
            .map(|(at, age)| at.saturating_sub(i64::try_from(age).unwrap_or(i64::MAX))),
        stale: snap_unix_ms.is_none_or(|at| now_unix_ms.saturating_sub(at) > stale_ms),
    }
}

/// The slot reader's reading as a snapshot carries it.
pub(crate) const fn slots_view(r: &SlotsRead) -> SlotsView {
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

/// The keys of the tabs that hold a build slot, each once, in order: a holder counts only while
/// its pid is still the process the slot recorded. `None` without a slot reading or a sweep.
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

/// Parse one of `hold-room`'s machine waits (`text`, with its leading `sys`).
fn machine_wait(text: &str) -> Option<AwaitSys> {
    match AwaitSys::parse(text) {
        Ok(Parsed::Machine(wait)) => Some(wait),
        _ => None,
    }
}

/// The facts of `snap` that `hold-room` judges at the board's cadence: readings within
/// [`STALE_AFTER`] speak for the look; older ones keep their age.
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

/// The board and what it judged across its looks.
#[derive(Debug, Default)]
pub(crate) struct Judged {
    /// The board as last taken.
    board: Option<FactsBoard>,
    /// `hold-room`'s slot term over the looks.
    latch: Latch,
    /// How long that term has held, ms, once it spans its `for=`.
    slots_free_ms: Option<u64>,
    /// Whether `hold-room`'s machine terms held at the last look; `None` where they could not be
    /// judged.
    room: Option<bool>,
    /// Each supervised session's last look at which a build slot's holder lay in its tab (unix
    /// ms), kept [`HELD_KEEP`].
    held: BTreeMap<String, i64>,
    /// The machine's pace and the slots reserved ([`Pace`]).
    pace: Pace,
    /// What each session's loop last told ([`told`]), by sid.
    said: BTreeMap<String, OccupancySaid>,
    /// The rows of the last [`ring::RING_ROWS`] minute looks ([`Look::Tick`]), oldest first
    /// ([`Self::look_row`]).
    rows: VecDeque<RingRow>,
    /// The ring's file was read into [`Self::rows`] (at the first look).
    seeded: bool,
    /// The supervised sessions as the last look named them (tab key, sid).
    sessions: Vec<(u64, String)>,
    /// Each session whose loop under `prompt` last asked the board for `hold-room`'s reading,
    /// with when (unix ms).
    wanting: BTreeMap<String, i64>,
    /// Each session's `hold-room` lines its loop told typed in the told hold: the last one's
    /// unix ms and count ([`Self::tell`]).
    rooms_typed: BTreeMap<String, (i64, usize)>,
    /// When the board was last sampled between looks (unix ms).
    sampled_ms: Option<i64>,
    /// A sampler runs ([`start_sampler`]).
    sampler: bool,
}

impl Judged {
    /// One look: `board`, taken from `snap`, judged with the supervised `sessions`.
    pub(crate) fn step(
        &mut self,
        board: FactsBoard,
        snap: Option<&Snapshot>,
        sessions: &[(u64, String)],
    ) {
        sessions.clone_into(&mut self.sessions);
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
        self.pace.retire(&self.held);
        self.board = Some(board);
    }

    /// Record a sample between looks: `board` from `snap`, judged with the last look's sessions
    /// and no ring row.
    pub(crate) fn sample(&mut self, board: FactsBoard, snap: Option<&Snapshot>) {
        self.sampled_ms = Some(board.taken_unix_ms);
        let sessions = std::mem::take(&mut self.sessions);
        self.step(board, snap, &sessions);
    }

    /// Record that session `sid`'s loop asked the board at `now_unix_ms` under `prompt`; any other
    /// `mode` clears every ask ([`Self::sampling`]).
    pub(crate) fn asked(&mut self, sid: &str, now_unix_ms: i64, mode: Occupancy) {
        if mode == Occupancy::Prompt {
            self.wanting.insert(sid.to_string(), now_unix_ms);
        } else {
            self.wanting.clear();
        }
    }

    /// Whether the board samples at `now_unix_ms` under `mode`.
    pub(crate) fn sampling(&self, now_unix_ms: i64, mode: Occupancy) -> bool {
        mode == Occupancy::Prompt
            && (self
                .wanting
                .values()
                .any(|at| now_unix_ms.saturating_sub(*at) <= ASK_KEEP_MS)
                || self.room_lead(now_unix_ms))
    }

    /// Whether a told hold's `hold-room` line is near at `now_unix_ms` ([`rules::room_due_ms`]).
    fn room_lead(&self, now_unix_ms: i64) -> bool {
        let lead_ms = machine_wait(HOLD_ROOM_SLOTS)
            .map_or(0, |w| i64::try_from(w.for_ms).unwrap_or(i64::MAX))
            .saturating_add(LOOK_EVERY_MS);
        self.said.iter().any(|(sid, said)| {
            let Some(hold) = said.hold.filter(|_| said.mode == Occupancy::Prompt) else {
                return false;
            };
            let spent = said
                .rules
                .iter()
                .any(|r| r.rule == RuleId::HoldRoom && r.verdict == SaidVerdict::Spent);
            let typed = self.room_typed(sid, hold.since_ms);
            if spent || typed.is_some_and(|(_, n)| n >= rules::HOLD_ROOM_PER_HOLD) {
                return false;
            }
            let due_ms = rules::room_due_ms(hold.since_ms, typed.map(|(at, _)| at));
            (due_ms.saturating_sub(lead_ms)..=due_ms.saturating_add(ASK_KEEP_MS))
                .contains(&now_unix_ms)
        })
    }

    /// Session `sid`'s `hold-room` lines told typed in its hold begun at `since_ms` (last unix ms
    /// and count).
    fn room_typed(&self, sid: &str, since_ms: i64) -> Option<(i64, usize)> {
        self.rooms_typed
            .get(sid)
            .copied()
            .filter(|(at, _)| since_ms <= *at)
    }

    /// Whether a sampler must start at `now_unix_ms` under `mode`, marking it running under the
    /// caller's lock ([`start_sampler`]).
    pub(crate) fn sampler_due(&mut self, now_unix_ms: i64, mode: Occupancy) -> bool {
        self.sampling(now_unix_ms, mode) && !std::mem::replace(&mut self.sampler, true)
    }

    /// Whether a sample is due at `now_unix_ms` under `mode` (sampling, and none taken within
    /// [`SAMPLE_EVERY`]).
    pub(crate) fn sample_due(&self, now_unix_ms: i64, mode: Occupancy) -> bool {
        self.sampling(now_unix_ms, mode)
            && self
                .sampled_ms
                .is_none_or(|at| now_unix_ms.saturating_sub(at) >= SAMPLE_EVERY_MS)
    }

    /// When the board's figures were read (unix ms), or `None` with no snapshot or no slots age.
    fn figures_ms(&self) -> Option<i64> {
        let b = self.board.as_ref().filter(|b| b.as_of_unix_ms > 0)?;
        Some(b.as_of_unix_ms.min(b.slots_as_of_unix_ms?))
    }

    /// Answer at a line's write for session `sid`'s line of `rule` (hold begun at
    /// `hold_since_ms`) at `now_unix_ms`.
    pub(crate) fn answer(
        &mut self,
        sid: &str,
        rule: RuleId,
        hold_since_ms: i64,
        now_unix_ms: i64,
    ) -> PaceGrant {
        if !rule.slot() {
            return self.pace(sid, rule, hold_since_ms, now_unix_ms);
        }
        let Some(read_ms) = self
            .figures_ms()
            .filter(|at| now_unix_ms.saturating_sub(*at) <= FACT_FRESH_MS)
        else {
            return PaceGrant::Stale;
        };
        let reading = OccupancyReading {
            taken_ms: read_ms,
            ..self.reading(sid, now_unix_ms)
        };
        let hold = HoldSeen {
            since_ms: hold_since_ms,
            declared: false,
        };
        if !rules::ready(&reading, Some(hold), now_unix_ms) {
            return PaceGrant::Unmet;
        }
        match self.pace(sid, rule, hold_since_ms, now_unix_ms) {
            PaceGrant::Granted => PaceGrant::Room(reading),
            held => held,
        }
    }

    /// Record what session `sid`'s loop last told ([`told`]); `None` forgets it.
    pub(crate) fn tell(&mut self, sid: &str, said: Option<OccupancySaid>) {
        match said {
            Some(said) => {
                if let Some(hold) = said.hold {
                    let typed = said
                        .rules
                        .iter()
                        .find(|r| r.rule == RuleId::HoldRoom && r.verdict == SaidVerdict::Typed)
                        .map(|r| r.at_ms);
                    let last = self.room_typed(sid, hold.since_ms);
                    let booked = match (typed, last) {
                        (Some(at), Some((last_at, n))) if at > last_at => Some((at, n + 1)),
                        (Some(at), None) => Some((at, 1)),
                        (_, last) => last,
                    };
                    match booked {
                        Some(booked) => self.rooms_typed.insert(sid.to_string(), booked),
                        None => self.rooms_typed.remove(sid),
                    };
                }
                self.said.insert(sid.to_string(), said);
            }
            None => {
                self.said.remove(sid);
                self.rooms_typed.remove(sid);
            }
        }
    }

    /// What session `sid`'s loop last told.
    pub(crate) fn said(&self, sid: &str) -> Option<&OccupancySaid> {
        self.said.get(sid)
    }

    /// Every session whose loop told a word this window still holds ([`Self::tell`]).
    pub(crate) fn told_sids(&self) -> Vec<String> {
        self.said.keys().cloned().collect()
    }

    /// Seed rows left by a predecessor's ring (oldest first), taken once before this window's
    /// first row.
    pub(crate) fn seed_rows(&mut self, rows: Vec<RingRow>) {
        if std::mem::replace(&mut self.seeded, true) {
            return;
        }
        let keep = rows.len().saturating_sub(ring::RING_ROWS);
        self.rows = rows.into_iter().skip(keep).collect();
    }

    /// Build the look's row at `now_unix_ms`, kept in [`Self::rows`] and returned for the ring's
    /// file: a session reads ready where its hold does, or where `met` names it (a ready wake of
    /// its book whose need holds, the plan's F14).
    pub(crate) fn look_row(
        &mut self,
        sessions: &[(u64, String)],
        met: &[String],
        now_unix_ms: i64,
    ) -> RingRow {
        let rows = sessions
            .iter()
            .map(|(_, sid)| {
                let reading = self.reading(sid, now_unix_ms);
                let said = self.said.get(sid);
                let hold = said.and_then(|s| s.hold).map(|h| HoldSeen {
                    since_ms: h.since_ms,
                    declared: h.declared,
                });
                RingSession {
                    sid: sid.clone(),
                    held_ms: reading.held_slot_last_ms,
                    ready: rules::ready(&reading, hold, now_unix_ms) || met.contains(sid),
                    guard: said
                        .and_then(OccupancySaid::guard)
                        .map(|g| g.word().to_string()),
                }
            })
            .collect();
        let row = RingRow::of(&self.reading("", now_unix_ms), rows);
        self.rows.push_back(row.clone());
        while self.rows.len() > ring::RING_ROWS {
            self.rows.pop_front();
        }
        row
    }

    /// The row a look of `look` takes at `now_unix_ms` ([`Self::look_row`]): a minute tick takes
    /// one; a poke takes none.
    pub(crate) fn looked(
        &mut self,
        sessions: &[(u64, String)],
        met: &[String],
        now_unix_ms: i64,
        look: Look,
    ) -> Option<RingRow> {
        (look == Look::Tick).then(|| self.look_row(sessions, met, now_unix_ms))
    }

    /// The sessions whose hold read ready at the last look.
    pub(crate) fn ready_now(&self) -> u16 {
        self.rows.back().map_or(0, RingRow::ready)
    }

    /// Whole slot-minutes idle while a session was ready over the day before `now_unix_ms`
    /// ([`ring::idle_ready_ms`]).
    pub(crate) fn idle_ready_min(&self, now_unix_ms: i64) -> u64 {
        ring::idle_ready_ms(&self.rows, now_unix_ms.saturating_sub(rules::DAY_MS)) / 60_000
    }

    /// The looks' rows from `since_ms` on, oldest first.
    pub(crate) fn rows_since(&self, since_ms: i64) -> Vec<RingRow> {
        self.rows
            .iter()
            .filter(|r| r.at >= since_ms)
            .cloned()
            .collect()
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

    /// Ask the pace for a line of `rule` in session `sid`'s hold begun at `hold_since_ms` at
    /// `now_unix_ms` ([`Pace::ask`]).
    pub(crate) fn pace(
        &mut self,
        sid: &str,
        rule: RuleId,
        hold_since_ms: i64,
        now_unix_ms: i64,
    ) -> PaceGrant {
        let slot = rule.slot();
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
            pool: Pool::Facts,
        };
        self.pace.ask(&ask, now_unix_ms)
    }
}

/// At most this many fact lines across the machine in any hour.
pub(crate) const MACHINE_LINES_PER_HOUR: usize = 6;

/// At most this many typed wake lines across the machine in any hour: a pool of their own, so a
/// wake line never spends the facts pool and is never refused by it (`hold-room` keeps its
/// lines).
pub(crate) const WAKE_LINES_PER_HOUR: usize = 6;

/// The machine pace's window, ms.
const HOUR_MS: i64 = 60 * 60_000;

/// A slot rule's line reserves one free build slot this long.
pub(crate) const RESERVE_MS: i64 = 10 * 60_000;

// A reservation lapses before its session's next line may go (every door's lines share the
// session's pace), so a holder's re-ask granted against it is its drawn line asked again, never a
// second line typed.
const _: () = assert!(RESERVE_MS < rules::SESSION_PACE_MS);

/// How long a hold refused for want of a slot stands first in line after its last ask.
pub(crate) const ASK_KEEP_MS: i64 = aterm_agent::supervise::occupancy::reading::STALE_AFTER_MS;

/// Which machine pool a line draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pool {
    /// A fact rule's line ([`MACHINE_LINES_PER_HOUR`]).
    Facts,
    /// A declared wake's typed line ([`WAKE_LINES_PER_HOUR`]).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the wake door's claim asks it; only the tests do yet"
        )
    )]
    Wake,
}

impl Pool {
    /// Its lines in any hour.
    const fn per_hour(self) -> usize {
        match self {
            Pool::Facts => MACHINE_LINES_PER_HOUR,
            Pool::Wake => WAKE_LINES_PER_HOUR,
        }
    }
}

/// One ask of the table.
struct Ask<'a> {
    sid: &'a str,
    /// The rule's line needs a free build slot.
    slot: bool,
    /// When the asking session's hold began, unix ms: the oldest first.
    hold_since_ms: i64,
    /// The build slots the board proves usable now.
    usable: u16,
    /// The machine pool its line draws.
    pool: Pool,
}

/// A slot reservation's answer ([`Pace::reserve`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reserve {
    /// A slot is reserved for it: its own, re-asked (kept as it was taken), or one left free.
    Granted,
    /// No free slot is left for it: it waits, by its age.
    Reserved,
}

/// A build slot reserved for a session's line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reservation {
    sid: String,
    /// When it was taken, unix ms.
    since_ms: i64,
    /// The pool whose line its holder drew for it: `None` for an `await wake` answer's, which
    /// drew none, so the holder's first line still draws one.
    line: Option<Pool>,
}

/// A hold refused for want of a slot, waiting.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Waiting {
    sid: String,
    hold_since_ms: i64,
    /// Its last ask, unix ms.
    asked_ms: i64,
}

/// The machine pace and the slot reservation table.
#[derive(Debug, Default, Clone)]
pub(crate) struct Pace {
    /// When each fact line of the last hour was granted, unix ms.
    lines: Vec<i64>,
    /// When each typed wake line of the last hour was granted, unix ms.
    wake_lines: Vec<i64>,
    /// The slots reserved, each for [`RESERVE_MS`] from its taking.
    reserved: Vec<Reservation>,
    /// The holds that asked for a slot and found none, by their age.
    waiting: Vec<Waiting>,
}

impl Pace {
    /// `pool`'s lines of the last hour at `now_ms`.
    fn pool(&mut self, pool: Pool, now_ms: i64) -> &mut Vec<i64> {
        let lines = match pool {
            Pool::Facts => &mut self.lines,
            Pool::Wake => &mut self.wake_lines,
        };
        lines.retain(|at| now_ms.saturating_sub(*at) < HOUR_MS);
        lines
    }

    /// Forget the reservations past [`RESERVE_MS`] and the asks past [`ASK_KEEP_MS`] at `now_ms`.
    fn age(&mut self, now_ms: i64) {
        self.reserved
            .retain(|r| now_ms.saturating_sub(r.since_ms) < RESERVE_MS);
        self.waiting
            .retain(|w| now_ms.saturating_sub(w.asked_ms) <= ASK_KEEP_MS);
    }

    /// `sid`'s live reservation, against which its re-ask is granted, kept as it was taken.
    fn holds(&mut self, sid: &str, now_ms: i64) -> Option<&mut Reservation> {
        if fault::armed(fault::Fault::CountOwn) {
            return None;
        }
        let own = self.reserved.iter_mut().find(|r| r.sid == sid)?;
        if fault::armed(fault::Fault::RenewOnAsk) {
            own.since_ms = now_ms;
        }
        Some(own)
    }

    /// Answer `ask` at `now_ms` with the machine's pace and slot reservation taken together. A
    /// slot line's re-ask by a session holding the reservation its line drew from the same pool
    /// (its write skipped, its loop asking again) is granted against that reservation: kept as it
    /// was taken, and no second line drawn. Any other line draws its pool's hourly lines (an
    /// `await wake` reservation's holder too, once), and a slot line takes its reservation
    /// ([`Self::reserve`]).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "WakeRoom",
            action = "ReserveA",
            project = "occupancy_host::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "WakeRoom",
            action = "ReserveB",
            project = "occupancy_host::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "WakeRoom",
            action = "ReserveC",
            project = "occupancy_host::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "WakeRoom",
            action = "RefuseA",
            project = "occupancy_host::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "WakeRoom",
            action = "RefuseB",
            project = "occupancy_host::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "WakeRoom",
            action = "RefuseC",
            project = "occupancy_host::conformance::project"
        )
    )]
    fn ask(&mut self, ask: &Ask<'_>, now_ms: i64) -> PaceGrant {
        self.age(now_ms);
        if ask.slot
            && self
                .holds(ask.sid, now_ms)
                .is_some_and(|own| own.line == Some(ask.pool))
        {
            return PaceGrant::Granted;
        }
        if self.pool(ask.pool, now_ms).len() >= ask.pool.per_hour() {
            return PaceGrant::Machine;
        }
        if ask.slot
            && self.reserve(ask.sid, ask.hold_since_ms, ask.usable, now_ms) == Reserve::Reserved
        {
            return PaceGrant::Reserved;
        }
        self.pool(ask.pool, now_ms).push(now_ms);
        if ask.slot
            && let Some(own) = self.reserved.iter_mut().find(|r| r.sid == ask.sid)
        {
            own.line = Some(ask.pool);
        }
        PaceGrant::Granted
    }

    /// Reserve a free build slot for session `sid`, aged from `age_ms` (its open hold's start,
    /// else its wake's ready since), with `usable` slots the board proves at `now_ms`: an `await
    /// wake` answer whose need names slots takes one here, in every mode but off. Granted where
    /// `sid`'s own live reservation stands (kept as it was taken), or where a slot is left once
    /// the other sessions' reservations and the older holds waiting are counted; else it waits,
    /// by its age. It never draws a line pool.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "WakeRoom",
            action = "ReserveA",
            project = "occupancy_host::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "WakeRoom",
            action = "ReserveB",
            project = "occupancy_host::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "WakeRoom",
            action = "ReserveC",
            project = "occupancy_host::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "WakeRoom",
            action = "RefuseA",
            project = "occupancy_host::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "WakeRoom",
            action = "RefuseB",
            project = "occupancy_host::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "WakeRoom",
            action = "RefuseC",
            project = "occupancy_host::conformance::project"
        )
    )]
    pub(crate) fn reserve(&mut self, sid: &str, age_ms: i64, usable: u16, now_ms: i64) -> Reserve {
        self.age(now_ms);
        if self.holds(sid, now_ms).is_some() {
            return Reserve::Granted;
        }
        let mine = (age_ms, sid);
        let others = self
            .reserved
            .iter()
            .filter(|r| r.sid != sid || fault::armed(fault::Fault::CountOwn))
            .count();
        let older = self
            .waiting
            .iter()
            .filter(|w| w.sid != sid && (w.hold_since_ms, w.sid.as_str()) < mine)
            .filter(|_| !fault::armed(fault::Fault::YoungestFirst))
            .count();
        self.waiting.retain(|w| w.sid != sid);
        if usize::from(usable) <= others + older {
            self.waiting.push(Waiting {
                sid: sid.to_string(),
                hold_since_ms: age_ms,
                asked_ms: now_ms,
            });
            return Reserve::Reserved;
        }
        self.reserved.retain(|r| r.sid != sid);
        self.reserved.push(Reservation {
            sid: sid.to_string(),
            since_ms: now_ms,
            line: None,
        });
        Reserve::Granted
    }

    /// Lapse every reservation whose session's tab was seen holding a build slot since it was
    /// taken (`held`: each session's last such look, unix ms): its build has the slot.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "WakeRoom",
            action = "LapseA",
            project = "occupancy_host::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "WakeRoom",
            action = "LapseB",
            project = "occupancy_host::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "WakeRoom",
            action = "LapseC",
            project = "occupancy_host::conformance::project"
        )
    )]
    fn retire(&mut self, held: &BTreeMap<String, i64>) {
        self.reserved
            .retain(|r| held.get(&r.sid).is_none_or(|at| *at < r.since_ms));
    }
}

/// The negative controls' fault points in the pace ([`conformance`], the pace tests) and in the
/// task record's fact word (`task_record::fact_edit`).
#[cfg(test)]
pub(crate) mod fault {
    use std::cell::Cell;

    /// A planted defect.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Fault {
        /// A holder's re-ask renews its reservation from the ask.
        RenewOnAsk,
        /// A slot ask counts no older hold waiting.
        YoungestFirst,
        /// A holder's re-ask counts its own reservation against it.
        CountOwn,
        /// The fact word's edit moves the record's `rev` too, so every `await task` past a rev
        /// with news standing wakes on it.
        NoteBumpsNews,
        /// An `await wake` answer's slot is asked of the typed lines' pace (`pace_on`), which
        /// under `observe` grants nothing.
        PaceOnAwait,
        /// An `await wake` answer whose need names slots takes no reservation.
        AwaitSkipsReserve,
        /// The wake K clock is fed only from a loop's break, which no loop of this build runs.
        KFromBreakOnly,
        /// A restored book's `pid:` atoms are not armed again: their births are not carried over.
        NoRearm,
    }

    thread_local! {
        static ARMED: Cell<Option<Fault>> = const { Cell::new(None) };
    }

    /// Run `f` with `fault` armed on this thread, disarmed after it even on unwind.
    pub(crate) fn with<T>(fault: Fault, f: impl FnOnce() -> T) -> T {
        struct Disarm;
        impl Drop for Disarm {
            fn drop(&mut self) {
                ARMED.with(|a| a.set(None));
            }
        }
        ARMED.with(|a| a.set(Some(fault)));
        let _disarm = Disarm;
        f()
    }

    /// Whether `fault` is armed on this thread.
    pub(crate) fn armed(fault: Fault) -> bool {
        ARMED.with(|a| a.get() == Some(fault))
    }
}

#[cfg(not(test))]
pub(crate) mod fault {
    /// A planted defect: none is armed outside the tests.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Fault {
        RenewOnAsk,
        YoungestFirst,
        CountOwn,
        NoteBumpsNews,
        PaceOnAwait,
        AwaitSkipsReserve,
        KFromBreakOnly,
        NoRearm,
    }

    /// Never armed.
    pub(crate) const fn armed(_: Fault) -> bool {
        false
    }
}

/// The steps of `WakeRoom` (`aterm_models_harness::wake_room_model`) that no function here
/// performs: a session's line or answer coming due, its write or skip, a build taking the slot
/// and a slot freeing, and the mutants. The reservation, the refusal and the lapse bind on
/// [`Pace::ask`], [`Pace::reserve`] and [`Pace::retire`]. Proof and test builds only.
#[cfg(any(test, debug_assertions))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "WakeRoom",
    action = "ReadyA",
    reason = "The loop's: a line or an answer that needs a slot comes due in session A."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "WakeRoom",
    action = "ReadyB",
    reason = "The loop's: a line or an answer that needs a slot comes due in session B."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "WakeRoom",
    action = "ReadyC",
    reason = "The loop's: a line or an answer that needs a slot comes due in session C."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "WakeRoom",
    action = "WriteA",
    reason = "The loop's write of session A's granted line or answer; the table only reserved."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "WakeRoom",
    action = "WriteB",
    reason = "The loop's write of session B's granted line or answer; the table only reserved."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "WakeRoom",
    action = "WriteC",
    reason = "The loop's write of session C's granted line or answer; the table only reserved."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "WakeRoom",
    action = "SkipA",
    reason = "The loop skips session A's granted write (a guard at the write): its re-ask is \
              the next ReserveA."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "WakeRoom",
    action = "SkipB",
    reason = "The loop skips session B's granted write: its re-ask is the next ReserveB."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "WakeRoom",
    action = "SkipC",
    reason = "The loop skips session C's granted write: its re-ask is the next ReserveC."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "WakeRoom",
    action = "BuildTakes",
    reason = "The world's: a build takes the free slot; the board's usable count says it."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "WakeRoom",
    action = "SlotFrees",
    reason = "The world's: a slot frees; the board's usable count says it."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "WakeRoom",
    action = "RenewWhileWaited",
    reason = "Mutant: a reservation that never lapses while another hold waits."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "WakeRoom",
    action = "HerdTell",
    reason = "Mutant: session B's ask counts no other session's reservation."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "WakeRoom",
    action = "YoungestFirst",
    reason = "Mutant: session C's ask counts no older hold waiting (fault YoungestFirst)."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "WakeRoom",
    action = "RenewOnAsk",
    reason = "Mutant: a holder's re-ask renews its reservation (fault RenewOnAsk)."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "WakeRoom",
    action = "CountOwn",
    reason = "Mutant: a holder's re-ask counts its own reservation and is refused (fault \
              CountOwn)."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, not an \
              expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn wake_room_environment_waivers() {}

/// The kernel's memory pressure as the supervisor names it; `None` unread.
const fn memory_pressure(p: Pressure) -> Option<MemoryPressure> {
    match p {
        Pressure::Normal => Some(MemoryPressure::Normal),
        Pressure::Warn => Some(MemoryPressure::Warn),
        Pressure::Critical => Some(MemoryPressure::Critical),
        Pressure::Unknown => None,
    }
}

/// Format `b`'s slots and disk level for the occupancy change log.
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

/// How many waits name a reader now ([`Arming::arm`]).
#[derive(Debug, Default)]
pub(crate) struct Arming {
    armed: AtomicUsize,
    /// Lines logged when the first is armed and when the last is gone; `None` logs nothing.
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

    /// None armed; `armed` is logged when the first is, `at_rest` when the last is gone.
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

/// Armed waits on the build slots (both edges logged for `lab slot-lane`).
pub(crate) static SLOTS_ARMED: Arming = Arming::logged(
    aterm_sysprobe::slots::ARMED_LINE,
    aterm_sysprobe::slots::AT_REST_LINE,
);

/// Armed fresh reads when a slot rule's write re-reads the board ([`pace_for`]).
pub(crate) static SLOTS_NOW: Arming = Arming::new();

/// Max age of a slots reading the probe thread hands back
/// ([`aterm_sysprobe::slots::SlotReader::read_within`]).
pub(crate) fn slots_period() -> Duration {
    use aterm_sysprobe::slots::{IDLE_PERIOD, PERIOD, REREAD};
    if SLOTS_NOW.armed() {
        REREAD
    } else if SLOTS_ARMED.armed() {
        PERIOD
    } else {
        IDLE_PERIOD
    }
}

/// The board and what it judged as last taken ([`look`]); `None` before the first look.
static BOARD: Mutex<Option<Judged>> = Mutex::new(None);

/// The home volume's level on the disk board, where the watch judged it.
fn home_level() -> Option<Level> {
    let home = aterm_types::dirs::home_dir()?;
    let board = crate::disk_board::SHELF.board_lock();
    board.row_at(&home).map(|r| r.level)
}

/// The owner's `[harness] occupancy` mode ([`set_mode`]): 0 `off`, 1 `observe`, 2 `prompt`.
static MODE: AtomicU8 = AtomicU8::new(1);

#[cfg(test)]
thread_local! {
    /// Thread-local occupancy mode override for parallel tests ([`set_mode`]).
    static MODE_FOR_TEST: std::cell::Cell<Option<Occupancy>> = const { std::cell::Cell::new(None) };
}

/// Set the owner's occupancy mode in force (`App::note_occupancy_mode`).
pub(crate) fn set_mode(mode: Occupancy) {
    #[cfg(test)]
    MODE_FOR_TEST.with(|m| m.set(Some(mode)));
    #[cfg(not(test))]
    MODE.store(
        match mode {
            Occupancy::Off => 0,
            Occupancy::Observe => 1,
            Occupancy::Prompt => 2,
        },
        Ordering::Release,
    );
}

/// The owner's mode ([`set_mode`]).
pub(crate) fn mode() -> Occupancy {
    #[cfg(test)]
    if let Some(mode) = MODE_FOR_TEST.with(std::cell::Cell::get) {
        return mode;
    }
    match MODE.load(Ordering::Acquire) {
        0 => Occupancy::Off,
        2 => Occupancy::Prompt,
        _ => Occupancy::Observe,
    }
}

/// The GUI's own gate on a write of the occupancy door's (the occupancy design's §8 row 2):
/// whether the owner's mode in force ([`mode`], from their root `[harness] occupancy`) is
/// `prompt`. Every host-side writer (a wake's claim, the task record's fact word, a ready-idle
/// Entry, a wake's event) asks it beside the loop's own grant, so a word a loop tells, or a replay
/// or an older loop says, never stands for the owner's `prompt`.
pub(crate) fn prompt_now() -> bool {
    mode() == Occupancy::Prompt
}

/// Which look the board takes: the minute tick (which records a ring row) or a poke between ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Look {
    Tick,
    Poke,
}

/// Append a tick's `row` to `file` while `mode` observes (`observe` or `prompt`).
///
/// # Errors
/// The file's append failed ([`RingFile::append`]).
pub(crate) fn append_look(
    file: &mut RingFile,
    mode: Occupancy,
    look: Look,
    row: Option<&RingRow>,
) -> std::io::Result<bool> {
    match row {
        Some(row) if look == Look::Tick && mode.observes() => file.append(row).map(|()| true),
        _ => Ok(false),
    }
}

/// The facts ring's file as this window writes it; `None` before the first look or with no state
/// directory.
static RING: Mutex<Option<RingFile>> = Mutex::new(None);

/// Record what session `sid`'s loop told (`IdleHost::occupancy_said`).
pub(crate) fn told(sid: &str, said: Option<OccupancySaid>) {
    BOARD
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(Judged::default)
        .tell(sid, said);
}

/// What session `sid`'s loop last told this window ([`told`]).
pub(crate) fn said_of(sid: &str) -> Option<OccupancySaid> {
    BOARD
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .and_then(|j| j.said(sid).cloned())
}

/// Reserve a free build slot for session `sid`'s `await wake` answer on the board's table
/// ([`Pace::reserve`]): aged from `age_ms`, with `usable` slots its snapshot proves at `now_ms`.
pub(crate) fn reserve_for_await(sid: &str, age_ms: i64, usable: u16, now_ms: i64) -> Reserve {
    BOARD
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(Judged::default)
        .pace
        .reserve(sid, age_ms, usable, now_ms)
}

/// Every session whose loop told this window a word ([`Judged::told_sids`]).
pub(crate) fn told_sids() -> Vec<String> {
    BOARD
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map(Judged::told_sids)
        .unwrap_or_default()
}

/// Take the board at `now_unix_ms` (the disk look's, of `kind`).
pub(crate) fn look(now_unix_ms: i64, sessions: &[(u64, String)], kind: Look) {
    let seed = {
        let mut ring = RING.lock().unwrap_or_else(PoisonError::into_inner);
        if ring.is_none() {
            *ring =
                aterm_types::dirs::state_dir().map(|state| RingFile::at(ring::ring_path(&state)));
            ring.as_ref().map(RingFile::read)
        } else {
            None
        }
    };
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
    // Read under the wake books' lock, let go before the board's is taken.
    let met: Vec<String> = sessions
        .iter()
        .map(|(_, sid)| sid)
        .filter(|sid| crate::occupancy_wakes::met(sid))
        .cloned()
        .collect();
    let (moved, row, start) = {
        let mut kept = BOARD.lock().unwrap_or_else(PoisonError::into_inner);
        let judged = kept.get_or_insert_with(Judged::default);
        let moved = judged.board.as_ref().map(change_words).as_deref() != Some(words.as_str());
        judged.step(board, held.as_ref().map(|h| &*h.snap), sessions);
        if let Some(rows) = seed {
            judged.seed_rows(rows);
        }
        let start = judged.sampler_due(now_unix_ms, mode());
        (
            moved,
            judged.looked(sessions, &met, now_unix_ms, kind),
            start,
        )
    };
    if start {
        start_sampler();
    }
    if moved {
        aterm_log::info!("occupancy board: {words}");
    }
    if let Some(file) = RING.lock().unwrap_or_else(PoisonError::into_inner).as_mut()
        && let Err(e) = append_look(file, mode(), kind, row.as_ref())
    {
        aterm_log::warn!("occupancy ring: {} not written: {e}", file.path().display());
    }
}

/// Occupancy status fields for session `sid` (`occupancy`, `occupancy_hold`, `ready`, `slots`,
/// `idle_ready_min`).
pub(crate) fn status_fields(sid: Option<&str>) -> String {
    let now_unix_ms = aterm_time::unix_millis()
        .and_then(|ms| i64::try_from(ms).ok())
        .unwrap_or(0);
    let board = BOARD.lock().unwrap_or_else(PoisonError::into_inner);
    status_words(mode(), board.as_ref(), sid, now_unix_ms)
}

/// [`status_fields`] from `judged` at `now_unix_ms`.
pub(crate) fn status_words(
    mode: Occupancy,
    judged: Option<&Judged>,
    sid: Option<&str>,
    now_unix_ms: i64,
) -> String {
    let hold = judged
        .zip(sid)
        .and_then(|(j, sid)| j.said(sid))
        .map_or("none", OccupancySaid::hold_word);
    let slots = judged
        .map(|j| j.reading("", now_unix_ms))
        .and_then(|r| r.slots)
        .map_or_else(|| "-".to_string(), |s| format!("{}/{}", s.usable, s.n));
    format!(
        "occupancy={} occupancy_hold={hold} ready={} slots={slots} idle_ready_min={}",
        mode.word(),
        judged.map_or(0, Judged::ready_now),
        judged.map_or(0, |j| j.idle_ready_min(now_unix_ms)),
    )
}

/// The verb's usage.
const USAGE: &str = "ERR usage: occupancy why|check --replay <dur>\n";

/// Handle `occupancy` (`why` or `check --replay <dur>`) on the control socket for session `sid`.
pub(crate) fn cmd_occupancy(sid: Option<&str>, rest: &str) -> String {
    let now_unix_ms = aterm_time::unix_millis()
        .and_then(|ms| i64::try_from(ms).ok())
        .unwrap_or(0);
    let offset_s = crate::presence::local_offset_s().unwrap_or(0);
    let words: Vec<&str> = rest.split_whitespace().collect();
    match words.as_slice() {
        [] | ["why"] => {
            let said = sid.and_then(|sid| {
                BOARD
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                    .and_then(|j| j.said(sid).cloned())
            });
            why_reply(mode(), said.as_ref(), now_unix_ms, offset_s)
        }
        ["check", "--replay", dur] => {
            let Some(window_ms) = aterm_agent::supervise::occupancy::replay::window_ms(dur) else {
                return USAGE.to_string();
            };
            let Some(sid) = sid else {
                return "ERR occupancy: no session\n".to_string();
            };
            let from_ms = now_unix_ms.saturating_sub(window_ms);
            let rows = BOARD
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .map(|j| j.rows_since(from_ms))
                .unwrap_or_default();
            let ledger = aterm_types::dirs::state_dir()
                .map(|state| aterm_agent::supervise::approvals::ledger_under(&state, Some(sid)))
                .and_then(|path| std::fs::read_to_string(path).ok())
                .unwrap_or_default();
            replay_reply(sid, &rows, &ledger, (from_ms, now_unix_ms), offset_s)
        }
        _ => USAGE.to_string(),
    }
}

/// `occupancy why`'s reply for `said` ([`cmd_occupancy`]).
pub(crate) fn why_reply(
    mode: Occupancy,
    said: Option<&OccupancySaid>,
    now_unix_ms: i64,
    offset_s: i64,
) -> String {
    let lines = match said {
        Some(said) => {
            aterm_agent::supervise::occupancy::said::why_lines(said, now_unix_ms, offset_s)
        }
        None => vec![format!("occupancy={} unsaid", mode.word())],
    };
    lines_reply(&lines)
}

/// Format `occupancy check --replay`'s reply for session `sid` over `rows` and `ledger` in
/// `(from_ms, to_ms)`.
pub(crate) fn replay_reply(
    sid: &str,
    rows: &[RingRow],
    ledger: &str,
    (from_ms, to_ms): (i64, i64),
    offset_s: i64,
) -> String {
    use aterm_agent::supervise::occupancy::{hold, replay};
    let spans = replay::spans_of(&hold::hold_rows_in(ledger, Some(sid)));
    let replayed = replay::replay(rows, &spans, sid, from_ms);
    let holds = spans
        .iter()
        .filter(|s| s.end_ms.is_none_or(|end| end >= from_ms) && s.since_ms <= to_ms)
        .count();
    lines_reply(&replay::replay_lines(
        &replayed,
        rows.len(),
        holds,
        (from_ms, to_ms),
        offset_s,
    ))
}

/// `OK <n>` and the lines.
fn lines_reply(lines: &[String]) -> String {
    let mut out = format!("OK {}\n", lines.len());
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Read the board as session `sid`'s host reads it now (`IdleHost::occupancy`,
/// [`Judged::reading`]), starting the sampler under `prompt` if due.
pub(crate) fn reading_for(sid: &str) -> OccupancyReading {
    let Some(now_unix_ms) = aterm_time::unix_millis().and_then(|ms| i64::try_from(ms).ok()) else {
        return BOARD
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map_or_else(OccupancyReading::default, |j| j.reading(sid, i64::MAX));
    };
    let (reading, start) = {
        let mut kept = BOARD.lock().unwrap_or_else(PoisonError::into_inner);
        let judged = kept.get_or_insert_with(Judged::default);
        judged.asked(sid, now_unix_ms, mode());
        let start = judged.sampler_due(now_unix_ms, mode());
        (judged.reading(sid, now_unix_ms), start)
    };
    if start {
        start_sampler();
    }
    reading
}

/// Query the pace for session `sid`'s line of `rule` (hold begun at `hold_since_ms`), re-reading
/// the board first for a slot rule under `prompt`.
pub(crate) fn pace_for(sid: &str, rule: RuleId, hold_since_ms: i64) -> PaceGrant {
    let mut reread = || {
        let _now = SLOTS_NOW.arm();
        match crate::sys_host::SYS.fresh(Duration::ZERO, true) {
            (Some(held), aterm_vitals::sys::Freshness::Fresh) => Some(Reread {
                landed_ms: i64::try_from(held.unix_ms).unwrap_or(i64::MAX),
                snap: held.snap,
                disk: home_level(),
            }),
            _ => None,
        }
    };
    let mut now = || aterm_time::unix_millis().and_then(|ms| i64::try_from(ms).ok());
    let grant = pace_on(
        &BOARD,
        mode(),
        (sid, rule, hold_since_ms),
        &mut reread,
        &mut now,
    );
    if grant.lets() {
        aterm_log::info!("occupancy pace @{sid}: {} {}", rule.name(), grant.word());
    }
    grant
}

/// The board re-read for a slot rule's write ([`pace_on`]).
pub(crate) struct Reread {
    /// The snapshot.
    pub(crate) snap: Arc<Snapshot>,
    /// When it landed, unix ms.
    pub(crate) landed_ms: i64,
    /// The disk's level ([`home_level`]).
    pub(crate) disk: Option<Level>,
}

/// Evaluate [`pace_for`] on `kept` under `mode` for session `sid`'s line of `rule` in its hold
/// begun at `hold_since_ms` ([`Judged::answer`]).
pub(crate) fn pace_on(
    kept: &Mutex<Option<Judged>>,
    mode: Occupancy,
    (sid, rule, hold_since_ms): (&str, RuleId, i64),
    reread: &mut dyn FnMut() -> Option<Reread>,
    now: &mut dyn FnMut() -> Option<i64>,
) -> PaceGrant {
    if rule.slot() && mode != Occupancy::Prompt {
        return PaceGrant::Stale;
    }
    let reread = if rule.slot() { reread() } else { None };
    let Some(now_unix_ms) = now() else {
        return PaceGrant::Machine;
    };
    let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
    let judged = kept.get_or_insert_with(Judged::default);
    judged.asked(sid, now_unix_ms, mode);
    if let Some(r) = reread {
        judged.sample(
            board_of(&r.snap, Some(r.landed_ms), r.disk, now_unix_ms),
            Some(&r.snap),
        );
    }
    judged.answer(sid, rule, hold_since_ms, now_unix_ms)
}

/// Run one pass of the sampler over `kept` at `now_unix_ms` under `mode`, returning `false` once
/// no session under `prompt` wants the board.
pub(crate) fn sample_pass(
    kept: &Mutex<Option<Judged>>,
    mode: Occupancy,
    now_unix_ms: i64,
    read: &mut dyn FnMut(i64) -> Option<(FactsBoard, Arc<Snapshot>)>,
) -> bool {
    {
        let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
        let judged = kept.get_or_insert_with(Judged::default);
        if !judged.sampling(now_unix_ms, mode) {
            judged.sampler = false;
            return false;
        }
        if !judged.sample_due(now_unix_ms, mode) {
            return true;
        }
    }
    if let Some((board, snap)) = read(now_unix_ms) {
        kept.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(Judged::default)
            .sample(board, Some(&snap));
    }
    true
}

/// Spawn the sampler thread, arming the build slots and running [`sample_pass`] every
/// [`SAMPLE_EVERY`].
fn start_sampler() {
    let spawned = std::thread::Builder::new()
        .name(format!(
            "{}occupancy-sample",
            crate::harness_host::THREAD_PREFIX
        ))
        .spawn(|| {
            let _armed = SLOTS_ARMED.arm();
            let mut read = |now_unix_ms: i64| {
                let (held, _) = crate::sys_host::SYS.fresh(SAMPLE_EVERY, false);
                let held = held?;
                let landed = i64::try_from(held.unix_ms).unwrap_or(i64::MAX);
                let board = board_of(&held.snap, Some(landed), home_level(), now_unix_ms);
                Some((board, held.snap))
            };
            loop {
                let began = Instant::now();
                let Some(now_unix_ms) =
                    aterm_time::unix_millis().and_then(|ms| i64::try_from(ms).ok())
                else {
                    BOARD
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .get_or_insert_with(Judged::default)
                        .sampler = false;
                    return;
                };
                if !sample_pass(&BOARD, mode(), now_unix_ms, &mut read) {
                    return;
                }
                std::thread::sleep(SAMPLE_EVERY.saturating_sub(began.elapsed()));
            }
        });
    if spawned.is_err() {
        aterm_log::warn!("occupancy board: the sampler did not start; the looks alone judge");
        BOARD
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(Judged::default)
            .sampler = false;
    }
}

#[cfg(test)]
#[path = "occupancy_host_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "occupancy_host_conformance.rs"]
mod conformance;
