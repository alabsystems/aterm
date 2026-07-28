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
//! **What that state IS has changed.** It used to be "no token is provisioned", which
//! the updater decided BEFORE any network call — and that was wrong the moment the
//! release channel became public: an unprovisioned Mac can read a public repo
//! perfectly well. The stranded state is now decided by the network, in
//! `github::classify_list_error`: a 401/403/404 with no credential to try. The token
//! chain still supplies the actionable detail (a `chmod 644` token file is a rejection
//! an operator can fix), but it no longer decides the verdict on its own.
//!
//! The state used to produce ONE log line per process and the eight-word status
//! `"idle: no update token provisioned"`. On a terminal that stays open for weeks
//! that line scrolls out of reach on day one, and "idle" reads like "nothing to do"
//! rather than "you will never get another release". This module replaces it with
//! three surfaces that an operator actually meets:
//!
//! 1. **`status.toml` / `aterm-ctl update status`** — the `outcome` field carries the
//!    full explanation INCLUDING the copy-pasteable fix
//!    ([`aterm_update_core::token::PROVISION_COMMAND`]) and every other cause the HTTP
//!    status cannot distinguish, rewritten on every check so it can never go stale.
//! 2. **the app log** — warned (not logged) on the first check, then RE-warned every
//!    [`RENOTICE_AFTER`], so a long-lived process keeps a live breadcrumb without the
//!    per-cycle spam that made the old line unreadable.
//! 3. **an OS notification**, once per process, through the GUI's existing
//!    `HealthNotify` hook — the same channel the "update pipeline is broken" notice
//!    uses. This is the only surface the owner sees without going looking.
//!
//! [`is_stranded`] exposes the state to the background loop so it can fire (3) and
//! stop paying for a network cadence it can never use.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use aterm_update_core::token::Diagnosis;

use crate::paths::Staging;

/// How long between repeats of the log warning while the machine stays stranded.
/// Long enough not to be spam in a week-long session, short enough that a log
/// captured at any point in a day contains the explanation at least once.
const RENOTICE_AFTER: Duration = Duration::from_secs(6 * 60 * 60);

/// Whether the LAST completed check found no token. Read by
/// [`crate::spawn_background_check`] to raise the one-shot OS notification and to
/// back the cadence off a check that cannot possibly succeed.
static STRANDED: AtomicBool = AtomicBool::new(false);

/// The last time the log warning was emitted, for the [`RENOTICE_AFTER`] throttle.
/// `None` until the first announcement.
fn last_warned() -> &'static Mutex<Option<Instant>> {
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

/// The [`RENOTICE_AFTER`] throttle for [`note_unusable_token`]. Separate from
/// [`last_warned`] because the two conditions are independent: a machine whose public
/// channel works fine can still be carrying a broken token file, and neither warning
/// may silence the other.
fn last_rejection_warned() -> &'static Mutex<Option<Instant>> {
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

/// The [`RENOTICE_AFTER`] throttle for [`note_rejected_credential`] — again its own
/// slot, for the same reason.
fn last_credential_warned() -> &'static Mutex<Option<Instant>> {
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

/// Warn at most once per [`RENOTICE_AFTER`] for the given throttle slot.
///
/// Every condition here is STANDING, not an event: it is re-observed on every check.
/// At the 75-second cadence an unthrottled warning is ~48 identical lines an hour,
/// which is precisely the noise that buried the diagnostics this module exists to
/// surface (see `cadence::FailureLog` for the same lesson on the failure path).
fn warn_throttled(slot: &'static Mutex<Option<Instant>>, message: &str) {
    let mut last = slot.lock().unwrap_or_else(|e| e.into_inner());
    if !last.is_none_or(|t| t.elapsed() >= RENOTICE_AFTER) {
        return;
    }
    *last = Some(Instant::now());
    crate::warn(message);
}

/// The most recent stranded explanation, so [`notification`] can describe the ACTUAL
/// state without re-deriving it (which would re-spawn `security`/`gh` and re-hit the
/// network). Holds no credential: the explanation is built from `&'static str` labels
/// and outcomes only.
fn last_explanation() -> &'static Mutex<Option<String>> {
    static LAST: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

/// Whether the most recent check found this machine stranded with no token.
#[must_use]
pub(crate) fn is_stranded() -> bool {
    STRANDED.load(Ordering::Relaxed)
}

