// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Priority-ordered bounded input queues for the bridge loop.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::wait_tally::{Hist, Row, Stamp, Tally, Unit};

/// One record off the bus: `(offset, subject, body)`.
pub type Record = (u64, String, Vec<u8>);

/// What the loop took, in priority order.
#[derive(Debug)]
pub enum Item {
    /// A `/f/<F>/fleet/>` record: a halt or a barrier, ahead of every other record.
    Fleet(Record),
    /// One `EVENT …` line off aterm's push lane.
    Event(String),
    /// A `/f/<F>/in/<node>/>` record from the durable group.
    Inbox(Record),
    /// A `/f/<F>/pub/*/*/say/>` broadcast record.
    Say(Record),
    /// aterm's push lane ended at EOF or on a transport error ([`Mailbox::push_aterm_closed`]).
    Closed(Source),
    /// A broker source's reader ended, and how its read ended ([`End`]).
    Lost(Source, End),
    /// The broker answered a subscription with an `Error` frame.
    Refused(Source, &'static str),
    /// A paused broker source drained to low water and can resubscribe from this offset.
    Resume(Source, u64),
    /// A GAP NOTICE on a broker source (astream R4's addendum A9): retention gave up records
    /// of its filter below the tail, taken in its place among the source's records.
    Gap(Source, astream_broker::Gap),
}

/// HOW A BROKER SUBSCRIPTION'S READ ENDED ([`Item::Lost`]), as its reader saw it. Only an end
/// of file is the broker's word that it ended the stream; a live read deadline that passed is
/// this end hearing nothing for that long, whichever side went quiet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// End of file: the broker closed the stream.
    Eof,
    /// The live read deadline passed with nothing read (astream's `recv_live_event`: the
    /// broker's idle marks re-arm it, so none reached the reader for this long).
    Silent(Duration),
    /// The read failed with an error of this kind.
    Failed(std::io::ErrorKind),
}

impl End {
    /// The end of a read that failed with `e` under the live read `deadline`: a timeout is
    /// [`End::Silent`], any other error [`End::Failed`].
    #[must_use]
    pub fn of(e: &std::io::Error, deadline: Duration) -> Self {
        match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => Self::Silent(deadline),
            kind => Self::Failed(kind),
        }
    }

    /// What ended the `face` subscription, as a log line's subject: a deadline as
    /// `bridge::span_of` writes one, exact (`30 s`, `250 ms`).
    #[must_use]
    pub fn said(self, face: &str) -> String {
        match self {
            Self::Eof => format!("the broker ended the {face} subscription (end of file)"),
            Self::Silent(deadline) => format!(
                "the {face} subscription read nothing for {}, its live read deadline",
                crate::bridge::span_of(u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX))
            ),
            Self::Failed(kind) => format!("the {face} subscription's read failed ({kind})"),
        }
    }
}

/// Outcome of pushing a broker record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Pushed {
    /// Queued with room remaining.
    Queued,
    /// Queued and reached the source bound; the source is now paused.
    Paused,
    /// Dropped because the reader belongs to a torn-down generation.
    Stale,
}

/// Which reader source produced an item or closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Fleet,
    Inbox,
    /// The fleet-wide broadcast face.
    Say,
    Aterm,
}

/// Queued inputs that replay must yield to. A fleet halt or lane closure is never delayed for
/// catch-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplayPressure {
    Clear,
    Pending,
    Urgent,
}

/// Maximum records one source may hold before pausing or waiting.
pub const QUEUE_ROWS_MAX: usize = 1024;

/// Maximum subject+body bytes one source may hold before pausing.
pub const QUEUE_BYTES_MAX: usize = 8 * 1024 * 1024;

/// Maximum aterm events taken in a row while an inbox record waits.
pub const EVENT_RUN_MAX: u32 = 64;

/// The longest the head of the inbox face waits behind aterm's events before its turn is due
/// (C01): since it was pushed, or since the inbox face's last take if that came later.
///
/// AN AGE, BECAUSE A RUN IS COUNTED IN EVENTS AND AN EVENT'S COST IS THE TIER'S. Each `post`
/// event's drain waits out a publish's ack before its peek, about 0.35 ms relaxed, 0.65 ordered
/// and 2.3 strict, so a run of [`EVENT_RUN_MAX`] held a delivery 21-27 ms, 39-44 and about 150
/// behind four askers' posts (B11). Bounded by its age, a delivery waits this long plus the one
/// item in hand on any tier. It starts at the idle ack grace (`bridge::IDLE_ACK_GRACE`), the
/// longest the loop lets a held inbox commit wait for a write to ride.
pub const INBOX_WAIT_MAX: Duration = Duration::from_millis(5);

/// The most inbox items (records and gap notices) one inbox turn takes before aterm's events
/// are next (C01).
///
/// A TURN TAKES WHAT WAS QUEUED WHEN IT BEGAN. Before C01 each delivery started the run again,
/// so every record behind the first waited a whole run of its own; taken together, the turn's
/// records share one inbox commit (`Bridge::hold_inbox_commit`: no commit goes out while an
/// inbox record is queued). The cap keeps a backlog from holding aterm's events back for more
/// than this many deliveries at a time, across consecutive turns too and whether or not the
/// face empties between them (only a taken event or a reset of the face starts the count
/// again), as [`EVENT_RUN_MAX`] keeps the events from holding the inbox back. At the cap, a
/// pending event goes next even when the next inbox item has aged past [`INBOX_WAIT_MAX`].
///
/// AS MANY AS A RUN, NOT FEWER: on the fabric bench a turn of 16 cost the burst's throughput and
/// a turn of 64 did not (C01, measured in `docs/measured/fabric/README.md`; only the throughput
/// was measured). The likely reading: a burst's deliveries are what let its senders post on
/// (aterm refuses a 65th unlisted row from one peer), so a burst is paced by its post ->
/// visible, and a turn of 16 split the bench's queue of deliveries (at most 56 posts in flight)
/// into turns between the drains. A queue longer than the cap, which the face can hold
/// ([`QUEUE_ROWS_MAX`]: two peers bursting to one node, or a backlog after a reconnect), is
/// split by a turn of 64 as well.
pub const INBOX_TURN_MAX: u32 = 64;

const fn record_bytes(r: &Record) -> usize {
    r.1.len() + r.2.len()
}

/// One broker source's share of the mailbox.
#[derive(Default)]
struct Face {
    /// Each with the clock its push was given, and its stamp while the wait tally is on
    /// ([`Queues::stamping`]).
    records: VecDeque<(Record, Instant, Option<Stamp>)>,
    /// Resident subject+body bytes, as the bound counts them.
    bytes: usize,
    /// The incarnation whose inputs are currently accepted.
    generation: u64,
    /// While the face is PAUSED: the offset its subscription resumes from.
    paused: Option<u64>,
    /// The gap notices waiting, each with how many records this generation had pushed when it
    /// came and the clock its push was given: one is due once every record pushed before it is
    /// taken. Two with no record between them are one ([`astream_broker::Gap::and`]), keeping
    /// the first's clock, so there are never more than records + 1.
    gaps: VecDeque<(u64, astream_broker::Gap, Instant)>,
    /// Records pushed under this generation.
    pushed: u64,
    /// Records taken under this generation.
    taken: u64,
}

impl Face {
    fn take(&mut self) -> Option<(Record, Option<Stamp>, Option<u64>)> {
        let (r, _, stamp) = self.records.pop_front()?;
        self.taken = self.taken.saturating_add(1);
        self.bytes = self.bytes.saturating_sub(record_bytes(&r));
        let resume = if at_low_water(self.records.len(), self.bytes) {
            self.paused.take()
        } else {
            None
        };
        Some((r, stamp, resume))
    }

    fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.records.clear();
        self.bytes = 0;
        self.paused = None;
        self.gaps.clear();
        self.pushed = 0;
        self.taken = 0;
    }

    /// The gap notice due now: every record pushed before it was taken.
    fn take_gap(&mut self) -> Option<astream_broker::Gap> {
        if self.gaps.front().is_some_and(|(at, ..)| *at <= self.taken) {
            return self.gaps.pop_front().map(|(_, g, _)| g);
        }
        None
    }

    fn push_gap(&mut self, g: astream_broker::Gap, when: Instant) {
        match self.gaps.back_mut() {
            Some((at, before, _)) if *at == self.pushed => *before = before.and(g),
            _ => self.gaps.push_back((self.pushed, g, when)),
        }
    }

    /// How many items are queued: records and gap notices.
    fn queued(&self) -> usize {
        self.records.len() + self.gaps.len()
    }

    /// The clock the push of the item due next was given: a due gap notice's, else the first
    /// record's.
    fn head_at(&self) -> Option<Instant> {
        match self.gaps.front() {
            Some((at, _, when)) if *at <= self.taken => Some(*when),
            _ => self.records.front().map(|(_, when, _)| *when),
        }
    }
}

