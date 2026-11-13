// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Listeners for one custody's appends, beside the process's sink: each hears the durable
//! appends of the table stores whose claims live in the claim directory it named, made by any
//! thread of this process, and no other custody's. An append by another process reaches no
//! listener here.
//!
//! A listener runs where the sink runs, under the control journal lock and the table flock, so
//! telling one never waits: a slot whose own listener is registering or dropping is passed
//! over, as an append made a moment before that listener came or after it went. A registration
//! takes a slot by its claim ([`CLAIMED`]) before it locks it, so it locks only a slot that
//! holds no listener: no append is kept from a listener that hears by another's coming.

use super::Appended;
use std::{
    path::{Path, PathBuf},
    sync::{
        PoisonError, RwLock, TryLockError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

/// The most listeners at once ([`listen`]): a process serves one custody, and a test binary one
/// for each runtime its tests run together.
const SLOTS: usize = 256;

struct Listener {
    claim_dir: PathBuf,
    hear: Box<dyn Fn(&Appended<'_>) + Send + Sync>,
}

static LISTENERS: [RwLock<Option<Listener>>; SLOTS] = [const { RwLock::new(None) }; SLOTS];
/// Which slots are taken: a registration claims one here ([`listen`]) and alone writes it
/// until its [`Listening`] drops, which empties the slot and then gives the claim back.
static CLAIMED: [AtomicBool; SLOTS] = [const { AtomicBool::new(false) }; SLOTS];
/// One past the highest slot a listener ever took: an append reads the slots below it.
static USED: AtomicUsize = AtomicUsize::new(0);

/// A listener's registration ([`listen`]): it hears until this drops.
#[must_use = "the listener hears only while this lives"]
#[derive(Debug)]
pub(crate) struct Listening {
    slot: usize,
}

impl Drop for Listening {
    fn drop(&mut self) {
        // Waits for the appends telling this listener now, each a few stores long.
        let cell = &LISTENERS[self.slot];
        *cell.write().unwrap_or_else(PoisonError::into_inner) = None;
        CLAIMED[self.slot].store(false, Ordering::Release);
    }
}

/// Have `hear` told of each durable append to a table store whose claim lives in `claim_dir`
/// (`claim_dir_of` a custody), made by any thread of this process from now until the
/// registration drops. `None` where [`SLOTS`] listeners hear already.
///
/// `hear` runs under the control journal lock and the table flock, as the process's sink does
/// ([`super::set_append_sink`]): it must not block, take a lock or allocate.
pub(crate) fn listen(
    claim_dir: PathBuf,
    hear: impl Fn(&Appended<'_>) + Send + Sync + 'static,
) -> Option<Listening> {
    // The first slot no registration claims. Only its claimant writes a slot, and a claimed
    // slot holds no listener until that write, so the lock below is never one a listener's
    // appends read it under: it waits at most for an append reading the empty slot, a few
    // instructions long.
    let slot = CLAIMED.iter().position(|claimed| {
        claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    })?;
    let cell = &LISTENERS[slot];
    *cell.write().unwrap_or_else(PoisonError::into_inner) = Some(Listener {
        claim_dir,
        hear: Box::new(hear),
    });
    USED.fetch_max(slot + 1, Ordering::AcqRel);
    Some(Listening { slot })
}

/// Tell each listener for `claim_dir` of `record`. Never waits: it reads each slot only where
/// that slot's own registration, coming or going, does not hold it.
pub(super) fn tell(record: &Appended<'_>, claim_dir: &Path) {
    for cell in &LISTENERS[..USED.load(Ordering::Acquire)] {
        let held = match cell.try_read() {
            Ok(held) => held,
            Err(TryLockError::Poisoned(held)) => held.into_inner(),
            Err(TryLockError::WouldBlock) => continue,
        };
        if let Some(listener) = &*held
            && listener.claim_dir == claim_dir
        {
            (listener.hear)(record);
        }
    }
}
