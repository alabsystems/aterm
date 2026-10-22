// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The announcement that this machine cannot read its release channel, and so is not updating.
//!
//! A machine that cannot READ its release channel is not slow to update — it is
//! PERMANENTLY stranded, and nothing about it looks broken: no failure, no streak, no
//! network error. The health ledger records zeroes because the check never gets far
//! enough to fail (and deliberately so: a configuration state is not a transient
//! fault, and counting it as one would bury the real signal).
//!
//! 1. `status.toml` / `aterm ctl update status`: `outcome` holds the full explanation,
//!    rewritten every check.
//! 2. The app log: a warning on the first check, repeated every [`RENOTICE_AFTER`].
//! 3. An OS notification once per strand ([`StrandNotice`]) through the GUI's `HealthNotify`
//!    hook.
//!
//! [`is_stranded`] lets the background loop fire (3) and back off a cadence that cannot
//! succeed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::paths::Staging;

/// Repeat interval of the log warning while stranded: not spam in a week-long session, yet a
/// log captured on any day holds the explanation.
const RENOTICE_AFTER: Duration = Duration::from_secs(6 * 60 * 60);

/// Whether the last completed check found the channel unreadable. The background loop
/// ([`crate::spawn_background_check_with_settings`]) reads it to raise the OS notification and
/// back off.
static STRANDED: AtomicBool = AtomicBool::new(false);

/// The last time the log warning was emitted, for the [`RENOTICE_AFTER`] throttle.
/// `None` until the first announcement.
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

/// Whether the most recent check found this machine unable to read its channel.
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

/// Announce the stranded state. Called on every check whose pointer says the channel cannot be
/// read: the status write happens every time, the log warning at most every
/// [`RENOTICE_AFTER`], the notification once per strand ([`StrandNotice`]).
///
/// `explanation` comes from `github::unreadable_explanation`: every cause a 404 cannot
/// distinguish, the consequence, and the remedy.
pub(crate) fn announce(staging: &Staging, current_build: u64, explanation: &str) {
    crate::status::record(staging, current_build, explanation);
    *last_explanation().lock().unwrap_or_else(|e| e.into_inner()) = Some(explanation.to_string());

    let first = !STRANDED.swap(true, Ordering::Relaxed);
    let mut last = last_warned().lock().unwrap_or_else(|e| e.into_inner());
    let due = last.is_none_or(|t| t.elapsed() >= RENOTICE_AFTER);
    if first || due {
        *last = Some(Instant::now());
        // `warn`, not `log`: a channel defect, and the one condition under which the
        // updater can never make progress.
        crate::warn(explanation);
    }
}

/// The background loop's latch for the OS notification: once per strand, not once per
/// process. A strand is announced once and then lives in `status.toml`; a readable check ends
/// it ([`clear`]), and a later strand is announced again.
#[derive(Debug, Default)]
pub(crate) struct StrandNotice {
    notified: bool,
}

impl StrandNotice {
    /// Whether this cycle should raise the notification, given whether the last
    /// completed check left the machine `stranded`. True on the first stranded cycle
    /// of each strand; a non-stranded cycle re-arms the latch.
    pub(crate) fn due(&mut self, stranded: bool) -> bool {
        if !stranded {
            self.notified = false;
            return false;
        }
        !std::mem::replace(&mut self.notified, true)
    }
}

/// The `(title, body)` for the one-shot OS notification. The body is the explanation
/// [`announce`] recorded, which already carries the consequence and the remedy.
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
    (
        // Not "will never": `clear()` drops the latch on the first readable check, so the
        // claim is the body's: this Mac stays put until the cause is fixed.
        crate::STRANDED_TITLE.to_string(),
        body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The latch re-arms when the strand clears, so a channel that heals and breaks again is
    /// announced again.
    #[test]
    fn the_strand_notice_fires_once_per_strand_and_re_arms_when_it_clears() {
        let mut notice = StrandNotice::default();
        assert!(!notice.due(false), "a readable channel announces nothing");
        assert!(notice.due(true), "the first stranded cycle announces");
        assert!(!notice.due(true), "and only the first");
        assert!(!notice.due(true));
        assert!(!notice.due(false), "the channel reads again");
        assert!(
            notice.due(true),
            "a second strand in the same process is announced again"
        );
        assert!(!notice.due(true));
    }

    #[test]
    fn announce_writes_the_full_explanation_into_the_status_outcome() {
        // The status `outcome` is what `aterm ctl update status` prints, so the whole
        // explanation must survive into it, not a shortened "idle:" summary.
        let _serialized = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("aterm-unread-status-{}", std::process::id()));
        let explanation = "aterm cannot read its release channel github.com/o/r: this \
                           machine stays on its current version until the channel is fixed";
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let staging = Staging {
            apply_lock: root.join("apply.lock"),
            stage_lock: root.join("stage.lock"),
            download: root.join("download"),
            staged_app: root.join("staged").join("aterm.app"),
            ready: root.join("ready.toml"),
            status: root.join("status.toml"),
            root: root.clone(),
        };
        announce(&staging, 1234, explanation);
        // The notification describes the state just recorded, with no second round trip.
        let (_, body) = notification();
        assert!(body.contains("github.com/o/r"), "{body}");
        let text = std::fs::read_to_string(&staging.status).expect("status written");
        assert!(text.contains("stays on its current version"), "{text}");
        let _: aterm_toml::Value = aterm_toml::from_str(&text).expect("status stays valid TOML");
        assert!(is_stranded(), "the latch arms for the background loop");
        clear();
        assert!(
            !is_stranded(),
            "and disarms as soon as the channel reads again"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_explanation_survives_status_reconciliation_into_update_status() {
        // `aterm ctl update status` prints `outcome=` through `status()` ->
        // `reconcile_status_outcome`, which neutralizes a persisted outcome that falsely
        // claims a stage; the unreadable explanation must pass through untouched.
        let explanation = String::from(
            "aterm cannot read its release channel github.com/o/r: this machine stays on \
             its current version until the channel is fixed",
        );
        let reconciled = crate::reconcile_status_outcome(1234, 1234, None, explanation.clone());
        assert_eq!(
            reconciled.outcome,
            crate::ReconciledStatusOutcome::Preserved(explanation),
        );
    }
}