/// A rule of [`Queues::take`] a unit test switched off, for its negative control.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Off {
    /// No bound: every inbox item waits behind every event.
    Bound,
    /// No age: only a run of [`EVENT_RUN_MAX`] makes the inbox due (the rule before C01).
    Age,
    /// No turn: one inbox item a turn, each resetting the run (the rule before C01).
    Turn,
    /// A short snapshot turn forgets the inbox items taken before it (the initial C01 rule).
    Carry,
    /// An empty inbox face starts the turn budget again (ff5a34af25's rule).
    Empty,
    /// A turn that takes [`INBOX_TURN_MAX`] leaves the run of events as it was.
    CapRun,
}

#[derive(Default)]
struct Queues {
    fleet: Face,
    /// aterm's events.
    events: VecDeque<(u64, String, Option<Stamp>)>,
    /// How many events have ever been pushed.
    events_pushed: u64,
    inbox: Face,
    say: Face,
    /// Closure notices, in arrival order: `Item::Closed`, `Item::Lost` and `Item::Refused` only.
    closed: VecDeque<Item>,
    /// Paused faces whose queue has drained: `Item::Resume`'s.
    resume: VecDeque<(Source, u64)>,
    /// The last `EVENT * bye <word>` the push reader read.
    bye: Option<aterm_types::fabric_grammar::Bye>,
    /// How many of aterm's events the loop has taken in a row while an inbox record waited.
    events_run: u32,
    /// The inbox items the turn under way still takes: those queued when it began, at most
    /// [`INBOX_TURN_MAX`]; 0 while no turn is under way.
    turn_left: u32,
    /// Inbox items taken since the loop last took an event or the inbox face was reset, across
    /// snapshot turns and the face's emptying between them. Once this reaches
    /// [`INBOX_TURN_MAX`], a pending event must be taken before another turn.
    turn_taken: u32,
    /// The clock the inbox face's last take was given: the head's wait behind events starts
    /// no earlier ([`INBOX_WAIT_MAX`]).
    inbox_took: Option<Instant>,
    /// Whether each push is stamped for the wait tally ([`Mailbox::stamp`]).
    stamping: bool,
    /// Whether the loop is parked on [`Mailbox::take`] now.
    parked: bool,
    /// The stamped items' waits, by kind ([`Mailbox::copy_wakes`]).
    wakes: Wakes,
    /// The rule a negative control switched off.
    #[cfg(test)]
    off: Option<Off>,
}

/// THE WAIT TALLY'S VIEW OF THE QUEUES ([`crate::wait_tally`]).
#[derive(Debug, Default)]
struct Wakes {
    event: Hist,
    record: Hist,
    queued_event: Hist,
    queued_record: Hist,
}

impl Wakes {
    fn taken(&mut self, stamp: Option<Stamp>, record: bool) {
        let Some(stamp) = stamp else {
            return;
        };
        let (ns, parked) = stamp.taken();
        let row = match (parked, record) {
            (true, false) => &mut self.event,
            (true, true) => &mut self.record,
            (false, false) => &mut self.queued_event,
            (false, true) => &mut self.queued_record,
        };
        row.add(ns, Unit::Ns);
    }
}

/// Position mark in aterm's event queue ([`Mailbox::event_mark`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct EventMark(u64);

fn is_drain_event(line: &str) -> bool {
    let mut toks = line.split_whitespace();
    toks.next() == Some("EVENT") && matches!(toks.nth(1), Some("post" | "fetch"))
}

const fn over_bound(rows: usize, bytes: usize) -> bool {
    rows > 0 && (rows >= QUEUE_ROWS_MAX || bytes >= QUEUE_BYTES_MAX)
}

const fn at_low_water(rows: usize, bytes: usize) -> bool {
    rows <= QUEUE_ROWS_MAX / 2 && bytes <= QUEUE_BYTES_MAX / 2
}

impl Queues {
    fn stamp(&self) -> Option<Stamp> {
        self.stamping.then(|| Stamp::now(self.parked))
    }

    fn is_empty(&self) -> bool {
        self.fleet.records.is_empty()
            && self.events.is_empty()
            && self.inbox.records.is_empty()
            && self.say.records.is_empty()
            && self.closed.is_empty()
            && self.resume.is_empty()
            && self.fleet.gaps.is_empty()
            && self.inbox.gaps.is_empty()
            && self.say.gaps.is_empty()
    }

    const fn face(&mut self, s: Source) -> Option<&mut Face> {
        match s {
            Source::Fleet => Some(&mut self.fleet),
            Source::Inbox => Some(&mut self.inbox),
            Source::Say => Some(&mut self.say),
            Source::Aterm => None,
        }
    }

    /// The next item by priority, judged at `now` ([`Queues::inbox_due`]).
    fn take(&mut self, now: Instant) -> Option<(Item, bool)> {
        if let Some(item) = self.closed.pop_front() {
            return Some((item, false));
        }
        if let Some(g) = self.fleet.take_gap() {
            return Some((Item::Gap(Source::Fleet, g), false));
        }
        if let Some(r) = self.take_record(Source::Fleet) {
            return Some((Item::Fleet(r), false));
        }
        if let Some((s, from)) = self.resume.pop_front() {
            return Some((Item::Resume(s, from), false));
        }
        if !self.inbox_due(now) {
            if let Some((_, e, stamp)) = self.events.pop_front() {
                self.wakes.taken(stamp, false);
                self.turn_taken = 0;
                if self.inbox.queued() > 0 {
                    self.events_run = self.events_run.saturating_add(1);
                }
                return Some((Item::Event(e), over_bound(self.events.len() + 1, 0)));
            }
        }
        if let Some(item) = self.take_inbox(now) {
            return Some((item, false));
        }
        if let Some(g) = self.say.take_gap() {
            return Some((Item::Gap(Source::Say, g), false));
        }
        self.take_record(Source::Say).map(|r| (Item::Say(r), false))
    }

    /// WHETHER THE INBOX FACE IS TAKEN BEFORE aterm's NEXT EVENT (C01): while a turn is under
    /// way, or once [`EVENT_RUN_MAX`] events were taken in a row while it waited, or once its
    /// head has waited [`INBOX_WAIT_MAX`] behind them. A turn that begins here takes every
    /// item queued now, within the remaining consecutive-inbox budget [`INBOX_TURN_MAX`].
    /// Exhausting that budget lets a pending event go next even if the head is old.
    fn inbox_due(&mut self, now: Instant) -> bool {
        let queued = self.inbox.queued();
        if queued == 0 {
            self.turn_left = 0;
            #[cfg(test)]
            if self.off == Some(Off::Empty) {
                self.turn_taken = 0;
            }
            return false;
        }
        #[cfg(test)]
        if self.off == Some(Off::Bound) {
            return false;
        }
        if self.turn_taken >= INBOX_TURN_MAX {
            return false;
        }
        if self.turn_left > 0 {
            return true;
        }
        let waited = self
            .inbox
            .head_at()
            .map(|at| self.inbox_took.map_or(at, |took| took.max(at)))
            .map_or(Duration::ZERO, |since| now.saturating_duration_since(since));
        #[cfg(test)]
        let waited = if self.off == Some(Off::Age) {
            Duration::ZERO
        } else {
            waited
        };
        if self.events_run < EVENT_RUN_MAX && waited < INBOX_WAIT_MAX {
            return false;
        }
        #[cfg(test)]
        let queued = if self.off == Some(Off::Turn) {
            1
        } else {
            queued
        };
        self.turn_left = u32::try_from(queued)
            .unwrap_or(u32::MAX)
            .min(INBOX_TURN_MAX - self.turn_taken);
        true
    }

    /// The inbox face's next item, a due gap notice before the next record; the run of events
    /// starts again once the face is empty or consecutive turns took [`INBOX_TURN_MAX`]. The
    /// turn budget does not start again when the face empties: a record that comes while the
    /// one in hand is handled, and is overdue when it is, would otherwise begin a turn with the
    /// whole budget each time and hold a pending event back without limit.
    fn take_inbox(&mut self, now: Instant) -> Option<Item> {
        let item = match self.inbox.take_gap() {
            Some(g) => Item::Gap(Source::Inbox, g),
            None => Item::Inbox(self.take_record(Source::Inbox)?),
        };
        self.inbox_took = Some(now);
        if self.turn_left > 0 {
            self.turn_left -= 1;
        }
        self.turn_taken = self.turn_taken.saturating_add(1).min(INBOX_TURN_MAX);
        let capped = self.turn_taken >= INBOX_TURN_MAX;
        #[cfg(test)]
        if self.off == Some(Off::Carry) && self.turn_left == 0 {
            self.turn_taken = 0;
        }
        #[cfg(test)]
        let capped = (capped && self.off != Some(Off::CapRun)) || self.off == Some(Off::Turn);
        if capped || self.inbox.queued() == 0 {
            self.events_run = 0;
        }
        if self.inbox.queued() == 0 {
            self.turn_left = 0;
            #[cfg(test)]
            if self.off == Some(Off::Empty) {
                self.turn_taken = 0;
            }
        }
        Some(item)
    }

