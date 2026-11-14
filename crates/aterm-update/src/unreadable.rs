// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The announcement that this machine cannot read its release channel, and so is not updating.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::paths::Staging;

/// Repeat interval of the log warning while stranded: not spam in a week-long session, yet a
/// log captured on any day holds the explanation.
const RENOTICE_AFTER: Duration = Duration::from_secs(6 * 60 * 60);

/// Whether the last check this process completed found the channel unreadable: its own check
/// loop's latch. What the machine's last check found is the status ledger's [`NOTE`], which
/// every process reads alike.
static STRANDED: AtomicBool = AtomicBool::new(false);

/// The delivery note (`status.toml`'s `delivery`) of a check that found the channel unreadable:
/// [`fn@crate::status`] reads the strand from it, so the process that checked, a window that did
/// not and a fresh `aterm update status` all say the same, until a check by any of them records
/// another outcome.
pub(crate) const NOTE: &str = "unreadable";

/// The last time the log warning was emitted, for the [`RENOTICE_AFTER`] throttle.
fn last_warned() -> &'static Mutex<Option<Instant>> {
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

/// The most recent stranded explanation, so [`notification`] can describe the ACTUAL
/// state without re-deriving it (which would re-hit the network).
fn last_explanation() -> &'static Mutex<Option<String>> {
    static LAST: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

/// Whether the most recent check this process made found this machine unable to read its
/// channel.
#[must_use]
pub(crate) fn is_stranded() -> bool {
    STRANDED.load(Ordering::Relaxed)
}

/// Record that the channel was readable: clears the stranded latch, so a machine fixed
/// mid-session stops announcing and a later strand is warned about again.
pub(crate) fn clear() {
    if STRANDED.swap(false, Ordering::Relaxed) {
        crate::log("the release channel is readable again — this machine is receiving updates");
        *last_warned().lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Announce the stranded state: in the status ledger, marked [`NOTE`] on this one record, and in
/// this process. The mark is that record's alone, never this process's delivery note, so no later
/// record this process writes (an apply's, a refusal's, the activation of a newer build) says the
/// strand again, nor writes it back over a readable check another process recorded since.
pub(crate) fn announce(staging: &Staging, current_build: u64, explanation: &str) {
    crate::status::record_noted(staging, current_build, explanation, NOTE);
    *last_explanation().lock().unwrap_or_else(|e| e.into_inner()) = Some(explanation.to_string());

    let first = !STRANDED.swap(true, Ordering::Relaxed);
    let mut last = last_warned().lock().unwrap_or_else(|e| e.into_inner());
    let due = last.is_none_or(|t| t.elapsed() >= RENOTICE_AFTER);
    if first || due {
        *last = Some(Instant::now());
        crate::warn(explanation);
    }
}

/// The background loop's latch for the OS notification: once per strand, not once per
/// process.
#[derive(Debug, Default)]
pub(crate) struct StrandNotice {
    notified: bool,
}

impl StrandNotice {
    /// Whether this cycle should raise the notification, given whether the last
    /// completed check left the machine `stranded`.
    pub(crate) const fn due(&mut self, stranded: bool) -> bool {
        if !stranded {
            self.notified = false;
            return false;
        }
        !std::mem::replace(&mut self.notified, true)
    }
}

/// The `(title, body)` for the one-shot OS notification.
#[must_use]
pub(crate) fn notification() -> (String, String) {
    let body = last_explanation()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| {
            "aterm cannot read its release channel. This machine stays on its current \
             version until the channel is fixed; run `aterm ctl update status` for the cause."
                .to_string()
        });
    (crate::STRANDED_TITLE.to_string(), body)
}

#[cfg(test)]
#[path = "unreadable_tests.rs"]
mod tests;
