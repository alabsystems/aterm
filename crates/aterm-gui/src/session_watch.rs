// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHO WATCHES A SESSION, AND THE WAKE THAT REACHES THEM. Each session's
//! [`Watchers`] list is a leaf of its context, and the writes that change what a
//! watcher reads ring that list themselves.
//!
//! A watcher is a parked control thread: a `subscribe` push loop, an `await`,
//! `ready`, `turn` or `wait`, the operator's observer. It holds one
//! [`Subscription`], a single-slot notify it parks on, registered on every list
//! it watches. [`Watchers::ring_watchers`] `try_send`s into each registered slot
//! and never blocks: a full slot is a wake already pending, so N rings between
//! two waits are one wake, and the watcher reads the latest state. A list
//! nobody watches costs a producer one atomic load.
//!
//! WHICH LIST RANG. One wake says only that some watched list rang, and a push
//! loop over N sessions must not read all N to find out which. Each
//! registration carries a dirty flag, returned by [`Subscription::watch`]: a
//! ring sets it before its `try_send`, and the watcher swaps it false before
//! it reads that list's state. A ring during the read sets it again, and a
//! ring into a full slot still sets it, so the flag is never lost: the pending
//! wake reaches the watcher, which finds the flag.
//!
//! THE PRODUCERS. A wake fires at the write that makes the state readable, on
//! the thread that made it:
//! - the PTY reader, once it has fed the engine and teed the burst to the byte
//!   fan-out (`spawn::publish_burst`), so a woken `bytes` watcher finds the
//!   burst queued;
//! - [`crate::session_timeline::SessionTimeline::record`]: meta, status and
//!   agent moves, the human row, render verdicts, the fabric's rows (`inbox`,
//!   `post`, `topic`, `fetch`, `inbox-seen`) and the lifecycle (`state-change`,
//!   `closing`), so an exit or a close wakes every park on the session, and a
//!   delivered row wakes a `mail` watch;
//! - [`crate::turn_ledger::TurnLedger::record`]: a landed turn;
//! - a resize path's booked row ([`crate::session_timeline::SessionTimeline::ingest_resizes`]);
//! - the store's roster journal, which rings the instance list
//!   ([`crate::session_store::SessionStore::instance_watchers`]) a `sessions`
//!   or `@*` subscription watches;
//! - the end of a host's own hold of the session's terminal ([`crate::TermGuard`]):
//!   a hold that moved the frame key ([`crate::frame_key::FrameKey`]) rings the
//!   list's FRAME WATCHERS, the subscriptions that read the screen
//!   ([`Subscription::watch_frame`]). That is every write the PTY reader does
//!   not make, on whatever thread: a selection, a config reload's colours, a
//!   Kitty animation frame, a resize, a line the host writes into a kept pane,
//!   a handoff's history import. The engine cannot see a hold end, so the
//!   list is attached to the terminal ([`Watchers::bind_frame`]); the reader's
//!   own holds leave the ring to its tee ([`crate::TermGuard::holder_rings`]).
//!   A write through a raw guard would ring nobody: the `term-write` census
//!   guard keeps every raw lock of a session's terminal to reading;
//! - the peer watch ([`crate::peer_watch`]), which rings a push stream's own
//!   list when its client hangs up.
//!
//! WHICH KIND OF WRITE RANG. The timeline, the turn ledger and a booked resize
//! row ring as RECORDS ([`Watchers::ring_record`]): the list's record count
//! steps before the ring, so a watcher can tell a ring that brought a record
//! from the PTY reader's, which brings output. A record is written by aterm's
//! own code, one per thing that happened (a post, a delivery, a status move, a
//! turn), never one per burst of a program's output. So a push loop reads the
//! `events` digest at once for a moved count, and paces it only for the
//! reader's rings (`subscribe::EVENTS_PACE`), which an output flood makes by
//! the thousand.
//!
//! LOCKS. `slots` is a strict leaf: held only to push, remove or `try_send`,
//! never while another lock is taken. It is taken under the lock that guards
//! the write (the timeline, the turn ledger, the store, the inbox before the
//! timeline, the terminal at the end of a host's hold), so a woken watcher
//! that reads that state waits for the write to finish and never sees half of
//! it. The lock-order census follows one hop and never a call through another
//! value, so it does not see those edges: the rule is stated here.
//!
//! HELD RINGS. A change a woken reader must find whole, though it is written in
//! steps under different locks, holds its lists' rings until the last step
//! ([`Watchers::hold_rings`]): a ring made meanwhile, on any thread, is owed,
//! and the last hold's release rings once. A session's close holds its own
//! list and the instance's from its deregister (the `closing` row, the
//! roster's exit) until its closed-session entry is linked, so a verb the close
//! wakes finds the session's log answering.
//!
//! NO CONTEXT. A subscription holds the [`Watchers`] `Arc` of each session it
//! watches, never the session's context, whose sink owns the PTY master.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_core::terminal::{FrameWatchers, Terminal};
use aterm_types::MutexExt as _;

