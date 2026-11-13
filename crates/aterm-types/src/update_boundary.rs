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
//! item's port, so tests that latch a handoff never hold up another test's items. An item no port
//! reaches holds this process's, and aterm-gui's tests read another for their handoffs, which keeps
//! that item and those handoffs apart the same way.

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
    /// An admitted harness native mutation (an image move, a fixed child restart, a Codex exec
    /// handoff or its cancellation's settlement, an account claim, fence or closure), through its
    /// kernel outcome and the durable record of it, or until it can start nothing more: a
    /// signalled restart past its deadline, or an image move whose actor is gone, stops holding
    /// it, and a record it still makes takes a fresh hold.
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
#[path = "update_boundary_native_boundary_tests.rs"]
mod native_boundary_tests;

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

    /// This process's registry: the one every item holds and, in a running aterm, the one the
    /// update handoff reads.
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

#[cfg(test)]
#[path = "update_boundary_tests.rs"]
mod tests;
