// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The boundary guards an update's handoff lands between (init-on-by-default design §6.7).
//!
//! An item that cannot be repeated safely holds a guard across its critical section, in the
//! crate that runs it: the Messages and FaceTime sign-out clicks (aterm-machines), a clone's
//! publish rename (aterm-machines) and a launchd bootstrap of the vault's jobs (aterm-secrets).
//! The handoff (aterm-gui's `app_update_handoff_boundary`) reads this process's registry
//! ([`Boundary::process`]) as it starts and again at its Commit, and is refused while any item
//! holds it.
//!
//! While a handoff is pending ([`Boundary::latch_handoff`]), no item enters its critical
//! section: one that asks for its guard waits ([`Boundary::hold`]) until the handoff is refused
//! (the item goes on) or commits (this process ends, and the item with it, never having entered
//! its section), and at most the bound the handoff gave its latch. An item that waited checked
//! what it acts on before the wait, so it checks again before it enters: `hold` takes those
//! checks, and runs them after any wait.
//!
//! A test makes a registry of its own ([`Boundary::new`]) and hands it to the item through the
//! item's port, so tests that latch a handoff never hold up another test's items.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::ThreadId;
// `aterm_time`'s clock, which a browser's wasm has too (`std::time::Instant::now` panics there).
use aterm_time::{Duration, Instant};

/// An item that cannot be repeated safely, so it holds a boundary guard across its critical
/// section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardedItem {
    /// The Accessibility click sequence that signs Messages or FaceTime out.
    SignOutSequence,
    /// A clone's publish rename (the staged checkout renamed onto `~/<name>`, and its staging
    /// removed).
    ClonePublishRename,
    /// A `launchctl bootstrap` of a vault job, with the bootout and kickstart around it.
    LaunchdBootstrap,
    /// An admitted harness image move or fixed child restart, through its kernel outcome.
    HarnessNativeMutation,
}

/// The pending handoff an item waits for ([`Boundary::latch_handoff`]).
#[derive(Clone, Copy)]
struct Pending {
    /// Its latch's number, so a latch released late never releases a later one.
    serial: u64,
    /// The end of its bound: an item waits no longer than this.
    until: Instant,
    /// The thread that runs the handoff: an item there never waits, since the handoff could not
    /// go on while it waited.
    owner: ThreadId,
}

/// The items holding a guard and the handoff pending. The count lets nested critical sections
/// of one kind release independently.
struct Registry {
    held: Vec<(GuardedItem, u32)>,
    pending: Option<Pending>,
    /// The latches taken so far.
    serial: u64,
    /// The items waiting for the pending handoff now.
    waiting: u32,
}

/// One registry of boundary guards and the handoff pending over them. The handoff and the
/// items it waits on run in one process, so the process has one ([`Self::process`]); a test
/// makes its own ([`Self::new`]).
pub struct Boundary {
    registry: Mutex<Registry>,
    /// Rung each time a handoff's latch is released, so a waiting item looks again.
    released: Condvar,
}

impl Default for Boundary {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod native_boundary_tests {
    use super::*;

    #[test]
    fn native_admission_never_parks_behind_an_app_handoff_or_cuts_one_in_half() {
        let boundary = Boundary::new();
        let latch = boundary.latch_handoff(Duration::from_secs(60));
        assert!(
            boundary
                .try_hold(GuardedItem::HarnessNativeMutation)
                .is_none()
        );
        drop(latch);
        let native = boundary
            .try_hold(GuardedItem::HarnessNativeMutation)
            .unwrap();
        assert_eq!(
            boundary.any_held(),
            Some(GuardedItem::HarnessNativeMutation)
        );
        let latch = boundary.latch_handoff(Duration::from_secs(60));
        assert!(boundary.is_held(GuardedItem::HarnessNativeMutation));
        drop(native);
        assert!(!boundary.is_held(GuardedItem::HarnessNativeMutation));
        drop(latch);
    }
}

impl std::fmt::Debug for Boundary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let registry = self.lock();
        formatter
            .debug_struct("Boundary")
            .field("held", &registry.held)
            .field("pending", &registry.pending.is_some())
            .field("waiting", &registry.waiting)
            .finish()
    }
}

/// This process's registry, which the update handoff reads.
static PROCESS: Boundary = Boundary::new();

