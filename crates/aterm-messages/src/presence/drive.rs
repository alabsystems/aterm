// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Presence driver sequencing session facts, the host's window hook ([`Desk::projecting`]), story watermarks, and timer [`deadline`]s.
use std::collections::HashMap;

use super::{Facts, Host, Level, Slot, View};
use crate::Instant;

/// Per-session presence slots and supervisor claim deadlines.
#[derive(Debug)]
pub struct Table<V: Host> {
    slots: HashMap<u64, Slot<V>>,
    /// When each session's `ttl=` supervisor claim lapses: the timer wakes
    /// there, because a supervisor that dies writes nothing and its box must
    /// still reach the human.
    supervisor_until: HashMap<u64, Instant>,
}

impl<V: Host> Default for Table<V> {
    fn default() -> Self {
        Self {
            slots: HashMap::new(),
            supervisor_until: HashMap::new(),
        }
    }
}

impl<V: Host> Table<V> {
    /// Returns `session`'s slot if it has been refreshed.
    #[must_use]
    pub fn slot(&self, session: u64) -> Option<&Slot<V>> {
        self.slots.get(&session)
    }

    /// Folds `facts` into `session`'s slot and returns whether any view-visible state changed.
    pub fn absorb(&mut self, session: u64, facts: Facts<V>, now: Instant) -> bool {
        let slot = self.slots.entry(session).or_insert_with(|| Slot::new(now));
        let first = slot.agent_seq_seen.is_none() && slot.shell.is_none();
        slot.absorb(facts, now) || first
    }

    /// Removes `session`'s slot and supervisor claim.
    pub fn retire(&mut self, session: u64) {
        self.slots.remove(&session);
        self.supervisor_until.remove(&session);
    }

    /// One told point on `session`'s slot (minted at `now` if none): its seq.
    fn tell(&mut self, session: u64, now: Instant) -> u64 {
        self.slots
            .entry(session)
            .or_insert_with(|| Slot::new(now))
            .tell()
    }

    /// The sessions whose cooperative lease has lapsed by `now`.
    fn lapsed_leases(&self, now: Instant) -> Vec<u64> {
        self.slots
            .iter()
            .filter(|(_, slot)| slot.lease_until.is_some_and(|d| d <= now))
            .map(|(session, _)| *session)
            .collect()
    }

    /// The sessions whose `ttl=` supervisor claim has lapsed by `now`.
    fn lapsed_supervisors(&self, now: Instant) -> Vec<u64> {
        self.supervisor_until
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(session, _)| *session)
            .collect()
    }

    /// The earliest lease or supervisor lapse, never before `now`. A
    /// supervisor's is the later of the lapse last read into the table and
    /// the claim's live one (`live`, [`Desk::supervisor_lapse`]): a renewal
    /// is no change (`meta set supervisor` by the holder it names posts no
    /// wake), so a deadline held at the lapse last read would wake the loop
    /// once a lease only to find the claim renewed, every 70 to 90 s for
    /// each supervised tab of a quiet desktop.
    fn deadline(&self, now: Instant, live: impl Fn(u64) -> Option<Instant>) -> Option<Instant> {
        let leases = self.slots.values().filter_map(|slot| slot.lease_until);
        let supervisors = self
            .supervisor_until
            .iter()
            .map(|(session, at)| live(*session).map_or(*at, |lapse| lapse.max(*at)));
        leases.chain(supervisors).map(|d| d.max(now)).min()
    }
}

/// Host interface for the presence driver.
pub trait Desk {
    /// Host presence seam type.
    type V: Host;
    /// Window identifier type.
    type Win: Copy + Eq;

    /// Returns the host's current clock instant.
    fn clock(&self) -> Instant;
    /// Returns a shared reference to the session [`Table`].
    fn table(&self) -> &Table<Self::V>;
    /// Returns a mutable reference to the session [`Table`].
    fn table_mut(&mut self) -> &mut Table<Self::V>;
    /// Visits every window's [`View`] in host window order.
    fn each_view(&self, f: &mut dyn FnMut(Self::Win, &View));
    /// Returns a mutable [`View`] for window `w`.
    fn view_of_mut(&mut self, w: Self::Win) -> Option<&mut View>;
    /// Returns all windows showing `session`, with front-showing windows first.
    fn windows_showing(&self, session: u64) -> Vec<Self::Win>;
    /// Returns whether the host still holds `session`.
    fn holds_session(&self, session: u64) -> bool;
    /// Reads current [`Facts`] for `session`, or `None` if no longer held.
    fn sense(&self, session: u64) -> Option<Facts<Self::V>>;
    /// Returns when `session`'s `ttl=` supervisor claim lapses.
    fn supervisor_lapse(&self, session: u64, now: Instant) -> Option<Instant>;
    /// Returns when the next rate-limited notification is due.
    fn herald_deadline(&self, now: Instant) -> Option<Instant>;
    /// Tells the host a refresh reached window `w`.
    fn projecting(&mut self, w: Self::Win);
    /// Re-stamps tab chips for `windows`.
    fn restamp_chips(&mut self, windows: Vec<Self::Win>);
    /// Publishes `session`'s need to the notification herald.
    fn herald(&mut self, session: u64);
    /// Posts any owed notifications permitted by the rate limit at `now`.
    fn herald_owed(&mut self, now: Instant);
    /// Removes and records `session`'s lapsed supervisor claim.
    fn lapse_supervisor(&mut self, session: u64);
    /// Forgets a closed session's slot, claim, and herald state.
    fn forget(&mut self, session: u64);
}

