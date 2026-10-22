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
//! condvar wakes the loop, which takes by priority — strictly, except that
//! an inbox record waits behind at most [`EVENT_RUN_MAX`] of aterm's events in
//! a row ([`Queues::take`]). Nothing polls.
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
//! `Closed` is never bounded: a bound on it would bound the bridge's liveness. A push blocks only
//! while its queue is over a cap and non-empty, so one record larger than [`QUEUE_BYTES_MAX`]
//! still makes progress.
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

use crate::wait_tally::{Hist, Row, Stamp, Tally, Unit};

/// One record off the bus: `(offset, subject, body)`, the shape
/// `astream_broker::Record` already is.
pub type Record = (u64, String, Vec<u8>);

/// What the loop took, in the order the loop must take them.
#[derive(Debug)]
pub enum Item {
    /// A `/f/<F>/fleet/>` record: a halt or a barrier, ahead of every other record.
    Fleet(Record),
    /// One `EVENT …` line off aterm's push lane.
    Event(String),
    /// A `/f/<F>/in/<node>/>` record from the durable group.
    Inbox(Record),
    /// A `/f/<F>/pub/*/*/say/>` broadcast record, fanned in to the local sessions that opted
    /// into its topic. Last ([`Queues::take`]).
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

/// The most of aterm's events the loop takes IN A ROW while an inbox record
/// waits ([`Queues::take`], aterm-0): at this many, the inbox record is next.
///
/// EVENTS FIRST, BECAUSE A RUN OF THEM IS MOSTLY ONE DRAIN'S WORK. A `post`
/// event is a drain of the outbox, and the first drain of a run lists and
/// publishes every post queued by then, so the events behind it find little
/// left: a run of events costs about one drain, and a delivery is one
/// `deliver` round trip each, whenever it is taken. (Since aterm reads a
/// record at once, a run's `post` events no longer arrive together, and a
/// drain that answered all its peek saw forgets the ones queued before that
/// peek: [`Mailbox::forget_queued_drains`]. The measurements below were taken
/// while aterm still batched them.) Strict turns — an event, a record, an
/// event — spread the drains between the deliveries, so each
/// drain found a post or two and did its whole work again, and the broker
/// saw the posts in smaller batches. Measured on the fabric bench's burst
/// (500 posts from 5 senders; docs/measured/fabric, 2026-10-21, dev build,
/// one quiet M5 Max): turns ran the strict arm at 272.6 and 272.9 messages a
/// second (`-4`, `-6`), below all four of the baseline's runs beside them
/// (286.4-311.2: `-2`, `-8`, `-10`, `-16`), in 413-414 broker batches
/// against 364-394. With this bound, 278.9 and 290.6 (`-12`, `-14`; 400 and
/// 393 batches), and on the reviewed loop 287.8 and 309.5 (`-21`, `-24`; 399
/// and 368) against 295.8 and 297.4 (`-18`, `-27`; 383 and 382): two runs a
/// build, not separable from the baseline — and not shown equal to it.
///
/// THE NUMBER IS NO MEASURED OPTIMUM. A bound of 16, tried the same morning
/// in runs that are not in the ledger (284 a second, 397-401 batches), sits
/// between the bound's own two runs on both measures, so nothing measured
/// tells 16 from 64. It is 64 by the argument above: a run's events are
/// mostly one drain's work, so a bound well past the few events a drain
/// leaves behind no longer splits a drain from its batch, and a record still
/// waits behind no more than that many events' local work.
///
/// AND A BOUND, BECAUSE NOTHING ELSE LIMITS A RUN: events come from every
/// session aterm hosts, so a node of many busy sessions could otherwise keep
/// a delivery behind its own outbound work for as long as they stay busy.
/// Before aterm-0 that wait was also a broker flush per drain — the loop
/// parked on every owed ack and commit (remaining-power's item 5); now it is
/// the loop's own work, and this bounds how much of it one record waits out.
pub const EVENT_RUN_MAX: u32 = 64;

/// The bytes one queued record occupies, as the bound counts them.
fn record_bytes(r: &Record) -> usize {
    r.1.len() + r.2.len()
}

/// One broker source's share of the mailbox.
#[derive(Default)]
struct Face {
    /// Each with its stamp while the wait tally is on ([`Queues::stamping`]).
    records: VecDeque<(Record, Option<Stamp>)>,
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
    /// aterm's events, each with the number of its push
    /// ([`Queues::events_pushed`]) and its stamp while the wait tally is on.
    events: VecDeque<(u64, String, Option<Stamp>)>,
    /// How many events have ever been pushed: the number the next one takes,
    /// and so the [`EventMark`] of this moment.
    events_pushed: u64,
    inbox: Face,
    say: Face,
    /// Closure notices, in arrival order: `Item::Closed` and `Item::Refused`
    /// only.
    closed: VecDeque<Item>,
    /// Paused faces whose queue has drained: `Item::Resume`'s.
    resume: VecDeque<(Source, u64)>,
    /// The last `EVENT * bye <word>` the push reader read, LATCHED as it is
    /// read ([`Mailbox::note_bye`]). Not a queue entry: the loop takes a
    /// closure notice ahead of every event, so a bye that arrived just before
    /// aterm closed the lanes would sit behind `Closed(Aterm)` and be lost
    /// with the loop. The latch is what the closure arm reads instead.
    bye: Option<aterm_types::fabric_grammar::Bye>,
    /// How many of aterm's events the loop has taken in a row while an inbox
    /// record waited: at [`EVENT_RUN_MAX`] the inbox record is taken next, and
    /// its take starts the count again ([`Queues::take`]).
    events_run: u32,
    /// Whether each push is stamped for the wait tally ([`Mailbox::stamp`]).
    stamping: bool,
    /// Whether the loop is parked on [`Mailbox::take`] now: a push then is a
    /// wake, and otherwise waits behind the loop's work.
    parked: bool,
    /// The stamped items' waits, by kind ([`Mailbox::copy_wakes`]).
    wakes: Wakes,
    /// The bound's NEGATIVE CONTROL: every event ahead of every inbox record,
    /// however many, the order before the bound. Only this module's own tests
    /// set it.
    #[cfg(test)]
    events_first: bool,
}

/// THE WAIT TALLY'S VIEW OF THE QUEUES ([`crate::wait_tally`]): each stamped
/// item's time from its push to its take, apart for an item pushed while the
/// loop was parked on [`Mailbox::take`] (its wake) and one pushed while the loop
/// worked (its wait behind that work), and apart for aterm's events and broker
/// records. An item forgotten before it was taken
/// ([`Mailbox::forget_queued_drains`], a reset) is not counted.
#[derive(Debug, Default)]
struct Wakes {
    event: Hist,
    record: Hist,
    queued_event: Hist,
    queued_record: Hist,
}

impl Wakes {
    /// An item stamped `stamp` (none while the tally is off) is taken now.
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

/// Where aterm's event queue stood at one moment ([`Mailbox::event_mark`]):
/// every event pushed before that moment is below it, and every event pushed
/// after it is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct EventMark(u64);

/// Whether `line` is `EVENT <local> post …` or `EVENT <local> fetch …`: an
/// event whose whole handling is an outbox drain
/// ([`Mailbox::forget_queued_drains`]).
fn is_drain_event(line: &str) -> bool {
    let mut toks = line.split_whitespace();
    toks.next() == Some("EVENT") && matches!(toks.nth(1), Some("post" | "fetch"))
}

/// Whether one source is over a bound and non-empty: an empty queue must accept one record
/// however large, or the lane wedges forever.
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
    /// A push's stamp: now, and whether the loop is parked; `None` while the
    /// wait tally is off, so an untallied bridge reads no clock for it.
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
    /// paused face's resume, aterm's events — but never more than
    /// [`EVENT_RUN_MAX`] of them in a row while an inbox record waits — the
    /// inbox group, then broadcast.
    ///
    /// A resume is taken ahead of the records because it is half a queue
    /// early: the subscription reopens while the loop works the other half.
    ///
    /// EVENTS FIRST, AND A BOUND ON THEM ([`EVENT_RUN_MAX`]): see its note for
    /// why an inbox record waits behind a run of events, why the run is
    /// bounded, and what strict turns cost. Neither order is a safety rule
    /// (§11.2's is the fleet face's, above): the two are separate sources with
    /// no order between them, and a record for a session whose
    /// `session-created` is still queued admits it first
    /// (`Bridge::on_inbox_record`).
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