use crate::session_timeline::SessionTimeline;
use crate::turn_ledger::TurnLedger;

/// One session's (or the instance's) watcher list.
#[derive(Default)]
pub(crate) struct Watchers {
    /// `slots.len()`, stored under the `slots` lock and read without it, so a
    /// producer on a list nobody watches takes no lock. A ring that reads 0 in
    /// the instant a registration lands misses that watcher: a watcher seeds
    /// what it reads before it registers, and its registration's flag starts
    /// set, so its first pass reads the change it missed.
    count: AtomicUsize,
    /// How many of the slots are frame watchers' ([`Subscription::watch_frame`]),
    /// stored like `count`: a hold of the session's terminal compares its frame
    /// only while this is not 0.
    frames: AtomicUsize,
    /// The registered slots. A STRICT LEAF (see the module doc).
    slots: Mutex<Vec<Slot>>,
    /// How many holds of this list's rings stand ([`Self::hold_rings`]).
    holds: AtomicUsize,
    /// The rings the standing holds owe ([`OWED_ALL`], [`OWED_FRAME`]), rung
    /// once by the last hold's release.
    owed: AtomicU8,
    /// How many records this list was rung for ([`Self::ring_record`]).
    records_rung: AtomicU64,
}

/// [`Watchers::owed`]: a ring of every watcher is owed.
const OWED_ALL: u8 = 1;
/// [`Watchers::owed`]: a ring of the frame watchers is owed.
const OWED_FRAME: u8 = 2;

/// One registration: its subscription's token, its dirty flag (see the module
/// doc), the subscription's single-slot notify, and whether it reads the
/// screen (a frame watcher's).
struct Slot {
    token: u64,
    dirty: Arc<AtomicBool>,
    notify: SyncSender<()>,
    frame: bool,
}

impl Watchers {
    /// Wake every watcher of this list. Never blocks, never fails: a full slot
    /// is a wake already pending, and a slot whose watcher is gone is removed by
    /// that watcher's drop. Each registration's dirty flag is set before its
    /// send, so the wake that reaches the watcher always finds it.
    /// While a hold stands ([`Self::hold_rings`]) the ring is owed instead.
    pub(crate) fn ring_watchers(&self) {
        if self.count.load(Ordering::Acquire) == 0 || self.owes(OWED_ALL) {
            return;
        }
        self.ring_slots(false);
    }

    /// [`Self::ring_watchers`] for a RECORD the session wrote: a timeline row,
    /// a booked resize row or a landed turn, which the `events` digest reads
    /// besides the engine. The record count ([`Self::records_rung`]) steps first,
    /// even when nobody watches or a hold owes the ring, so a watcher this ring
    /// reaches finds the count moved (see WHICH KIND OF WRITE RANG in the
    /// module doc). The caller holds the lock its record was written under,
    /// as for every ring here.
    pub(crate) fn ring_record(&self) {
        self.records_rung.fetch_add(1, Ordering::Release);
        self.ring_watchers();
    }

    /// How many records this list was rung for ([`Self::ring_record`]). A
    /// watcher loads it after it swaps its dirty flag, so a record whose ring
    /// set the flag is counted in what it loads.
    #[must_use]
    pub(crate) fn records_rung(&self) -> u64 {
        self.records_rung.load(Ordering::Acquire)
    }

