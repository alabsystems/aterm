// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The scheduling round, as a data structure.
//!
//! §11.2's ordering rule is a safety rule, not a fairness one:
//!
//! > drains the `fleet/>` tail **ahead of** the inbox group on every scheduling
//! > round so a halt never queues behind a redelivery backlog
//!
//! One FIFO cannot express that, so each source has its own queue and one
//! condvar wakes the loop, which takes strictly by priority. Nothing polls.
//! And "every scheduling round" includes the rounds INSIDE one item: a duty
//! that walks the bus in many requests takes the urgent queues between them
//! ([`Mailbox::take_urgent`]), so a halt waits one request, not one walk.
//!
//! ## The queues are bounded, and nothing is dropped at the bound
//!
//! Readers push as fast as the broker delivers while one consumer pays a
//! synchronous aterm round trip per record, and a body may be 16 MiB. Unbounded,
//! a peer's burst or a broker replaying a backlog exhausts bridge memory, and
//! the bridge's death cuts every session it governs off the fleet (and, under
//! `[fabric] hold_on_bridge_lost = true`, holds them). So each queue has a bound
//! ([`QUEUE_ROWS_MAX`], [`QUEUE_BYTES_MAX`]), and what happens there depends on
//! who is on the far side of the reader:
//!
//! * A BROKER SOURCE (`Fleet`, `Inbox`, `Say`) PAUSES. The record that reaches
//!   the bound is queued and its reader closes its own subscription and ends
//!   ([`Pushed::Paused`]). Once the loop has taken that queue down to half its
//!   bounds it is handed [`Item::Resume`] with the offset after that record and
//!   subscribes that one face again from there. The broker delivers gaplessly
//!   from any offset, so the loop takes exactly the records, in exactly the
//!   order, one unbroken subscription would have handed it.
//!
//!   A reader may NOT wait at the bound with its socket unread instead. astream
//!   drops no record for a slow reader; it stops writing, and a subscription
//!   whose socket write makes no progress for its stream bound
//!   (`astream_broker::broker::STREAM_WRITE_TIMEOUT`, 30 s, a floor) is ended
//!   by the broker with no word said. The loop would read that end as the
//!   broker going away and reconnect every subscription, failing every parked
//!   `post --wait` on a broker that was healthy throughout.
//! * `Event` BLOCKS — aterm's push lane already coalesces and emits `GAP`
//!   (§4.2), so blocking hands the decision to the side that can report it.
//!
//! A closure notice is never bounded: a bound on it would bound the bridge's
//! liveness. A queue is at its bound only while it is over a cap AND non-empty,
//! so one record larger than [`QUEUE_BYTES_MAX`] still makes progress.
//!
//! ## Every broker input carries its incarnation
//!
//! A reader thread outlives its subscription: after `close_subscriptions` its
//! parked `recv` returns microseconds later, unordered against the reconnect.
//! Clearing queues on reconnect drops only what was already pushed; the
//! in-flight `Closed` would tear down the fresh connection (a reconnect loop)
//! and a stale record would be applied late. So each broker source has a
//! generation, [`Mailbox::reset_source`] bumps one (a face closed on its own,
//! the broadcast face when the last topic goes) and
//! [`Mailbox::reset_broker_sources`] all three, each reader captures its own at
//! spawn, and a push from any other generation is dropped whenever it arrives —
//! `fabric::BridgeGeneration`'s rule on the aterm side. A resumed face keeps its
//! generation: its old reader pushed nothing after the record that paused it.
//! `Source::Aterm` is the inherited push lane, which no reconnect touches, so
//! its closure is never generation-scoped.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// One record off the bus: `(offset, subject, body)`, the shape
/// `astream_broker::Record` already is.
pub type Record = (u64, String, Vec<u8>);

