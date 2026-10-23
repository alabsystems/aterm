// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Presence driver sequencing session facts, window projections ([`Desk::pressing_session`]), story watermarks, and timer [`deadline`]s.
use std::collections::HashMap;

use super::pill::{Pill, PillAct, PillKind};
use super::{
    Facts, Host, Level, NeedSeverity, Rim, Slot, StopCause, View, pill_due, pill_text, pill_words,
};
use crate::Instant;

/// Maximum need text length in cells carried by a pill.
pub const PILL_WHAT_CELLS: usize = 96;

/// The longest tab label a pill carries, in cells.
const PILL_LABEL_CELLS: usize = 32;

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
    /// wake), so a deadline held at the lapse read before it woke the loop
    /// once a lease only to find the claim renewed — every 70 to 90 s for
    /// each supervised tab of a quiet desktop (the Together run, 2026-10-16).
    fn deadline(&self, now: Instant, live: impl Fn(u64) -> Option<Instant>) -> Option<Instant> {
        let leases = self.slots.values().filter_map(|slot| slot.lease_until);
        let supervisors = self
            .supervisor_until
            .iter()
            .map(|(session, at)| live(*session).map_or(*at, |lapse| lapse.max(*at)));
        leases.chain(supervisors).map(|d| d.max(now)).min()
    }
}

/// Terminal session shown in a window ([`Desk::shown`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown {
    /// Local session identifier.
    pub session: u64,
    /// Zero-based tab index in the window.
    pub tab: usize,
    /// Whether the tab is the window's active tab.
    pub front_tab: bool,
}

/// Most pressing session in a window ([`Desk::pressing_session`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pressing {
    /// Local session identifier.
    pub session: u64,
    /// Zero-based tab index in the window.
    pub tab: usize,
    /// Whether the tab is the window's active tab.
    pub front_tab: bool,
    /// Count of other sessions in the window that also need the user.
    pub more: usize,
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
    /// Returns the [`View`] for window `w`.
    fn view_of(&self, w: Self::Win) -> Option<&View>;
    /// Returns a mutable [`View`] for window `w`.
    fn view_of_mut(&mut self, w: Self::Win) -> Option<&mut View>;
    /// Returns the active front session of window `w`.
    fn front_session(&self, w: Self::Win) -> Option<u64>;
    /// Returns all terminal sessions shown in window `w` in tab order.
    fn shown(&self, w: Self::Win) -> Vec<Shown>;
    /// Returns all windows showing `session`, with front-showing windows first.
    fn windows_showing(&self, session: u64) -> Vec<Self::Win>;
    /// Returns whether the host still holds `session`.
    fn holds_session(&self, session: u64) -> bool;
    /// Reads current [`Facts`] for `session`, or `None` if no longer held.
    fn sense(&self, session: u64) -> Option<Facts<Self::V>>;
    /// Returns when `session`'s `ttl=` supervisor claim lapses.
    fn supervisor_lapse(&self, session: u64, now: Instant) -> Option<Instant>;
    /// Returns whether window `w`'s pill is visible on screen.
    fn pill_visible(&self, w: Self::Win) -> bool;
    /// Returns whether window borders and pills are enabled (`[presence] rim`).
    fn needs_shown(&self) -> bool;
    /// Returns the tab label for `session`.
    fn label_of(&self, session: u64) -> String;
    /// Returns whether the restart action is offered for `session`.
    fn restart_offered(&self, session: u64) -> bool;
    /// Returns `session`'s global identifier for `chrome` output.
    fn sid_of(&self, session: u64) -> Option<String>;
    /// Returns when the next rate-limited notification is due.
    fn herald_deadline(&self, now: Instant) -> Option<Instant>;
    /// Notifies the host before window `w` is re-projected.
    fn projecting(&mut self, w: Self::Win);
    /// Requests a repaint of window `w`.
    fn request_redraw(&self, w: Self::Win);
    /// Re-stamps tab chips and updates pills for `windows`.
    fn restamp_chips(&mut self, windows: Vec<Self::Win>);
    /// Updates pills whose age changed and returns windows needing a chip restamp.
    fn push_pill(&mut self, windows: Vec<Self::Win>) -> Vec<Self::Win>;
    /// Publishes `session`'s need to the notification herald.
    fn herald(&mut self, session: u64);
    /// Posts any owed notifications permitted by the rate limit at `now`.
    fn herald_owed(&mut self, now: Instant);
    /// Removes and records `session`'s lapsed supervisor claim.
    fn lapse_supervisor(&mut self, session: u64);
    /// Forgets a closed session's slot, claim, and herald state.
    fn forget(&mut self, session: u64);

    /// Returns the most pressing session in window `w` ([`pressing`] over [`Self::shown`]).
    fn pressing_session(&self, w: Self::Win) -> Option<Pressing> {
        pressing(self.table(), &self.shown(w), self.front_session(w))
    }

    /// Returns the next needing session in window `w` using its front session as the cursor ([`next_need`] over [`Self::shown`]).
    fn next_need_after(&self, w: Self::Win, named: u64) -> Option<u64> {
        next_need(self.table(), &self.shown(w), self.front_session(w), named)
    }
}