/// Every window, in the host's order.
fn windows<D: Desk>(d: &D) -> Vec<D::Win> {
    let mut out = Vec::new();
    d.each_view(&mut |w, _| out.push(w));
    out
}

/// Refreshes `session`'s facts; where they moved, tells the host at every window showing it
/// ([`Desk::projecting`]) and restamps their chips.
pub fn refresh_session<D: Desk>(d: &mut D, session: u64) {
    let now = d.clock();
    match d.supervisor_lapse(session, now) {
        Some(at) => {
            d.table_mut().supervisor_until.insert(session, at);
        }
        None => {
            d.table_mut().supervisor_until.remove(&session);
        }
    }
    d.herald(session);
    let Some(facts) = d.sense(session) else {
        return;
    };
    if !d.table_mut().absorb(session, facts, now) {
        return;
    }
    let windows = d.windows_showing(session);
    for w in &windows {
        d.projecting(*w);
    }
    d.restamp_chips(windows);
}

/// Tells the host a refresh reached window `w` ([`Desk::projecting`]).
pub fn refresh_window<D: Desk>(d: &mut D, w: D::Win) {
    d.projecting(w);
}

/// Tells the host a refresh reached every window ([`Desk::projecting`]).
pub fn refresh_all<D: Desk>(d: &mut D) {
    for w in windows(d) {
        d.projecting(w);
    }
}

/// Records human interaction with `session` in window `w` and advances its story watermark if calm.
pub fn human_acted_on<D: Desk>(d: &mut D, w: D::Win, session: u64) {
    let Some(seq) = d
        .table()
        .slot(session)
        .filter(|slot| slot.calm())
        .map(|slot| slot.story_seq)
    else {
        return;
    };
    if let Some(v) = d.view_of_mut(w) {
        v.watermarks.insert(session, seq);
    }
}

/// Records a told story point for `session` (`ctl story <verb>`, whose word
/// is the host's log mark) and returns its sequence number.
pub fn tell<D: Desk>(d: &mut D, session: u64) -> Option<u64> {
    if !d.holds_session(session) {
        return None;
    }
    refresh_session(d, session);
    let now = d.clock();
    Some(d.table_mut().tell(session, now))
}

/// Returns the earliest timer deadline across leases, supervisors, and the herald.
pub fn deadline<D: Desk>(d: &D, now: Instant) -> Option<Instant> {
    let table = d
        .table()
        .deadline(now, |session| d.supervisor_lapse(session, now));
    [table, d.herald_deadline(now)].into_iter().flatten().min()
}

/// Processes timer expirations at `now`.
pub fn tick<D: Desk>(d: &mut D, now: Instant) {
    for session in d.table().lapsed_supervisors(now) {
        if !d.holds_session(session) {
            d.forget(session);
            continue;
        }
        d.lapse_supervisor(session);
        refresh_session(d, session);
    }
    d.herald_owed(now);
    for session in d.table().lapsed_leases(now) {
        if !d.holds_session(session) {
            d.forget(session);
            continue;
        }
        refresh_session(d, session);
        if let Some(slot) = d.table_mut().slots.get_mut(&session)
            && slot.lease_until.is_some_and(|x| x <= now)
        {
            slot.lease_until = None;
        }
    }
}

/// The highest story watermark any window holds for `session` (0: no window
/// has read its story).
fn read_mark<D: Desk>(d: &D, session: u64) -> u64 {
    let mut mark = 0;
    d.each_view(&mut |_, v| mark = mark.max(v.watermark(session)));
    mark
}

/// Returns `session`'s [`Level`] against the highest story watermark across all windows.
#[must_use]
pub fn session_level<D: Desk>(d: &D, session: u64) -> Level {
    d.table()
        .slot(session)
        .map_or(Level::Quiet, |slot| slot.level(read_mark(d, session)))
}

/// Formats the presence fields (`hand=`, `hand_pid=`, `level=`, `story=`, `why=`) for `status`.
#[must_use]
pub fn status_tail<D: Desk>(d: &D, session: u64) -> String {
    let level = session_level(d, session).wire();
    match d.table().slot(session) {
        Some(slot) => format!(
            "hand={} hand_pid={} level={level} story={} why={}",
            slot.hand.wire::<D::V>(),
            slot.hand.pid_wire(),
            slot.story_seq,
            slot.why(read_mark(d, session))
        ),
        None => format!("hand=- hand_pid=- level={level} story=0 why=-"),
    }
}
