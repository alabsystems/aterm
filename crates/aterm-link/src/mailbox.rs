// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SCHEDULING ROUND, as a data structure.
//!
//! §11.2 states one ordering rule about the bridge's inputs and it is a safety
//! rule, not a fairness one:
//!
//! > drains the `fleet/>` tail **ahead of** the inbox group on every scheduling
//! > round so a halt never queues behind a redelivery backlog
//!
//! A single FIFO channel cannot express that: a halt published behind ten
//! thousand redeliveries arrives after them. So each source has its own queue
//! and one condvar wakes the loop; the loop then takes strictly by priority.
//! Nothing polls and nothing sleeps — a reader thread pushes and notifies, and
//! the loop blocks until there is something to take.
//!
//! ## THE QUEUES ARE BOUNDED, AND THE SHED POLICY IS BACK-PRESSURE
//!
//! Every other bound in this design is written down and argued (`RING_CAP`,
//! `SENDER_QUOTA`, `OUTBOX_CAP`, `PRODUCER_CAP`, `FEED_TRIES_MAX`,
//! `SCREEN_MAX`); this one used to be the exception, and an exception here is
//! worse than most. Four reader threads push as fast as the broker delivers
//! while ONE consumer pays a synchronous aterm round trip per inbox record, and
//! a record body may be up to 16 MiB. Unbounded, a co-permitted peer's burst —
//! or a broker replaying a backlog after an outage — is bridge memory
//! exhaustion, and the bridge's death is the fail-closed halt of every session
//! it governs. T9's stated residual is "log flooding"; unbounded queues turn
//! that into a remote kill.
//!
//! The shed policy is BLOCK, not drop, and the reason is written per source
//! because it is not the same reason twice:
//!
//! * `Fleet` — a halt may never be dropped. Blocking is the only admissible
//!   answer.
//! * `Inbox` — the group cursor is committed only after the endpoint takes the
//!   record, so a subscription the broker sheds under its own write timeout is
//!   redelivered in full on the next attach. Blocking restores exactly the
//!   socket back-pressure the reader thread had removed.
//! * `Term` — a drive record must not be dropped silently (§6.5 is the whole
//!   rung), and a stale one must not be applied late; blocking leaves that
//!   decision where it already is, in [`Mailbox::reset_broker_sources`].
//! * `Event` — aterm's own push lane already has a shed policy of its own: it
//!   coalesces and emits `GAP`, which §4.2 says is published rather than
//!   dropped. Blocking hands the decision back to the side that can report it.
//!
//! `Closed` is NEVER bounded: it is the notice that the connection carrying
//! everything behind it is gone, and a bound that could delay it would be a
//! bound on the bridge's own liveness.
//!
//! ## EVERY BROKER INPUT CARRIES THE INCARNATION IT BELONGS TO
//!
//! A reader thread outlives the subscription it reads: `close_subscriptions`
//! shuts the socket down, and the parked `recv` returns some microseconds later,
//! on its own thread, with no ordering against the reconnect that is already
//! running on the loop's thread. Clearing the queues on reconnect can therefore
//! only drop what has ALREADY been pushed — the notice or record still in flight
//! lands in the FRESH mailbox, where a `Closed` tears down a connection that was
//! just brought up (a reconnect loop, not a retry) and a stale `Term` record is
//! the late-applied keystroke [`Mailbox::reset_broker_sources`] says must not
//! happen.
//!
//! So a broker input is accepted only under the GENERATION it was subscribed in:
//! [`Mailbox::reset_broker_sources`] bumps it, each reader captures it at spawn,
//! and a push from any other generation is dropped whenever it arrives rather
//! than only when it arrives early enough. This is `fabric::BridgeGeneration`'s
//! rule on the aterm side, for the same reason. `Source::Aterm` is not a broker
//! source — it is the inherited push lane, which no reconnect touches — so its
//! closure notice is never generation-scoped ([`Mailbox::push_aterm_closed`]).
//!
//! A push blocks only while its queue is over a cap AND non-empty, so a single
//! record larger than [`QUEUE_BYTES_MAX`] still makes progress rather than
//! wedging the lane forever.

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
    /// A `/f/<F>/term/<node>/>` record — the drive face.
    Term(Record),
    /// One `EVENT …` line off aterm's push lane.
    Event(String),
    /// A `/f/<F>/in/<node>/>` record from the durable group. LAST.
    Inbox(Record),
    /// A source ended: its reader thread saw EOF or an error. Named so the loop
    /// can tell "the broker went away" (reconnect) from "aterm went away" (exit).
    Closed(Source),
}

