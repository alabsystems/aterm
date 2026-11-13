// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `await`s this aterm's control server holds parked, each by the client process it serves:
//! the jobs pass's evidence that a quiet process waits on aterm ([`JobProc::aterm_wait`]), never a
//! guess from its name or arguments. An `aterm ctl await task …`, `aterm ctl @self await wake …`
//! and `amacs --wait` (its `await edit`) each park here while the server waits for them.
//!
//! [`JobProc::aterm_wait`]: aterm_vitals::jobs::JobProc::aterm_wait

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long past its own `timeout=` (with none, the longest any wait verb waits,
/// [`aterm_control::wire::WAIT_MAX_MS`]) an await still counts as parked: past it, the server's
/// wait has outrun what it was asked for, and the client is quiet for no reason aterm vouches for.
const GRACE: Duration = Duration::from_secs(60);

/// One parked await: the client's pid, its birth in the jobs engine's unit
/// ([`aterm_sysprobe::jobs::ProcFacts::start`]), and when it has outrun its timeout.
struct Entry {
    id: u64,
    pid: u32,
    birth: u64,
    until: Instant,
}

/// The parked awaits of one server ([`PARKED`]; a test makes its own).
pub(crate) struct Parked {
    entries: Mutex<Vec<Entry>>,
    next: AtomicU64,
}

/// This process's control server's parked awaits.
pub(crate) static PARKED: Parked = Parked::new();

impl Parked {
    pub(crate) const fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            next: AtomicU64::new(0),
        }
    }

    /// Parks an await for the process `pid` born at `birth`, outrun at `until`, until the returned
    /// guard drops.
    pub(crate) fn hold(&self, pid: u32, birth: u64, until: Instant) -> Held<'_> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Entry {
                id,
                pid,
                birth,
                until,
            });
        Held { parked: self, id }
    }

    /// Whether an await parked here at `now` is for the process `pid` born at `birth` and has not
    /// outrun its timeout: a reused pid, a birth unread and an await past its timeout are not.
    pub(crate) fn holds(&self, pid: u32, birth: Option<u64>, now: Instant) -> bool {
        let Some(birth) = birth else {
            return false;
        };
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|e| e.pid == pid && e.birth == birth && now < e.until)
    }
}

/// An await parked in [`Parked`] until dropped.
pub(crate) struct Held<'a> {
    parked: &'a Parked,
    id: u64,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.parked
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|e| e.id != self.id);
    }
}

/// Marks each of `procs` that an await parked in `parked` at `now` is for
/// ([`JobProc::aterm_wait`]), by its pid and birth.
///
/// [`JobProc::aterm_wait`]: aterm_vitals::jobs::JobProc::aterm_wait
pub(crate) fn mark(procs: &mut [aterm_vitals::jobs::JobProc], parked: &Parked, now: Instant) {
    for p in procs {
        p.aterm_wait = parked.holds(p.pid, p.start, now);
    }
}

/// Parks in [`PARKED`] the request `verb rest` while this thread serves it, when it is an `await`
/// and the connection is a direct local one whose client's birth reads; `None` otherwise.
pub(crate) fn enter(verb: &str, rest: &str) -> Option<Held<'static>> {
    enter_in(&PARKED, verb, rest)
}

/// [`enter`] into `parked`.
fn enter_in<'a>(parked: &'a Parked, verb: &str, rest: &str) -> Option<Held<'a>> {
    if verb != "await" {
        return None;
    }
    let pid = crate::control::serving_peer_pid()?;
    let birth = birth_of(pid)?;
    let wait = timeout_ms(rest).unwrap_or(aterm_control::wire::WAIT_MAX_MS);
    let until = Instant::now() + Duration::from_millis(wait) + GRACE;
    Some(parked.hold(pid, birth, until))
}

/// The process `pid`'s start as the jobs engine's reader reads it, so the pass compares like with
/// like (kernel reads only).
fn birth_of(pid: u32) -> Option<u64> {
    aterm_sysprobe::jobs::JobsReader::new()
        .read(aterm_sysprobe::jobs::ReadReq { pid, fds: false })
        .and_then(|f| f.start)
}

/// The `timeout=<ms>` (or `timeout <ms>`) an await's words give, clamped as its handler clamps it
/// ([`aterm_control::wire::wait_ms`]).
fn timeout_ms(rest: &str) -> Option<u64> {
    let mut words = rest.split_whitespace();
    while let Some(word) = words.next() {
        let value = match word.strip_prefix("timeout=") {
            Some(value) => value,
            None if word == "timeout" => words.next()?,
            None => continue,
        };
        return aterm_control::wire::wait_ms(value);
    }
    None
}

#[cfg(test)]
#[path = "parked_awaits_tests.rs"]
mod tests;
