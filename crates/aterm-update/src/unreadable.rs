// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The "this machine can never update" announcement.
//!
//! A machine that cannot READ its release channel is not slow to update — it is
//! PERMANENTLY stranded, and nothing about it looks broken: no failure, no streak, no
//! network error. The health ledger records zeroes because the check never gets far
//! enough to fail (and deliberately so: a configuration state is not a transient
//! fault, and counting it as one would bury the real signal).
//!
//! The state is decided by the network, in `github::resolve_web_head`: the evergreen
//! pointer answering 404 — the channel has no published release, or the repository is
//! private, renamed or gone. The updater reads its channel with no credential, so there
//! is nothing on this machine to repair; the explanation names the causes and the
//! remedy, and three surfaces carry it:
//!
//! 1. **`status.toml` / `aterm-ctl update status`** — the `outcome` field carries the
//!    full explanation, rewritten on every check so it can never go stale.
//! 2. **the app log** — warned (not logged) on the first check, then RE-warned every
//!    [`RENOTICE_AFTER`], so a long-lived process keeps a live breadcrumb without
//!    per-cycle spam.
//! 3. **an OS notification**, once per process, through the GUI's existing
//!    `HealthNotify` hook — the same channel the "update pipeline is broken" notice
//!    uses. This is the only surface the owner sees without going looking.
//!
//! [`is_stranded`] exposes the state to the background loop so it can fire (3) and
//! back off a network cadence that cannot succeed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::paths::Staging;

/// How long between repeats of the log warning while the machine stays stranded.
/// Long enough not to be spam in a week-long session, short enough that a log
/// captured at any point in a day contains the explanation at least once.
const RENOTICE_AFTER: Duration = Duration::from_secs(6 * 60 * 60);

/// Whether the LAST completed check found the channel unreadable. Read by
/// [`crate::spawn_background_check`] to raise the one-shot OS notification and to
/// back the cadence off a check that cannot possibly succeed.
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

/// Record that the release channel WAS readable — clears the stranded latch so a
/// machine fixed mid-session (the remedy works without a restart) stops announcing,
/// and re-arms the announcement should the channel become unreadable again.
pub(crate) fn clear() {
    if STRANDED.swap(false, Ordering::Relaxed) {
        crate::log("the release channel is readable again — this machine is receiving updates");
        *last_warned().lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Announce the stranded state on all three surfaces. Called on every check whose
/// pointer answered that the channel cannot be read, but only the status write happens
/// every time; the log warning is throttled to [`RENOTICE_AFTER`] and the notification
/// is fired once per process by the caller of [`is_stranded`].
///
/// `explanation` comes from `github::unreadable_explanation`: it names the observed
/// HTTP status, every cause that status cannot distinguish, and the remedy for each.
pub(crate) fn announce(staging: &Staging, current_build: u64, explanation: &str) {
    crate::status::record(staging, current_build, explanation);
    *last_explanation().lock().unwrap_or_else(|e| e.into_inner()) = Some(explanation.to_string());

    let first = !STRANDED.swap(true, Ordering::Relaxed);
    let mut last = last_warned().lock().unwrap_or_else(|e| e.into_inner());
    let due = last.is_none_or(|t| t.elapsed() >= RENOTICE_AFTER);
    if first || due {
        *last = Some(Instant::now());
        // `warn`, not `log`: this is a defect in the channel, not a routine decision,
        // and it is the one condition under which the updater can never make progress.
        crate::warn(explanation);
    }
}

/// The `(title, body)` for the one-shot OS notification, built from the explanation
/// [`announce`] recorded. Split out so the wording is testable without a GUI.
#[must_use]
pub(crate) fn notification() -> (String, String) {
    let why = last_explanation()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| {
            "aterm cannot read its release repository. Run `aterm-ctl update status` for \
             the exact cause."
                .to_string()
        });
    (
        // NOT "will never": `clear()` drops the stranded latch on the first readable
        // check, so the honest claim is the one the body makes — this Mac stays put
        // UNTIL the cause is fixed.
        "aterm is not updating on this machine".to_string(),
        format!(
            "{why}\n\nThis machine will stay on its current build until the channel is \
             fixed. Then run `aterm-ctl update check`."
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notification_names_the_consequence_and_the_next_step() {
        // `notification()` reads the process-global `last_explanation()`, which the
        // sibling test fills; hold the lock it holds and start from "nothing recorded".
        let _serialized = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *last_explanation().lock().unwrap_or_else(|e| e.into_inner()) = None;
        let (title, body) = notification();
        assert!(title.contains("not updating"), "{title}");
        assert!(
            !title.contains("never"),
            "the title must not claim a permanence the code does not enforce: {title}"
        );
        assert!(body.contains("stay on its current build"), "{body}");
        assert!(body.contains("aterm-ctl update status"), "{body}");
        assert!(
            !body.contains("token"),
            "no credential is ever a remedy: {body}"
        );
    }

    #[test]
    fn announce_writes_the_full_explanation_into_the_status_outcome() {
        // The status `outcome` IS what `aterm-ctl update status` prints, so the whole
        // explanation has to survive into it — not a shortened "idle:" summary.
        let _serialized = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("aterm-unread-status-{}", std::process::id()));
        let explanation = "aterm cannot read its release channel github.com/o/r (HTTP 404): \
                           this machine will NEVER receive an update until the channel is \
                           repaired";
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
        assert!(text.contains("NEVER receive an update"), "{text}");
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
        // `aterm-ctl update status` prints `outcome=`, which comes from `status()` ->
        // `reconcile_status_outcome`. That reducer NEUTRALIZES a persisted outcome that
        // falsely claims a stage; the unreadable explanation must pass through untouched.
        let explanation = String::from(
            "aterm cannot read its release channel github.com/o/r (HTTP 404): this machine \
             will NEVER receive an update",
        );
        let reconciled = crate::reconcile_status_outcome(1234, 1234, None, explanation.clone());
        assert_eq!(
            reconciled.outcome,
            crate::ReconciledStatusOutcome::Preserved(explanation),
        );
    }
}