/// Record that the release channel WAS readable — clears the stranded latch so a
/// machine fixed mid-session (the remedy works without a restart) stops announcing,
/// and re-arms the announcement should the channel become unreadable again.
///
/// Called on a successful releases LIST, authenticated or not: on a public channel
/// "we can read it" is the whole property, and a token is only one way to get there.
pub(crate) fn clear() {
    if STRANDED.swap(false, Ordering::Relaxed) {
        crate::log("the release channel is readable again — this machine is receiving updates");
        *last_warned().lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Announce the stranded state on all three surfaces. Called on every check whose
/// releases LIST came back unreadable with no credential left to try, but only the
/// status write happens every time; the log warning is throttled to [`RENOTICE_AFTER`]
/// and the notification is fired once per process by the caller of [`is_stranded`].
///
/// `explanation` comes from `github::unreadable_explanation`: it names the observed
/// HTTP status, every cause that status cannot distinguish, and the remedy for each.
/// It is built from fixed literals and `&'static str` probe labels, so it can never
/// carry a credential.
pub(crate) fn announce_unreadable(staging: &Staging, current_build: u64, explanation: &str) {
    crate::status::record(staging, current_build, explanation);
    *last_explanation().lock().unwrap_or_else(|e| e.into_inner()) = Some(explanation.to_string());

    let first = !STRANDED.swap(true, Ordering::Relaxed);
    let mut last = last_warned().lock().unwrap_or_else(|e| e.into_inner());
    let due = last.is_none_or(|t| t.elapsed() >= RENOTICE_AFTER);
    if first || due {
        *last = Some(Instant::now());
        // `warn`, not `log`: this is a defect in the machine's configuration, not a
        // routine decision, and it is the one condition under which the updater can
        // never make progress.
        crate::warn(explanation);
    }
}

/// Note that the token chain found a source that was PRESENT and refused, on a check
/// that nonetheless succeeded (the channel is public, so the machine still updates).
///
/// This is not a stranded state and must not arm [`is_stranded`] — but "you
/// provisioned a token and I threw it away" is still worth saying: it costs this
/// machine the 5000-requests/hour authenticated budget and leaves it sharing the
/// ~60/hour per-IP anonymous one. Throttled like the stranded warning; silent when
/// nothing was rejected (an absent source is just "not configured", which on a public
/// channel is a perfectly normal state and must stay quiet).
pub(crate) fn note_unusable_token(diagnosis: &Diagnosis) {
    let rejections = diagnosis.rejections();
    if rejections.is_empty() {
        return;
    }
    warn_throttled(
        last_rejection_warned(),
        &format!(
            "an update token is present but unusable ({}) — updates still work over the \
             public channel, but this machine shares the anonymous ~60 requests/hour per IP \
             budget instead of its own 5000/hour. Fix it with: {}",
            rejections.join("; "),
            aterm_update_core::token::PROVISION_COMMAND
        ),
    );
}

/// Note that GITHUB refused the token this machine resolved, and the check carried on
/// anonymously.
///
/// Distinct from [`note_unusable_token`]: there the token never left the machine (our
/// own chain refused it); here it was well-formed, was sent, and the SERVER rejected
/// it — a rotation, a `gh auth logout`, or a PAT scoped to a different repo. Not a
/// strand (the public channel still works) and not a failure (nothing broke), but the
/// operator holds a credential they believe works and does not.
///
/// Throttled, because it is re-observed on EVERY check: unthrottled this is ~48
/// identical warnings an hour for as long as the stale token sits there.
pub(crate) fn note_rejected_credential(message: &str) {
    warn_throttled(last_credential_warned(), message);
}

/// The `(title, body)` for the one-shot OS notification, built from the explanation
/// [`announce_unreadable`] recorded. Split out so the wording is testable without a
/// GUI.
#[must_use]
pub(crate) fn notification() -> (String, String) {
    let why = last_explanation()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| {
            format!(
                "aterm cannot read its release repository. If the channel is private, \
                 provision a token by running: {}",
                aterm_update_core::token::PROVISION_COMMAND
            )
        });
    (
        "aterm will never auto-update on this Mac".to_string(),
        format!(
            "{why}\n\nThis Mac will stay on its current build until you fix it. Then run \
             `aterm-ctl update check`."
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic diagnosis, so these tests never depend on whether the developer
    /// running them happens to have `gh` authenticated.
    fn unprovisioned() -> Diagnosis {
        use aterm_update_core::token::{ProbeOutcome, SourceProbe};
        Diagnosis {
            resolved: None,
            probes: vec![SourceProbe {
                source: "$ATERM_UPDATE_TOKEN",
                outcome: ProbeOutcome::Absent,
            }],
        }
    }

    #[test]
    fn the_notification_names_the_consequence_and_the_exact_fix() {
        let (title, body) = notification();
        assert!(
            title.contains("never auto-update"),
            "the title must state the consequence, not the mechanism: {title}"
        );
        assert!(
            body.contains(aterm_update_core::token::PROVISION_COMMAND),
            "the body must carry the copy-pasteable remedy: {body}"
        );
        assert!(
            body.contains("stay on its current build"),
            "the body must say what happens if it is ignored: {body}"
        );
    }

    #[test]
    fn announce_writes_the_full_explanation_into_the_status_outcome() {
        // The status `outcome` IS what `aterm-ctl update status` prints, so the
        // remedy has to survive into it — not a shortened "idle:" summary.
        let _serialized = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("aterm-nt-status-{}", std::process::id()));
        let explanation = format!(
            "aterm cannot read its release channel github.com/o/r (HTTP 404) and no update \
             token is provisioned, so this machine will NEVER receive an update until it is \
             fixed. Run: {}",
            aterm_update_core::token::PROVISION_COMMAND
        );
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
        announce_unreadable(&staging, 1234, &explanation);
        // The notification now describes the state `announce_unreadable` just
        // recorded, without re-deriving it (no re-walk of the token chain, no second
        // network round trip).
        let (_, body) = notification();
        assert!(body.contains("gh auth token"), "{body}");
        assert!(
            body.contains("github.com/o/r"),
            "the notification must name the channel that could not be read: {body}"
        );
        let text = std::fs::read_to_string(&staging.status).expect("status written");
        assert!(
            text.contains("NEVER receive an update"),
            "the status must state the consequence: {text}"
        );
        assert!(
            text.contains("gh auth token"),
            "the status must carry the remedy command: {text}"
        );
        let _: toml::Value = toml::from_str(&text).expect("status stays valid TOML");
        assert!(is_stranded(), "the latch arms for the background loop");
        clear();
        assert!(
            !is_stranded(),
            "and disarms as soon as the channel reads again"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A broken token on a machine whose PUBLIC channel works is a nuisance, not a
    /// strand: it must be said, but it must not arm the "you will never update again"
    /// latch — that latch backs the cadence off and fires an OS notification, and
    /// doing either to a machine that is updating fine is exactly the false alarm
    /// this whole module exists to avoid.
    #[test]
    fn an_unusable_token_on_a_readable_channel_is_not_a_strand() {
        use aterm_update_core::token::{ProbeOutcome, SourceProbe};

        // Asserted as a DELTA rather than an absolute, so this stays correct beside
        // the sibling tests that legitimately arm the latch in the same process.
        let before = is_stranded();
        note_unusable_token(&unprovisioned());
        assert_eq!(
            is_stranded(),
            before,
            "an ABSENT token source is the normal public-channel state and must not \
             touch the latch"
        );
        note_unusable_token(&Diagnosis {
            resolved: None,
            probes: vec![SourceProbe {
                source: "0600 update-token file",
                outcome: ProbeOutcome::Rejected("chmod 600 it"),
            }],
        });
        assert_eq!(
            is_stranded(),
            before,
            "a REJECTED source is worth warning about but is not a stranded machine"
        );
    }

    #[test]
    fn the_explanation_survives_status_reconciliation_into_update_status() {
        // `aterm-ctl update status` prints `outcome=`, which comes from
        // `status()` -> `reconcile_status_outcome`. That reducer NEUTRALIZES a
        // persisted outcome that falsely claims a stage. The no-token explanation
        // must pass through untouched, or the loudest surface silently swallows it.
        let explanation = unprovisioned().no_token_explanation();
        let reconciled = crate::reconcile_status_outcome(1234, 1234, None, explanation.clone());
        assert_eq!(
            reconciled.outcome,
            crate::ReconciledStatusOutcome::Preserved(explanation),
            "the no-token explanation must reach `aterm-ctl update status` verbatim"
        );
    }
}