/// What the loop took, in the order the loop must take them.
#[derive(Debug)]
pub enum Item {
    /// A `/f/<F>/fleet/>` record — a halt or a barrier. FIRST, always.
    Fleet(Record),
    /// One `EVENT …` line off aterm's push lane.
    Event(String),
    /// A `/f/<F>/in/<node>/>` record from the durable group.
    Inbox(Record),
    /// A `/f/<F>/pub/*/*/say/>` BROADCAST record, fanned in to whichever local
    /// sessions opted into its topic. LAST — see [`Queues::take`].
    Say(Record),
    /// A source ended: its reader thread saw EOF or a transport error. Named so
    /// the loop can tell a broker subscription ending (ask the broker, then
    /// reconnect) from "aterm went away" (exit).
    Closed(Source),
    /// The broker answered a subscription with an `Error` frame, under the
    /// `reason=` token the bridge reports that refusal by. Not a loss: the
    /// broker is there, and refused this subscription.
    Refused(Source, &'static str),
    /// A PAUSED broker source has drained to half its bounds: subscribe it
    /// again from this offset, the one after the last record it queued. See
    /// the module header.
    Resume(Source, u64),
}

/// What became of a broker record a reader pushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Pushed {
    /// Queued, with room behind it: read on.
    Queued,
    /// Queued, and its queue is now at its bound: the source is PAUSED. The
    /// reader closes its subscription and ends; the loop is handed
    /// [`Item::Resume`] once the queue has drained. See the module header.
    Paused,
    /// Not queued: the reader's subscription belongs to a generation that was
    /// torn down. The reader ends.
    Stale,
}

/// Which reader ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Fleet,
    Inbox,
    /// The fleet-wide broadcast face. A separate reader from `Inbox` because it
    /// is a separate subscription with a separate priority, and a separate
    /// bound: a shout is not allowed to starve addressed mail.
    Say,
    Aterm,
}

/// Queued inputs that replay must yield to. A fleet halt or lane closure is
/// never delayed for catch-up; local events and addressed mail allow the
/// roster's one-page progress exception after its control work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplayPressure {
    Clear,
    Pending,
    Urgent,
}

/// The most records one source may hold before it pauses (a broker source) or
/// its reader waits (aterm's events): deep enough for an ordinary burst,
/// shallow enough to stay a queue rather than a heap.
pub const QUEUE_ROWS_MAX: usize = 1024;

/// The most subject+body bytes one source may hold before it pauses. The row
/// cap alone is no memory bound: a thousand 16 MiB bodies is 16 GiB.
pub const QUEUE_BYTES_MAX: usize = 8 * 1024 * 1024;

/// The bytes one queued record occupies, as the bound counts them.
fn record_bytes(r: &Record) -> usize {
    r.1.len() + r.2.len()
}

/// One broker source's share of the mailbox.
#[derive(Default)]
struct Face {
    records: VecDeque<Record>,
    /// Resident subject+body bytes, as the bound counts them.
    bytes: usize,
    /// The incarnation whose inputs are currently accepted. See the module
    /// header.
    generation: u64,
    /// While the face is PAUSED: the offset its subscription resumes from.
    paused: Option<u64>,
}

impl Face {
    /// Its head; a paused face this take brings down to its low water answers
    /// where it resumes, once.
    fn take(&mut self) -> Option<(Record, Option<u64>)> {
        let r = self.records.pop_front()?;
        self.bytes = self.bytes.saturating_sub(record_bytes(&r));
        let resume = if at_low_water(self.records.len(), self.bytes) {
            self.paused.take()
        } else {
            None
        };
        Some((r, resume))
    }

    /// Forget everything queued, and move the generation.
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
    events: VecDeque<String>,
    inbox: Face,
    say: Face,
    /// Closure notices, in arrival order: `Item::Closed` and `Item::Refused`
    /// only.
    closed: VecDeque<Item>,
    /// Paused faces whose queue has drained: `Item::Resume`'s.
    resume: VecDeque<(Source, u64)>,
}

/// Whether one source is over a bound AND non-empty: an empty queue must accept
/// one record however large, or the lane wedges forever.
fn over_bound(rows: usize, bytes: usize) -> bool {
    rows > 0 && (rows >= QUEUE_ROWS_MAX || bytes >= QUEUE_BYTES_MAX)
}

/// Whether a paused source has drained far enough to subscribe again: half of
/// each bound, so one pause moves at least half a queue and the new
/// subscription is open while the loop still has the other half to take.
fn at_low_water(rows: usize, bytes: usize) -> bool {
    rows <= QUEUE_ROWS_MAX / 2 && bytes <= QUEUE_BYTES_MAX / 2
}

