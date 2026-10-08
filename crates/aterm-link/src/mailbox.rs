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
//!
//! ## The queues are bounded; the shed policy is back-pressure
//!
//! Readers push as fast as the broker delivers while one consumer pays a
//! synchronous aterm round trip per record, and a body may be 16 MiB. Unbounded,
//! a peer's burst or a broker replaying a backlog exhausts bridge memory, and
//! the bridge's death halts every session it governs. The policy is block, not
//! drop, for a different reason per source:
//!
//! * `Fleet` — a halt may never be dropped.
//! * `Inbox` — the group cursor commits only after the endpoint takes the
//!   record, so blocking restores socket back-pressure and anything the broker
//!   sheds is redelivered on the next attach.
//! * `Event` — aterm's push lane already coalesces and emits `GAP` (§4.2), so
//!   blocking hands the decision to the side that can report it.
//!
//! `Closed` is never bounded: a bound on it would bound the bridge's liveness.
//! A push blocks only while its queue is over a cap AND non-empty, so one record
//! larger than [`QUEUE_BYTES_MAX`] still makes progress.
//!
//! ## Every broker input carries its incarnation
//!
//! A reader thread outlives its subscription: after `close_subscriptions` its
//! parked `recv` returns microseconds later, unordered against the reconnect.
//! Clearing queues on reconnect drops only what was already pushed; the
//! in-flight `Closed` would tear down the fresh connection (a reconnect loop)
//! and a stale record would be applied late. So [`Mailbox::reset_broker_sources`]
//! bumps a generation, each reader captures it at spawn, and a push from any
//! other generation is dropped whenever it arrives — `fabric::BridgeGeneration`'s
//! rule on the aterm side. `Source::Aterm` is the inherited push lane, which no
//! reconnect touches, so its closure is never generation-scoped.

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
    /// A source ended: its reader thread saw EOF or an error. Named so the loop
    /// can tell "the broker went away" (reconnect) from "aterm went away" (exit).
    Closed(Source),
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

/// The most records one source may hold before its reader waits: deep enough
/// for an ordinary burst, shallow enough to stay a queue rather than a heap.
pub const QUEUE_ROWS_MAX: usize = 1024;

/// The most subject+body bytes one source may hold before its reader waits. The
/// row cap alone is no memory bound: a thousand 16 MiB bodies is 16 GiB.
pub const QUEUE_BYTES_MAX: usize = 8 * 1024 * 1024;

/// The bytes one queued record occupies, as the bound counts them.
fn record_bytes(r: &Record) -> usize {
    r.1.len() + r.2.len()
}

#[derive(Default)]
struct Queues {
    fleet: VecDeque<Record>,
    events: VecDeque<String>,
    inbox: VecDeque<Record>,
    say: VecDeque<Record>,
    closed: VecDeque<Source>,
    /// Resident bytes per record queue, in the order `Source` names them.
    bytes: [usize; 3],
    /// The broker incarnation whose inputs are currently accepted. Bumped by
    /// [`Mailbox::reset_broker_sources`]; see the module header.
    broker_gen: u64,
}

/// Which byte counter a record source uses.
const FLEET: usize = 0;
const INBOX: usize = 1;
const SAY: usize = 2;

/// Whether one source is over a bound AND non-empty: an empty queue must accept
/// one record however large, or the lane wedges forever.
fn over_bound(rows: usize, bytes: usize) -> bool {
    rows > 0 && (rows >= QUEUE_ROWS_MAX || bytes >= QUEUE_BYTES_MAX)
}

impl Queues {
    fn is_empty(&self) -> bool {
        self.fleet.is_empty()
            && self.events.is_empty()
            && self.inbox.is_empty()
            && self.say.is_empty()
            && self.closed.is_empty()
    }

    /// The priority, in one place: a closure notice (anything behind it is
    /// stale), the fleet halt, aterm's events, the inbox group, then broadcast.
    ///
    /// Say is last because nobody on this node chose its volume: any
    /// co-permitted peer may shout as often as it likes, and each record costs
    /// one synchronous `deliver` per subscribed session.
    ///
    /// The `bool` is whether the take released room for a blocked producer;
    /// readers park only at their own queue's bound.
    fn take(&mut self) -> Option<(Item, bool)> {
        if let Some(s) = self.closed.pop_front() {
            return Some((Item::Closed(s), false));
        }
        if let Some(r) = self.fleet.pop_front() {
            let released = over_bound(self.fleet.len() + 1, self.bytes[FLEET]);
            self.bytes[FLEET] = self.bytes[FLEET].saturating_sub(record_bytes(&r));
            return Some((Item::Fleet(r), released));
        }
        if let Some(e) = self.events.pop_front() {
            return Some((Item::Event(e), over_bound(self.events.len() + 1, 0)));
        }
        if let Some(r) = self.inbox.pop_front() {
            let released = over_bound(self.inbox.len() + 1, self.bytes[INBOX]);
            self.bytes[INBOX] = self.bytes[INBOX].saturating_sub(record_bytes(&r));
            return Some((Item::Inbox(r), released));
        }
        self.say.pop_front().map(|r| {
            let released = over_bound(self.say.len() + 1, self.bytes[SAY]);
            self.bytes[SAY] = self.bytes[SAY].saturating_sub(record_bytes(&r));
            (Item::Say(r), released)
        })
    }
}