    /// Wake this list's frame watchers only: the screen moved and nothing else
    /// a watcher reads did. Never blocks, and is held, like
    /// [`Self::ring_watchers`].
    pub(crate) fn ring_frame_watchers(&self) {
        if self.frames.load(Ordering::Acquire) == 0 || self.owes(OWED_FRAME) {
            return;
        }
        self.ring_slots(true);
    }

    /// Ring every slot, or only the frame watchers'.
    fn ring_slots(&self, frames_only: bool) {
        for slot in self
            .slots
            .lock_or_recover()
            .iter()
            .filter(|slot| !frames_only || slot.frame)
        {
            slot.ring();
        }
    }

    /// Hold this list's rings until the returned guard drops: a ring made
    /// meanwhile, on any thread, is owed, and the last standing hold's release
    /// rings once for all of them (see HELD RINGS in the module doc).
    pub(crate) fn hold_rings(self: &Arc<Self>) -> HeldRings {
        self.holds.fetch_add(1, Ordering::SeqCst);
        HeldRings(self.clone())
    }

    /// Whether a hold stands, owing `ring` to its release when one does. The
    /// owed bit is set BEFORE the second look at the holds, and a release
    /// drops its count before it takes the owed bits, so a ring that races the
    /// last release is rung by one of the two, never by neither.
    fn owes(&self, ring: u8) -> bool {
        if self.holds.load(Ordering::SeqCst) == 0 {
            return false;
        }
        self.owed.fetch_or(ring, Ordering::SeqCst);
        self.holds.load(Ordering::SeqCst) != 0
    }

    /// Attach this list to `term` as its frame watchers: a host's hold of
    /// `term` that moves its frame rings [`Self::ring_frame_watchers`]
    /// ([`crate::TermGuard`]). Called where a session's terminal and context
    /// are put together.
    pub(crate) fn bind_frame(self: &Arc<Self>, term: &Mutex<Terminal>) {
        crate::term_lock(term).set_frame_watchers(self.clone());
    }

    /// How many subscriptions watch this list: `who`'s `watchers=`, one per
    /// subscription whatever its streams.
    #[must_use]
    pub(crate) fn count(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    fn enlist(&self, token: u64, dirty: Arc<AtomicBool>, notify: SyncSender<()>, frame: bool) {
        let mut slots = self.slots.lock_or_recover();
        slots.push(Slot {
            token,
            dirty,
            notify,
            frame,
        });
        self.publish_counts(&slots);
    }

    fn delist(&self, token: u64) {
        let mut slots = self.slots.lock_or_recover();
        slots.retain(|slot| slot.token != token);
        self.publish_counts(&slots);
    }

    fn publish_counts(&self, slots: &[Slot]) {
        self.count.store(slots.len(), Ordering::Release);
        let frames = slots.iter().filter(|slot| slot.frame).count();
        self.frames.store(frames, Ordering::Release);
    }
}

impl Slot {
    /// Set the flag, then send: the wake that reaches the watcher finds it.
    fn ring(&self) {
        self.dirty.store(true, Ordering::Release);
        let _ = self.notify.try_send(());
    }
}

/// A hold of one list's rings ([`Watchers::hold_rings`]). The last standing
/// hold's drop rings what the holds owed, once.
#[must_use = "a hold rings what it owes when it drops"]
pub(crate) struct HeldRings(Arc<Watchers>);

impl Drop for HeldRings {
    fn drop(&mut self) {
        let list = &self.0;
        if list.holds.fetch_sub(1, Ordering::SeqCst) != 1 {
            return;
        }
        let owed = list.owed.swap(0, Ordering::SeqCst);
        if owed & OWED_ALL != 0 {
            list.ring_slots(false);
        } else if owed & OWED_FRAME != 0 {
            list.ring_slots(true);
        }
    }
}

/// The session's list as its terminal's frame watchers ([`Watchers::bind_frame`]).
impl FrameWatchers for Watchers {
    fn watched(&self) -> bool {
        self.frames.load(Ordering::Acquire) != 0
    }