impl Queues {
    fn is_empty(&self) -> bool {
        self.fleet.records.is_empty()
            && self.events.is_empty()
            && self.inbox.records.is_empty()
            && self.say.records.is_empty()
            && self.closed.is_empty()
            && self.resume.is_empty()
    }

    /// A broker source's face; `None` for aterm's own lane, which carries no
    /// records and no generation.
    fn face(&mut self, s: Source) -> Option<&mut Face> {
        match s {
            Source::Fleet => Some(&mut self.fleet),
            Source::Inbox => Some(&mut self.inbox),
            Source::Say => Some(&mut self.say),
            Source::Aterm => None,
        }
    }

    /// The priority, in one place: a closure notice (the reconnect it starts
    /// clears and re-reads everything queued behind it), the fleet halt, a
    /// paused face's resume, aterm's events, the inbox group, then broadcast.
    ///
    /// A resume is taken ahead of the records because it is half a queue
    /// early: the subscription reopens while the loop works the other half.
    ///
    /// Say is last because nobody on this node chose its volume: any
    /// co-permitted peer may shout as often as it likes, and each record costs
    /// one synchronous `deliver` per subscribed session.
    ///
    /// The `bool` is whether the take released room for aterm's event reader,
    /// the one producer that waits at its bound.
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
        if let Some(e) = self.events.pop_front() {
            return Some((Item::Event(e), over_bound(self.events.len() + 1, 0)));
        }
        if let Some(r) = self.take_record(Source::Inbox) {
            return Some((Item::Inbox(r), false));
        }
        self.take_record(Source::Say).map(|r| (Item::Say(r), false))
    }

    /// One broker source's head; a paused source this take brings down to its
    /// low water is owed its [`Item::Resume`].
    fn take_record(&mut self, s: Source) -> Option<Record> {
        let (r, resume) = self.face(s)?.take()?;
        if let Some(from) = resume {
            self.resume.push_back((s, from));
        }
        Some(r)
    }

    /// [`Mailbox::take_urgent`]'s order: every fleet record, the fleet face's
    /// resume, then a BROKER reader's closure. `Closed(Aterm)` is the loop's
    /// alone.
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

    /// Forget everything one broker source has queued — its records, its
    /// pause, its resume, its closure notices — and move its generation:
    /// [`Mailbox::reset_source`]. A no-op for aterm's lane.
    fn reset(&mut self, s: Source) {
        let Some(face) = self.face(s) else {
            return;
        };
        face.reset();
        self.resume.retain(|(face, _)| *face != s);
        self.closed.retain(|item| match item {
            Item::Closed(face) | Item::Refused(face, _) => *face != s,
            _ => true,
        });
    }
}

/// The bridge's one input queue set.
#[cfg_attr(trust_verify, trust::paired)]
#[derive(Default)]
pub struct Mailbox {
    queues: Mutex<Queues>,
    ready: Condvar,
    /// Woken when the loop takes an event: the back-pressure half of the pair,
    /// for the one reader that waits at its bound.
    drained: Condvar,
}

impl Mailbox {
    fn lock(&self) -> std::sync::MutexGuard<'_, Queues> {
        self.queues.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Whether the loop has an input that outranks broadcast replay waiting.
    /// A page already in flight cannot be preempted, but the bridge checks this
    /// before each next page so an arriving halt or addressed message never
    /// waits behind an entire replay slice.
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

    /// The incarnation of broker source `s` a reader spawned NOW must stamp its
    /// pushes with.
    #[must_use]
    pub fn broker_generation(&self, s: Source) -> u64 {
        self.lock().face(s).map_or(0, |face| face.generation)
    }