/// Which reader ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Fleet,
    Term,
    Inbox,
    Aterm,
}

/// The most records ONE source may hold before its reader is made to wait.
///
/// A thousand rows is a deep enough buffer that an ordinary burst never touches
/// it, and shallow enough that the resident cost is a queue rather than a heap.
pub const QUEUE_ROWS_MAX: usize = 1024;

/// The most bytes ONE source may hold before its reader is made to wait, over
/// subjects and bodies. The row cap alone is not a memory bound: a record body
/// may be up to 16 MiB, so a thousand of them is sixteen gigabytes.
pub const QUEUE_BYTES_MAX: usize = 8 * 1024 * 1024;

/// The bytes one queued record occupies, as the bound counts them.
fn record_bytes(r: &Record) -> usize {
    r.1.len() + r.2.len()
}

#[derive(Default)]
struct Queues {
    fleet: VecDeque<Record>,
    term: VecDeque<Record>,
    events: VecDeque<String>,
    inbox: VecDeque<Record>,
    closed: VecDeque<Source>,
    /// Resident bytes per record queue, in the order `Source` names them.
    bytes: [usize; 3],
    /// The broker incarnation whose inputs are currently accepted. Bumped by
    /// [`Mailbox::reset_broker_sources`]; see the module header.
    broker_gen: u64,
}

/// Which byte counter a record source uses.
const FLEET: usize = 0;
const TERM: usize = 1;
const INBOX: usize = 2;

/// Whether one source is over a bound AND has something to shed. Both halves
/// matter: a queue that is over the byte cap because of ONE huge record must
/// still accept it, or the lane wedges forever.
fn over_bound(rows: usize, bytes: usize) -> bool {
    rows > 0 && (rows >= QUEUE_ROWS_MAX || bytes >= QUEUE_BYTES_MAX)
}

impl Queues {
    fn is_empty(&self) -> bool {
        self.fleet.is_empty()
            && self.term.is_empty()
            && self.events.is_empty()
            && self.inbox.is_empty()
            && self.closed.is_empty()
    }