impl Boundary {
    /// Take a boundary without waiting for a pending handoff. This briefly holds only the
    /// registry mutex and invokes no callback, so a native constructor may use it at its
    /// final metadata/table admission boundary. A pending handoff refuses admission.
    #[must_use]
    pub fn try_hold(&self, item: GuardedItem) -> Option<Held<'_>> {
        let mut registry = self.lock();
        if registry
            .pending
            .is_some_and(|pending| Instant::now() < pending.until)
        {
            return None;
        }
        match registry.held.iter_mut().find(|(kind, _)| *kind == item) {
            Some((_, count)) => *count += 1,
            None => registry.held.push((item, 1)),
        }
        Some(Held {
            boundary: self,
            item,
        })
    }

    /// A registry with nothing held and no handoff pending.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            registry: Mutex::new(Registry {
                held: Vec::new(),
                pending: None,
                serial: 0,
                waiting: 0,
            }),
            released: Condvar::new(),
        }
    }

    /// This process's registry: the one the update handoff reads, and every item's outside a
    /// test.
    #[must_use]
    pub fn process() -> &'static Self {
        &PROCESS
    }

    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hold a boundary guard for `item` across a critical section that cannot be repeated
    /// safely. The guard releases when the returned [`Held`] drops; while it is held, an
    /// update's handoff is refused. While a handoff is pending ([`Self::latch_handoff`]) on
    /// another thread, this first waits for it to commit or be refused, at most the bound its
    /// latch gave; after any such wait it runs `recheck`, the checks the item made before it
    /// asked, since what they read may have changed meanwhile, and an `Err` from them is the
    /// item's to stand aside with: no guard is taken. With no wait `recheck` never runs.
    ///
    /// # Errors
    /// `recheck`'s, after a wait.
    pub fn hold<E>(
        &self,
        item: GuardedItem,
        mut recheck: impl FnMut() -> Result<(), E>,
    ) -> Result<Held<'_>, E> {
        let me = std::thread::current().id();
        loop {
            let mut registry = self.lock();
            let mut waited = false;
            while let Some(pending) = registry.pending.filter(|pending| pending.owner != me) {
                let now = Instant::now();
                if now >= pending.until {
                    break;
                }
                waited = true;
                registry.waiting += 1;
                registry = self
                    .released
                    .wait_timeout(registry, pending.until.duration_since(now))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
                registry.waiting -= 1;
            }
            if !waited {
                match registry.held.iter_mut().find(|(kind, _)| *kind == item) {
                    Some((_, count)) => *count += 1,
                    None => registry.held.push((item, 1)),
                }
                return Ok(Held {
                    boundary: self,
                    item,
                });
            }
            // The checks run outside the registry's lock: they read the Mac, and a handoff that
            // latches meanwhile is waited for again on the next turn, then checked for again.
            drop(registry);
            recheck()?;
        }
    }

    /// An item at a boundary now, if any (the first that took the guard).
    #[must_use]
    pub fn any_held(&self) -> Option<GuardedItem> {
        self.lock().held.first().map(|(item, _)| *item)
    }

    /// Whether `item` holds the guard now.
    #[must_use]
    pub fn is_held(&self, item: GuardedItem) -> bool {
        self.lock().held.iter().any(|(kind, _)| *kind == item)
    }

    /// An update's handoff is pending on this thread from now until the returned
    /// [`HandoffLatch`] drops, at most `bound`: no item enters its critical section meanwhile
    /// ([`Self::hold`] waits). The handoff takes it before it reads the guard, so no item takes
    /// the guard between that read and the latch, and drops it when it is refused or abandoned,
    /// a panic's unwinding included; a Commit ends the process with it held, and an item still
    /// waiting ends with the process, never having entered its section.
    pub fn latch_handoff(&self, bound: Duration) -> HandoffLatch<'_> {
        let mut registry = self.lock();
        registry.serial = registry.serial.wrapping_add(1);
        let serial = registry.serial;
        registry.pending = Some(Pending {
            serial,
            until: Instant::now() + bound,
            owner: std::thread::current().id(),
        });
        HandoffLatch {
            boundary: self,
            serial,
        }
    }

    /// Whether an update's handoff is pending now ([`Self::latch_handoff`]).
    #[must_use]
    pub fn handoff_pending(&self) -> bool {
        self.lock().pending.is_some()
    }

    /// How many items wait for the pending handoff now ([`Self::hold`]).
    #[must_use]
    pub fn waiting(&self) -> u32 {
        self.lock().waiting
    }
}