/// The sessions `shown` that need the person, most pressing first by the
/// need alone — a stuck need before a decide one, then the oldest, then tab
/// order — each once, at its first place.
fn needing_order<V: Host>(table: &Table<V>, shown: &[Shown]) -> Vec<u64> {
    let mut needing: Vec<(Shown, &Slot<V>)> = Vec::new();
    for s in shown {
        if needing.iter().any(|(n, _)| n.session == s.session) {
            continue;
        }
        if let Some(slot) = table.slot(s.session).filter(|slot| slot.need.is_some()) {
            needing.push((*s, slot));
        }
    }
    needing.sort_by_key(|(s, slot)| (rank(slot.severity()), slot.need_since, s.tab));
    needing.into_iter().map(|(s, _)| s.session).collect()
}

/// A need's rank: stuck first, then decide, then none.
const fn rank(severity: Option<NeedSeverity>) -> u8 {
    match severity {
        Some(NeedSeverity::Stuck) => 0,
        Some(NeedSeverity::Decide) => 1,
        None => 2,
    }
}

/// Returns the next needing session after the needing `front`, else after `named`.
/// A front report without a pill skips `named` if it would be next, so the
/// pill's More action reaches the other needs. Returns `None` with fewer than
/// two needs or when neither cursor is among them.
#[must_use]
pub fn next_need<V: Host>(
    table: &Table<V>,
    shown: &[Shown],
    front: Option<u64>,
    named: u64,
) -> Option<u64> {
    let order = needing_order(table, shown);
    if order.len() < 2 {
        return None;
    }
    let here = front.filter(|f| order.contains(f)).unwrap_or(named);
    let at = order.iter().position(|s| *s == here)?;
    let next = order[(at + 1) % order.len()];
    let next = if next == named
        && here != named
        && table
            .slot(here)
            .and_then(|slot| slot.need.as_ref())
            .is_some_and(|need| !need.show_pill)
    {
        order[(at + 2) % order.len()]
    } else {
        next
    };
    Some(next)
}

/// Returns the most pressing session among `shown`.
#[must_use]
pub fn pressing<V: Host>(
    table: &Table<V>,
    shown: &[Shown],
    front: Option<u64>,
) -> Option<Pressing> {
    let mut seen: Vec<u64> = Vec::with_capacity(shown.len());
    let mut needing: Vec<(Shown, &Slot<V>)> = Vec::new();
    for s in shown {
        if seen.contains(&s.session) {
            continue;
        }
        seen.push(s.session);
        if let Some(slot) = table.slot(s.session).filter(|slot| slot.need.is_some()) {
            needing.push((*s, slot));
        }
    }
    let more = needing.len().saturating_sub(1);
    needing
        .into_iter()
        .min_by_key(|(s, slot)| {
            (
                rank(slot.severity()),
                Some(s.session) != front,
                slot.need_since,
                s.tab,
            )
        })
        .map(|(s, _)| Pressing {
            session: s.session,
            tab: s.tab,
            front_tab: s.front_tab,
            more,
        })
}

/// Every window, in the host's order.
fn windows<D: Desk>(d: &D) -> Vec<D::Win> {
    let mut out = Vec::new();
    d.each_view(&mut |w, _| out.push(w));
    out
}

/// Refreshes `session`'s facts and re-projects every window showing it.
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
        project_window(d, *w);
    }
    d.restamp_chips(windows);
}

/// What `w` shows: the border, the session it is lit for and the pill. The
/// most pressing need first; with none, the person's own hold on the front
/// session; else nothing.
fn compose<D: Desk>(d: &D, w: D::Win) -> (Rim, Option<u64>, Option<Pill>) {
    if !d.needs_shown() {
        return (Rim::None, None, None);
    }
    if let Some(p) = d.pressing_session(w)
        && let Some(slot) = d.table().slot(p.session)
        && let Some(need) = slot.need.as_ref()
    {
        let rim = Rim::of(need.severity);
        let session = p.session;
        let more = p.more;
        let p = if need.show_pill {
            p
        } else {
            let shown: Vec<_> = d
                .shown(w)
                .into_iter()
                .filter(|s| {
                    d.table()
                        .slot(s.session)
                        .and_then(|slot| slot.need.as_ref())
                        .is_some_and(|need| need.show_pill)
                })
                .collect();
            let Some(p) = pressing(d.table(), &shown, d.front_session(w)) else {
                return (rim, Some(session), None);
            };
            p
        };
        let Some(slot) = d.table().slot(p.session) else {
            return (rim, Some(session), None);
        };
        let Some(need) = slot.need.as_ref() else {
            return (rim, Some(session), None);
        };
        let severity = need.severity;
        let act = match need.cause {
            _ if !p.front_tab || severity != NeedSeverity::Stuck => None,
            StopCause::Frozen if d.restart_offered(p.session) => Some(PillAct::Restart),
            StopCause::Suspended => Some(PillAct::Resume),
            StopCause::Frozen | StopCause::Wall => None,
        };
        let label = if p.front_tab && d.front_session(w) == Some(p.session) {
            String::new()
        } else {
            pill_text::<D::V>(&d.label_of(p.session), PILL_LABEL_CELLS)
        };
        let pill = Pill {
            kind: PillKind::Need(severity),
            session: p.session,
            label,
            tab: (!p.front_tab).then_some(p.tab + 1),
            what: pill_text::<D::V>(&need.what, PILL_WHAT_CELLS),
            since: (!p.front_tab).then_some(slot.need_since),
            act,
            more,
        };
        return (rim, Some(session), Some(pill));
    }
    if let Some(front) = d.front_session(w)
        && d.table()
            .slot(front)
            .is_some_and(|slot| slot.held_by_person)
    {
        let pill = Pill {
            kind: PillKind::Yours,
            session: front,
            label: String::new(),
            tab: None,
            what: String::new(),
            since: None,
            act: Some(PillAct::HandBack),
            more: 0,
        };
        return (Rim::Yours, Some(front), Some(pill));
    }
    (Rim::None, None, None)
}

