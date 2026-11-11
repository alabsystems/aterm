// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Priority-ordered bounded input queues for the bridge loop.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::wait_tally::{Hist, Row, Stamp, Tally, Unit};

/// One record off the bus: `(offset, subject, body)`.
pub type Record = (u64, String, Vec<u8>);

/// The `(producer_id, producer_seq)` a record's tail stamped on it, where its connection asked
/// (astream's `Subscription::last_origin`): the bridge's with the mesh on, so it knows a record
/// the node service moved across this Mac's own key rotation from the read that brought it
/// (`Bridge::moved_from`, §11 C3b). `None` where the connection did not ask or the broker
/// stamps none.
pub type Origin = Option<(u64, u64)>;

/// What the loop took, in priority order.
#[derive(Debug)]
pub enum Item {
    /// A `/f/<F>/fleet/>` record: a halt or a barrier, ahead of every other record.
    Fleet(Record),
    /// A `/f/<F>/halt/>` record (§11 C4): a Mac's halt or halt-off, relayed to every Mac, as
    /// urgent as the fleet face and taken right after it.
    Halt(Record),
    /// One `EVENT …` line off aterm's push lane.
    Event(String),
    /// A `/f/<F>/in/<node>/>` record from the durable group, with the origin its tail stamped.
    Inbox(Record, Origin),
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
    /// Every Mac's halt subject, with the mesh on.
    Halt,
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
    records: VecDeque<(Record, Origin, Instant, Option<Stamp>)>,
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
    fn take(&mut self) -> Option<(Record, Origin, Option<Stamp>, Option<u64>)> {
        let (r, origin, _, stamp) = self.records.pop_front()?;
        self.taken = self.taken.saturating_add(1);
        self.bytes = self.bytes.saturating_sub(record_bytes(&r));
        let resume = if at_low_water(self.records.len(), self.bytes) {
            self.paused.take()
        } else {
            None
        };
        Some((r, origin, stamp, resume))
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

    /// Whether a gap notice is due now: every record pushed before it was taken.
    fn gap_due(&self) -> bool {
        self.gaps.front().is_some_and(|(at, ..)| *at <= self.taken)
    }

    /// The gap notice due now: every record pushed before it was taken.
    fn take_gap(&mut self) -> Option<astream_broker::Gap> {
        if self.gap_due() {
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
            _ => self.records.front().map(|(_, _, when, _)| *when),
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
    halt: Face,
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
            && self.halt.records.is_empty()
            && self.events.is_empty()
            && self.inbox.records.is_empty()
            && self.say.records.is_empty()
            && self.closed.is_empty()
            && self.resume.is_empty()
            && self.fleet.gaps.is_empty()
            && self.halt.gaps.is_empty()
            && self.inbox.gaps.is_empty()
            && self.say.gaps.is_empty()
    }

    const fn face(&mut self, s: Source) -> Option<&mut Face> {
        match s {
            Source::Fleet => Some(&mut self.fleet),
            Source::Halt => Some(&mut self.halt),
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
        if let Some((r, _)) = self.take_record(Source::Fleet) {
            return Some((Item::Fleet(r), false));
        }
        if let Some(g) = self.halt.take_gap() {
            return Some((Item::Gap(Source::Halt, g), false));
        }
        if let Some((r, _)) = self.take_record(Source::Halt) {
            return Some((Item::Halt(r), false));
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
        self.take_record(Source::Say)
            .map(|(r, _)| (Item::Say(r), false))
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
            None => {
                let (r, origin) = self.take_record(Source::Inbox)?;
                Item::Inbox(r, origin)
            }
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

    /// Onto `run`, the inbox records [`Queues::take`] would hand over next, one after the other,
    /// while nothing it puts ahead of them has come: at most `max` in all, each counted against
    /// the turn as its own take is. It ends where `take` would hand over anything else: a
    /// closure, the fleet face's record or due gap notice, a resume, an event while the inbox
    /// is not due, or the inbox's own due gap notice.
    fn take_inbox_run(&mut self, now: Instant, max: usize, run: &mut Vec<(Record, Origin)>) {
        while run.len() < max
            && self.closed.is_empty()
            && self.fleet.records.is_empty()
            && !self.fleet.gap_due()
            && self.halt.records.is_empty()
            && !self.halt.gap_due()
            && self.resume.is_empty()
            && !self.inbox.gap_due()
            && (self.events.is_empty() || self.inbox_due(now))
        {
            match self.take_inbox(now) {
                Some(Item::Inbox(r, origin)) => run.push((r, origin)),
                _ => return,
            }
        }
    }

    fn take_record(&mut self, s: Source) -> Option<(Record, Origin)> {
        let (r, origin, stamp, resume) = self.face(s)?.take()?;
        self.wakes.taken(stamp, true);
        if let Some(from) = resume {
            self.resume.push_back((s, from));
        }
        Some((r, origin))
    }

    fn take_urgent(&mut self) -> Option<Item> {
        if let Some(g) = self.fleet.take_gap() {
            return Some(Item::Gap(Source::Fleet, g));
        }
        if let Some((r, _)) = self.take_record(Source::Fleet) {
            return Some(Item::Fleet(r));
        }
        if let Some(g) = self.halt.take_gap() {
            return Some(Item::Gap(Source::Halt, g));
        }
        if let Some((r, _)) = self.take_record(Source::Halt) {
            return Some(Item::Halt(r));
        }
        if let Some((source, from)) = self
            .resume
            .iter()
            .position(|(source, _)| matches!(source, Source::Fleet | Source::Halt))
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
            || !q.halt.records.is_empty()
            || q.resume
                .iter()
                .any(|(source, _)| matches!(source, Source::Fleet | Source::Halt))
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

    /// Push one record from broker source `s`, with the `origin` its tail stamped, under its
    /// reader `generation`, read at `at` (the reader's clock: `Fx::now`'s world), which its wait
    /// is judged from ([`INBOX_WAIT_MAX`]).
    pub fn push(
        &self,
        s: Source,
        r: Record,
        origin: Origin,
        generation: u64,
        at: Instant,
    ) -> Pushed {
        let mut q = self.lock();
        let stamp = q.stamp();
        let Some(face) = q.face(s).filter(|face| face.generation == generation) else {
            return Pushed::Stale;
        };
        let resume_from = r.0.saturating_add(1);
        face.bytes = face.bytes.saturating_add(record_bytes(&r));
        face.pushed = face.pushed.saturating_add(1);
        face.records.push_back((r, origin, at, stamp));
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
        for s in [Source::Fleet, Source::Halt, Source::Inbox, Source::Say] {
            q.reset(s);
        }
    }

    /// THE URGENT ITEMS: the fleet face's records, each of its gap notices in its place among
    /// them ([`Item::Gap`]), then the halt face's likewise, either one's resume, then a broker
    /// closure.
    pub(crate) fn take_urgent(&self) -> Option<Item> {
        self.lock().take_urgent()
    }

    /// Up to `max` inbox records more for the run in hand ([`Queues::take_inbox_run`]), never
    /// waiting: nothing queued is an empty run.
    pub(crate) fn take_inbox_run(&self, now: Instant, max: usize) -> Vec<(Record, Origin)> {
        let mut run = Vec::new();
        self.lock().take_inbox_run(now, max, &mut run);
        run
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
#[path = "mailbox_tests.rs"]
mod tests;