    fn frame_moved(&self) {
        self.ring_frame_watchers();
    }
}

/// The source of subscription tokens: unique per process, so a subscription
/// removes exactly its own slot from a list another subscription also watches.
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(0);

/// A watcher's end: one single-slot notify, registered on every list it
/// watches, and removed from each when it drops.
pub(crate) struct Subscription {
    token: u64,
    /// The blocking end [`Self::wait`] parks on. It never reads a hang-up:
    /// `notify` below keeps a sender alive for the subscription's life.
    rx: Receiver<()>,
    /// The send end, cloned into each list [`Self::watch`] registers on.
    notify: SyncSender<()>,
    /// The lists this subscription is registered on, each with its
    /// registration's dirty flag.
    watching: Vec<(Arc<Watchers>, Arc<AtomicBool>)>,
}

impl Subscription {
    /// A subscription registered on nothing: its wait only times out until it
    /// watches a list.
    #[must_use]
    pub(crate) fn new() -> Self {
        // Capacity one: at most one pending wake (coalescing).
        let (notify, rx) = sync_channel(1);
        Subscription {
            token: NEXT_TOKEN.fetch_add(1, Ordering::Relaxed),
            rx,
            notify,
            watching: Vec::new(),
        }
    }

    /// A subscription on one list: a parked verb's. It keeps no flag: with one
    /// list, the wake itself says which rang.
    #[must_use]
    pub(crate) fn single(watchers: &Arc<Watchers>) -> Self {
        let mut sub = Self::new();
        let _ = sub.watch(watchers);
        sub
    }

    /// Register on `watchers` too, and return the registration's dirty flag:
    /// every ring of the list sets it, and the watcher swaps it false before it
    /// reads what the list guards. It starts SET, so the first read after the
    /// registration covers whatever the list's session changed between the
    /// watcher's seed and its registration, which no ring could reach.
    /// Idempotent: a second registration on one list would double its wakes
    /// and outlive the first removal, so it returns the first one's flag.
    pub(crate) fn watch(&mut self, watchers: &Arc<Watchers>) -> Arc<AtomicBool> {
        self.enlist(watchers, false)
    }

    /// [`Self::watch`], as a reader of the session's screen: a host's hold of
    /// the session's terminal that moves its frame rings this registration
    /// too ([`Watchers::ring_frame_watchers`]). A push loop with a content
    /// stream (`screen`, `cursor`, `cells`) watches its sessions so.
    pub(crate) fn watch_frame(&mut self, watchers: &Arc<Watchers>) -> Arc<AtomicBool> {
        self.enlist(watchers, true)
    }

    /// Register on `watchers` once: a second registration returns the first
    /// one's flag and keeps its kind.
    fn enlist(&mut self, watchers: &Arc<Watchers>, frame: bool) -> Arc<AtomicBool> {
        if let Some((_, dirty)) = self.watching.iter().find(|(w, _)| Arc::ptr_eq(w, watchers)) {
            return dirty.clone();
        }
        let dirty = Arc::new(AtomicBool::new(true));
        watchers.enlist(self.token, dirty.clone(), self.notify.clone(), frame);
        self.watching.push((watchers.clone(), dirty.clone()));
        dirty
    }

    /// Leave `watchers`: a `@*` subscription outlives any number of sessions,
    /// and a list it kept would hold a slot every ring walks.
    pub(crate) fn unwatch(&mut self, watchers: &Arc<Watchers>) {
        if let Some(at) = self
            .watching
            .iter()
            .position(|(w, _)| Arc::ptr_eq(w, watchers))
        {
            self.watching.swap_remove(at).0.delist(self.token);
        }
    }

