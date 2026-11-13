// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A value one thread publishes and any thread reads without waiting.
//!
//! [`Published<T>`] holds an `Arc<T>` and a revision behind one lock that is only ever held for
//! a pointer swap, so a reader that paints or answers a keystroke takes it with `try_lock`
//! ([`Published::try_current`]): a contended read answers `None` and the reader keeps the `Arc`
//! it already has. The revision moves only when the published value differs from the one held
//! ([`Published::publish`]), so it is a frame key: equal revisions are equal values, and a
//! publisher that folds the same inputs again wakes nobody.
//!
//! Each read sets an `asked` flag the publisher takes ([`Published::take_asked`]): a producer
//! that reads only while somebody looks starts and paces itself by it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, TryLockError};

/// What a read of a [`Published`] value gives: the value and the revision it was published at.
#[derive(Debug)]
pub struct Current<T> {
    /// 0 for the first value; one more for each published value that differed from the last.
    pub revision: u64,
    /// The value, shared with every other reader.
    pub value: Arc<T>,
}

impl<T> Clone for Current<T> {
    fn clone(&self) -> Self {
        Self {
            revision: self.revision,
            value: Arc::clone(&self.value),
        }
    }
}

struct Shared<T> {
    held: Mutex<Current<T>>,
    asked: AtomicBool,
}

/// A published value: cloned handles read and publish the same one.
pub struct Published<T>(Arc<Shared<T>>);

impl<T> Clone for Published<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Published<T> {
    /// A publication holding `first` at revision 0.
    pub fn new(first: T) -> Self {
        Self(Arc::new(Shared {
            held: Mutex::new(Current {
                revision: 0,
                value: Arc::new(first),
            }),
            asked: AtomicBool::new(false),
        }))
    }

    /// The value and its revision, without waiting: `None` while another thread holds the lock
    /// for its swap, which is no new value, so the caller keeps what it has. Every call, busy or
    /// not, is an ask ([`Self::take_asked`]).
    pub fn try_current(&self) -> Option<Current<T>> {
        self.0.asked.store(true, Ordering::Release);
        match self.0.held.try_lock() {
            Ok(held) => Some(held.clone()),
            Err(TryLockError::Poisoned(error)) => Some(error.into_inner().clone()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    /// The value and its revision, waiting out a swap: for a worker thread, a command or a
    /// test, never a painting path. It is no ask.
    pub fn current(&self) -> Current<T> {
        self.0
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Whether any reader asked since the last call, clearing the flag.
    pub fn take_asked(&self) -> bool {
        self.0.asked.swap(false, Ordering::AcqRel)
    }
}

impl<T: PartialEq> Published<T> {
    /// Publish `next`: where it differs from the value held, it replaces it and the revision
    /// moves by one; an equal value is dropped and the revision stands. Returns whether it
    /// moved. The comparison runs outside the lock, so no reader is refused for its length; a
    /// value another publisher swapped in meanwhile is compared again.
    pub fn publish(&self, next: T) -> bool {
        let lock = || {
            self.0
                .held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        };
        let mut seen = lock().clone();
        loop {
            if *seen.value == next {
                return false;
            }
            let mut held = lock();
            if held.revision == seen.revision {
                *held = Current {
                    revision: seen.revision.saturating_add(1),
                    value: Arc::new(next),
                };
                return true;
            }
            seen = held.clone();
        }
    }
}

#[cfg(test)]
#[path = "publish_tests.rs"]
mod tests;