    /// One broker source's head; a paused source this take brings down to its
    /// low water is owed its [`Item::Resume`].
    fn take_record(&mut self, s: Source) -> Option<Record> {
        let (r, stamp, resume) = self.face(s)?.take()?;
        self.wakes.taken(stamp, true);
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
    ///
    /// The inbox's reset ends the run of events counted for the record it
    /// forgets ([`Queues::events_run`]), as that record's take would have: the
    /// count is of events taken while a record WAITS, and a record queued
    /// after the reset has waited behind none of them.
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

    /// Whether an input that outranks broadcast replay is waiting. The bridge checks before
    /// each replay page, so a halt or addressed message waits one page, not a replay slice.
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

    /// Stamp every push from now on for the wait tally (`true`), or no more:
    /// the bridge turns it on with its recorder ([`crate::wait_tally`]).
    pub(crate) fn stamp(&self, on: bool) {
        self.lock().stamping = on;
    }

    /// Whether a taker is parked on [`Mailbox::take`] now: a test's cue that a
    /// push from another thread is a wake.
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

    /// Whether NOTHING is queued for the loop — no record, event, closure or
    /// resume. The bridge reads the acks of its pipelined publishes when this
    /// holds, so a burst of work shares group commits and an idle bridge waits
    /// for its one record as before.
    pub(crate) fn is_idle(&self) -> bool {
        self.lock().is_empty()
    }

    /// Whether an inbox-group record is queued behind the one being handled:
    /// the bridge commits the group once its queue runs dry, so a burst costs
    /// one commit rather than one per record.
    pub(crate) fn inbox_queued(&self) -> bool {
        !self.lock().inbox.records.is_empty()
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

    /// This moment in aterm's event queue: the events pushed so far are below
    /// it, every later one is not ([`Mailbox::forget_queued_drains`]).
    pub fn event_mark(&self) -> EventMark {
        EventMark(self.lock().events_pushed)
    }

    /// Forget every queued `post` and `fetch` event, of any session, that was
    /// pushed before `mark`, and leave every other event in place and in
    /// order. Returns how many it forgot.
    ///
    /// The bridge calls it once an outbox drain ([`crate::bridge`]'s
    /// `drain_outbox`) has answered all its peek listed, with the mark it took
    /// just before that peek: those events asked for the drain that just ran.
    ///
    /// A DRAIN EVENT IS A LEVEL, NOT A MESSAGE. It says "the outbox has work"
    /// and carries none of it: the drain peeks the whole outbox, and aterm
    /// queues a post's row (or a fetch's slot) BEFORE it records the event
    /// that rings this lane (`aterm-gui-fabric`'s `cmd_post` and its fetch,
    /// under the guard that admits the row). So every drain event below a
    /// peek's mark names work that peek saw, and one pushed after the mark
    /// stays for a drain of its own. Taken one by one, a run of them is a
    /// drain each: each waits for the posts the one before it published to be
    /// answered, and peeks an outbox that one already emptied, while the
    /// inbox records behind them wait ([`EVENT_RUN_MAX`]). aterm used to batch
    /// them on its side, reading a session's events at most once per 16 ms;
    /// it reads a record at once now (`aterm-gui`'s `subscribe.rs`,
    /// `EVENTS_PACE`), so they arrive one at a time and are batched here, by
    /// how many came while the loop was busy: none in a trickle, the run in a
    /// burst.
    ///
    /// ONLY A DRAIN THAT ANSWERED ALL ITS PEEK SAW MAY CALL IT. A peek can
    /// stop short of the outbox (aterm lists at most its drain budget, 4 MiB,
    /// and says `more`), and a drain can leave what it listed queued (a
    /// sender held by a refusal, a link lost, a roster it could not read).
    /// The events it would forget are then what runs the next drain at once;
    /// forgotten, the work would wait for a new event or the two-second
    /// roster backstop.
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

    /// Push one aterm `EVENT` line, waiting while the event queue is over its row bound (lines
    /// are small and coalesced, so only rows count).
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

    /// Latch aterm's last word before it closes the lanes — called by the push
    /// reader the moment it reads `EVENT * bye <word>`, BEFORE the line is
    /// queued, so the word is never behind the `Closed(Aterm)` the reader
    /// pushes at end-of-file. A later word replaces an earlier one: a handoff
    /// that rolled back and then quit says `quit` last.
    pub fn note_bye(&self, bye: aterm_types::fabric_grammar::Bye) {
        self.lock().bye = Some(bye);
    }

    /// The word [`Mailbox::note_bye`] latched, if aterm said one.
    #[must_use]
    pub fn bye(&self) -> Option<aterm_types::fabric_grammar::Bye> {
        self.lock().bye
    }

    /// Report that aterm went away, never generation-scoped: the inherited verb and push
    /// descriptors survive every reconnect, and this closure alone makes the run loop return.
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
        // The generation makes this total: clearing reaches only what was already pushed,
        // not a killed reader's late push.
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
    /// Fleet records come first here, unlike [`Mailbox::take`]: between items a closure means
    /// a reconnect whose `Last` read re-reads every halt, but inside a duty a delivered record
    /// is applied at once and the closure then ends the duty. `Closed(Aterm)` is never taken
    /// here: exiting is the loop's.
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

    /// **THE WAIT TALLY'S WAKE ROWS COUNT A PUSH THAT FOUND THE LOOP PARKED,
    /// AND ITS QUEUED ROWS ONE THAT FOUND IT WORKING** ([`Wakes`]), each by
    /// kind. An event and an inbox record pushed before the take are queued;
    /// an event and a record each pushed once another thread is parked on
    /// [`Mailbox::take`] are wakes; every item lands in one row, once.
    /// NEGATIVE CONTROL: the same pushes unstamped count nothing.
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

    /// **AN INBOX RECORD WAITS BEHIND AT MOST [`EVENT_RUN_MAX`] EVENTS IN A
    /// ROW.** Three inbox records wait while twice the bound and five more
    /// events are queued: the loop takes a whole run of events, then a record,
    /// another run, a record, the last five, then the last record — events
    /// first, as a run of them is mostly one drain's work, and never more than
    /// the bound while a record waits. A halt queued mid-run is still taken
    /// next. And a run with no record waiting is no run: the count starts only
    /// once a record waits.
    ///
    /// NEGATIVE CONTROL: with the bound taken away (`events_first`), every
    /// record waits behind every event.
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
                // A halt that arrives mid-run is taken next.
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

        // NO RECORD WAITING, NO RUN: events taken alone count nothing, so a
        // record that arrives after them waits behind a whole run of its own.
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

    /// **THE DRAIN EVENTS QUEUED BEFORE THE MARK ARE FORGOTTEN, AND NOTHING
    /// ELSE.** Behind a taken `post` event, the `post` and `fetch` events of
    /// every session pushed before the mark go, and every other line stays,
    /// in its order: an `inbox`, an `inbox-seen`, a `GAP`, and a `topic` row
    /// whose payload says `post`. A drain event pushed after the mark (while
    /// the drain ran, after its peek) stays, for a drain of its own, and so
    /// does one pushed after the forget.
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

    /// **A RUN ENDS WITH THE RECORD IT WAS COUNTED FOR, HOWEVER THAT RECORD
    /// LEAVES.** A record waits while a whole run of events is taken, and the
    /// reconnect's reset forgets the record ([`Mailbox::reset_broker_sources`])
    /// — or the loop's own close of the face does ([`Mailbox::reset_source`]).
    /// A fresh record queued after it has waited behind no event, so the event
    /// queued beside it is taken first: the count starts only while a record
    /// waits. CONTROL: a record that is not forgotten is next once its run is
    /// spent.
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

        // The killed reader waking up after the reconnect.
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