/// A held boundary guard; releasing it is dropping it.
#[derive(Debug)]
#[must_use = "the guard releases as soon as this is dropped"]
pub struct Held<'a> {
    boundary: &'a Boundary,
    item: GuardedItem,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        let mut registry = self.boundary.lock();
        if let Some(at) = registry
            .held
            .iter()
            .position(|(kind, _)| *kind == self.item)
        {
            registry.held[at].1 = registry.held[at].1.saturating_sub(1);
            if registry.held[at].1 == 0 {
                registry.held.remove(at);
            }
        }
    }
}

/// A pending handoff's latch ([`Boundary::latch_handoff`]); releasing it is dropping it.
#[derive(Debug)]
#[must_use = "the latch releases as soon as this is dropped"]
pub struct HandoffLatch<'a> {
    boundary: &'a Boundary,
    serial: u64,
}

impl Drop for HandoffLatch<'_> {
    fn drop(&mut self) {
        let mut registry = self.boundary.lock();
        if registry
            .pending
            .is_some_and(|pending| pending.serial == self.serial)
        {
            registry.pending = None;
        }
        drop(registry);
        self.boundary.released.notify_all();
    }
}

/// This process's [`Boundary::any_held`].
#[must_use]
pub fn any_held() -> Option<GuardedItem> {
    Boundary::process().any_held()
}

/// This process's [`Boundary::is_held`].
#[must_use]
pub fn is_held(item: GuardedItem) -> bool {
    Boundary::process().is_held(item)
}

/// This process's [`Boundary::latch_handoff`].
pub fn latch_handoff(bound: Duration) -> HandoffLatch<'static> {
    Boundary::process().latch_handoff(bound)
}

/// This process's [`Boundary::handoff_pending`].
#[must_use]
pub fn handoff_pending() -> bool {
    Boundary::process().handoff_pending()
}