/// Project `w` ([`compose`]) onto its view: whether anything the painter
/// reads moved (a repaint is asked for then).
fn project_window<D: Desk>(d: &mut D, w: D::Win) -> bool {
    let (rim, pressing, pill) = compose(d, w);
    d.projecting(w);
    let moved = d
        .view_of_mut(w)
        .is_some_and(|v| v.show(rim, pressing, pill));
    if moved {
        d.request_redraw(w);
    }
    moved
}

/// Re-projects window `w` and restamps its tab chips if needed.
pub fn refresh_window<D: Desk>(d: &mut D, w: D::Win) {
    if project_window(d, w) {
        d.restamp_chips(vec![w]);
    }
}

/// Re-projects all windows.
pub fn refresh_all<D: Desk>(d: &mut D) {
    let moved: Vec<D::Win> = windows(d)
        .into_iter()
        .filter(|w| project_window(d, *w))
        .collect();
    if !moved.is_empty() {
        d.restamp_chips(moved);
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

/// Calls [`human_acted_on`] for window `w`'s front session.
pub fn human_acted<D: Desk>(d: &mut D, w: D::Win) {
    if let Some(session) = d.front_session(w) {
        human_acted_on(d, w, session);
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

/// Returns the earliest timer deadline across visible pills, leases, supervisors, and the herald.
pub fn deadline<D: Desk>(d: &D, now: Instant) -> Option<Instant> {
    let mut deadline: Option<Instant> = None;
    let mut fold = |x: Instant| {
        if deadline.is_none_or(|cur| x < cur) {
            deadline = Some(x);
        }
    };
    d.each_view(&mut |w, v| {
        // An occluded, minimized or headless window's pill is read by
        // nobody, so its age arms nothing (`chrome` composes it at the read).
        if v.pill.as_ref().is_some_and(|p| p.since.is_some()) && d.pill_visible(w) {
            fold(v.pill_due.map_or(now, |x| x.max(now)));
        }
    });
    if let Some(x) = d
        .table()
        .deadline(now, |session| d.supervisor_lapse(session, now))
    {
        fold(x);
    }
    if let Some(x) = d.herald_deadline(now) {
        fold(x);
    }
    deadline
}

/// Processes timer expirations at `now` and returns windows whose views changed.
pub fn tick<D: Desk>(d: &mut D, now: Instant) -> Vec<D::Win> {
    let mut out = Vec::new();
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
    let mut aged = Vec::new();
    for w in windows(d) {
        let visible = d.pill_visible(w);
        let Some(v) = d.view_of_mut(w) else {
            continue;
        };
        let Some(since) = v.pill.as_ref().and_then(|p| p.since) else {
            continue;
        };
        if !visible || v.pill_due.is_some_and(|x| x > now) {
            continue;
        }
        let first = v.pill_due.is_none();
        v.pill_due = Some(pill_due(since, now));
        if !first {
            aged.push(w);
        }
    }
    if !aged.is_empty() {
        for w in d.push_pill(aged) {
            if let Some(v) = d.view_of_mut(w) {
                v.seed = v.seed.wrapping_add(1).max(1);
                out.push(w);
            }
        }
    }
    out
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

/// Formats the `presence` line for `chrome` on the front window.
#[must_use]
pub fn chrome_line<D: Desk>(d: &D, front: Option<D::Win>) -> String {
    let now = d.clock();
    let view = front.and_then(|w| d.view_of(w));
    let rim = view.map_or(Rim::None, |v| v.rim).wire();
    let pressing = view
        .and_then(|v| v.pressing)
        .and_then(|s| d.sid_of(s))
        .map_or_else(|| "-".to_string(), |sid| format!("@{sid}"));
    let pill = view
        .and_then(|v| v.pill.as_ref())
        .map(|p| pill_words(p, now).text())
        .unwrap_or_default();
    format!(
        "presence rim={rim} pressing={pressing} pill={}",
        quoted(&pill)
    )
}

/// `s` in double quotes, with `"` and `\` backslash-escaped.
fn quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
