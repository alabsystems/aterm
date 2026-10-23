// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OCCUPANCY FACTS BOARD (`docs/DESIGN-harness-occupancy-2026-10-20.md`,).
//! Under the same lock it keeps the pace and reservation table (design §4.2, [`Pace`]),
//! which a loop under `occupancy = "prompt"` takes before it books its intent ([`pace_for`]).
//!
//! SAMPLING (§3.1). The board is taken on the window's minute look. Under the owner's `prompt` a
//! sampler takes it every [`SAMPLE_EVERY`] between the looks, the build slots read at that cadence
//! ([`SLOTS_ARMED`]), so `slots>=1 for=20m` is judged on those samples ([`sample_pass`]): while a
//! session's loop has a `hold-room` line due or pending (it asks the board's reading only then,
//! once a look: [`Judged::asked`]), and from the term's `for=` before a told hold's line can go
//! (started at the look: [`Judged::room_lead`]). Under `observe` and `off` nothing samples: the
//! looks alone.
//! At the line's write the host's pace re-reads the board ([`pace_for`]: a reading taken for the
//! call, the slots read anew in it, [`SLOTS_NOW`]) and answers the figures the line types, no
//! older than `FACT_FRESH_MS`, or holds the line ([`Judged::answer`]).
//!
//! SURFACING (§6, slice 1e). Under the same lock, too: what each session's loop last told
//! ([`told`], `IdleHost::occupancy_said`), from which each look judges the sessions whose hold
//! is ready ([`rules::ready`]) and the build slots idle while they were
//! ([`ring::idle_ready_ms`]); the minute look's row, kept a day in memory and appended to the
//! facts ring (`<state>/occupancy/facts.ring`) while the owner's mode observes ([`set_mode`],
//! [`append_look`]); the status fields ([`status_fields`]); and the `occupancy` verb
//! ([`cmd_occupancy`]).

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
/// terms this often (design §3.1), the build slots' armed cadence.
pub(crate) const SAMPLE_EVERY: Duration = aterm_sysprobe::slots::PERIOD;

/// [`SAMPLE_EVERY`] in whole milliseconds.
const SAMPLE_EVERY_MS: i64 = SAMPLE_EVERY.as_millis() as i64;

/// The oldest the figures a slot rule's line types may be at its write (design §3.1).
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
    /// When the slot reader read them (unix ms: the snapshot's landing less the reading's age);
    /// `None` where the snapshot says no age.
    pub(crate) slots_as_of_unix_ms: Option<i64>,
    /// The snapshot is older than [`STALE_AFTER`], or there is none.
    pub(crate) stale: bool,
}

/// A load average times 100, rounded; `None` for a value no load is.
fn x100(load: f64) -> Option<u32> {
    (load.is_finite() && load >= 0.0)
        .then(|| (load * 100.0).round().min(f64::from(u32::MAX)) as u32)
}

/// The board of `snap`, which landed at `snap_unix_ms` (`None`: no snapshot
/// yet, `snap` then the empty one), with the home volume at `disk`, taken at
/// `now_unix_ms`.
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

/// THE BOARD AND WHAT IT JUDGED ACROSS ITS LOOKS.
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
    /// What each session's loop last told ([`told`]), by sid.
    said: BTreeMap<String, OccupancySaid>,
    /// The rows of the last [`ring::RING_ROWS`] minute looks ([`Look::Tick`]), oldest first:
    /// the facts ring as this window holds it ([`Self::look_row`]).
    rows: VecDeque<RingRow>,
    /// The ring's file was read into [`Self::rows`] (at the first look).
    seeded: bool,
    /// The supervised sessions as the last look named them (tab key, sid): a sample's.
    sessions: Vec<(u64, String)>,
    /// Each session whose loop, under `prompt`, last asked the board for `hold-room`'s
    /// reading, with when (unix ms): the asks the sampler keeps to ([`Self::sampling`]).
    wanting: BTreeMap<String, i64>,
    /// Each session's `hold-room` lines its loop told typed in the told hold: the last one's
    /// unix ms and how many ([`Self::tell`]), when the rule's next line can come due
    /// ([`Self::room_lead`]).
    rooms_typed: BTreeMap<String, (i64, usize)>,
    /// When the board was last sampled between looks (unix ms).
    sampled_ms: Option<i64>,
    /// A sampler runs ([`start_sampler`]).
    sampler: bool,
}

