// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ownership witnesses for title membership, independent of windows and terminal payloads.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

#[cfg(any(test, debug_assertions))]
thread_local! {
    static OMIT_MEMBERSHIP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static REPLACE_BINDINGS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Exercise missing intrinsic membership invalidation through real mutations.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn with_title_membership_fault<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            OMIT_MEMBERSHIP.with(|active| active.set(self.0));
        }
    }
    let restore = Restore(OMIT_MEMBERSHIP.with(|active| active.replace(true)));
    let result = run();
    drop(restore);
    result
}

/// Exercise the rejected destructive replacement of a live canonical subscription.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn with_title_binding_replacement_fault<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            REPLACE_BINDINGS.with(|flag| flag.set(self.0));
        }
    }
    let restore = Restore(REPLACE_BINDINGS.with(|flag| flag.replace(true)));
    let result = run();
    drop(restore);
    result
}

#[derive(Debug, Default)]
struct Authority {
    revision: AtomicUsize,
    topology: AtomicUsize,
    pending: Mutex<BTreeSet<u64>>,
}

fn advance(revision: &AtomicUsize) {
    // Saturation permanently disables certificates rather than allowing an ABA at overflow.
    let _ = revision.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1));
}

/// An aggregate invalidation witness and coalesced owner obligations, never a membership index.
#[derive(Debug, Default)]
pub struct TitleAuthority(Arc<Authority>);

/// Identity and generation of one aggregate, including replacement of its entire root.
#[derive(Clone, Debug)]
pub struct TitleRevision {
    authority: Arc<Authority>,
    observed: usize,
    topology: bool,
}

impl TitleRevision {
    /// Whether this certificate still belongs to the current, unchanged authority.
    #[must_use]
    pub fn is_current(&self, authority: &TitleAuthority) -> bool {
        let current = if self.topology {
            authority.0.topology.load(Ordering::Relaxed)
        } else {
            authority.0.revision.load(Ordering::Relaxed)
        };
        Arc::ptr_eq(&self.authority, &authority.0)
            && current != usize::MAX
            && current == self.observed
    }
}

impl TitleAuthority {
    /// Snapshot all membership and delivery invalidations.
    #[must_use]
    pub fn revision(&self) -> TitleRevision {
        self.snapshot(false)
    }

    /// Snapshot topology independently of successful delivery writes.
    #[must_use]
    pub fn topology_revision(&self) -> TitleRevision {
        self.snapshot(true)
    }

    fn snapshot(&self, topology: bool) -> TitleRevision {
        TitleRevision {
            authority: Arc::clone(&self.0),
            observed: if topology {
                &self.0.topology
            } else {
                &self.0.revision
            }
            .load(Ordering::Relaxed),
            topology,
        }
    }

    /// Take only the coalesced dirty owners; reads do not invalidate anything.
    pub fn take_pending(&mut self) -> BTreeSet<u64> {
        std::mem::take(&mut *self.0.pending.lock().unwrap_or_else(|p| p.into_inner()))
    }

    /// Keep an incomplete delivery owed after a contended reconciliation.
    pub fn owe(&self, owner: u64) {
        advance(&self.0.revision);
        self.0
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(owner);
    }
}

#[derive(Debug, Default)]
struct Owner {
    bindings: Mutex<Vec<Weak<Binding>>>,
}

#[derive(Debug)]
struct Binding {
    owner: u64,
    authority: Weak<Authority>,
}

impl Binding {
    fn changed(&self, topology: bool, owed: bool) {
        if let Some(authority) = self.authority.upgrade() {
            advance(&authority.revision);
            if topology {
                advance(&authority.topology);
            }
            if owed {
                authority
                    .pending
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(self.owner);
            }
        }
    }
}

/// A shared observation identifies a payload; it cannot bind it or revoke custody.
///
/// ```compile_fail
/// use aterm_gui_tabs::tab_model::{TitleOwner, TitleAuthority};
/// let payload = TitleOwner::default();
/// let shared = payload.identity();
/// shared.bind(0, &TitleAuthority::default());
/// ```
///
/// ```compile_fail
/// use aterm_gui_tabs::tab_model::{TitleOwner, TitleAuthority};
/// let payload = TitleOwner::default();
/// let shared = payload.identity();
/// shared.unbind(0, &TitleAuthority::default());
/// ```
#[derive(Clone, Debug)]
pub struct TitleIdentity(Arc<Owner>);

impl TitleIdentity {
    /// Compare payload identities without inspecting membership or owning a binding.
    #[must_use]
    pub fn same_identity(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Unique invalidation subject stored in a payload, never returned through a shared accessor.
#[derive(Debug, Default)]
pub struct TitleOwner(Arc<Owner>);

/// Non-cloneable subscription custody, retained outside a window's mutable payload projection.
/// Dropping an old incarnation can revoke only that subscription. Overlapping move guards may
/// temporarily subscribe the same subject to both owners; neither abandons the other.
#[derive(Debug)]
pub struct TitleLease {
    subject: TitleIdentity,
    binding: Arc<Binding>,
}

impl TitleLease {
    /// Compare custody's subject with the current payload without scanning its contents.
    #[must_use]
    pub fn same_identity(&self, subject: &TitleIdentity) -> bool {
        self.subject.same_identity(subject)
    }
}

impl Drop for TitleLease {
    fn drop(&mut self) {
        self.binding.changed(true, true);
    }
}

impl TitleOwner {
    /// A read-only identity cannot recreate this unique subject or any subscription capability.
    #[must_use]
    pub fn identity(&self) -> TitleIdentity {
        TitleIdentity(Arc::clone(&self.0))
    }

    /// Issue one independent custody capability from a mutable payload boundary. Existing
    /// canonical subscriptions stay live until their own capabilities are dropped. This supports
    /// carries and swaps even when the previous owner guard has not dropped yet.
    #[must_use]
    pub fn bind(&mut self, owner: u64, authority: &TitleAuthority) -> TitleLease {
        let binding = Arc::new(Binding {
            owner,
            authority: Arc::downgrade(&authority.0),
        });
        let mut bindings = self.0.bindings.lock().unwrap_or_else(|p| p.into_inner());
        #[cfg(any(test, debug_assertions))]
        if REPLACE_BINDINGS.with(std::cell::Cell::get) {
            bindings.clear();
        }
        bindings.retain(|old| old.strong_count() != 0);
        bindings.push(Arc::downgrade(&binding));
        binding.changed(true, true);
        TitleLease {
            subject: self.identity(),
            binding,
        }
    }

    fn changed(&self, topology: bool, owed: bool) {
        self.0
            .bindings
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|weak| {
                if let Some(binding) = weak.upgrade() {
                    binding.changed(topology, owed);
                    true
                } else {
                    false
                }
            });
    }

    /// Invalidate before a membership mutation or mutable topology loan can escape.
    pub fn membership_changed(&self) {
        #[cfg(any(test, debug_assertions))]
        if OMIT_MEMBERSHIP.with(std::cell::Cell::get) {
            return;
        }
        self.changed(true, true);
    }

    /// Successful delivery invalidates certificates without creating topology work.
    pub fn delivered(&self) {
        self.changed(false, false);
    }

    /// Removing delivery evidence creates an owed refresh, independently of topology.
    pub fn delivery_removed(&self) {
        self.changed(false, true);
    }
}