/// The bridge's one input queue set.
#[derive(Default)]
pub struct Mailbox {
    queues: Mutex<Queues>,
    ready: Condvar,
    /// Woken when the loop takes something, or when a reconnect clears the
    /// broker queues: the back-pressure half of the pair.
    drained: Condvar,
    /// How many times a pusher has parked on a bound, ever (monotone).
    ///
    /// Lets tests assert the bound by observing the park itself rather than
    /// sleeping and reading a flag: without the bound nothing parks and the
    /// counter never moves, so a test cannot pass on scheduling luck.
    parks: std::sync::atomic::AtomicU64,
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
        if !q.closed.is_empty() || !q.fleet.is_empty() {
            ReplayPressure::Urgent
        } else if !q.events.is_empty() || !q.inbox.is_empty() {
            ReplayPressure::Pending
        } else {
            ReplayPressure::Clear
        }
    }

    /// The broker incarnation a reader spawned NOW must stamp its pushes with.
    #[must_use]
    pub fn broker_generation(&self) -> u64 {
        self.lock().broker_gen
    }

    /// How many times a pusher has parked on a bound. See [`Mailbox::parks`].
    #[cfg(test)]
    fn parked(&self) -> u64 {
        self.parks.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Push a fleet record, WAITING while the fleet queue is over its bound.
    ///
    /// Dropped if `generation` is not the live broker generation; re-checked
    /// after every wait, so a reader parked across a reconnect sheds its record.
    pub fn push_fleet(&self, r: Record, generation: u64) {
        let n = record_bytes(&r);
        let mut q = self.lock();
        while over_bound(q.fleet.len(), q.bytes[FLEET]) {
            if q.broker_gen != generation {
                return;
            }
            q = self.wait_for_room(q);
        }
        if q.broker_gen != generation {
            return;
        }
        q.bytes[FLEET] += n;
        q.fleet.push_back(r);
        drop(q);
        self.ready.notify_all();
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

    /// Push an inbox-group record, WAITING while the inbox queue is over its
    /// bound — which is what puts the back-pressure back on the socket.
    pub fn push_inbox(&self, r: Record, generation: u64) {
        let n = record_bytes(&r);
        let mut q = self.lock();
        while over_bound(q.inbox.len(), q.bytes[INBOX]) {
            if q.broker_gen != generation {
                return;
            }
            q = self.wait_for_room(q);
        }
        if q.broker_gen != generation {
            return;
        }
        q.bytes[INBOX] += n;
        q.inbox.push_back(r);
        drop(q);
        self.ready.notify_all();
    }

    /// Push a BROADCAST record, WAITING while the say queue is over its bound.
    ///
    /// The say face is a plain subscription with no group cursor, so blocking
    /// buys only socket back-pressure: a broadcast the bridge was too busy to
    /// read is one the broker sheds and nobody delivers — the honest outcome for
    /// an opt-in fan-out with no delivery promise.
    pub fn push_say(&self, r: Record, generation: u64) {
        let n = record_bytes(&r);
        let mut q = self.lock();
        while over_bound(q.say.len(), q.bytes[SAY]) {
            if q.broker_gen != generation {
                return;
            }
            q = self.wait_for_room(q);
        }
        if q.broker_gen != generation {
            return;
        }
        q.bytes[SAY] += n;
        q.say.push_back(r);
        drop(q);
        self.ready.notify_all();
    }

    /// Park until the loop takes something (or a reconnect clears the queues).
    ///
    /// The timeout is a hang detector, not a poll: [`Mailbox::take`] wakes it.
    fn wait_for_room<'a>(
        &'a self,
        q: std::sync::MutexGuard<'a, Queues>,
    ) -> std::sync::MutexGuard<'a, Queues> {
        // COUNTED BEFORE THE WAIT, and never decremented: see [`Mailbox::parks`].
        self.parks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.drained
            .wait_timeout(q, Duration::from_millis(250))
            .unwrap_or_else(|p| p.into_inner())
            .0
    }

    /// Report that a BROKER reader ended, under the generation it was reading in.
    ///
    /// A notice from a torn-down subscription is dropped: it names a connection
    /// the loop already replaced, and acting on it would drop a healthy one.
    pub fn push_closed(&self, s: Source, generation: u64) {
        let mut q = self.lock();
        if q.broker_gen != generation {
            return;
        }
        q.closed.push_back(s);
        drop(q);
        self.ready.notify_all();
    }

    /// Report that ATERM went away — never generation-scoped.
    ///
    /// The verb and push descriptors are inherited once at exec and no reconnect
    /// touches them; this closure is the only thing that makes the run loop
    /// return.
    pub fn push_aterm_closed(&self) {
        self.lock().closed.push_back(Source::Aterm);
        self.ready.notify_all();
    }

    /// FORGET EVERY BROKER INPUT, on a reconnect.
    ///
    /// A queued closure notice from a dead subscription would tear the fresh
    /// connection down again (a loop, not a retry). Losing the records costs
    /// nothing: the fleet face resubscribes from zero and the inbox group
    /// redelivers everything uncommitted. `Closed(Aterm)` is kept — it is not a
    /// broker event.
    pub fn reset_broker_sources(&self) {
        let mut q = self.lock();
        // The generation is what makes this total: clearing reaches only what
        // was already pushed, not a killed reader's late push.
        q.broker_gen = q.broker_gen.wrapping_add(1);
        q.fleet.clear();
        q.inbox.clear();
        q.say.clear();
        q.bytes = [0; 3];
        q.closed.retain(|s| *s == Source::Aterm);
        drop(q);
        // A reader parked on the bound belongs to a subscription that is being
        // torn down; waking it is what lets its thread notice and end.
        self.drained.notify_all();
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

    /// A halt behind a full inbox backlog still arrives first. The backlog is one
    /// short of [`QUEUE_ROWS_MAX`] so this single-threaded test does not park on
    /// its own back-pressure.
    #[test]
    fn a_halt_outranks_an_inbox_backlog_however_deep_it_is() {
        let mb = Mailbox::default();
        for off in 0..(QUEUE_ROWS_MAX as u64 - 1) {
            mb.push_inbox(rec(off), 0);
        }
        mb.push_fleet(rec(999_999), 0);
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

    /// The full order, once: closed, fleet, event, inbox, say.
    #[test]
    fn the_priority_is_the_declared_one() {
        let mb = Mailbox::default();
        mb.push_say(rec(7), 0);
        mb.push_inbox(rec(4), 0);
        mb.push_event("EVENT 1 post 1".into());
        mb.push_fleet(rec(1), 0);
        mb.push_closed(Source::Inbox, 0);
        let mut seen = Vec::new();
        while let Some(item) = mb.take(Duration::from_millis(0)) {
            seen.push(match item {
                Item::Closed(_) => "closed",
                Item::Fleet(_) => "fleet",
                Item::Event(_) => "event",
                Item::Inbox(_) => "inbox",
                Item::Say(_) => "say",
            });
        }
        // Pushed in reverse so the order below is priority, not arrival.
        assert_eq!(seen, ["closed", "fleet", "event", "inbox", "say"]);
    }

    #[test]
    fn replay_yields_to_every_queue_ahead_of_the_say_face() {
        let mb = Mailbox::default();
        assert_eq!(mb.replay_pressure(), ReplayPressure::Clear);
        mb.push_say(rec(1), 0);
        assert_eq!(mb.replay_pressure(), ReplayPressure::Clear);

        mb.push_inbox(rec(2), 0);
        assert_eq!(mb.replay_pressure(), ReplayPressure::Pending);
        assert!(matches!(mb.take(Duration::ZERO), Some(Item::Inbox(_))));
        assert_eq!(mb.replay_pressure(), ReplayPressure::Clear);

        mb.push_event("EVENT 1 topic add probe".into());
        assert_eq!(mb.replay_pressure(), ReplayPressure::Pending);
        assert!(matches!(mb.take(Duration::ZERO), Some(Item::Event(_))));
        assert_eq!(mb.replay_pressure(), ReplayPressure::Clear);

        mb.push_fleet(rec(3), 0);
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
            std::thread::spawn(move || mb.push_fleet(rec(7), 0))
        };
        let got = mb.take(HANG_DETECTOR).expect("woken by the push");
        assert!(matches!(got, Item::Fleet((7, _, _))));
        writer.join().expect("writer thread");
    }

    /// A reconnect forgets the old connection's inputs and nothing else.
    #[test]
    fn a_reconnect_forgets_the_old_connections_inputs_but_not_aterms() {
        let mb = Mailbox::default();
        mb.push_fleet(rec(1), 0);
        mb.push_inbox(rec(3), 0);
        mb.push_say(rec(9), 0);
        mb.push_event("EVENT 1 post 1".into());
        mb.push_closed(Source::Inbox, 0);
        mb.push_aterm_closed();
        mb.reset_broker_sources();
        let mut seen = Vec::new();
        while let Some(item) = mb.take(Duration::from_millis(0)) {
            seen.push(match item {
                Item::Closed(Source::Aterm) => "closed-aterm",
                Item::Closed(_) => "closed-broker",
                Item::Fleet(_) => "fleet",
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
    /// post-restart herd); a late record would be applied late.
    #[test]
    fn a_late_push_from_the_previous_incarnation_is_discarded() {
        let mb = Mailbox::default();
        let stale = mb.broker_generation();
        mb.reset_broker_sources();
        let fresh = mb.broker_generation();
        assert_ne!(stale, fresh, "a reconnect moves the generation");

        // The reader the reconnect killed, waking up after it: a closure notice
        // and a record, both from the subscription that no longer exists.
        mb.push_closed(Source::Fleet, stale);
        mb.push_inbox(rec(2), stale);
        mb.push_fleet(rec(3), stale);
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
        mb.push_aterm_closed();
        assert!(matches!(
            mb.take(Duration::from_millis(0)),
            Some(Item::Closed(Source::Aterm))
        ));
    }

    /// A reader waits at the row bound and is released by a take.
    #[test]
    fn a_reader_blocks_at_the_row_bound_and_is_released_by_a_take() {
        let mb = Arc::new(Mailbox::default());
        let pushed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let writer = {
            let (mb, pushed) = (mb.clone(), pushed.clone());
            std::thread::spawn(move || {
                for off in 0..(QUEUE_ROWS_MAX as u64 + 4) {
                    mb.push_inbox(rec(off), 0);
                    pushed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            })
        };
        // Wait for the park itself (see `Mailbox::parks`), not a timer.
        let deadline = std::time::Instant::now() + HANG_DETECTOR;
        while mb.parked() == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the writer never parked at the row bound (pushed {})",
                pushed.load(std::sync::atomic::Ordering::SeqCst)
            );
            std::thread::yield_now();
        }
        assert_eq!(
            pushed.load(std::sync::atomic::Ordering::SeqCst),
            QUEUE_ROWS_MAX,
            "the reader must be parked ON the bound, not past it"
        );
        // Draining releases it, one slot at a time, and nothing was dropped.
        let mut seen = 0u64;
        while seen < QUEUE_ROWS_MAX as u64 + 4 {
            match mb.take(HANG_DETECTOR) {
                Some(Item::Inbox((off, _, _))) => {
                    assert_eq!(off, seen, "the queue is FIFO and sheds nothing");
                    seen += 1;
                }
                other => panic!("{other:?}"),
            }
        }
        writer.join().expect("writer thread");
    }

    /// The byte bound bites before the row bound on big records, and one record
    /// over the whole byte cap still passes an empty queue.
    #[test]
    fn the_byte_bound_holds_and_one_oversized_record_still_passes() {
        let mb = Arc::new(Mailbox::default());
        let big = |off: u64| (off, "/s".to_string(), vec![0u8; QUEUE_BYTES_MAX / 4]);
        for off in 0..4 {
            mb.push_inbox(big(off), 0);
        }
        assert_eq!(mb.parked(), 0, "four quarter-cap records fit");
        let writer = {
            let mb = mb.clone();
            std::thread::spawn(move || mb.push_inbox(big(4), 0))
        };
        // The fifth record is over the byte cap, so the writer must park.
        let deadline = std::time::Instant::now() + HANG_DETECTOR;
        while mb.parked() == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "four records at a quarter of the byte cap each must stop the fifth"
            );
            std::thread::yield_now();
        }
        // The drain releases it, in order.
        for want in 0..5u64 {
            match mb.take(HANG_DETECTOR) {
                Some(Item::Inbox((off, _, _))) => assert_eq!(off, want, "FIFO, nothing shed"),
                other => panic!("{other:?}"),
            }
        }
        writer.join().expect("writer thread");

        // An empty queue takes a record however large.
        let mb2 = Mailbox::default();
        mb2.push_inbox((9, "/s".to_string(), vec![0u8; QUEUE_BYTES_MAX * 2]), 0);
        assert!(matches!(
            mb2.take(Duration::from_millis(0)),
            Some(Item::Inbox((9, _, _)))
        ));
    }
}
