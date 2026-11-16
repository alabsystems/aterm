// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed delivery evidence: immutable reads cannot create or erase a delivered epoch.

use crate::tab_model::{TitleAuthority, TitleIdentity, TitleLease, TitleOwner};
use std::collections::HashMap;
use std::ops::Deref;

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fault {
    Completion,
    Root,
    View,
    Delivery,
    Pending,
    Reconcile,
    GeneralSnapshot,
    TopologyRescan,
    IncomingEvidence,
    MintPending,
}

#[cfg(test)]
thread_local! {
    static FAULT: std::cell::Cell<Option<Fault>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn faulty(fault: Fault) -> bool {
    FAULT.with(|active| active.get() == Some(fault))
}

#[cfg(test)]
pub(crate) fn with_fault<T>(fault: Fault, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<Fault>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FAULT.with(|active| active.set(self.0));
        }
    }
    let restore = Restore(FAULT.with(|active| active.replace(Some(fault))));
    let result = run();
    drop(restore);
    result
}

#[cfg(test)]
type BeforePublication = Box<dyn FnOnce(&mut crate::App)>;
#[cfg(test)]
thread_local! {
    static BEFORE_PUBLICATION: std::cell::RefCell<Option<BeforePublication>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(crate) fn with_before_publication<T>(hook: BeforePublication, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<BeforePublication>);
    impl Drop for Restore {
        fn drop(&mut self) {
            BEFORE_PUBLICATION.with(|slot| {
                *slot.borrow_mut() = self.0.take();
            });
        }
    }
    let restore = Restore(BEFORE_PUBLICATION.with(|slot| slot.replace(Some(hook))));
    let result = run();
    drop(restore);
    result
}
#[cfg(test)]
pub(crate) fn before_publication(app: &mut crate::App) {
    let hook = BEFORE_PUBLICATION.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook(app);
    }
}

#[derive(Default)]
pub(crate) struct TitleEpochs {
    epochs: HashMap<u64, u64>,
    owner: TitleOwner,
}

impl TitleEpochs {
    pub(crate) fn owner(&self) -> TitleIdentity {
        self.owner.identity()
    }

    pub(crate) fn invalidate_membership_evidence(&mut self) {
        #[cfg(test)]
        if faulty(Fault::IncomingEvidence) {
            return;
        }
        if !self.epochs.is_empty() {
            self.epochs.clear();
            self.owner.delivery_removed();
        }
    }

    pub(crate) fn bind_owner(&mut self, id: u64, authority: &TitleAuthority) -> TitleLease {
        self.invalidate_membership_evidence();
        self.owner.bind(id, authority)
    }

    pub(crate) fn insert(&mut self, session: u64, epoch: u64) -> Option<u64> {
        let old = self.epochs.insert(session, epoch);
        if old != Some(epoch) {
            self.owner.delivered();
        }
        old
    }

    pub(crate) fn remove(&mut self, session: &u64) -> Option<u64> {
        let old = self.epochs.remove(session);
        if old.is_some() {
            #[cfg(test)]
            if faulty(Fault::Delivery) {
                return old;
            }
            self.owner.delivery_removed();
        }
        old
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&u64, &u64) -> bool) {
        let before = self.epochs.len();
        self.epochs.retain(|sid, epoch| keep(sid, epoch));
        if self.epochs.len() != before {
            #[cfg(test)]
            if faulty(Fault::Delivery) {
                return;
            }
            self.owner.delivery_removed();
        }
    }
}

impl Deref for TitleEpochs {
    type Target = HashMap<u64, u64>;
    fn deref(&self) -> &Self::Target {
        &self.epochs
    }
}