    /// Push one record off broker source `s`, under the generation its reader
    /// was spawned in. NEVER WAITS: a record of the live generation is always
    /// queued, and the answer says whether the reader reads on
    /// ([`Pushed::Queued`]) or ends because its queue is at its bound
    /// ([`Pushed::Paused`]: the source resumes from the offset after this
    /// record) or because its subscription was torn down ([`Pushed::Stale`],
    /// nothing queued). See the module header.
    pub fn push(&self, s: Source, r: Record, generation: u64) -> Pushed {
        let mut q = self.lock();
        let Some(face) = q.face(s).filter(|face| face.generation == generation) else {
            return Pushed::Stale;
        };
        let resume_from = r.0.saturating_add(1);
        face.bytes = face.bytes.saturating_add(record_bytes(&r));
        face.records.push_back(r);
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

    /// Push one aterm `EVENT` line, WAITING while the event queue is over its
    /// row bound (lines are small and coalesced, so only rows count).
    pub fn push_event(&self, line: String) {
        let mut q = self.lock();
        while over_bound(q.events.len(), 0) {
            q = self.wait_for_room(q);
        }
        q.events.push_back(line);
        drop(q);
        self.ready.notify_all();
    }

    /// Park until the loop takes an event.
    ///
    /// The timeout is a hang detector, not a poll: [`Mailbox::take`] wakes it.
    fn wait_for_room<'a>(
        &'a self,
        q: std::sync::MutexGuard<'a, Queues>,
    ) -> std::sync::MutexGuard<'a, Queues> {
        self.drained
            .wait_timeout(q, Duration::from_millis(250))
            .unwrap_or_else(|p| p.into_inner())
            .0
    }

    /// Report that a BROKER reader ended at EOF or on a transport error, under
    /// the generation it was reading in.
    ///
    /// A notice from a torn-down subscription is dropped: it names a connection
    /// the loop already replaced, and acting on it would drop a healthy one.
    pub fn push_closed(&self, s: Source, generation: u64) {
        self.push_end(s, generation, Item::Closed(s));
    }

    /// Report that the broker REFUSED a reader's subscription, under the
    /// `reason=` token the bridge reports that refusal by: [`Item::Refused`].
    /// Generation-scoped like [`Mailbox::push_closed`].
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

    /// Report that ATERM went away — never generation-scoped.
    ///
    /// The verb and push descriptors are inherited once at exec and no reconnect
    /// touches them; this closure is the only thing that makes the run loop
    /// return.
    pub fn push_aterm_closed(&self) {
        self.lock().closed.push_back(Item::Closed(Source::Aterm));
        self.ready.notify_all();
    }

    /// Whether aterm's closure ([`Mailbox::push_aterm_closed`]) is queued — a
    /// PEEK: the notice stays for the `take` that ends the loop. It is asked
    /// between the steps of one round, which the loop's `take` cannot see into.
    pub(crate) fn aterm_closed(&self) -> bool {
        self.lock()
            .closed
            .iter()
            .any(|item| matches!(item, Item::Closed(Source::Aterm)))
    }

    /// FORGET ONE BROKER SOURCE'S INPUTS, for a face the loop closed on its own
    /// (the broadcast face, once no local session has a topic): its queued
    /// records, its pause and resume, its closure notices, and its generation,
    /// so the reader the close ends pushes nothing the loop will act on. A
    /// no-op for aterm's lane.
    pub fn reset_source(&self, s: Source) {
        self.lock().reset(s);
    }

    /// FORGET EVERY BROKER INPUT, on a reconnect.
    ///
    /// A queued closure notice from a dead subscription would tear the fresh
    /// connection down again (a loop, not a retry). Losing the records costs
    /// nothing, because every face resumes from its own durable point: the
    /// reattach's `Last` read re-reads every halt and the fleet face resumes
    /// at its mark, the inbox group redelivers everything uncommitted, and the
    /// broadcast face resubscribes from the lowest topic cursor still owed.
    /// `Closed(Aterm)` is kept — it is not a broker event.
    pub fn reset_broker_sources(&self) {
        let mut q = self.lock();
        // The generation is what makes this total: clearing reaches only what
        // was already pushed, not a killed reader's late push.
        for s in [Source::Fleet, Source::Inbox, Source::Say] {
            q.reset(s);
        }
    }

    /// THE URGENT ITEMS, WITHOUT WAITING, for the loop to take BETWEEN the
    /// broker requests of one long duty rather than only between items: a
    /// fleet record — a halt — its paused reader's resume, and a broker
    /// reader's closure. The priority above holds inside an item too, which
    /// is what §11.2's "a halt never
    /// queues behind a redelivery backlog" needs of a duty that walks a
    /// backlog in many requests. A fleet resume must reopen its subscription
    /// during the duty, or later halts cannot arrive until the duty ends.
    /// Inbox and say resumes stay with the outer loop.
    ///
    /// THE FLEET RECORDS COME FIRST HERE, unlike [`Mailbox::take`]'s closure
    /// first: between two items a closure means a reconnect whose `Last` read
    /// re-reads every halt, so the records behind it are redundant; inside a
    /// duty a record already delivered is applied at once, and the closure
    /// then ends the duty. `Closed(Aterm)` is never taken here: exiting is the
    /// loop's.
    pub(crate) fn take_urgent(&self) -> Option<Item> {
        self.lock().take_urgent()
    }

    /// Block until there is something to take, or `timeout` elapses.
    ///
    /// The timeout is not a poll: it lets the loop run periodic duties (presence
    /// refresh, reconnect) without a second timer.
    pub fn take(&self, timeout: Duration) -> Option<Item> {
        let mut q = self.lock();
        if let Some((item, released)) = q.take() {
            drop(q);
            if released {
                self.drained.notify_all();
            }
            return Some(item);
        }
        let (mut q, _) = self
            .ready
            .wait_timeout_while(q, timeout, |q: &mut Queues| q.is_empty())
            .unwrap_or_else(|p| p.into_inner());
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
    use std::sync::Arc;

    /// How long a wait for something that MUST happen — an item, a park — may
    /// take before it is read as a hang (AGENTS.md: a hang detector is a
    /// minute, never a latency budget).
    const HANG_DETECTOR: Duration = Duration::from_secs(60);

    fn rec(off: u64) -> Record {
        (off, format!("/s/{off}"), Vec::new())
    }

    /// A halt behind a full inbox backlog still arrives first.
    #[test]
    fn a_halt_outranks_an_inbox_backlog_however_deep_it_is() {
        let mb = Mailbox::default();
        for off in 0..(QUEUE_ROWS_MAX as u64 - 1) {
            assert_eq!(mb.push(Source::Inbox, rec(off), 0), Pushed::Queued);
        }
        assert_eq!(mb.push(Source::Fleet, rec(999_999), 0), Pushed::Queued);
        let first = mb.take(HANG_DETECTOR).expect("an item");
        match first {
            Item::Fleet((off, _, _)) => assert_eq!(off, 999_999),
            other => panic!("the halt did not come first: {other:?}"),
        }
        // And the backlog is still there, in order.
        match mb.take(HANG_DETECTOR).expect("an item") {
            Item::Inbox((off, _, _)) => assert_eq!(off, 0),
            other => panic!("{other:?}"),
        }
    }

    /// The full order, once: closed, fleet, resume, event, inbox, say.
    #[test]
    fn the_priority_is_the_declared_one() {
        let mb = Mailbox::default();
        // A one-record face paused by its byte cap, and drained: its resume is
        // owed (see `a_full_source_pauses_and_resumes_after_its_last_record`).
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
        // Pushed out of order so the order below is priority, not arrival.
        assert_eq!(seen, ["closed", "fleet", "resume", "event", "inbox", "say"]);
    }

    /// BETWEEN THE REQUESTS OF A DUTY, only the urgent queues are taken: every
    /// fleet record, then a broker reader's closure — a refusal is one. aterm's
    /// closure, the events, the inbox and the broadcasts stay for the loop, in
    /// its order.
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

    /// A push from another thread wakes the wait; the timeout is a hang detector.
    #[test]
    fn a_push_from_another_thread_wakes_the_wait() {
        let mb = Arc::new(Mailbox::default());
        let writer = {
            let mb = mb.clone();
            std::thread::spawn(move || mb.push(Source::Fleet, rec(7), 0))
        };
        let got = mb.take(HANG_DETECTOR).expect("woken by the push");
        assert!(matches!(got, Item::Fleet((7, _, _))));
        assert_eq!(writer.join().expect("writer thread"), Pushed::Queued);
    }

    /// A reconnect forgets the old connection's inputs — records, closures, a
    /// pause and a resume — and nothing else.
    #[test]
    fn a_reconnect_forgets_the_old_connections_inputs_but_not_aterms() {
        let mb = Mailbox::default();
        let big = |off| (off, "/s".to_string(), vec![0u8; QUEUE_BYTES_MAX]);
        // The inbox paused and drained (a resume owed), the say face paused.
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

    /// A notice or record from the subscription a reconnect killed is discarded
    /// whenever it arrives, not only if it arrives before the queue is cleared.
    /// A late `Closed` would drop a connection that just came up (reachable on a
    /// partial `attach_broker` failure, e.g. the broker's `MAX_CONNS` under a
    /// post-restart herd); a late record would be applied late. And the
    /// generation is PER SOURCE: closing the broadcast face alone leaves the
    /// other faces' readers live.
    #[test]
    fn a_late_push_from_the_previous_incarnation_is_discarded() {
        let mb = Mailbox::default();
        let stale = mb.broker_generation(Source::Fleet);
        mb.reset_broker_sources();
        let fresh = mb.broker_generation(Source::Fleet);
        assert_ne!(stale, fresh, "a reconnect moves the generation");

        // The reader the reconnect killed, waking up after it: a closure notice
        // and a record, both from the subscription that no longer exists.
        mb.push_closed(Source::Fleet, stale);
        mb.push_refused(Source::Inbox, stale, "subscribe");
        assert_eq!(mb.push(Source::Inbox, rec(2), stale), Pushed::Stale);
        assert_eq!(mb.push(Source::Fleet, rec(3), stale), Pushed::Stale);
        assert!(
            mb.take(Duration::from_millis(0)).is_none(),
            "nothing from the previous incarnation may reach the fresh loop"
        );

        // And the fresh readers are unaffected.
        mb.push_closed(Source::Fleet, fresh);
        assert!(matches!(
            mb.take(Duration::from_millis(0)),
            Some(Item::Closed(Source::Fleet))
        ));

        // ONE FACE CLOSED ALONE: its queue, its notices and its generation go,
        // and every other face's reader pushes on as before.
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

    /// Aterm's own closure is never generation-scoped.
    #[test]
    fn aterms_closure_survives_every_broker_generation() {
        let mb = Mailbox::default();
        mb.push_aterm_closed();
        mb.reset_broker_sources();
        assert!(matches!(
            mb.take(Duration::from_millis(0)),
            Some(Item::Closed(Source::Aterm))
        ));
        mb.reset_broker_sources();
        mb.reset_source(Source::Aterm);
        mb.push_aterm_closed();
        assert!(matches!(
            mb.take(Duration::from_millis(0)),
            Some(Item::Closed(Source::Aterm))
        ));
    }

    /// A BROKER SOURCE AT ITS ROW BOUND PAUSES, and never waits: the record that
    /// reaches the bound is queued and answered `Paused`, the queue holds no more
    /// than the bound, and once the loop has taken it down to half the bound the
    /// resume comes first, from the offset after that record. Nothing is dropped
    /// or reordered. Single-threaded, so a push that waited would hang the test.
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

    /// The byte bound bites before the row bound on big records, and one record
    /// over the whole byte cap still passes an empty queue.
    #[test]
    fn the_byte_bound_holds_and_one_oversized_record_still_passes() {
        let mb = Mailbox::default();
        // Exactly a quarter of the byte cap each, subject included.
        let big = |off: u64| (off, "/s".to_string(), vec![0u8; QUEUE_BYTES_MAX / 4 - 2]);
        for off in 0..3 {
            assert_eq!(mb.push(Source::Say, big(off), 0), Pushed::Queued);
        }
        // The fourth reaches the byte cap: queued, and the source pauses.
        assert_eq!(mb.push(Source::Say, big(3), 0), Pushed::Paused);
        let mut order = Vec::new();
        while let Some(item) = mb.take(Duration::ZERO) {
            order.push(match item {
                Item::Say((off, _, _)) => format!("say {off}"),
                Item::Resume(s, from) => format!("resume {s:?} {from}"),
                other => panic!("{other:?}"),
            });
        }
        // Half the byte cap is left after the second take: the resume is next.
        assert_eq!(order, ["say 0", "say 1", "resume Say 4", "say 2", "say 3"]);

        // An empty queue takes a record however large.
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