    /// THE PRIORITY, in one place. A closure notice outranks everything (the
    /// connection it names is gone, so anything queued behind it is stale), then
    /// the fleet halt, then the drive face, then aterm's own events, and the
    /// inbox group last.
    fn take(&mut self) -> Option<Item> {
        if let Some(s) = self.closed.pop_front() {
            return Some(Item::Closed(s));
        }
        if let Some(r) = self.fleet.pop_front() {
            self.bytes[FLEET] = self.bytes[FLEET].saturating_sub(record_bytes(&r));
            return Some(Item::Fleet(r));
        }
        if let Some(r) = self.term.pop_front() {
            self.bytes[TERM] = self.bytes[TERM].saturating_sub(record_bytes(&r));
            return Some(Item::Term(r));
        }
        if let Some(e) = self.events.pop_front() {
            return Some(Item::Event(e));
        }
        self.inbox.pop_front().map(|r| {
            self.bytes[INBOX] = self.bytes[INBOX].saturating_sub(record_bytes(&r));
            Item::Inbox(r)
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
    /// How many times a pusher has PARKED on a bound, ever.
    ///
    /// MONOTONE, AND IT EXISTS FOR THE TESTS THAT ASSERT A NON-EVENT. "The
    /// writer did not get past the bound" is a negative, and the only honest
    /// way to test one is to observe the positive that causes it: that a
    /// pusher actually reached [`Mailbox::wait_for_room`]. A test that instead
    /// sleeps and reads a flag the writer sets AFTER its push passes just as
    /// happily when the bound has been deleted and the thread was simply not
    /// scheduled in time — a false green over a memory bound whose own doc
    /// calls it "a safety bound, not a tidiness one". Spinning on this to a
    /// hang-detector deadline cannot pass without the bound: with no bound,
    /// nothing ever parks and the counter never moves.
    parks: std::sync::atomic::AtomicU64,
}

impl Mailbox {
    fn lock(&self) -> std::sync::MutexGuard<'_, Queues> {
        self.queues.lock().unwrap_or_else(|p| p.into_inner())
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
    /// Dropped if `generation` is not the live broker generation — the subscription it
    /// came from has been torn down. Re-checked after every wait, so a reader
    /// parked on the bound when the reconnect happens sheds its record instead of
    /// pushing it into the fresh mailbox.
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

    /// Push a drive-face record, WAITING while the drive queue is over its bound.
    /// Generation-scoped, as [`Mailbox::push_fleet`] is — and here the stale case
    /// is the one the module header calls out by name: a drive record from a
    /// torn-down subscription is a keystroke that would be applied late.
    pub fn push_term(&self, r: Record, generation: u64) {
        let n = record_bytes(&r);
        let mut q = self.lock();
        while over_bound(q.term.len(), q.bytes[TERM]) {
            if q.broker_gen != generation {
                return;
            }
            q = self.wait_for_room(q);
        }
        if q.broker_gen != generation {
            return;
        }
        q.bytes[TERM] += n;
        q.term.push_back(r);
        drop(q);
        self.ready.notify_all();
    }

    /// Push one aterm `EVENT` line, WAITING while the event queue is over its
    /// row bound. Lines are small and already coalesced by aterm, so only the
    /// row count applies.
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

    /// Park until the loop takes something (or a reconnect clears the queues).
    ///
    /// BOUNDED, and the bound is a hang detector rather than a poll: the wait is
    /// woken by [`Mailbox::take`], and re-checking on a timer only means a
    /// reader thread whose loop had exited cannot be wedged invisibly.
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
    /// the loop has already replaced, and acting on it drops a healthy one. That
    /// is the shape the aterm side fixed with `fabric::BridgeGeneration` — a late
    /// guard from a previous incarnation reporting a live replacement
    /// disconnected — and clearing the queue on reconnect cannot fix it, because
    /// clearing only reaches notices that have already been pushed.
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
    /// The verb and push descriptors are inherited once, at exec, and no
    /// reconnect touches them: there is no stale incarnation of this lane, and
    /// its closure is the only thing that makes the run loop return. A bound or a
    /// generation on it would be a bound on the bridge's own liveness.
    pub fn push_aterm_closed(&self) {
        self.lock().closed.push_back(Source::Aterm);
        self.ready.notify_all();
    }

    /// FORGET EVERY BROKER INPUT, on a reconnect.
    ///
    /// The queued records came from subscriptions that no longer exist and the
    /// queued closure notices name them; leaving either in place makes the first
    /// `take` after a successful reconnect tear the fresh connection down again,
    /// which is an infinite loop, not a retry. Losing the records costs nothing:
    /// the fleet face resubscribes from zero, the inbox group re-delivers
    /// everything it never committed, and a stale drive record must not be
    /// applied late anyway. `Closed(Aterm)` is KEPT — aterm going away is not a
    /// broker event and it is the one closure the loop must still act on.
    pub fn reset_broker_sources(&self) {
        let mut q = self.lock();
        // THE GENERATION MOVES FIRST, and it is what makes this total: clearing
        // reaches only what has already been pushed, while a reader killed BY
        // this reconnect pushes microseconds later, on its own thread, into the
        // fresh mailbox. See the module header.
        q.broker_gen = q.broker_gen.wrapping_add(1);
        q.fleet.clear();
        q.term.clear();
        q.inbox.clear();
        q.bytes = [0; 3];
        q.closed.retain(|s| *s == Source::Aterm);
        drop(q);
        // A reader parked on the bound belongs to a subscription that is being
        // torn down; waking it is what lets its thread notice and end.
        self.drained.notify_all();
    }

    /// Block until there is something to take, or `timeout` elapses.
    ///
    /// The timeout is not a poll interval: nothing is discovered by waking up.
    /// It exists so the loop can run its periodic duties (a presence refresh, a
    /// reconnect attempt) without a second timer, and it is generous.
    pub fn take(&self, timeout: Duration) -> Option<Item> {
        let mut q = self.lock();
        if let Some(item) = q.take() {
            drop(q);
            self.drained.notify_all();
            return Some(item);
        }
        let (mut q, _) = self
            .ready
            .wait_timeout_while(q, timeout, |q: &mut Queues| q.is_empty())
            .unwrap_or_else(|p| p.into_inner());
        let item = q.take();
        drop(q);
        if item.is_some() {
            self.drained.notify_all();
        }
        item
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn rec(off: u64) -> Record {
        (off, format!("/s/{off}"), Vec::new())
    }

    /// A HALT BEHIND A BACKLOG still arrives first. This is the whole reason the
    /// mailbox is not one channel: a fleet halt queued after a full inbox
    /// backlog would be applied a whole backlog of records too late, and a halt
    /// that late is not a halt.
    ///
    /// The backlog is one short of [`QUEUE_ROWS_MAX`] rather than ten thousand
    /// because the queue is now BOUNDED: a single-threaded test that pushed
    /// past the bound would park on its own back-pressure. The property is
    /// unchanged — a halt outranks however deep the inbox is allowed to get.
    #[test]
    fn a_halt_outranks_an_inbox_backlog_however_deep_it_is() {
        let mb = Mailbox::default();
        for off in 0..(QUEUE_ROWS_MAX as u64 - 1) {
            mb.push_inbox(rec(off), 0);
        }
        mb.push_fleet(rec(999_999), 0);
        let first = mb.take(Duration::from_secs(30)).expect("an item");
        match first {
            Item::Fleet((off, _, _)) => assert_eq!(off, 999_999),
            other => panic!("the halt did not come first: {other:?}"),
        }
        // And the backlog is still there, in order.
        match mb.take(Duration::from_secs(30)).expect("an item") {
            Item::Inbox((off, _, _)) => assert_eq!(off, 0),
            other => panic!("{other:?}"),
        }
    }

    /// The full order, once: closed, fleet, term, event, inbox.
    #[test]
    fn the_priority_is_the_declared_one() {
        let mb = Mailbox::default();
        mb.push_inbox(rec(4), 0);
        mb.push_event("EVENT 1 post 1".into());
        mb.push_term(rec(2), 0);
        mb.push_fleet(rec(1), 0);
        mb.push_closed(Source::Inbox, 0);
        let mut seen = Vec::new();
        while let Some(item) = mb.take(Duration::from_millis(0)) {
            seen.push(match item {
                Item::Closed(_) => "closed",
                Item::Fleet(_) => "fleet",
                Item::Term(_) => "term",
                Item::Event(_) => "event",
                Item::Inbox(_) => "inbox",
            });
        }
        assert_eq!(seen, ["closed", "fleet", "term", "event", "inbox"]);
    }

    /// The wait is EVENT-DRIVEN: a push from another thread wakes it, and the
    /// timeout is only the ceiling. The generous bound here is a hang detector,
    /// not a performance assertion — a healthy wake is microseconds.
    #[test]
    fn a_push_from_another_thread_wakes_the_wait() {
        let mb = Arc::new(Mailbox::default());
        let writer = {
            let mb = mb.clone();
            std::thread::spawn(move || mb.push_fleet(rec(7), 0))
        };
        let got = mb.take(Duration::from_secs(30)).expect("woken by the push");
        assert!(matches!(got, Item::Fleet((7, _, _))));
        writer.join().expect("writer thread");
    }

    /// A RECONNECT FORGETS THE OLD CONNECTION and nothing else. Without this the
    /// first `take` after a successful reattach picks up the closure notice the
    /// OLD subscription left and tears the fresh one down — a loop, not a retry.
    /// `Closed(Aterm)` survives, because aterm going away is not a broker event.
    #[test]
    fn a_reconnect_forgets_the_old_connections_inputs_but_not_aterms() {
        let mb = Mailbox::default();
        mb.push_fleet(rec(1), 0);
        mb.push_term(rec(2), 0);
        mb.push_inbox(rec(3), 0);
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
                Item::Term(_) => "term",
                Item::Event(_) => "event",
                Item::Inbox(_) => "inbox",
            });
        }
        assert_eq!(
            seen,
            ["closed-aterm", "event"],
            "only aterm's own inputs survive a broker reconnect"
        );
    }

    /// A NOTICE FROM THE SUBSCRIPTION THE RECONNECT KILLED IS DISCARDED WHENEVER
    /// IT ARRIVES — which is not the same property as clearing the queue.
    ///
    /// `attach_broker` closes the old subscriptions and then clears the mailbox
    /// on its own thread, but closing is exactly what makes a still-live reader
    /// end: its parked `recv` returns microseconds LATER, on another thread, and
    /// `push_closed` was unconditional. So the notice landed in the fresh
    /// mailbox, the loop read it as "a broker subscription ended", and dropped a
    /// connection that had just come up — every churn re-running
    /// `bring_presence_up`, `reconcile_halt` and `write_holds` over every hosted
    /// session. Reachable whenever a subscription is still alive at reconnect
    /// time: a partial `attach_broker` failure (the broker's `MAX_CONNS` under a
    /// post-restart herd, which `attach_broker` explicitly anticipates), or one
    /// subscription ending while the others live.
    ///
    /// The stale RECORD half is the same defect and the same fix: a `Term`
    /// record from a torn-down subscription is a keystroke applied late.
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
        mb.push_term(rec(1), stale);
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

    /// ATERM'S OWN CLOSURE IS NEVER GENERATION-SCOPED. The verb and push
    /// descriptors are inherited once at exec and no reconnect touches them, so
    /// there is no stale incarnation of that lane — and its notice is the only
    /// thing that makes the run loop return.
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

    /// An empty mailbox with a zero timeout answers `None` rather than parking:
    /// the loop's periodic duties must be reachable when nothing is arriving.
    #[test]
    fn an_empty_mailbox_times_out() {
        let mb = Mailbox::default();
        assert!(mb.take(Duration::from_millis(0)).is_none());
    }

    /// A READER IS MADE TO WAIT AT THE ROW BOUND, and is released by the loop
    /// taking one. Unbounded, this thread would push a million records into the
    /// bridge's heap while the single consumer paid an aterm round trip each;
    /// the bridge's death by OOM is the fail-closed halt of every session it
    /// governs, so the queue's bound is a safety bound, not a tidiness one.
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
        // The writer parks at the bound: it can never get past it while nothing
        // drains. WAITED FOR AS AN EVENT, not slept past — `parks` moves only
        // when a pusher actually reaches the back-pressure wait, so this loop
        // ends on the thing being asserted rather than on a timer. The deadline
        // is a hang detector: without the bound nothing ever parks and the
        // counter never moves, which is the failure.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
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
            match mb.take(Duration::from_secs(30)) {
                Some(Item::Inbox((off, _, _))) => {
                    assert_eq!(off, seen, "the queue is FIFO and sheds nothing");
                    seen += 1;
                }
                other => panic!("{other:?}"),
            }
        }
        writer.join().expect("writer thread");
    }

    /// THE BYTE BOUND BITES BEFORE THE ROW BOUND on big records — a thousand
    /// 16 MiB records is sixteen gigabytes, so a row count alone is not a
    /// memory bound — and ONE record over the whole byte cap still gets
    /// through, because a bound that could refuse a single record forever would
    /// wedge the lane instead of slowing it.
    ///
    /// THE ASSERTION IS THE PARK, NOT A SLEEP. This test used to spawn the
    /// writer, sleep 50 ms and assert a flag the writer sets AFTER its push was
    /// still false. Nothing in that proved the thread had reached `push_inbox`
    /// at all: delete the byte half of `over_bound` and, on a loaded box where
    /// the thread is not scheduled inside the window, the assertion holds and
    /// the suite reports a memory bound that is gone. So the writer's park is
    /// waited FOR — `Mailbox::parked` moves only when a pusher reaches the
    /// back-pressure wait — and the drain that releases it is what the ordering
    /// assertions below are about.
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
        // The fifth record takes the queue over the byte cap, so the writer
        // MUST park. Without the byte half of the bound it never does, and this
        // loop runs to its hang detector instead of passing on a timer.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while mb.parked() == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "four records at a quarter of the byte cap each must stop the fifth"
            );
            std::thread::yield_now();
        }
        // AND THE DRAIN IS WHAT RELEASES IT, in order: the four that were
        // already in, then the one that was waiting on room.
        for want in 0..5u64 {
            match mb.take(Duration::from_secs(30)) {
                Some(Item::Inbox((off, _, _))) => assert_eq!(off, want, "FIFO, nothing shed"),
                other => panic!("{other:?}"),
            }
        }
        writer.join().expect("writer thread");

        // AND THE ESCAPE HATCH: an empty queue takes a record however large.
        let mb2 = Mailbox::default();
        mb2.push_inbox((9, "/s".to_string(), vec![0u8; QUEUE_BYTES_MAX * 2]), 0);
        assert!(matches!(
            mb2.take(Duration::from_millis(0)),
            Some(Item::Inbox((9, _, _)))
        ));
    }
}
