// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session [`Watchers`] lists and single-slot [`Subscription`] notifications.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_core::terminal::{FrameWatchers, Terminal};
use aterm_types::MutexExt as _;

use crate::session_timeline::SessionTimeline;
use crate::turn_ledger::TurnLedger;

/// Per-session or per-instance watcher list.
#[derive(Default)]
pub(crate) struct Watchers {
    /// `slots.len()`, stored under the `slots` lock and read without it, so a
    /// producer on a list nobody watches takes no lock.
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
    /// Wakes every registered watcher without blocking.
    pub(crate) fn ring_watchers(&self) {
        if self.count.load(Ordering::Acquire) == 0 || self.owes(OWED_ALL) {
            return;
        }
        self.ring_slots(false);
    }

    /// Increments the record counter and wakes every watcher ([`Self::ring_watchers`]).
    pub(crate) fn ring_record(&self) {
        self.records_rung.fetch_add(1, Ordering::Release);
        self.ring_watchers();
    }

    /// Returns the number of record rings ([`Self::ring_record`]).
    #[must_use]
    pub(crate) fn records_rung(&self) -> u64 {
        self.records_rung.load(Ordering::Acquire)
    }

    /// Wakes only frame watchers registered on this list.
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

    /// Holds rings on this list until the returned [`HeldRings`] guard drops.
    pub(crate) fn hold_rings(self: &Arc<Self>) -> HeldRings {
        self.holds.fetch_add(1, Ordering::SeqCst);
        HeldRings(self.clone())
    }

    /// Whether a hold stands, owing `ring` to its release when one does.
    fn owes(&self, ring: u8) -> bool {
        if self.holds.load(Ordering::SeqCst) == 0 {
            return false;
        }
        self.owed.fetch_or(ring, Ordering::SeqCst);
        self.holds.load(Ordering::SeqCst) != 0
    }

    /// Binds this watcher list to `term` as its frame watcher ([`Self::ring_frame_watchers`]).
    pub(crate) fn bind_frame(self: &Arc<Self>, term: &Mutex<Terminal>) {
        crate::term_lock(term).set_frame_watchers(self.clone());
    }

    /// The list [`Self::bind_frame`] attached to `t`, which its session's PTY reader rings at every burst.
    #[must_use]
    pub(crate) fn of(t: &Terminal) -> Option<Arc<Watchers>> {
        let bound: Arc<dyn std::any::Any + Send + Sync> = t.frame_watchers()?.clone();
        bound.downcast().ok()
    }

    /// Returns the number of active subscriptions watching this list.
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

/// Guard holding rings on a [`Watchers`] list ([`Watchers::hold_rings`]).
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

/// Routes terminal frame changes to [`Watchers::ring_frame_watchers`].
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

/// Single-slot watcher notification handle registered across one or more [`Watchers`] lists.
pub(crate) struct Subscription {
    token: u64,
    /// The blocking end [`Self::wait`] parks on.
    rx: Receiver<()>,
    /// The send end, cloned into each list [`Self::watch`] registers on.
    notify: SyncSender<()>,
    /// The lists this subscription is registered on, each with its
    /// registration's dirty flag.
    watching: Vec<(Arc<Watchers>, Arc<AtomicBool>)>,
}

impl Subscription {
    /// Creates an unregistered subscription.
    #[must_use]
    pub(crate) fn new() -> Self {
        let (notify, rx) = sync_channel(1);
        Subscription {
            token: NEXT_TOKEN.fetch_add(1, Ordering::Relaxed),
            rx,
            notify,
            watching: Vec::new(),
        }
    }

    /// Creates a subscription registered on a single `watchers` list.
    #[must_use]
    pub(crate) fn single(watchers: &Arc<Watchers>) -> Self {
        let mut sub = Self::new();
        let _ = sub.watch(watchers);
        sub
    }

    /// Registers on `watchers` and returns its dirty flag.
    pub(crate) fn watch(&mut self, watchers: &Arc<Watchers>) -> Arc<AtomicBool> {
        self.enlist(watchers, false)
    }

    /// Registers on `watchers` as a frame watcher ([`Self::watch`]) and returns its dirty flag.
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

    /// Unregisters this subscription from `watchers`.
    pub(crate) fn unwatch(&mut self, watchers: &Arc<Watchers>) {
        if let Some(at) = self
            .watching
            .iter()
            .position(|(w, _)| Arc::ptr_eq(w, watchers))
        {
            self.watching.swap_remove(at).0.delist(self.token);
        }
    }

    /// Parks until a watched list rings or `bound` elapses, returning `true` on wake.
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

/// Session timeline and turn ledger bound to a shared [`Watchers`] list.
pub(crate) struct Observed {
    pub(crate) watchers: Arc<Watchers>,
    pub(crate) turns: Arc<Mutex<TurnLedger>>,
    pub(crate) timeline: Arc<Mutex<SessionTimeline>>,
}

impl Observed {
    /// Creates an [`Observed`] bundle with `turns` and an empty timeline.
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

    /// A FULL SLOT NEVER LOSES A DIRTY FLAG.
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

    /// A HELD LIST RINGS ONCE, AT ITS LAST HOLD'S RELEASE.
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