    fn take_record(&mut self, s: Source) -> Option<Record> {
        let (r, stamp, resume) = self.face(s)?.take()?;
        self.wakes.taken(stamp, true);
        if let Some(from) = resume {
            self.resume.push_back((s, from));
        }
        Some(r)
    }

    fn take_urgent(&mut self) -> Option<Item> {
        if let Some(g) = self.fleet.take_gap() {
            return Some(Item::Gap(Source::Fleet, g));
        }
        if let Some(r) = self.take_record(Source::Fleet) {
            return Some(Item::Fleet(r));
        }
        if let Some((source, from)) = self
            .resume
            .iter()
            .position(|(source, _)| *source == Source::Fleet)
            .and_then(|at| self.resume.remove(at))
        {
            return Some(Item::Resume(source, from));
        }
        let at = self
            .closed
            .iter()
            .position(|item| !matches!(item, Item::Closed(Source::Aterm)))?;
        self.closed.remove(at)
    }

    fn reset(&mut self, s: Source) {
        let Some(face) = self.face(s) else {
            return;
        };
        face.reset();
        if s == Source::Inbox {
            self.events_run = 0;
            self.turn_left = 0;
            self.turn_taken = 0;
            self.inbox_took = None;
        }
        self.resume.retain(|(face, _)| *face != s);
        self.closed.retain(|item| match item {
            Item::Closed(face) | Item::Lost(face, _) | Item::Refused(face, _) => *face != s,
            _ => true,
        });
    }
}

/// The bridge's priority-ordered input queue set.
#[cfg_attr(trust_verify, trust::paired)]
#[derive(Default)]
pub struct Mailbox {
    queues: Mutex<Queues>,
    ready: Condvar,
    /// Woken when the loop takes an event.
    drained: Condvar,
}

impl Mailbox {
    fn lock(&self) -> std::sync::MutexGuard<'_, Queues> {
        self.queues.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Whether an input that outranks broadcast replay is waiting.
    pub(crate) fn replay_pressure(&self) -> ReplayPressure {
        let q = self.lock();
        if !q.closed.is_empty()
            || !q.fleet.records.is_empty()
            || q.resume.iter().any(|(source, _)| *source == Source::Fleet)
        {
            ReplayPressure::Urgent
        } else if !q.events.is_empty() || !q.inbox.records.is_empty() || !q.inbox.gaps.is_empty() {
            ReplayPressure::Pending
        } else {
            ReplayPressure::Clear
        }
    }

    /// Stamp every push from now on for the wait tally (`true`).
    pub(crate) fn stamp(&self, on: bool) {
        self.lock().stamping = on;
    }

    /// Whether a taker is parked on [`Mailbox::take`] now.
    #[cfg(test)]
    pub(crate) fn parked(&self) -> bool {
        self.lock().parked
    }

    /// Copy the stamped items' waits into the tally's wake rows.
    pub(crate) fn copy_wakes(&self, t: &mut Tally) {
        let q = self.lock();
        for (row, h) in [
            (Row::WakeEvent, &q.wakes.event),
            (Row::WakeRecord, &q.wakes.record),
            (Row::QueuedEvent, &q.wakes.queued_event),
            (Row::QueuedRecord, &q.wakes.queued_record),
        ] {
            t.rows[row.index()] = *h;
        }
    }

    /// Whether NOTHING is queued for the loop — no record, event, closure or resume.
    pub(crate) fn is_idle(&self) -> bool {
        self.lock().is_empty()
    }

    /// Whether an inbox-group record is queued behind the one being handled.
    pub(crate) fn inbox_queued(&self) -> bool {
        !self.lock().inbox.records.is_empty()
    }

    /// Current incarnation of broker source `s` for stamping a new reader.
    #[must_use]
    pub fn broker_generation(&self, s: Source) -> u64 {
        self.lock().face(s).map_or(0, |face| face.generation)
    }

    /// Push one record from broker source `s` under its reader `generation`, read at `at` (the
    /// reader's clock: `Fx::now`'s world), which its wait is judged from ([`INBOX_WAIT_MAX`]).
    pub fn push(&self, s: Source, r: Record, generation: u64, at: Instant) -> Pushed {
        let mut q = self.lock();
        let stamp = q.stamp();
        let Some(face) = q.face(s).filter(|face| face.generation == generation) else {
            return Pushed::Stale;
        };
        let resume_from = r.0.saturating_add(1);
        face.bytes = face.bytes.saturating_add(record_bytes(&r));
        face.pushed = face.pushed.saturating_add(1);
        face.records.push_back((r, at, stamp));
        let pushed = if over_bound(face.records.len(), face.bytes) {
            face.paused = Some(resume_from);
            Pushed::Paused
        } else {
            Pushed::Queued
        };
        drop(q);
        self.ready.notify_all();
        pushed
    }

    /// Push one gap notice from broker source `s` under its reader `generation`, in its place
    /// among the source's records ([`Item::Gap`]), read at `at` as [`Mailbox::push`]'s record
    /// is; `false` for a torn-down generation's.
    pub fn push_gap(
        &self,
        s: Source,
        g: astream_broker::Gap,
        generation: u64,
        at: Instant,
    ) -> bool {
        let mut q = self.lock();
        let Some(face) = q.face(s).filter(|face| face.generation == generation) else {
            return false;
        };
        face.push_gap(g, at);
        drop(q);
        self.ready.notify_all();
        true
    }

    /// Where paused broker source `s` resumes from, while it is paused.
    #[cfg(test)]
    pub(crate) fn paused(&self, s: Source) -> Option<u64> {
        self.lock().face(s).and_then(|face| face.paused)
    }

    /// Snapshot the current position in aterm's event queue ([`Mailbox::forget_queued_drains`]).
    pub fn event_mark(&self) -> EventMark {
        EventMark(self.lock().events_pushed)
    }

    /// Forget queued `post` and `fetch` events pushed before `mark`, returning the count removed.
    pub fn forget_queued_drains(&self, mark: EventMark) -> usize {
        let mut q = self.lock();
        let before = q.events.len();
        q.events
            .retain(|(pushed, line, _)| *pushed >= mark.0 || !is_drain_event(line));
        let forgot = before - q.events.len();
        drop(q);
        if forgot > 0 {
            self.drained.notify_all();
        }
        forgot
    }

    /// Push one aterm `EVENT` line, waiting while the event queue is over its row bound.
    pub fn push_event(&self, line: String) {
        let mut q = self.lock();
        while over_bound(q.events.len(), 0) {
            q = self.wait_for_room(q);
        }
        let pushed = q.events_pushed;
        q.events_pushed = pushed.saturating_add(1);
        let stamp = q.stamp();
        q.events.push_back((pushed, line, stamp));
        drop(q);
        self.ready.notify_all();
    }