    /// Park until a watched list rings, or until `bound` elapses when there is
    /// one: `true` on a wake. A coalesced wake is one wake; the caller re-reads
    /// the latest state. Unbounded, only a ring ends the wait: `notify` keeps a
    /// sender alive for the subscription's life.
    #[must_use]
    pub(crate) fn wait(&self, bound: impl Into<Option<Duration>>) -> bool {
        match bound.into() {
            Some(timeout) => matches!(self.rx.recv_timeout(timeout), Ok(())),
            None => self.rx.recv().is_ok(),
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        for (watchers, _) in &self.watching {
            watchers.delist(self.token);
        }
    }
}

/// The observed leaves of a new session's context, wired to ONE watcher list:
/// the timeline and the turn ledger ring the list `watchers` names. Every
/// session context is built from one, so a record wakes exactly the watchers
/// registered through `ctx.watchers`.
pub(crate) struct Observed {
    pub(crate) watchers: Arc<Watchers>,
    pub(crate) turns: Arc<Mutex<TurnLedger>>,
    pub(crate) timeline: Arc<Mutex<SessionTimeline>>,
}

impl Observed {
    /// A fresh list, with `turns` (an empty ledger, or one a handoff carried)
    /// and an empty timeline ringing it.
    #[must_use]
    pub(crate) fn new(turns: TurnLedger) -> Observed {
        let watchers = Arc::<Watchers>::default();
        Observed {
            turns: Arc::new(Mutex::new(turns.watched_by(watchers.clone()))),
            timeline: Arc::new(Mutex::new(SessionTimeline::watched_by(watchers.clone()))),
            watchers,
        }
    }
}

impl Default for Observed {
    fn default() -> Self {
        Observed::new(TurnLedger::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ring wakes every subscription on its list and no other, and N rings
    /// between two waits are one wake.
    #[test]
    fn a_ring_wakes_its_lists_watchers_once_and_no_others() {
        let (a, b) = (Arc::<Watchers>::default(), Arc::<Watchers>::default());
        let on_a = Subscription::single(&a);
        let on_b = Subscription::single(&b);
        for _ in 0..3 {
            a.ring_watchers();
        }
        assert!(on_a.wait(Duration::ZERO), "a's watcher is woken");
        assert!(!on_a.wait(Duration::ZERO), "three rings are one wake");
        assert!(!on_b.wait(Duration::ZERO), "b's watcher is not woken by a");
    }

    /// One subscription on two lists is woken by either, and leaves exactly the
    /// list it unwatches; a drop leaves the rest.
    #[test]
    fn a_subscription_watches_many_lists_and_leaves_exactly_its_own() {
        let (a, b) = (Arc::<Watchers>::default(), Arc::<Watchers>::default());
        let other = Subscription::single(&a);
        let mut sub = Subscription::new();
        assert!(!sub.wait(Duration::ZERO), "registered on nothing");
        sub.watch(&a);
        sub.watch(&a);
        sub.watch(&b);
        assert_eq!((a.count(), b.count()), (2, 1), "a second watch is a no-op");
        b.ring_watchers();
        assert!(sub.wait(Duration::ZERO), "woken by either list");
        sub.unwatch(&a);
        assert_eq!(a.count(), 1, "only its own slot left a");
        a.ring_watchers();
        assert!(!sub.wait(Duration::ZERO), "a no longer wakes it");
        assert!(other.wait(Duration::ZERO), "a still wakes the other");
        drop(sub);
        assert_eq!(b.count(), 0, "the drop left b");
        b.ring_watchers();
    }

    /// A FULL SLOT NEVER LOSES A DIRTY FLAG. A ring into a slot that already
    /// holds a wake sends nothing, so the flag is the only record of which list
    /// rang: it is set whether or not the send lands, and the one pending wake
    /// finds both lists' flags. A flag starts set (the first read after a
    /// registration covers the seed-to-registration gap) and a second watch of
    /// one list hands back the same flag.
    #[test]
    fn a_full_slot_never_loses_a_dirty_flag() {
        let (a, b, c) = (
            Arc::<Watchers>::default(),
            Arc::<Watchers>::default(),
            Arc::<Watchers>::default(),
        );
        let mut sub = Subscription::new();
        let flags = [sub.watch(&a), sub.watch(&b), sub.watch(&c)];
        assert!(
            flags.iter().all(|f| f.swap(false, Ordering::AcqRel)),
            "a registration's flag starts set"
        );
        assert!(Arc::ptr_eq(&sub.watch(&a), &flags[0]), "one flag per list");
        a.ring_watchers();
        b.ring_watchers();
        assert!(sub.wait(Duration::ZERO), "one wake is pending");
        assert!(
            !sub.wait(Duration::ZERO),
            "b's ring found the slot full and sent nothing"
        );
        let rang: Vec<bool> = flags
            .iter()
            .map(|f| f.swap(false, Ordering::AcqRel))
            .collect();
        assert_eq!(
            rang,
            [true, true, false],
            "both rings are on their flags, and c's is not"
        );
    }

    /// A HELD LIST RINGS ONCE, AT ITS LAST HOLD'S RELEASE. Rings made under a
    /// hold wake nobody and set no flag; the inner hold's release rings
    /// nothing while the outer one stands; the outer one's rings the watchers
    /// once, each flag set. A frame ring held alone is rung to the frame
    /// watchers only. NEGATIVE CONTROL: the same ring with no hold wakes at
    /// once.
    #[test]
    fn a_held_list_rings_once_at_its_last_holds_release() {
        let list = Arc::<Watchers>::default();
        let mut sub = Subscription::new();
        let dirty = sub.watch(&list);
        dirty.store(false, Ordering::Release);
        let mut screen = Subscription::new();
        let frame_dirty = screen.watch_frame(&list);
        frame_dirty.store(false, Ordering::Release);

        list.ring_watchers();
        assert!(
            sub.wait(Duration::ZERO),
            "the negative control: no hold, a wake"
        );
        assert!(screen.wait(Duration::ZERO));
        dirty.store(false, Ordering::Release);
        frame_dirty.store(false, Ordering::Release);

        let outer = list.hold_rings();
        let inner = list.hold_rings();
        list.ring_watchers();
        list.ring_watchers();
        assert!(!sub.wait(Duration::ZERO), "a held ring wakes nobody");
        assert!(!dirty.load(Ordering::Acquire), "and sets no flag");
        drop(inner);
        assert!(!sub.wait(Duration::ZERO), "the outer hold still stands");
        drop(outer);
        assert!(sub.wait(Duration::ZERO), "the last release rings");
        assert!(!sub.wait(Duration::ZERO), "once");
        assert!(dirty.swap(false, Ordering::AcqRel), "with the flag set");
        assert!(screen.wait(Duration::ZERO), "every watcher");
        frame_dirty.store(false, Ordering::Release);

        let held = list.hold_rings();
        list.ring_frame_watchers();
        assert!(
            !screen.wait(Duration::ZERO),
            "a held frame ring wakes nobody"
        );
        drop(held);
        assert!(
            screen.wait(Duration::ZERO),
            "its release rings the frame watcher"
        );
        assert!(frame_dirty.load(Ordering::Acquire));
        assert!(!sub.wait(Duration::ZERO), "and no other");
        assert!(!dirty.load(Ordering::Acquire));
    }

    /// A ring never blocks on a watcher that stopped reading: its slot holds
    /// one pending wake and the rest are dropped.
    #[test]
    fn a_ring_never_blocks_on_a_watcher_that_stopped_reading() {
        let list = Arc::<Watchers>::default();
        let sub = Subscription::single(&list);
        for _ in 0..10_000 {
            list.ring_watchers();
        }
        assert!(sub.wait(Duration::ZERO));
        assert!(!sub.wait(Duration::ZERO));
    }

    /// A RECORD WAKES ITS SESSION'S WATCHERS AND NO OTHER'S, AND IS COUNTED:
    /// the timeline and the turn ledger of one [`Observed`] ring the list it
    /// names as records, each stepping its record count once, and a plain ring
    /// (the PTY reader's) steps no count.
    #[test]
    fn a_record_wakes_its_sessions_watchers_and_no_others() {
        let (one, two) = (Observed::default(), Observed::default());
        let on_one = Subscription::single(&one.watchers);
        let on_two = Subscription::single(&two.watchers);
        one.timeline
            .lock()
            .unwrap()
            .record("cwd-change", "cwd=/tmp".to_string());
        assert!(on_one.wait(Duration::ZERO), "a timeline record wakes it");
        assert!(!on_two.wait(Duration::ZERO), "another session's does not");
        assert_eq!(
            (one.watchers.records_rung(), two.watchers.records_rung()),
            (1, 0)
        );
        one.turns
            .lock()
            .unwrap()
            .record(crate::subscribe::digest_fixture::turn_record(1), || 2);
        assert!(on_one.wait(Duration::ZERO), "a turn record wakes it");
        assert!(!on_two.wait(Duration::ZERO));
        assert_eq!(one.watchers.records_rung(), 2, "a turn record is counted");
        one.watchers.ring_watchers();
        assert!(on_one.wait(Duration::ZERO), "an output ring wakes it");
        assert_eq!(one.watchers.records_rung(), 2, "and is no record");
    }
}