impl Judged {
    /// THE LOOK that took `board` from `snap`, with the supervised `sessions`.
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
        // A reservation ends where its session's tab was seen holding a slot
        // since: the build it was for took one.
        let held = &self.held;
        self.pace
            .reserved
            .retain(|r| held.get(&r.sid).is_none_or(|at| *at < r.since_ms));
        self.board = Some(board);
    }

    /// A SAMPLE between looks ([module docs](self)): `board` from `snap`, judged as a look with
    /// the last look's sessions, and no ring row.
    pub(crate) fn sample(&mut self, board: FactsBoard, snap: Option<&Snapshot>) {
        self.sampled_ms = Some(board.taken_unix_ms);
        let sessions = std::mem::take(&mut self.sessions);
        self.step(board, snap, &sessions);
    }

    /// SESSION `sid`'S LOOP ASKED THE BOARD at `now_unix_ms` under the owner's `mode`: a loop asks
    /// the reading only where `hold-room` could be due by all but the machine, and its pace only
    /// for a due line, so under `prompt` the line is due or pending and the board samples
    /// ([`Self::sampling`]). Under `observe` and `off` an ask asks nothing more.
    pub(crate) fn asked(&mut self, sid: &str, now_unix_ms: i64, mode: Occupancy) {
        if mode == Occupancy::Prompt {
            self.wanting.insert(sid.to_string(), now_unix_ms);
        } else {
            self.wanting.clear();
        }
    }

    /// Whether the board samples at `now_unix_ms` under `mode`: under `prompt`, while a session
    /// asked within [`ASK_KEEP_MS`] (a loop that wants the reading asks once a look), or while a
    /// told hold's `hold-room` line nears its cadence ([`Self::room_lead`]).
    pub(crate) fn sampling(&self, now_unix_ms: i64, mode: Occupancy) -> bool {
        mode == Occupancy::Prompt
            && (self
                .wanting
                .values()
                .any(|at| now_unix_ms.saturating_sub(*at) <= ASK_KEEP_MS)
                || self.room_lead(now_unix_ms))
    }

    /// WHETHER A TOLD HOLD'S `hold-room` LINE IS NEAR at `now_unix_ms`: its slot term,
    /// `slots>=1 for=20m`, spans the twenty minutes before the line can first go, and a loop asks
    /// the board only once its line can ([`rules::wants_reading`]), so those minutes are sampled
    /// from what each loop told. A session whose loop runs under `prompt` with an open hold, the
    /// rule neither said spent nor its cap's lines told typed, and `now` from the term's `for=`
    /// (and one look, the sampler starting at a look) before the rule's cadence lets the line go
    /// ([`rules::room_due_ms`], from the hold's start and its last line told typed) to
    /// [`ASK_KEEP_MS`] after, by when its loop asks the board itself.
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

    /// Session `sid`'s `hold-room` lines told typed in its hold begun at `since_ms`: the last
    /// one's unix ms and how many; `None` where none was, or the last was another hold's.
    fn room_typed(&self, sid: &str, since_ms: i64) -> Option<(i64, usize)> {
        self.rooms_typed
            .get(sid)
            .copied()
            .filter(|(at, _)| since_ms <= *at)
    }

    /// Whether a sampler must start at `now_unix_ms` under `mode`: the board samples and none
    /// runs, marked running under the caller's lock, so one starts ([`start_sampler`]).
    pub(crate) fn sampler_due(&mut self, now_unix_ms: i64, mode: Occupancy) -> bool {
        self.sampling(now_unix_ms, mode) && !std::mem::replace(&mut self.sampler, true)
    }

    /// Whether a sample is due at `now_unix_ms` under `mode`: sampling, and none taken within
    /// [`SAMPLE_EVERY`].
    pub(crate) fn sample_due(&self, now_unix_ms: i64, mode: Occupancy) -> bool {
        self.sampling(now_unix_ms, mode)
            && self
                .sampled_ms
                .is_none_or(|at| now_unix_ms.saturating_sub(at) >= SAMPLE_EVERY_MS)
    }

    /// When the board's figures were read (unix ms): its snapshot's landing, or its slots'
    /// reading where older; `None` with no snapshot or no age for the slots.
    fn figures_ms(&self) -> Option<i64> {
        let b = self.board.as_ref().filter(|b| b.as_of_unix_ms > 0)?;
        Some(b.as_of_unix_ms.min(b.slots_as_of_unix_ms?))
    }

    /// THE ANSWER AT A LINE'S WRITE for session `sid`'s line of `rule`, its hold begun at
    /// `hold_since_ms`, at `now_unix_ms` (`IdleHost::occupancy_pace`; [`pace_for`] re-read the
    /// board just before): for a slot rule its figures first — [`PaceGrant::Stale`] unless they
    /// were read within [`FACT_FRESH_MS`], [`PaceGrant::Unmet`] unless `hold-room`'s terms hold
    /// on them ([`rules::ready`]) — then the pace ([`Self::pace`]), its grant
    /// [`PaceGrant::Room`] with the reading the line types (stamped when its figures were
    /// read), never a bare grant; for a nudge the pace alone.
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

    /// What session `sid`'s loop last told ([`told`]): a hold that closed or a session gone
    /// tells again; `None` forgets it. A `hold-room` line told typed is booked
    /// ([`Self::rooms_typed`]): one stamped later than the last booked counts one more in the
    /// told hold, and a told hold begun after the last booked starts the count over. A hold told
    /// closed keeps the count, as a loop's end tells no hold and its successor carries the hold.
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

    /// The rows a predecessor's ring left (oldest first), taken once, before this window's
    /// first row.
    pub(crate) fn seed_rows(&mut self, rows: Vec<RingRow>) {
        if std::mem::replace(&mut self.seeded, true) {
            return;
        }
        let keep = rows.len().saturating_sub(ring::RING_ROWS);
        self.rows = rows.into_iter().skip(keep).collect();
    }

    /// THE LOOK'S ROW at `now_unix_ms`, kept ([`Self::rows`]) and returned for the ring's file:
    /// the board's machine facts, and each supervised session's last slot-holding look, whether
    /// its told hold reads ready on the board ([`rules::ready`]) and its told first guard.
    pub(crate) fn look_row(&mut self, sessions: &[(u64, String)], now_unix_ms: i64) -> RingRow {
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
                    ready: rules::ready(&reading, hold, now_unix_ms),
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

    /// THE ROW A LOOK OF `look` TAKES at `now_unix_ms` ([`Self::look_row`]): a minute's tick
    /// takes one; a poke between ticks takes none, so the rows stay a row a minute and a day of
    /// them a day.
    pub(crate) fn looked(
        &mut self,
        sessions: &[(u64, String)],
        now_unix_ms: i64,
        look: Look,
    ) -> Option<RingRow> {
        (look == Look::Tick).then(|| self.look_row(sessions, now_unix_ms))
    }

    /// The sessions whose hold read ready at the last look.
    pub(crate) fn ready_now(&self) -> u16 {
        self.rows.back().map_or(0, RingRow::ready)
    }

    /// The slot-minutes idle while a session was ready over the day before `now_unix_ms`
    /// ([`ring::idle_ready_ms`]), whole: read in place, as `status` asks it for each session.
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
const fn memory_pressure(p: Pressure) -> Option<MemoryPressure> {
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

/// THE READS THAT MUST BE NEW: a slot rule's write re-reading the board ([`pace_for`]). No edge
/// is logged: the slots' are `lab slot-lane`'s to count.
pub(crate) static SLOTS_NOW: Arming = Arming::new();

/// How old a slots reading the probe thread hands back may be
/// ([`aterm_sysprobe::slots::SlotReader::read_within`]): one paired gap while a write re-reads
/// ([`SLOTS_NOW`]), the armed period while a wait names them ([`SLOTS_ARMED`]), else the idle
/// minute.
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

/// The board and what it judged, as last taken ([`look`]); `None` before
/// the first look.
static BOARD: Mutex<Option<Judged>> = Mutex::new(None);

/// The home volume's level on the disk board, where the watch judged it.
fn home_level() -> Option<Level> {
    let home = aterm_types::dirs::home_dir()?;
    let board = crate::disk_board::SHELF.board_lock();
    board.row_at(&home).map(|r| r.level)
}

/// The owner's `[harness] occupancy` in the policy the window's loops run ([`set_mode`]): 0
/// `off`, 1 `observe`, 2 `prompt`; `observe`, the default, before the launch sets it.
static MODE: AtomicU8 = AtomicU8::new(1);

#[cfg(test)]
thread_local! {
    /// A test's own mode ([`set_mode`] on its thread): tests run in parallel, each with its
    /// own app, and each that adopts a policy sets the mode.
    static MODE_FOR_TEST: std::cell::Cell<Option<Occupancy>> = const { std::cell::Cell::new(None) };
}

/// THE OWNER'S MODE IN FORCE (`App::note_occupancy_mode`: the launch's policy before the host
/// starts, then each the watcher posts): the status field says it, and the facts ring is
/// written only while it observes.
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

/// WHICH LOOK the board takes: the disk watch's minute tick, which takes the ring's row, or a
/// poke between ticks (the disk watch's), which refreshes the board alone. The ring is a row a
/// minute (§6), so its [`ring::RING_ROWS`] rows are a day however often the watch is poked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Look {
    Tick,
    Poke,
}

/// THE RING'S APPEND of a look of `look`'s `row` under the owner's `mode`: a tick's row, while
/// the mode observes (`observe` or `prompt`), to `file`; under `off` nothing is written (slice
/// 1's F4), and a poke has no row. Whether a row was written.
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

/// The facts ring's file, as this window writes it; `None` before the first look, or with no
/// state directory.
static RING: Mutex<Option<RingFile>> = Mutex::new(None);

/// What session `sid`'s loop told (`IdleHost::occupancy_said`, through the window's note):
/// kept on the board for the status, `why` and the next look's ready.
pub(crate) fn told(sid: &str, said: Option<OccupancySaid>) {
    BOARD
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(Judged::default)
        .tell(sid, said);
}

/// TAKE THE BOARD at `now_unix_ms` (the disk look's, of `kind`).
pub(crate) fn look(now_unix_ms: i64, sessions: &[(u64, String)], kind: Look) {
    // The ring's file is read once, at the first look, apart from the board's lock.
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
    let (moved, row, start) = {
        let mut kept = BOARD.lock().unwrap_or_else(PoisonError::into_inner);
        let judged = kept.get_or_insert_with(Judged::default);
        let moved = judged.board.as_ref().map(change_words).as_deref() != Some(words.as_str());
        judged.step(board, held.as_ref().map(|h| &*h.snap), sessions);
        if let Some(rows) = seed {
            judged.seed_rows(rows);
        }
        // A told hold's `hold-room` line near its cadence is sampled from the look
        // ([`Judged::room_lead`]), before its loop asks the board.
        let start = judged.sampler_due(now_unix_ms, mode());
        (moved, judged.looked(sessions, now_unix_ms, kind), start)
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

/// THE STATUS FIELDS of session `sid` (`status`; design §6): `occupancy=<mode>` (the owner's,
/// [`mode`]), `occupancy_hold=<declared|undeclared|none>` (as its loop last told it),
/// `ready=<n>` (the sessions ready at the last look), `slots=<usable>/<n>` (`-` unread) and
/// `idle_ready_min=<n>` (the slot-minutes idle while a session was ready, over the last day).
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

/// `occupancy` ON THE CONTROL SOCKET for session `sid` (design §6): `why` (the default), the
/// last word its loop told in §6's form; `check --replay <dur>`, the decider re-run over the
/// window's rows of the last `<dur>` with the session's `fact-hold@v1` ledger rows (read from
/// `<state>/drive/<sid>.jsonl`). Read-only.
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

/// `occupancy check --replay`'s reply for session `sid` over `rows` (the window's, oldest
/// first) with its ledger text `ledger`, over the window `(from_ms, to_ms)`.
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

/// THE BOARD AS SESSION `sid`'S HOST READS IT NOW (`IdleHost::occupancy`, [`Judged::reading`]).
/// The ask is noted ([`Judged::asked`]): under `prompt` it starts the sampler where none runs.
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

/// THE PACE FOR SESSION `sid`'S LINE of `rule`, its hold begun at
/// `hold_since_ms` (`IdleHost::occupancy_pace`, [`pace_on`] on the window's board): for a slot
/// rule under the owner's `prompt` the board re-read first, by a reading taken after this call
/// with the slots read anew in it ([`SLOTS_NOW`]), outside the board's lock; then the answer,
/// taken with the board under its one lock.
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

/// The board re-read for a slot rule's write ([`pace_on`]): a snapshot, when it landed (unix
/// ms), and the disk's level beside it.
pub(crate) struct Reread {
    /// The snapshot.
    pub(crate) snap: Arc<Snapshot>,
    /// When it landed, unix ms.
    pub(crate) landed_ms: i64,
    /// The disk's level ([`home_level`]).
    pub(crate) disk: Option<Level>,
}

/// [`pace_for`] on the board `kept` under the owner's `mode`, for session `sid`'s line of
/// `rule` in its hold begun at `hold_since_ms`: a slot rule's line, which types the board's
/// figures, re-read by `reread` under `prompt` alone — under `observe` and `off` nothing is read
/// and it waits ([`PaceGrant::Stale`]), as a loop whose policy still says `prompt` can ask after
/// the owner's mode moved; then `now`, the wall clock after it (a clock that cannot be read
/// holds the line at the machine's pace), and the answer under the board's lock
/// ([`Judged::answer`]).
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

/// ONE PASS OF THE SAMPLER ([module docs](self)) over the board `kept` at `now_unix_ms` under the
/// owner's `mode`: where a sample is due ([`Judged::sample_due`]), `read` takes the board (live,
/// from a snapshot at most [`SAMPLE_EVERY`] old, pulled where none is) outside the lock, and the
/// board steps on it ([`Judged::sample`]). Whether the sampler goes on: `false` once no session
/// under `prompt` wants the board, the sampler marked stopped under the same lock, so the next
/// ask starts another.
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

/// THE SAMPLER'S THREAD: the build slots armed while it runs, a pass every [`SAMPLE_EVERY`]
/// ([`sample_pass`]) until one finds no session under `prompt` wanting the board. A thread that
/// cannot start leaves the sampler stopped, so the next ask tries again.
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