    fn wait_for_room<'a>(
        &'a self,
        q: std::sync::MutexGuard<'a, Queues>,
    ) -> std::sync::MutexGuard<'a, Queues> {
        self.drained
            .wait_timeout(q, Duration::from_millis(250))
            .unwrap_or_else(|p| p.into_inner())
            .0
    }

    /// Report that a broker reader ended, and how its read ended ([`Item::Lost`]).
    pub fn push_lost(&self, s: Source, generation: u64, end: End) {
        self.push_end(s, generation, Item::Lost(s, end));
    }

    /// Report that the broker refused a reader's subscription.
    pub fn push_refused(&self, s: Source, generation: u64, reason: &'static str) {
        self.push_end(s, generation, Item::Refused(s, reason));
    }

    fn push_end(&self, s: Source, generation: u64, item: Item) {
        let mut q = self.lock();
        if q.face(s).is_none_or(|face| face.generation != generation) {
            return;
        }
        q.closed.push_back(item);
        drop(q);
        self.ready.notify_all();
    }

    /// Latch aterm's last `bye` word before it closes the lanes.
    pub fn note_bye(&self, bye: aterm_types::fabric_grammar::Bye) {
        self.lock().bye = Some(bye);
    }

    /// The word [`Mailbox::note_bye`] latched, if aterm said one.
    #[must_use]
    pub fn bye(&self) -> Option<aterm_types::fabric_grammar::Bye> {
        self.lock().bye
    }

    /// Report that aterm went away.
    pub fn push_aterm_closed(&self) {
        self.lock().closed.push_back(Item::Closed(Source::Aterm));
        self.ready.notify_all();
    }

    /// Whether aterm's closure ([`Mailbox::push_aterm_closed`]) is queued.
    pub(crate) fn aterm_closed(&self) -> bool {
        self.lock()
            .closed
            .iter()
            .any(|item| matches!(item, Item::Closed(Source::Aterm)))
    }

    /// Forget one broker source's queued inputs and advance its generation.
    pub fn reset_source(&self, s: Source) {
        self.lock().reset(s);
    }

    /// Forget every broker input and advance all broker generations on reconnect.
    pub fn reset_broker_sources(&self) {
        let mut q = self.lock();
        for s in [Source::Fleet, Source::Inbox, Source::Say] {
            q.reset(s);
        }
    }

    /// THE URGENT ITEMS: the fleet face's records, each of its gap notices in its place among
    /// them ([`Item::Gap`]), its resume, then a broker closure.
    pub(crate) fn take_urgent(&self) -> Option<Item> {
        self.lock().take_urgent()
    }

    /// Block until an item is ready to take or `timeout` elapses, judging the inbox face's wait
    /// at `now` (the caller's clock, `Fx::now`'s world). A take that parked judges at `now` as
    /// well, which can only under-count an age: a park begins with nothing queued, so whatever
    /// it takes came after `now` and is judged to have waited nothing yet.
    pub fn take(&self, timeout: Duration, now: Instant) -> Option<Item> {
        let mut q = self.lock();
        if let Some((item, released)) = q.take(now) {
            drop(q);
            if released {
                self.drained.notify_all();
            }
            return Some(item);
        }
        q.parked = true;
        let (mut q, _) = self
            .ready
            .wait_timeout_while(q, timeout, |q: &mut Queues| q.is_empty())
            .unwrap_or_else(|p| p.into_inner());
        q.parked = false;
        let item = q.take(now);
        drop(q);
        if item.as_ref().is_some_and(|(_, released)| *released) {
            self.drained.notify_all();
        }
        item.map(|(item, _)| item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(off: u64) -> Record {
        (off, format!("/s/{off}"), Vec::new())
    }

    /// The one instant the order tests push and take at: no inbox item ages, so only the run
    /// and the turn decide ([`INBOX_WAIT_MAX`]'s own tests move a clock of their own).
    fn t0() -> Instant {
        static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *T0.get_or_init(Instant::now)
    }

    #[test]
    fn a_push_to_a_parked_taker_is_a_wake_and_one_to_a_working_loop_is_queued() {
        use std::sync::Arc;
        let counted = |stamping: bool| -> Tally {
            let mb = Arc::new(Mailbox::default());
            mb.stamp(stamping);
            mb.push_event("EVENT 1 post 1".into());
            assert_eq!(mb.push(Source::Inbox, rec(4), 0, t0()), Pushed::Queued);
            assert!(matches!(
                mb.take(Duration::ZERO, t0()),
                Some(Item::Event(_))
            ));
            assert!(matches!(
                mb.take(Duration::ZERO, t0()),
                Some(Item::Inbox((4, _, _)))
            ));
            for record in [false, true] {
                let taker = {
                    let mb = Arc::clone(&mb);
                    std::thread::spawn(move || mb.take(aterm_spec::wait::MUST_HAPPEN, t0()))
                };
                aterm_spec::wait::until_within(
                    aterm_spec::wait::MUST_HAPPEN,
                    "the taker parked on the mailbox",
                    || mb.parked().then_some(()),
                );
                if record {
                    assert_eq!(mb.push(Source::Inbox, rec(5), 0, t0()), Pushed::Queued);
                } else {
                    mb.push_event("EVENT 1 post 2".into());
                }
                let took = taker.join().expect("the taker");
                assert!(
                    match took {
                        Some(Item::Event(_)) => !record,
                        Some(Item::Inbox((5, _, _))) => record,
                        _ => false,
                    },
                    "the parked take answered the push: {took:?}"
                );
                assert!(!mb.parked(), "a take that answered is parked no more");
            }
            let mut t = Tally::default();
            mb.copy_wakes(&mut t);
            t
        };
        let on = counted(true);
        assert_eq!(
            [
                Row::WakeEvent,
                Row::WakeRecord,
                Row::QueuedEvent,
                Row::QueuedRecord
            ]
            .map(|row| on.row(row).count),
            [1, 1, 1, 1],
            "each item in its own row, once"
        );
        assert_eq!(
            counted(false),
            Tally::default(),
            "NEGATIVE CONTROL: unstamped pushes count nothing"
        );
    }

    #[test]
    fn the_priority_is_the_declared_one() {
        let mb = Mailbox::default();
        let big = (2, "/s".to_string(), vec![0u8; QUEUE_BYTES_MAX]);
        assert_eq!(mb.push(Source::Fleet, big, 0, t0()), Pushed::Paused);
        assert!(matches!(
            mb.take(Duration::ZERO, t0()),
            Some(Item::Fleet(_))
        ));
        mb.push_event("EVENT 1 post 1".into());
        assert_eq!(mb.push(Source::Say, rec(7), 0, t0()), Pushed::Queued);
        assert_eq!(mb.push(Source::Inbox, rec(4), 0, t0()), Pushed::Queued);
        assert_eq!(mb.push(Source::Fleet, rec(1), 0, t0()), Pushed::Queued);
        mb.push_lost(Source::Inbox, 0, End::Eof);
        let mut seen = Vec::new();
        while let Some(item) = mb.take(Duration::from_millis(0), t0()) {
            seen.push(match item {
                Item::Closed(_) | Item::Lost(..) | Item::Refused(..) => "closed",
                Item::Fleet(_) => "fleet",
                Item::Resume(..) => "resume",
                Item::Event(_) => "event",
                Item::Inbox(_) => "inbox",
                Item::Say(_) => "say",
                Item::Gap(..) => "gap",
            });
        }
        assert_eq!(seen, ["closed", "fleet", "resume", "event", "inbox", "say"]);
    }

    /// A GAP NOTICE IS TAKEN IN ITS PLACE among its source's records (astream's tail writes it
    /// before the records past what retention gave up): records 1 and 2, a notice, record 9 are
    /// taken in that order; two notices with no record between them are one (`Gap::and`); a
    /// notice of a torn-down generation is refused; and a notice alone wakes a parked take.
    /// NEGATIVE CONTROL (run, B22): a `push_gap` that queues nothing fails the order.
    #[test]
    fn a_gap_notice_is_taken_in_its_place_among_the_records() {
        use astream_broker::Gap;
        let mb = Mailbox::default();
        let gap = |from, to, records| Gap { from, to, records };
        assert_eq!(mb.push(Source::Inbox, rec(1), 0, t0()), Pushed::Queued);
        assert_eq!(mb.push(Source::Inbox, rec(2), 0, t0()), Pushed::Queued);
        assert!(mb.push_gap(Source::Inbox, gap(3, 6, Some(2)), 0, t0()));
        assert!(mb.push_gap(Source::Inbox, gap(6, 9, Some(1)), 0, t0()));
        assert_eq!(mb.push(Source::Inbox, rec(9), 0, t0()), Pushed::Queued);
        assert!(
            !mb.push_gap(Source::Inbox, gap(0, 1, None), 7, t0()),
            "a stale reader's"
        );
        let mut seen = Vec::new();
        while let Some(item) = mb.take(Duration::ZERO, t0()) {
            seen.push(match item {
                Item::Inbox((off, _, _)) => format!("inbox {off}"),
                Item::Gap(Source::Inbox, g) => format!("gap {}..{} {:?}", g.from, g.to, g.records),
                other => panic!("{other:?}"),
            });
        }
        assert_eq!(
            seen,
            ["inbox 1", "inbox 2", "gap 3..9 Some(3)", "inbox 9"],
            "in place, the two adjacent notices one"
        );
        assert!(mb.is_idle());
        let mb = std::sync::Arc::new(mb);
        let taker = {
            let mb = std::sync::Arc::clone(&mb);
            std::thread::spawn(move || mb.take(aterm_spec::wait::MUST_HAPPEN, t0()))
        };
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the taker parked on the mailbox",
            || mb.parked().then_some(()),
        );
        assert!(mb.push_gap(Source::Inbox, gap(10, 12, None), 0, t0()));
        assert!(
            matches!(
                taker.join().expect("the taker"),
                Some(Item::Gap(
                    Source::Inbox,
                    Gap {
                        from: 10,
                        to: 12,
                        records: None
                    }
                ))
            ),
            "a notice alone wakes the loop"
        );
        mb.reset_source(Source::Inbox);
        assert!(
            !mb.push_gap(Source::Inbox, gap(0, 1, None), 0, t0()),
            "reset: generation 0 is gone"
        );
        assert!(mb.push_gap(Source::Inbox, gap(0, 1, None), 1, t0()));
        assert!(!mb.is_idle());
    }

    /// AN INBOX GAP NOTICE IS INBOX WORK: alone on its face it still waits behind at most
    /// [`EVENT_RUN_MAX`] events, as a record does, and it holds broadcast replay back as one does
    /// ([`ReplayPressure::Pending`]). NEGATIVE CONTROL (run, B22 land): with the gap clause gone
    /// from `waiting`, the notice waits behind every event; gone from the pressure, replay reads
    /// `Clear` over it.
    #[test]
    fn an_inbox_gap_notice_waits_as_an_inbox_record_does() {
        use astream_broker::Gap;
        let k = usize::try_from(EVENT_RUN_MAX).expect("a small bound");
        let mb = Mailbox::default();
        assert_eq!(mb.replay_pressure(), ReplayPressure::Clear);
        let gap = Gap {
            from: 3,
            to: 6,
            records: Some(2),
        };
        assert!(mb.push_gap(Source::Inbox, gap, 0, t0()));
        assert_eq!(
            mb.replay_pressure(),
            ReplayPressure::Pending,
            "a lone inbox notice holds replay back"
        );
        for i in 0..2 * k {
            mb.push_event(format!("EVENT 1 post {i}"));
        }
        let mut seen = Vec::new();
        while let Some(item) = mb.take(Duration::ZERO, t0()) {
            seen.push(match item {
                Item::Event(_) => "event".to_string(),
                Item::Gap(Source::Inbox, g) => format!("gap {}..{}", g.from, g.to),
                other => panic!("{other:?}"),
            });
        }
        let mut want: Vec<String> = std::iter::repeat_n("event".to_string(), k).collect();
        want.push("gap 3..6".to_string());
        want.extend(std::iter::repeat_n("event".to_string(), k));
        assert_eq!(seen, want, "a run of {k}, then the notice");
        assert_eq!(mb.replay_pressure(), ReplayPressure::Clear);
    }

    /// A FLEET GAP NOTICE KEEPS ITS PLACE ON THE URGENT PATH, as on [`Mailbox::take`]: a notice
    /// queued ahead of the fleet records and one between them are handed out where they came.
    /// NEGATIVE CONTROL (run, B22 land): a `take_urgent` that drains the records without looking
    /// for a due notice hands both records out first, and the notices fall to `take`.
    #[test]
    fn a_fleet_gap_notice_keeps_its_place_on_the_urgent_path() {
        use astream_broker::Gap;
        let gap = |from, to| Gap {
            from,
            to,
            records: None,
        };
        let mb = Mailbox::default();
        assert!(mb.push_gap(Source::Fleet, gap(0, 2), 0, t0()));
        assert_eq!(mb.push(Source::Fleet, rec(2), 0, t0()), Pushed::Queued);
        assert!(mb.push_gap(Source::Fleet, gap(3, 5), 0, t0()));
        assert_eq!(mb.push(Source::Fleet, rec(5), 0, t0()), Pushed::Queued);
        let mut urgent = Vec::new();
        while let Some(item) = mb.take_urgent() {
            urgent.push(match item {
                Item::Fleet((off, _, _)) => format!("fleet {off}"),
                Item::Gap(Source::Fleet, g) => format!("gap {}..{}", g.from, g.to),
                other => panic!("not urgent: {other:?}"),
            });
        }
        assert_eq!(urgent, ["gap 0..2", "fleet 2", "gap 3..5", "fleet 5"]);
        assert!(mb.is_idle(), "nothing left for take");
    }

    /// A RUN OF [`EVENT_RUN_MAX`] EVENTS MAKES THE INBOX DUE, and its turn takes every record
    /// queued then (no clock moves here, so no record ages). NEGATIVE CONTROLS: unbounded
    /// ([`Off::Bound`]), every record waits behind every event; with no turn ([`Off::Turn`], the
    /// rule before C01), each record after the first waits a whole run of its own.
    #[test]
    fn an_inbox_record_waits_behind_at_most_a_bounded_run_of_events() {
        let k = usize::try_from(EVENT_RUN_MAX).expect("a small bound");
        let order = |off: Option<Off>| {
            let mb = Mailbox::default();
            mb.lock().off = off;
            for i in 0..3 {
                assert_eq!(mb.push(Source::Inbox, rec(10 + i), 0, t0()), Pushed::Queued);
            }
            for i in 0..2 * k + 5 {
                mb.push_event(format!("EVENT 1 post {i}"));
            }
            let mut seen = Vec::new();
            while let Some(item) = mb.take(Duration::ZERO, t0()) {
                seen.push(match item {
                    Item::Event(_) => "event".to_string(),
                    Item::Inbox((off, _, _)) => format!("inbox {off}"),
                    Item::Fleet((off, _, _)) => format!("fleet {off}"),
                    other => panic!("{other:?}"),
                });
                if seen.len() == 3 {
                    assert_eq!(mb.push(Source::Fleet, rec(1), 0, t0()), Pushed::Queued);
                }
            }
            seen
        };
        let mut want: Vec<String> = Vec::new();
        let events = |n: usize| std::iter::repeat_n("event".to_string(), n);
        want.extend(events(3));
        want.push("fleet 1".to_string());
        want.extend(events(k - 3));
        want.extend(["inbox 10", "inbox 11", "inbox 12"].map(String::from));
        want.extend(events(k + 5));
        assert_eq!(
            order(None),
            want,
            "a run of {k}, then the records in one turn"
        );

        let mut control: Vec<String> = Vec::new();
        control.extend(events(3));
        control.push("fleet 1".to_string());
        control.extend(events(2 * k + 5 - 3));
        control.extend(["inbox 10", "inbox 11", "inbox 12"].map(String::from));
        assert_eq!(
            order(Some(Off::Bound)),
            control,
            "NEGATIVE CONTROL: unbounded, every record waits behind every event"
        );

        let mut one_each: Vec<String> = Vec::new();
        one_each.extend(events(3));
        one_each.push("fleet 1".to_string());
        one_each.extend(events(k - 3));
        one_each.push("inbox 10".to_string());
        one_each.extend(events(k));
        one_each.push("inbox 11".to_string());
        one_each.extend(events(5));
        one_each.push("inbox 12".to_string());
        assert_eq!(
            order(Some(Off::Turn)),
            one_each,
            "NEGATIVE CONTROL: one record a turn, each waits a run of its own"
        );

        let mb = Mailbox::default();
        for i in 0..k {
            mb.push_event(format!("EVENT 1 post {i}"));
        }
        for _ in 0..k {
            assert!(matches!(
                mb.take(Duration::ZERO, t0()),
                Some(Item::Event(_))
            ));
        }
        assert_eq!(mb.push(Source::Inbox, rec(20), 0, t0()), Pushed::Queued);
        for i in 0..=k {
            mb.push_event(format!("EVENT 1 post {i}"));
        }
        for _ in 0..k {
            assert!(matches!(
                mb.take(Duration::ZERO, t0()),
                Some(Item::Event(_))
            ));
        }
        assert!(
            matches!(mb.take(Duration::ZERO, t0()), Some(Item::Inbox((20, _, _)))),
            "the record is next once its own run is spent"
        );
        assert!(matches!(
            mb.take(Duration::ZERO, t0()),
            Some(Item::Event(_))
        ));
    }

    /// One take of [`Mailbox::take`] under a moving clock: what it took, and when.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Took {
        Event(Duration),
        Inbox(u64, Duration),
        Gap(Duration),
    }

    /// Take everything from `mb`, the clock starting at `start` and moving by the next of `cost`
    /// (cycled) after each item: what the item in hand took the loop. `at` pushes more inbox
    /// records: `(after this many takes, offset)`, pushed at the clock then.
    fn take_timed(
        mb: &Mailbox,
        start: Instant,
        cost: &[Duration],
        at: &[(usize, u64)],
    ) -> Vec<Took> {
        let mut now = start;
        let mut took = Vec::new();
        let mut cost = cost.iter().cycle();
        loop {
            for (_, off) in at.iter().filter(|(n, _)| *n == took.len()) {
                assert_eq!(mb.push(Source::Inbox, rec(*off), 0, now), Pushed::Queued);
            }
            let Some(item) = mb.take(Duration::ZERO, now) else {
                return took;
            };
            let t = now.saturating_duration_since(start);
            took.push(match item {
                Item::Event(_) => Took::Event(t),
                Item::Inbox((off, _, _)) => Took::Inbox(off, t),
                Item::Gap(Source::Inbox, _) => Took::Gap(t),
                other => panic!("{other:?}"),
            });
            now += *cost.next().expect("a cost");
        }
    }

    /// THE WAIT LAW (C01): no event is taken past an inbox item that has waited
    /// [`INBOX_WAIT_MAX`] behind events, so an inbox item waits behind events no longer than the
    /// bound plus the one item in hand when it fell due; and the records queued together are
    /// taken in one turn, no event between them. Records pushed at `pushed` (since `start`)
    /// were taken in `took`: `Err` names the take that broke the law.
    fn wait_law(took: &[Took], pushed: &[(u64, Duration)]) -> Result<(), String> {
        for (off, at) in pushed {
            let taken = took
                .iter()
                .position(|t| matches!(t, Took::Inbox(o, _) if o == off))
                .ok_or_else(|| format!("inbox {off} was never taken"))?;
            let mut since = *at;
            for t in &took[..taken] {
                match t {
                    Took::Event(when) if when.saturating_sub(since) >= INBOX_WAIT_MAX => {
                        return Err(format!(
                            "an event at {when:?} passed inbox {off}, waiting since {since:?}"
                        ));
                    }
                    Took::Inbox(_, when) | Took::Gap(when) if when > &since => since = *when,
                    _ => {}
                }
            }
        }
        let mut groups: Vec<Vec<u64>> = Vec::new();
        for (off, at) in pushed {
            match groups.last_mut() {
                Some(g) if pushed.iter().any(|(o, a)| o == &g[0] && a == at) => g.push(*off),
                _ => groups.push(vec![*off]),
            }
        }
        for g in groups {
            let at = |off: &u64| {
                took.iter()
                    .position(|t| matches!(t, Took::Inbox(o, _) if o == off))
            };
            let (first, last) = (at(&g[0]), at(g.last().expect("a group")));
            if let (Some(first), Some(last)) = (first, last) {
                if took[first..=last]
                    .iter()
                    .any(|t| matches!(t, Took::Event(_)))
                {
                    return Err(format!(
                        "the records {g:?}, queued together, took two turns"
                    ));
                }
            }
        }
        Ok(())
    }

    /// AN INBOX RECORD WAITS BEHIND aterm's EVENTS NO LONGER THAN [`INBOX_WAIT_MAX`] PLUS ONE
    /// ITEM, and the records queued together are taken in one turn ([`wait_law`]): three
    /// records queued together behind 64 events spread over 16 times the bound are taken
    /// together once the bound is out, a record that comes during the run waits the bound from
    /// its own push, and one event that alone outlasts the bound is the one item. NEGATIVE
    /// CONTROLS: with no age ([`Off::Age`], the rule before C01), every record waits out the
    /// run of 64, so an event passes a record that waited the bound; with no turn
    /// ([`Off::Turn`]), the three records take three turns.
    #[test]
    fn an_inbox_record_waits_behind_events_no_longer_than_the_bound_plus_one_item() {
        let k = usize::try_from(EVENT_RUN_MAX).expect("a small bound");
        let step = INBOX_WAIT_MAX / 4;
        let long = INBOX_WAIT_MAX * 3;
        let run = |off: Option<Off>| {
            let mb = Mailbox::default();
            mb.lock().off = off;
            let start = t0();
            for i in 0..k {
                mb.push_event(format!("EVENT 1 post {i}"));
            }
            for i in 0..3 {
                assert_eq!(
                    mb.push(Source::Inbox, rec(10 + i), 0, start),
                    Pushed::Queued
                );
            }
            let cost: Vec<Duration> = (0..k).map(|i| if i == 20 { long } else { step }).collect();
            take_timed(&mb, start, &cost, &[(12, 20), (19, 21)])
        };
        let took = run(None);
        let pushed_at = |off: u64| {
            let n = if off == 20 { 12 } else { 19 };
            match took[n - 1] {
                Took::Event(t) | Took::Inbox(_, t) | Took::Gap(t) => {
                    t + if n - 1 == 20 { long } else { step }
                }
            }
        };
        let pushed = [
            (10, Duration::ZERO),
            (11, Duration::ZERO),
            (12, Duration::ZERO),
            (20, pushed_at(20)),
            (21, pushed_at(21)),
        ];
        assert_eq!(wait_law(&took, &pushed), Ok(()), "{took:?}");
        let first = took
            .iter()
            .position(|t| matches!(t, Took::Inbox(10, _)))
            .expect("inbox 10");
        assert_eq!(
            took[first],
            Took::Inbox(10, INBOX_WAIT_MAX),
            "the turn starts as the bound runs out, long before the run of {k}: {took:?}"
        );
        let after_long = took
            .iter()
            .position(|t| matches!(t, Took::Inbox(21, _)))
            .expect("inbox 21");
        assert!(
            matches!(took[after_long - 1], Took::Event(_)),
            "the long event was in hand as 21 fell due, and 21 is next: {took:?}"
        );

        let no_age = run(Some(Off::Age));
        assert!(
            wait_law(&no_age, &pushed).is_err_and(|e| e.contains("passed inbox 10")),
            "NEGATIVE CONTROL: a run of {k} alone holds a record past the bound: {no_age:?}"
        );
        let no_turn = run(Some(Off::Turn));
        assert!(
            wait_law(&no_turn, &pushed).is_err_and(|e| e.contains("took two turns")),
            "NEGATIVE CONTROL: one record a turn splits what was queued together: {no_turn:?}"
        );
    }

    /// Consecutive short snapshot turns share the cap even if a reader keeps two items
    /// queued by pushing another after each take. Without carrying that budget across turns,
    /// the run stays at [`EVENT_RUN_MAX`] forever and no pending event is taken. The supplied
    /// clock also covers items that each outlast the age bound, without waiting in real time.
    #[test]
    fn refilling_short_inbox_turns_yield_to_an_event_at_the_cumulative_cap() {
        let cap = usize::try_from(INBOX_TURN_MAX).expect("a small cap");
        let run = |off, cost| {
            let mb = Mailbox::default();
            mb.lock().off = off;
            let start = t0();
            for i in 0..EVENT_RUN_MAX + 1 {
                mb.push_event(format!("EVENT 1 post {i}"));
            }
            for i in 0..2 {
                assert_eq!(mb.push(Source::Inbox, rec(i), 0, start), Pushed::Queued);
            }
            for _ in 0..EVENT_RUN_MAX {
                assert!(matches!(
                    mb.take(Duration::ZERO, start),
                    Some(Item::Event(_))
                ));
            }
            let mut inbox = 0;
            for i in 0..=cap {
                let now = start + cost * u32::try_from(i).expect("within the cap");
                match mb.take(Duration::ZERO, now) {
                    Some(Item::Inbox(_)) => {
                        inbox += 1;
                        assert_eq!(
                            mb.push(Source::Inbox, rec(2 + i as u64), 0, now),
                            Pushed::Queued
                        );
                    }
                    Some(Item::Event(_)) => return Some(inbox),
                    other => panic!("{other:?}"),
                }
            }
            None
        };
        for cost in [Duration::ZERO, INBOX_WAIT_MAX * 2] {
            assert_eq!(run(None, cost), Some(cap));
            assert_eq!(run(Some(Off::Carry), cost), None);
        }
    }

    /// AN EVENT WAITS BEHIND AT MOST [`INBOX_TURN_MAX`] INBOX ITEMS IN A ROW, THOUGH THE FACE
    /// EMPTIES BETWEEN THEM: one record at a time comes while the one in hand is handled, and
    /// each handling outlasts [`INBOX_WAIT_MAX`], so every record is overdue at the next take
    /// and begins a turn of its own; the turns share one budget, which only a taken event (or a
    /// reset of the face) starts again, so the pending event goes after the cap. NEGATIVE
    /// CONTROL: with an empty face starting the budget again ([`Off::Empty`], ff5a34af25's
    /// rule), each record's turn has the whole budget and the event is never taken. CONTROL:
    /// before C01 ([`Off::Age`]) the event went first.
    #[test]
    fn an_event_waits_behind_at_most_the_cap_though_the_face_empties_between_deliveries() {
        let cap = usize::try_from(INBOX_TURN_MAX).expect("a small cap");
        let slow = INBOX_WAIT_MAX + INBOX_WAIT_MAX / 5;
        let records_before_the_event = |off: Option<Off>| {
            let mb = Mailbox::default();
            mb.lock().off = off;
            let start = t0();
            assert_eq!(mb.push(Source::Inbox, rec(0), 0, start), Pushed::Queued);
            mb.push_event("EVENT 1 post 1".into());
            let mut now = start + INBOX_WAIT_MAX;
            for n in 0..4 * cap {
                match mb.take(Duration::ZERO, now) {
                    Some(Item::Event(_)) => return Some(n),
                    Some(Item::Inbox(_)) => {}
                    other => panic!("{other:?}"),
                }
                let during = now + INBOX_WAIT_MAX / 50;
                assert_eq!(
                    mb.push(Source::Inbox, rec(1 + n as u64), 0, during),
                    Pushed::Queued
                );
                now += slow;
            }
            None
        };
        assert_eq!(
            records_before_the_event(None),
            Some(cap),
            "the cap, then the event"
        );
        assert_eq!(
            records_before_the_event(Some(Off::Empty)),
            None,
            "NEGATIVE CONTROL: a budget that an empty face starts again holds the event back"
        );
        assert_eq!(
            records_before_the_event(Some(Off::Age)),
            Some(0),
            "CONTROL: before C01 the event went first"
        );
    }

    /// A TURN THAT TAKES THE CAP STARTS THE RUN AGAIN: a turn that [`EVENT_RUN_MAX`] events in
    /// a row began, with more queued than [`INBOX_TURN_MAX`], takes the cap and leaves the run
    /// at nothing, so the events go on until the run or the age (from the turn's last take)
    /// makes the rest due; no clock moves here, so here the events run out first. NEGATIVE
    /// CONTROL: a cap that keeps the run ([`Off::CapRun`]) makes the rest due after one event.
    #[test]
    fn a_turn_that_takes_the_cap_starts_the_run_again() {
        let k = usize::try_from(EVENT_RUN_MAX).expect("a small bound");
        let c = u64::from(INBOX_TURN_MAX);
        let order = |off: Option<Off>| {
            let mb = Mailbox::default();
            mb.lock().off = off;
            for i in 0..k + 3 {
                mb.push_event(format!("EVENT 1 post {i}"));
            }
            for off in 0..=c {
                assert_eq!(mb.push(Source::Inbox, rec(off), 0, t0()), Pushed::Queued);
            }
            let mut seen = Vec::new();
            while let Some(item) = mb.take(Duration::ZERO, t0()) {
                seen.push(match item {
                    Item::Event(_) => "event".to_string(),
                    Item::Inbox((off, _, _)) => format!("inbox {off}"),
                    other => panic!("{other:?}"),
                });
            }
            seen
        };
        let events = |n: usize| std::iter::repeat_n("event".to_string(), n);
        let turn = || (0..c).map(|off| format!("inbox {off}"));
        let mut want: Vec<String> = events(k).chain(turn()).collect();
        want.extend(events(3));
        want.push(format!("inbox {c}"));
        assert_eq!(
            order(None),
            want,
            "the run, the cap, then the events run on"
        );
        let mut control: Vec<String> = events(k).chain(turn()).collect();
        control.extend(events(1));
        control.push(format!("inbox {c}"));
        control.extend(events(2));
        assert_eq!(
            order(Some(Off::CapRun)),
            control,
            "NEGATIVE CONTROL: a cap that keeps the run makes the rest due after one event"
        );
    }

    /// A TURN TAKES WHAT WAS QUEUED AS IT BEGAN, AT MOST [`INBOX_TURN_MAX`]: of the cap and four
    /// more records queued together the turn takes the cap, and the other four wait the bound
    /// again, from the turn's last take, while the events run; a record pushed during a turn is
    /// not in it; and the run starts again once the face is empty, not at a turn that leaves a
    /// record pushed during it (it does at the cap too:
    /// `a_turn_that_takes_the_cap_starts_the_run_again`).
    #[test]
    fn an_inbox_turn_takes_what_was_queued_as_it_began_and_at_most_the_cap() {
        let cap = usize::try_from(INBOX_TURN_MAX).expect("a small cap");
        let c = u64::from(INBOX_TURN_MAX);
        let late = c + 100;
        let mb = Mailbox::default();
        let start = t0();
        for i in 0..8 {
            mb.push_event(format!("EVENT 1 post {i}"));
        }
        for off in 0..c + 4 {
            assert_eq!(mb.push(Source::Inbox, rec(off), 0, start), Pushed::Queued);
        }
        let due = start + INBOX_WAIT_MAX;
        let mut seen = Vec::new();
        for _ in 0..cap + 1 {
            seen.push(match mb.take(Duration::ZERO, due) {
                Some(Item::Inbox((off, _, _))) => format!("inbox {off}"),
                Some(Item::Event(_)) => "event".to_string(),
                other => panic!("{other:?}"),
            });
        }
        let mut want: Vec<String> = (0..c).map(|off| format!("inbox {off}")).collect();
        want.push("event".to_string());
        assert_eq!(seen, want, "the cap, then the events");
        assert!(
            matches!(
                mb.take(Duration::ZERO, due + INBOX_WAIT_MAX / 2),
                Some(Item::Event(_))
            ),
            "the rest wait from the turn's last take, not from their push"
        );
        let again = due + INBOX_WAIT_MAX;
        let mut rest = Vec::new();
        for _ in 0..4 {
            match mb.take(Duration::ZERO, again) {
                Some(Item::Inbox((off, _, _))) => rest.push(off),
                other => panic!("{other:?}"),
            }
            if rest == [c, c + 1] {
                assert_eq!(mb.push(Source::Inbox, rec(late), 0, again), Pushed::Queued);
            }
        }
        assert_eq!(rest, [c, c + 1, c + 2, c + 3], "the rest in one turn");
        assert_eq!(
            mb.lock().events_run,
            2,
            "a turn that leaves a record pushed during it keeps the run"
        );
        let mut seen = Vec::new();
        while let Some(item) = mb.take(Duration::ZERO, again + INBOX_WAIT_MAX / 2) {
            seen.push(match item {
                Item::Inbox((off, _, _)) => format!("inbox {off}"),
                Item::Event(_) => "event".to_string(),
                other => panic!("{other:?}"),
            });
        }
        let mut want: Vec<String> = std::iter::repeat_n("event".to_string(), 6).collect();
        want.push(format!("inbox {late}"));
        assert_eq!(
            seen, want,
            "{late} was not queued as the turn began: it waits its own bound, here the events out"
        );
        assert_eq!(
            mb.lock().events_run,
            0,
            "an empty face starts the run again"
        );
    }

    #[test]
    fn the_drain_events_queued_before_the_mark_are_forgotten_and_nothing_else() {
        let mb = Mailbox::default();
        for line in [
            "EVENT 1 post 1 to=@s-r kind=note",
            "EVENT 2 inbox 7 from=s-a kind=note off=40",
            "EVENT 1 post 2 to=@s-r kind=note",
            "EVENT 3 fetch off=9",
            "EVENT 2 inbox-seen 7 off=40 floor=- seen=7",
            "GAP 2 events-dropped=1",
            "EVENT 4 topic add post since=head",
        ] {
            mb.push_event(line.to_string());
        }
        assert!(matches!(
            mb.take(Duration::ZERO, t0()),
            Some(Item::Event(line)) if line.starts_with("EVENT 1 post 1 ")
        ));
        let mark = mb.event_mark();
        mb.push_event("EVENT 1 post 3 to=@s-r kind=note".to_string());
        assert_eq!(mb.forget_queued_drains(mark), 2, "post 2 and the fetch");
        mb.push_event("EVENT 1 post 4 to=@s-r kind=note".to_string());
        let mut left = Vec::new();
        while let Some(Item::Event(line)) = mb.take(Duration::ZERO, t0()) {
            left.push(line);
        }
        assert_eq!(
            left,
            [
                "EVENT 2 inbox 7 from=s-a kind=note off=40",
                "EVENT 2 inbox-seen 7 off=40 floor=- seen=7",
                "GAP 2 events-dropped=1",
                "EVENT 4 topic add post since=head",
                "EVENT 1 post 3 to=@s-r kind=note",
                "EVENT 1 post 4 to=@s-r kind=note",
            ]
        );
        assert_eq!(
            mb.forget_queued_drains(mark),
            0,
            "nothing left below the mark"
        );
    }

    #[test]
    fn a_reset_of_the_inbox_ends_the_run_it_was_counted_for() {
        let k = usize::try_from(EVENT_RUN_MAX).expect("a small bound");
        let first_after = |reset: fn(&Mailbox)| {
            let mb = Mailbox::default();
            assert_eq!(mb.push(Source::Inbox, rec(10), 0, t0()), Pushed::Queued);
            for i in 0..k {
                mb.push_event(format!("EVENT 1 post {i}"));
            }
            for _ in 0..k {
                assert!(matches!(
                    mb.take(Duration::ZERO, t0()),
                    Some(Item::Event(_))
                ));
            }
            reset(&mb);
            let g = mb.broker_generation(Source::Inbox);
            mb.push_event("EVENT 1 post fresh".to_string());
            assert_eq!(mb.push(Source::Inbox, rec(20), g, t0()), Pushed::Queued);
            match mb.take(Duration::ZERO, t0()) {
                Some(Item::Event(_)) => "event".to_string(),
                Some(Item::Inbox((off, _, _))) => format!("inbox {off}"),
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(
            first_after(Mailbox::reset_broker_sources),
            "event",
            "the reconnect's reset ended the run"
        );
        assert_eq!(
            first_after(|mb| mb.reset_source(Source::Inbox)),
            "event",
            "the face's own close ended the run"
        );
        assert_eq!(
            first_after(|mb| mb.reset_source(Source::Say)),
            "inbox 10",
            "CONTROL: another face's reset leaves the waiting record and its run"
        );

        let mb = Mailbox::default();
        for off in 0..3 {
            assert_eq!(mb.push(Source::Inbox, rec(off), 0, t0()), Pushed::Queued);
        }
        mb.push_event("EVENT 1 post 0".to_string());
        let due = t0() + INBOX_WAIT_MAX;
        assert!(matches!(
            mb.take(Duration::ZERO, due),
            Some(Item::Inbox((0, _, _)))
        ));
        mb.reset_source(Source::Inbox);
        let g = mb.broker_generation(Source::Inbox);
        assert_eq!(mb.push(Source::Inbox, rec(9), g, due), Pushed::Queued);
        assert!(
            matches!(mb.take(Duration::ZERO, due), Some(Item::Event(_))),
            "the reset ended the turn under way: the new record waits its own bound"
        );
    }

    #[test]
    fn a_duty_takes_the_halts_then_a_broker_closure_and_nothing_else() {
        let mb = Mailbox::default();
        mb.push_aterm_closed();
        let _ = mb.push(Source::Say, rec(7), 0, t0());
        let _ = mb.push(Source::Inbox, rec(4), 0, t0());
        mb.push_event("EVENT 1 post 1".into());
        mb.push_lost(Source::Fleet, 0, End::Eof);
        mb.push_refused(Source::Say, 0, "subscribe");
        let _ = mb.push(Source::Fleet, rec(1), 0, t0());
        let _ = mb.push(Source::Fleet, rec(2), 0, t0());
        let mut urgent = Vec::new();
        while let Some(item) = mb.take_urgent() {
            urgent.push(match item {
                Item::Fleet((off, _, _)) => format!("fleet {off}"),
                Item::Lost(s, _) => format!("closed {s:?}"),
                Item::Refused(s, why) => format!("refused {s:?} {why}"),
                other => panic!("not urgent: {other:?}"),
            });
        }
        assert_eq!(
            urgent,
            [
                "fleet 1",
                "fleet 2",
                "closed Fleet",
                "refused Say subscribe"
            ]
        );
        let mut rest = Vec::new();
        while let Some(item) = mb.take(Duration::ZERO, t0()) {
            rest.push(match item {
                Item::Closed(s) => format!("closed {s:?}"),
                Item::Event(_) => "event".into(),
                Item::Inbox(_) => "inbox".into(),
                Item::Say(_) => "say".into(),
                other => format!("{other:?}"),
            });
        }
        assert_eq!(rest, ["closed Aterm", "event", "inbox", "say"]);
    }

    #[test]
    fn replay_yields_to_every_queue_ahead_of_the_say_face() {
        let mb = Mailbox::default();
        assert_eq!(mb.replay_pressure(), ReplayPressure::Clear);
        let _ = mb.push(Source::Say, rec(1), 0, t0());
        assert_eq!(mb.replay_pressure(), ReplayPressure::Clear);

        let _ = mb.push(Source::Inbox, rec(2), 0, t0());
        assert_eq!(mb.replay_pressure(), ReplayPressure::Pending);
        assert!(matches!(
            mb.take(Duration::ZERO, t0()),
            Some(Item::Inbox(_))
        ));
        assert_eq!(mb.replay_pressure(), ReplayPressure::Clear);

        mb.push_event("EVENT 1 topic add probe".into());
        assert_eq!(mb.replay_pressure(), ReplayPressure::Pending);
        assert!(matches!(
            mb.take(Duration::ZERO, t0()),
            Some(Item::Event(_))
        ));
        assert_eq!(mb.replay_pressure(), ReplayPressure::Clear);

        let _ = mb.push(Source::Fleet, rec(3), 0, t0());
        assert_eq!(mb.replay_pressure(), ReplayPressure::Urgent);
        assert!(matches!(
            mb.take(Duration::ZERO, t0()),
            Some(Item::Fleet(_))
        ));
        assert_eq!(mb.replay_pressure(), ReplayPressure::Clear);

        mb.push_aterm_closed();
        assert_eq!(mb.replay_pressure(), ReplayPressure::Urgent);
    }

    #[test]
    fn a_reconnect_forgets_the_old_connections_inputs_but_not_aterms() {
        let mb = Mailbox::default();
        let big = |off| (off, "/s".to_string(), vec![0u8; QUEUE_BYTES_MAX]);
        assert_eq!(mb.push(Source::Inbox, big(5), 0, t0()), Pushed::Paused);
        assert!(matches!(
            mb.take(Duration::ZERO, t0()),
            Some(Item::Inbox(_))
        ));
        assert_eq!(mb.push(Source::Say, big(9), 0, t0()), Pushed::Paused);
        let _ = mb.push(Source::Fleet, rec(1), 0, t0());
        let _ = mb.push(Source::Inbox, rec(3), 0, t0());
        mb.push_event("EVENT 1 post 1".into());
        mb.push_lost(Source::Inbox, 0, End::Eof);
        mb.push_refused(Source::Fleet, 0, "subscribe");
        mb.push_aterm_closed();
        mb.reset_broker_sources();
        assert_eq!(
            mb.paused(Source::Say),
            None,
            "a reconnect lifts every pause"
        );
        let mut seen = Vec::new();
        while let Some(item) = mb.take(Duration::from_millis(0), t0()) {
            seen.push(match item {
                Item::Closed(Source::Aterm) => "closed-aterm",
                Item::Closed(_) | Item::Lost(..) | Item::Refused(..) => "closed-broker",
                Item::Fleet(_) => "fleet",
                Item::Resume(..) => "resume",
                Item::Event(_) => "event",
                Item::Inbox(_) => "inbox",
                Item::Say(_) => "say",
                Item::Gap(..) => "gap",
            });
        }
        assert_eq!(
            seen,
            ["closed-aterm", "event"],
            "only aterm's own inputs survive a broker reconnect"
        );
    }

    #[test]
    fn a_late_push_from_the_previous_incarnation_is_discarded() {
        let mb = Mailbox::default();
        let stale = mb.broker_generation(Source::Fleet);
        mb.reset_broker_sources();
        let fresh = mb.broker_generation(Source::Fleet);
        assert_ne!(stale, fresh, "a reconnect moves the generation");

        mb.push_lost(Source::Fleet, stale, End::Eof);
        mb.push_refused(Source::Inbox, stale, "subscribe");
        assert_eq!(mb.push(Source::Inbox, rec(2), stale, t0()), Pushed::Stale);
        assert_eq!(mb.push(Source::Fleet, rec(3), stale, t0()), Pushed::Stale);
        assert!(
            mb.take(Duration::from_millis(0), t0()).is_none(),
            "nothing from the previous incarnation may reach the fresh loop"
        );

        mb.push_lost(Source::Fleet, fresh, End::Eof);
        assert!(matches!(
            mb.take(Duration::from_millis(0), t0()),
            Some(Item::Lost(Source::Fleet, End::Eof))
        ));

        let say = mb.broker_generation(Source::Say);
        let inbox = mb.broker_generation(Source::Inbox);
        assert_eq!(mb.push(Source::Say, rec(4), say, t0()), Pushed::Queued);
        mb.push_lost(Source::Say, say, End::Eof);
        mb.reset_source(Source::Say);
        assert_ne!(mb.broker_generation(Source::Say), say);
        assert_eq!(mb.broker_generation(Source::Inbox), inbox);
        assert_eq!(mb.push(Source::Say, rec(5), say, t0()), Pushed::Stale);
        mb.push_lost(Source::Say, say, End::Eof);
        assert_eq!(mb.push(Source::Inbox, rec(6), inbox, t0()), Pushed::Queued);
        assert!(matches!(
            mb.take(Duration::ZERO, t0()),
            Some(Item::Inbox((6, _, _)))
        ));
        assert!(
            mb.take(Duration::ZERO, t0()).is_none(),
            "the say face left nothing"
        );
    }

    #[test]
    fn a_full_source_pauses_and_resumes_after_its_last_record() {
        let mb = Mailbox::default();
        let bound = QUEUE_ROWS_MAX as u64;
        for off in 0..bound - 1 {
            assert_eq!(mb.push(Source::Inbox, rec(off), 0, t0()), Pushed::Queued);
        }
        assert_eq!(mb.paused(Source::Inbox), None);
        assert_eq!(
            mb.push(Source::Inbox, rec(bound - 1), 0, t0()),
            Pushed::Paused,
            "the record that fills the queue pauses its source"
        );
        assert_eq!(mb.paused(Source::Inbox), Some(bound));
        let mut seen = 0u64;
        let mut resumed_at = None;
        while let Some(item) = mb.take(Duration::ZERO, t0()) {
            match item {
                Item::Inbox((off, _, _)) => {
                    assert_eq!(off, seen, "the queue is FIFO and sheds nothing");
                    seen += 1;
                }
                Item::Resume(Source::Inbox, from) => {
                    assert_eq!(from, bound, "it resumes after the last record it queued");
                    assert!(resumed_at.replace(seen).is_none(), "one resume per pause");
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(seen, bound);
        assert_eq!(
            resumed_at,
            Some(bound / 2),
            "the resume is owed at half the bound, ahead of the rest"
        );
        assert_eq!(mb.paused(Source::Inbox), None);
    }

    #[test]
    fn the_byte_bound_holds_and_one_oversized_record_still_passes() {
        let mb = Mailbox::default();
        let big = |off: u64| (off, "/s".to_string(), vec![0u8; QUEUE_BYTES_MAX / 4 - 2]);
        for off in 0..3 {
            assert_eq!(mb.push(Source::Say, big(off), 0, t0()), Pushed::Queued);
        }
        assert_eq!(mb.push(Source::Say, big(3), 0, t0()), Pushed::Paused);
        let mut order = Vec::new();
        while let Some(item) = mb.take(Duration::ZERO, t0()) {
            order.push(match item {
                Item::Say((off, _, _)) => format!("say {off}"),
                Item::Resume(s, from) => format!("resume {s:?} {from}"),
                other => panic!("{other:?}"),
            });
        }
        assert_eq!(order, ["say 0", "say 1", "resume Say 4", "say 2", "say 3"]);

        let mb2 = Mailbox::default();
        let huge = (9, "/s".to_string(), vec![0u8; QUEUE_BYTES_MAX * 2]);
        assert_eq!(mb2.push(Source::Inbox, huge, 0, t0()), Pushed::Paused);
        assert!(matches!(
            mb2.take(Duration::from_millis(0), t0()),
            Some(Item::Inbox((9, _, _)))
        ));
        assert!(matches!(
            mb2.take(Duration::from_millis(0), t0()),
            Some(Item::Resume(Source::Inbox, 10))
        ));
    }
}
