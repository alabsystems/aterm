// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use aterm_agent::harness::relaunch_type::RunLineRefusal;

use crate::session_timeline::{MetaLock, Resume};

/// What a tapped tab's relaunch did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tapped {
    pub(crate) paused: Vec<Duration>,
    pub(crate) typed: Vec<Vec<String>>,
}

static TAPS: Mutex<Vec<(String, Tapped)>> = Mutex::new(Vec::new());

/// Tap tab `sid`.
pub(crate) fn on(sid: &str) {
    TAPS.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((sid.to_string(), Tapped::default()));
}

/// What tab `sid`'s relaunch did so far, `None` where it is not tapped.
pub(crate) fn read(sid: &str) -> Option<Tapped> {
    TAPS.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(tapped, _)| tapped == sid)
        .map(|(_, tapped)| tapped.clone())
}

fn with<T>(sid: &str, f: impl FnOnce(&mut Tapped) -> T) -> Option<T> {
    TAPS.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter_mut()
        .find(|(tapped, _)| tapped == sid)
        .map(|(_, tapped)| f(tapped))
}

pub(super) fn paused(sid: &str, pause: Duration, stands: &dyn Fn() -> bool) -> Option<bool> {
    with(sid, |tapped| tapped.paused.push(pause)).map(|()| stands())
}

/// A stand-in for the live snapshot read ([`super::child_kept`]).
pub(crate) type Reader = Arc<dyn Fn(&str, u32) -> Option<Resume> + Send + Sync>;

/// The sessions whose kept children [`super::child_kept`] reads, each by its metadata lock
/// (held weakly, so no other session takes its address), with the stand-in that reads them.
static READERS: Mutex<Vec<(Weak<MetaLock>, Reader)>> = Mutex::new(Vec::new());

/// Read the kept children of the session holding `meta` through `read` ([`super::child_kept`]
/// with the kernel stand-in that reads each child at its birth): a test build reads no live
/// snapshot.
pub(crate) fn reads(meta: &Arc<MetaLock>, read: Reader) {
    READERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((Arc::downgrade(meta), read));
}

/// The stand-in that reads the kept children of the session holding `meta`, if any.
pub(super) fn reader(meta: &Arc<MetaLock>) -> Option<Reader> {
    READERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(held, _)| held.as_ptr() == Arc::as_ptr(meta))
        .map(|(_, read)| Arc::clone(read))
}

pub(super) fn typed(sid: &str, words: &[String]) -> Option<Result<(), RunLineRefusal>> {
    with(sid, |tapped| {
        tapped.typed.push(words.to_vec());
        Ok(())
    })
}