/// This process's [`Boundary::waiting`].
#[must_use]
pub fn waiting() -> u32 {
    Boundary::process().waiting()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::mpsc;

    /// The hang detector for what must happen: a minute, never a latency budget.
    const MUST_HAPPEN: Duration = Duration::from_secs(60);

    /// Wait, at most [`MUST_HAPPEN`], until `holds` does.
    fn until(what: &str, holds: impl Fn() -> bool) {
        let deadline = Instant::now() + MUST_HAPPEN;
        while !holds() {
            assert!(Instant::now() < deadline, "{what} never happened");
            aterm_uds::deadline::poll_yield();
        }
    }

    /// Checks that pass, and never run.
    fn unchecked() -> Result<(), ()> {
        Ok(())
    }

    /// A clone's publish rename that asks for its guard while a handoff is pending does not
    /// enter its critical section: it waits, holding nothing, until the handoff is refused, then
    /// checks again and goes on once. Before the latch it took the guard at once, after the
    /// handoff had read the guard free, and the Commit's `_exit` could cut it in half.
    #[test]
    fn an_item_waits_for_a_pending_handoff_and_goes_on_once_it_is_refused() {
        let boundary = Boundary::new();
        let latch = boundary.latch_handoff(MUST_HAPPEN);
        assert!(boundary.handoff_pending());
        let checked = AtomicU32::new(0);
        let (ran, finished) = mpsc::channel();
        std::thread::scope(|scope| {
            let item = scope.spawn(|| {
                let _guard = boundary
                    .hold(GuardedItem::ClonePublishRename, || {
                        checked.fetch_add(1, Ordering::SeqCst);
                        Ok::<(), ()>(())
                    })
                    .expect("its checks pass");
                ran.send(()).unwrap();
            });
            until("the rename waiting for the handoff", || {
                boundary.waiting() == 1
            });
            assert!(
                !boundary.is_held(GuardedItem::ClonePublishRename),
                "nothing is in its critical section while the handoff is pending"
            );
            assert!(finished.try_recv().is_err(), "the rename has not run");
            // The handoff is refused: its latch drops, and the rename checks again and goes on.
            drop(latch);
            assert!(!boundary.handoff_pending());
            finished
                .recv_timeout(MUST_HAPPEN)
                .expect("the rename runs once the handoff is refused");
            item.join().unwrap();
        });
        assert!(finished.try_recv().is_err(), "once");
        assert_eq!(checked.load(Ordering::SeqCst), 1, "checked again once");
        assert_eq!(boundary.waiting(), 0);
        assert_eq!(boundary.any_held(), None);
    }

    /// An item that waited out a handoff checks again what it acts on before it enters its
    /// section: what its checks read before the wait may have changed meanwhile (the person
    /// walked away, the vault changed, a policy turned off). A check that now fails stands the
    /// item aside, holding nothing; with no wait the checks never run, so nothing is read twice.
    /// Before, the item entered its section on checks made before a wait of up to eight minutes.
    #[test]
    fn an_item_that_waited_for_a_handoff_checks_again_and_stands_aside_on_a_failed_check() {
        let boundary = Boundary::new();
        let gone = AtomicBool::new(false);
        let recheck = || {
            if gone.load(Ordering::SeqCst) {
                Err("the person walked away while the handoff was pending")
            } else {
                Ok(())
            }
        };
        // No handoff pending: no wait, so no check runs again.
        gone.store(true, Ordering::SeqCst);
        let guard = boundary
            .hold(GuardedItem::SignOutSequence, recheck)
            .expect("no wait, no check again");
        drop(guard);
        gone.store(false, Ordering::SeqCst);
        let latch = boundary.latch_handoff(MUST_HAPPEN);
        let held = std::thread::scope(|scope| {
            let item = scope.spawn(|| {
                boundary
                    .hold(GuardedItem::SignOutSequence, recheck)
                    .map(|_guard| ())
            });
            until("the sign-out waiting for the handoff", || {
                boundary.waiting() == 1
            });
            gone.store(true, Ordering::SeqCst);
            drop(latch);
            item.join().unwrap()
        });
        assert_eq!(
            held,
            Err("the person walked away while the handoff was pending"),
            "the item stood aside on its check after the wait"
        );
        assert_eq!(boundary.any_held(), None, "it took no guard");
        assert_eq!(boundary.waiting(), 0);
    }

    /// The wait is bounded by the handoff's own bound: past it, the item goes on (and the
    /// handoff's Commit reads the guard again). On the handoff's own thread nothing waits, since
    /// the handoff could not go on meanwhile.
    #[test]
    fn the_wait_ends_at_the_handoffs_bound_and_never_on_its_own_thread() {
        let boundary = Boundary::new();
        {
            let _spent = boundary.latch_handoff(Duration::ZERO);
            std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        let _guard = boundary
                            .hold(GuardedItem::SignOutSequence, unchecked)
                            .expect("past the bound it goes on");
                    })
                    .join()
                    .unwrap();
            });
        }
        let _latch = boundary.latch_handoff(MUST_HAPPEN);
        let guard = boundary
            .hold(GuardedItem::LaunchdBootstrap, unchecked)
            .expect("the handoff's own thread never waits");
        assert_eq!(boundary.any_held(), Some(GuardedItem::LaunchdBootstrap));
        drop(guard);
        assert_eq!(boundary.waiting(), 0);
    }

    /// A handoff that panics releases its latch as the panic unwinds, so no item waits for a
    /// handoff that is gone; a later latch is never released by an earlier one's drop.
    #[test]
    fn a_panicking_handoff_releases_its_latch() {
        let boundary = Boundary::new();
        std::thread::scope(|scope| {
            let handoff = scope.spawn(|| {
                let _latch = boundary.latch_handoff(MUST_HAPPEN);
                panic!("injected: the handoff panics");
            });
            assert!(handoff.join().is_err());
        });
        assert!(!boundary.handoff_pending(), "the unwinding released it");
        let earlier = boundary.latch_handoff(MUST_HAPPEN);
        let later = boundary.latch_handoff(MUST_HAPPEN);
        drop(earlier);
        assert!(
            boundary.handoff_pending(),
            "the later handoff is still pending"
        );
        drop(later);
        assert!(!boundary.handoff_pending());
    }

    /// Holding a guard shows in `any_held` until it drops; a nested hold of the same kind keeps
    /// it held until the last releases. Registries are separate: a test's is not the process's.
    #[test]
    fn a_held_guard_shows_until_the_last_holder_drops() {
        let boundary = Boundary::new();
        assert_eq!(boundary.any_held(), None);
        let outer = boundary
            .hold(GuardedItem::ClonePublishRename, unchecked)
            .unwrap();
        assert_eq!(boundary.any_held(), Some(GuardedItem::ClonePublishRename));
        let inner = boundary
            .hold(GuardedItem::ClonePublishRename, unchecked)
            .unwrap();
        drop(inner);
        assert_eq!(
            boundary.any_held(),
            Some(GuardedItem::ClonePublishRename),
            "the outer still holds"
        );
        assert!(
            !std::ptr::eq(&boundary, Boundary::process()),
            "a test's registry is its own"
        );
        drop(outer);
        assert_eq!(
            boundary.any_held(),
            None,
            "released when the last holder drops"
        );
    }
}
