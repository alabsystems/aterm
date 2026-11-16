// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! One lock order for every session: the fabric's Inbox, then a session's metadata, then its
//! timeline (`task_record::mark_session_task_with_context` takes them in that order, holding the
//! Inbox through [`crate::SessionFabric::run_unfenced`]). A thread that waits on an Inbox while it
//! holds metadata deadlocks against a task mark, which holds the Inbox and waits on metadata. The
//! order is class-wide: holding one session's metadata while waiting on another session's Inbox
//! closes the same cycle through two task marks.
//!
//! [`MetaLock`] is a session's metadata mutex. In a build with debug assertions it counts the
//! guards each thread holds, and an Inbox acquisition beneath one panics, naming the order,
//! before it can block. A release build counts nothing.
//!
//! The lock-order census reads one call deep and never through a receiver whose type it cannot
//! name, so an Inbox taken beneath metadata through a method of a local, or of a field whose type
//! another crate defines, is invisible to it; this records the order the code actually takes.
//!
//! A session's hold is not read through the Inbox: [`crate::SessionFabric::holds`] and
//! [`crate::SessionFabric::hold`] read a copy each change stores under the Inbox, a leaf lock, so
//! they are read beneath metadata in any build.

use std::cell::Cell;
use std::sync::{LockResult, MutexGuard, PoisonError, TryLockError, TryLockResult};

thread_local! {
    /// The metadata guards this thread holds ([`MetaGuard`]); counted only with debug assertions.
    static META_HELD: Cell<usize> = const { Cell::new(0) };
}

/// A session's metadata mutex, ranked after every session's fabric Inbox.
#[derive(Debug, Default)]
pub struct MetaLock<T> {
    inner: std::sync::Mutex<T>,
}

/// A held [`MetaLock`]: the metadata, and this thread's count of held metadata guards.
#[derive(Debug)]
pub struct MetaGuard<'a, T> {
    guard: MutexGuard<'a, T>,
    _held: Held,
}

impl<T> MetaLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: std::sync::Mutex::new(value),
        }
    }

    /// Block for the metadata, as [`std::sync::Mutex::lock`] does.
    ///
    /// # Errors
    /// The guard, poisoned, when a holder panicked; `into_inner` recovers it.
    pub fn lock(&self) -> LockResult<MetaGuard<'_, T>> {
        match self.inner.lock() {
            Ok(guard) => Ok(MetaGuard::held(guard)),
            Err(poisoned) => Err(PoisonError::new(MetaGuard::held(poisoned.into_inner()))),
        }
    }

    /// Take the metadata only if no other holder has it, as [`std::sync::Mutex::try_lock`] does.
    ///
    /// # Errors
    /// `WouldBlock` while another thread holds it; the guard, poisoned, when a holder panicked.
    pub fn try_lock(&self) -> TryLockResult<MetaGuard<'_, T>> {
        match self.inner.try_lock() {
            Ok(guard) => Ok(MetaGuard::held(guard)),
            Err(TryLockError::Poisoned(poisoned)) => Err(TryLockError::Poisoned(PoisonError::new(
                MetaGuard::held(poisoned.into_inner()),
            ))),
            Err(TryLockError::WouldBlock) => Err(TryLockError::WouldBlock),
        }
    }
}

impl<'a, T> MetaGuard<'a, T> {
    fn held(guard: MutexGuard<'a, T>) -> Self {
        Self {
            guard,
            _held: Held::enter(),
        }
    }
}

impl<T> std::ops::Deref for MetaGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> std::ops::DerefMut for MetaGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

/// One metadata guard on this thread's count. A guard never leaves its thread (`MutexGuard` is
/// not `Send`), so the count is exact.
#[derive(Debug)]
struct Held;

impl Held {
    fn enter() -> Self {
        if cfg!(debug_assertions) {
            META_HELD.with(|held| held.set(held.get() + 1));
        }
        Self
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if cfg!(debug_assertions) {
            META_HELD.with(|held| held.set(held.get() - 1));
        }
    }
}

/// Called before this thread waits on a session's Inbox.
///
/// # Panics
/// With debug assertions, when this thread holds a session's metadata: read the fabric before
/// taking metadata, or take both in the order above.
pub(crate) fn before_inbox() {
    debug_assert!(
        META_HELD.with(Cell::get) == 0,
        "lock order: a session's fabric Inbox is taken while this thread holds a session's \
         metadata; the order is Inbox, then metadata, then timeline, so read the fabric before \
         taking metadata"
    );
}

#[cfg(test)]
#[path = "lock_rank_tests.rs"]
mod tests;
