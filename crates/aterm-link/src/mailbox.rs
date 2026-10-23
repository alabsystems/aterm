// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Priority-ordered bounded input queues for the bridge loop.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

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
    /// A source ended at EOF or on a transport error.
    Closed(Source),
    /// The broker answered a subscription with an `Error` frame.
    Refused(Source, &'static str),
    /// A paused broker source drained to low water and can resubscribe from this offset.
    Resume(Source, u64),
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

const fn record_bytes(r: &Record) -> usize {
    r.1.len() + r.2.len()
}

/// One broker source's share of the mailbox.
#[derive(Default)]
struct Face {
    /// Each with its stamp while the wait tally is on ([`Queues::stamping`]).
    records: VecDeque<(Record, Option<Stamp>)>,
    /// Resident subject+body bytes, as the bound counts them.
    bytes: usize,
    /// The incarnation whose inputs are currently accepted.
    generation: u64,
    /// While the face is PAUSED: the offset its subscription resumes from.
    paused: Option<u64>,
}

impl Face {
    fn take(&mut self) -> Option<(Record, Option<Stamp>, Option<u64>)> {
        let (r, stamp) = self.records.pop_front()?;
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
    }
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
    /// Closure notices, in arrival order: `Item::Closed` and `Item::Refused` only.
    closed: VecDeque<Item>,
    /// Paused faces whose queue has drained: `Item::Resume`'s.
    resume: VecDeque<(Source, u64)>,
    /// The last `EVENT * bye <word>` the push reader read.
    bye: Option<aterm_types::fabric_grammar::Bye>,
    /// How many of aterm's events the loop has taken in a row while an inbox record waited.
    events_run: u32,
    /// Whether each push is stamped for the wait tally ([`Mailbox::stamp`]).
    stamping: bool,
    /// Whether the loop is parked on [`Mailbox::take`] now.
    parked: bool,
    /// The stamped items' waits, by kind ([`Mailbox::copy_wakes`]).
    wakes: Wakes,
    /// The bound's NEGATIVE CONTROL.
    #[cfg(test)]
    events_first: bool,
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
    }

    const fn face(&mut self, s: Source) -> Option<&mut Face> {
        match s {
            Source::Fleet => Some(&mut self.fleet),
            Source::Inbox => Some(&mut self.inbox),
            Source::Say => Some(&mut self.say),
            Source::Aterm => None,
        }
    }

    fn take(&mut self) -> Option<(Item, bool)> {
        if let Some(item) = self.closed.pop_front() {
            return Some((item, false));
        }
        if let Some(r) = self.take_record(Source::Fleet) {
            return Some((Item::Fleet(r), false));
        }
        if let Some((s, from)) = self.resume.pop_front() {
            return Some((Item::Resume(s, from), false));
        }
        let waiting = !self.inbox.records.is_empty();
        #[cfg(test)]
        let inbox_due = waiting && !self.events_first && self.events_run >= EVENT_RUN_MAX;
        #[cfg(not(test))]
        let inbox_due = waiting && self.events_run >= EVENT_RUN_MAX;
        if !inbox_due {
            if let Some((_, e, stamp)) = self.events.pop_front() {
                self.wakes.taken(stamp, false);
                if waiting {
                    self.events_run += 1;
                }
                return Some((Item::Event(e), over_bound(self.events.len() + 1, 0)));
            }
        }
        if let Some(r) = self.take_record(Source::Inbox) {
            self.events_run = 0;
            return Some((Item::Inbox(r), false));
        }
        self.take_record(Source::Say).map(|r| (Item::Say(r), false))
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
        }
        self.resume.retain(|(face, _)| *face != s);
        self.closed.retain(|item| match item {
            Item::Closed(face) | Item::Refused(face, _) => *face != s,
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
        } else if !q.events.is_empty() || !q.inbox.records.is_empty() {
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

    /// Push one record from broker source `s` under its reader `generation`.
    pub fn push(&self, s: Source, r: Record, generation: u64) -> Pushed {
        let mut q = self.lock();
        let stamp = q.stamp();
        let Some(face) = q.face(s).filter(|face| face.generation == generation) else {
            return Pushed::Stale;
        };
        let resume_from = r.0.saturating_add(1);
        face.bytes = face.bytes.saturating_add(record_bytes(&r));
        face.records.push_back((r, stamp));
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

    /// Report that a broker reader ended at EOF or on a transport error.
    pub fn push_closed(&self, s: Source, generation: u64) {
        self.push_end(s, generation, Item::Closed(s));
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

    /// THE URGENT ITEMS.
    pub(crate) fn take_urgent(&self) -> Option<Item> {
        self.lock().take_urgent()
    }

    /// Block until an item is ready to take or `timeout` elapses.
    pub fn take(&self, timeout: Duration) -> Option<Item> {
        let mut q = self.lock();
        if let Some((item, released)) = q.take() {
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
        let item = q.take();
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

    #[test]
    fn a_push_to_a_parked_taker_is_a_wake_and_one_to_a_working_loop_is_queued() {
        use std::sync::Arc;
        let counted = |stamping: bool| -> Tally {
            let mb = Arc::new(Mailbox::default());
            mb.stamp(stamping);
            mb.push_event("EVENT 1 post 1".into());
            assert_eq!(mb.push(Source::Inbox, rec(4), 0), Pushed::Queued);
            assert!(matches!(mb.take(Duration::ZERO), Some(Item::Event(_))));
            assert!(matches!(
                mb.take(Duration::ZERO),
                Some(Item::Inbox((4, _, _)))
            ));
            for record in [false, true] {
                let taker = {
                    let mb = Arc::clone(&mb);
                    std::thread::spawn(move || mb.take(aterm_spec::wait::MUST_HAPPEN))
                };
                aterm_spec::wait::until_within(
                    aterm_spec::wait::MUST_HAPPEN,
                    "the taker parked on the mailbox",
                    || mb.parked().then_some(()),
                );
                if record {
                    assert_eq!(mb.push(Source::Inbox, rec(5), 0), Pushed::Queued);
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
        assert_eq!(mb.push(Source::Fleet, big, 0), Pushed::Paused);
        assert!(matches!(mb.take(Duration::ZERO), Some(Item::Fleet(_))));
        mb.push_event("EVENT 1 post 1".into());
        assert_eq!(mb.push(Source::Say, rec(7), 0), Pushed::Queued);
        assert_eq!(mb.push(Source::Inbox, rec(4), 0), Pushed::Queued);
        assert_eq!(mb.push(Source::Fleet, rec(1), 0), Pushed::Queued);
        mb.push_closed(Source::Inbox, 0);
        let mut seen = Vec::new();
        while let Some(item) = mb.take(Duration::from_millis(0)) {
            seen.push(match item {
                Item::Closed(_) | Item::Refused(..) => "closed",
                Item::Fleet(_) => "fleet",
                Item::Resume(..) => "resume",
                Item::Event(_) => "event",
                Item::Inbox(_) => "inbox",
                Item::Say(_) => "say",
            });
        }
        assert_eq!(seen, ["closed", "fleet", "resume", "event", "inbox", "say"]);
    }

    #[test]
    fn an_inbox_record_waits_behind_at_most_a_bounded_run_of_events() {
        let k = usize::try_from(EVENT_RUN_MAX).expect("a small bound");
        let order = |events_first: bool| {
            let mb = Mailbox::default();
            mb.lock().events_first = events_first;
            for i in 0..3 {
                assert_eq!(mb.push(Source::Inbox, rec(10 + i), 0), Pushed::Queued);
            }
            for i in 0..2 * k + 5 {
                mb.push_event(format!("EVENT 1 post {i}"));
            }
            let mut seen = Vec::new();
            while let Some(item) = mb.take(Duration::ZERO) {
                seen.push(match item {
                    Item::Event(_) => "event".to_string(),
                    Item::Inbox((off, _, _)) => format!("inbox {off}"),
                    Item::Fleet((off, _, _)) => format!("fleet {off}"),
                    other => panic!("{other:?}"),
                });
                if seen.len() == 3 {
                    assert_eq!(mb.push(Source::Fleet, rec(1), 0), Pushed::Queued);
                }
            }
            seen
        };
        let mut want: Vec<String> = Vec::new();
        let events = |n: usize| std::iter::repeat_n("event".to_string(), n);
        want.extend(events(3));
        want.push("fleet 1".to_string());
        want.extend(events(k - 3));
        want.push("inbox 10".to_string());
        want.extend(events(k));
        want.push("inbox 11".to_string());
        want.extend(events(5));
        want.push("inbox 12".to_string());
        assert_eq!(order(false), want, "a run of {k}, a record, and so on");

        let mut control: Vec<String> = Vec::new();
        control.extend(events(3));
        control.push("fleet 1".to_string());
        control.extend(events(2 * k + 5 - 3));
        control.extend(["inbox 10", "inbox 11", "inbox 12"].map(String::from));
        assert_eq!(
            order(true),
            control,
            "NEGATIVE CONTROL: unbounded, every record waits behind every event"
        );

        let mb = Mailbox::default();
        for i in 0..k {
            mb.push_event(format!("EVENT 1 post {i}"));
        }
        for _ in 0..k {
            assert!(matches!(mb.take(Duration::ZERO), Some(Item::Event(_))));
        }
        assert_eq!(mb.push(Source::Inbox, rec(20), 0), Pushed::Queued);
        for i in 0..=k {
            mb.push_event(format!("EVENT 1 post {i}"));
        }
        for _ in 0..k {
            assert!(matches!(mb.take(Duration::ZERO), Some(Item::Event(_))));
        }
        assert!(
            matches!(mb.take(Duration::ZERO), Some(Item::Inbox((20, _, _)))),
            "the record is next once its own run is spent"
        );
        assert!(matches!(mb.take(Duration::ZERO), Some(Item::Event(_))));
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
            mb.take(Duration::ZERO),
            Some(Item::Event(line)) if line.starts_with("EVENT 1 post 1 ")
        ));
        let mark = mb.event_mark();
        mb.push_event("EVENT 1 post 3 to=@s-r kind=note".to_string());
        assert_eq!(mb.forget_queued_drains(mark), 2, "post 2 and the fetch");
        mb.push_event("EVENT 1 post 4 to=@s-r kind=note".to_string());
        let mut left = Vec::new();
        while let Some(Item::Event(line)) = mb.take(Duration::ZERO) {
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
            assert_eq!(mb.push(Source::Inbox, rec(10), 0), Pushed::Queued);
            for i in 0..k {
                mb.push_event(format!("EVENT 1 post {i}"));
            }
            for _ in 0..k {
                assert!(matches!(mb.take(Duration::ZERO), Some(Item::Event(_))));
            }
            reset(&mb);
            let g = mb.broker_generation(Source::Inbox);
            mb.push_event("EVENT 1 post fresh".to_string());
            assert_eq!(mb.push(Source::Inbox, rec(20), g), Pushed::Queued);
            match mb.take(Duration::ZERO) {
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
    }

    #[test]
    fn a_duty_takes_the_halts_then_a_broker_closure_and_nothing_else() {
        let mb = Mailbox::default();
        mb.push_aterm_closed();
        let _ = mb.push(Source::Say, rec(7), 0);
        let _ = mb.push(Source::Inbox, rec(4), 0);
        mb.push_event("EVENT 1 post 1".into());
        mb.push_closed(Source::Fleet, 0);
        mb.push_refused(Source::Say, 0, "subscribe");
        let _ = mb.push(Source::Fleet, rec(1), 0);
        let _ = mb.push(Source::Fleet, rec(2), 0);
        let mut urgent = Vec::new();
        while let Some(item) = mb.take_urgent() {
            urgent.push(match item {
                Item::Fleet((off, _, _)) => format!("fleet {off}"),
                Item::Closed(s) => format!("closed {s:?}"),
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
        while let Some(item) = mb.take(Duration::ZERO) {
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
        let _ = mb.push(Source::Say, rec(1), 0);
        assert_eq!(mb.replay_pressure(), ReplayPressure::Clear);

        let _ = mb.push(Source::Inbox, rec(2), 0);
        assert_eq!(mb.replay_pressure(), ReplayPressure::Pending);
        assert!(matches!(mb.take(Duration::ZERO), Some(Item::Inbox(_))));
        assert_eq!(mb.replay_pressure(), ReplayPressure::Clear);

        mb.push_event("EVENT 1 topic add probe".into());
        assert_eq!(mb.replay_pressure(), ReplayPressure::Pending);
        assert!(matches!(mb.take(Duration::ZERO), Some(Item::Event(_))));
        assert_eq!(mb.replay_pressure(), ReplayPressure::Clear);

        let _ = mb.push(Source::Fleet, rec(3), 0);
        assert_eq!(mb.replay_pressure(), ReplayPressure::Urgent);
        assert!(matches!(mb.take(Duration::ZERO), Some(Item::Fleet(_))));
        assert_eq!(mb.replay_pressure(), ReplayPressure::Clear);

        mb.push_aterm_closed();
        assert_eq!(mb.replay_pressure(), ReplayPressure::Urgent);
    }

    #[test]
    fn a_reconnect_forgets_the_old_connections_inputs_but_not_aterms() {
        let mb = Mailbox::default();
        let big = |off| (off, "/s".to_string(), vec![0u8; QUEUE_BYTES_MAX]);
        assert_eq!(mb.push(Source::Inbox, big(5), 0), Pushed::Paused);
        assert!(matches!(mb.take(Duration::ZERO), Some(Item::Inbox(_))));
        assert_eq!(mb.push(Source::Say, big(9), 0), Pushed::Paused);
        let _ = mb.push(Source::Fleet, rec(1), 0);
        let _ = mb.push(Source::Inbox, rec(3), 0);
        mb.push_event("EVENT 1 post 1".into());
        mb.push_closed(Source::Inbox, 0);
        mb.push_refused(Source::Fleet, 0, "subscribe");
        mb.push_aterm_closed();
        mb.reset_broker_sources();
        assert_eq!(
            mb.paused(Source::Say),
            None,
            "a reconnect lifts every pause"
        );
        let mut seen = Vec::new();
        while let Some(item) = mb.take(Duration::from_millis(0)) {
            seen.push(match item {
                Item::Closed(Source::Aterm) => "closed-aterm",
                Item::Closed(_) | Item::Refused(..) => "closed-broker",
                Item::Fleet(_) => "fleet",
                Item::Resume(..) => "resume",
                Item::Event(_) => "event",
                Item::Inbox(_) => "inbox",
                Item::Say(_) => "say",
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

        mb.push_closed(Source::Fleet, stale);
        mb.push_refused(Source::Inbox, stale, "subscribe");
        assert_eq!(mb.push(Source::Inbox, rec(2), stale), Pushed::Stale);
        assert_eq!(mb.push(Source::Fleet, rec(3), stale), Pushed::Stale);
        assert!(
            mb.take(Duration::from_millis(0)).is_none(),
            "nothing from the previous incarnation may reach the fresh loop"
        );

        mb.push_closed(Source::Fleet, fresh);
        assert!(matches!(
            mb.take(Duration::from_millis(0)),
            Some(Item::Closed(Source::Fleet))
        ));

        let say = mb.broker_generation(Source::Say);
        let inbox = mb.broker_generation(Source::Inbox);
        assert_eq!(mb.push(Source::Say, rec(4), say), Pushed::Queued);
        mb.push_closed(Source::Say, say);
        mb.reset_source(Source::Say);
        assert_ne!(mb.broker_generation(Source::Say), say);
        assert_eq!(mb.broker_generation(Source::Inbox), inbox);
        assert_eq!(mb.push(Source::Say, rec(5), say), Pushed::Stale);
        mb.push_closed(Source::Say, say);
        assert_eq!(mb.push(Source::Inbox, rec(6), inbox), Pushed::Queued);
        assert!(matches!(
            mb.take(Duration::ZERO),
            Some(Item::Inbox((6, _, _)))
        ));
        assert!(
            mb.take(Duration::ZERO).is_none(),
            "the say face left nothing"
        );
    }

    #[test]
    fn a_full_source_pauses_and_resumes_after_its_last_record() {
        let mb = Mailbox::default();
        let bound = QUEUE_ROWS_MAX as u64;
        for off in 0..bound - 1 {
            assert_eq!(mb.push(Source::Inbox, rec(off), 0), Pushed::Queued);
        }
        assert_eq!(mb.paused(Source::Inbox), None);
        assert_eq!(
            mb.push(Source::Inbox, rec(bound - 1), 0),
            Pushed::Paused,
            "the record that fills the queue pauses its source"
        );
        assert_eq!(mb.paused(Source::Inbox), Some(bound));
        let mut seen = 0u64;
        let mut resumed_at = None;
        while let Some(item) = mb.take(Duration::ZERO) {
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
            assert_eq!(mb.push(Source::Say, big(off), 0), Pushed::Queued);
        }
        assert_eq!(mb.push(Source::Say, big(3), 0), Pushed::Paused);
        let mut order = Vec::new();
        while let Some(item) = mb.take(Duration::ZERO) {
            order.push(match item {
                Item::Say((off, _, _)) => format!("say {off}"),
                Item::Resume(s, from) => format!("resume {s:?} {from}"),
                other => panic!("{other:?}"),
            });
        }
        assert_eq!(order, ["say 0", "say 1", "resume Say 4", "say 2", "say 3"]);

        let mb2 = Mailbox::default();
        let huge = (9, "/s".to_string(), vec![0u8; QUEUE_BYTES_MAX * 2]);
        assert_eq!(mb2.push(Source::Inbox, huge, 0), Pushed::Paused);
        assert!(matches!(
            mb2.take(Duration::from_millis(0)),
            Some(Item::Inbox((9, _, _)))
        ));
        assert!(matches!(
            mb2.take(Duration::from_millis(0)),
            Some(Item::Resume(Source::Inbox, 10))
        ));
    }
}
