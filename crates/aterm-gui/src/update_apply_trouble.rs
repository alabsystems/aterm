// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The apply lane's refusal, in words a person can act on.
//!
//! One value ([`ApplyTrouble`]) and one wording law. Every standing "an update is available"
//! affordance (the macOS Version menu, the command palette's Version row, the native Settings
//! `/updates` route, the update card) reads the same value and renders it at its own width, so
//! no surface can claim a stage is merely waiting when the engine has already tried and failed.
//!
//! The wording obeys three rules:
//!
//! 1. **No internal text reaches the user**: neither the enum name (`ChildDied`) nor the
//!    ledger's own sentence, which is engineering prose ("overlap handoff failed safely: …").
//!    [`CAUSES`] is keyed on strings this program writes; everything else takes [`GENERIC`],
//!    and [`ApplyTrouble::cause_is_named`] tells a surface with room to point at the log.
//! 2. **A stage waiting for a quiet window is not trouble.** That is a refusal
//!    (`Deferred`/`Blocked` → `record_apply_refusal`) that advances no streak and keeps
//!    reading as "ready": [`ApplyTrouble::new`] returns `None` for zero attempts.
//! 3. **"It will retry" and "it will not" differ for the reader**, so [`ApplyRetry`] is
//!    carried, never inferred, and every rendering states it.

/// Whether the automatic apply lane will still try this exact staged build by itself, or has
/// stopped and waits to be asked. The event loop decides (a live `AutoApplyIntent`, or an
/// `AutoApplyManualOnly` latch with a lapse deadline, with automatic apply enabled), so it is
/// passed in, from the facts `App::react_to_update_apply_outcome` consults.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ApplyRetry {
    /// The cause is a person's to change (the installed copy cannot be the swap's rollback
    /// source, or its folder is not writable by this user) and no retry succeeds until they
    /// do. Surfaces say the remedy the standing block names ([`PersonRemedy`]), never one
    /// re-derived from the ledger's latest reason.
    NeedsPerson(PersonRemedy),
    /// The lane still intends this artifact: a live intent or a latch with a lapse deadline,
    /// both of which `about_to_wait` comes back to ("nothing to do"). The wording is true of
    /// both: an intent's `retry_at` is folded into the loop's deadline
    /// (`fold_auto_apply_deadline`), while a manual-only latch is released by
    /// `lapse_expired_auto_apply_manual_only` at the top of `arm_native_auto_apply`, the next
    /// time the lane is asked (on an idle machine, the next background check). "By itself"
    /// covers both; a promised wake instant would not.
    Scheduled,
    /// The manual-only latch holds with no lapse deadline (or automatic apply is
    /// switched off): this build moves only when the person asks it to.
    ManualOnly,
    /// The stood-down build is the installed activation and its boot trial has
    /// no launch to spare (`App::latched_activation_trial_spent`): any start,
    /// the Version menu's included, is the one the trial rolls back, so no
    /// surface asks for one. Only a newer release moves it.
    NewerRelease,
}

/// What a person does about a refusal only a person can clear, carried by
/// [`ApplyRetry::NeedsPerson`] from the block that answered it. Not read off the ledger's
/// latest reason: while a folder's block stands, the person's own retry can fail for an
/// ordinary reason first (the apply lock's wait, `codesign` past its budget, a capture
/// refusal), and the remedy stays the block's; the cause clause still says how the last
/// attempt ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PersonRemedy {
    /// The install location's folder cannot be written by this user
    /// (`aterm_update::is_install_location_refusal`): move the app, or have an
    /// administrator install the update. Reinstalling the signed release would
    /// land in the same folder and be refused again.
    MoveOrAskAdmin,
    /// The installed copy cannot be the swap's rollback source, or this process
    /// does not run from one: install the signed release.
    InstallSignedRelease,
}

impl PersonRemedy {
    /// The remedy a person's-to-clear `reason` names, for a surface that has the
    /// refusal itself and no standing block to ask (the row of a person's press
    /// whose failure took no block).
    #[must_use]
    pub(crate) fn of_refusal(reason: &str) -> Self {
        if aterm_update::is_install_location_refusal(reason) {
            Self::MoveOrAskAdmin
        } else {
            Self::InstallSignedRelease
        }
    }

    /// The sentence, for a detail with a line to spend. It ends on the retry,
    /// which verifies afresh: a cached refusal never answers a person's own
    /// press.
    #[must_use]
    pub(crate) fn sentence(self) -> &'static str {
        match self {
            Self::MoveOrAskAdmin => {
                "Move aterm to a folder you can write to, such as ~/Applications, or have an \
                 administrator install the update, then retry."
            }
            Self::InstallSignedRelease => {
                "Install the signed release from the release DMG, then retry."
            }
        }
    }

    /// The few words, for a menu row's tail.
    #[must_use]
    pub(crate) fn row_tail(self) -> &'static str {
        match self {
            Self::MoveOrAskAdmin => "move aterm or ask an admin",
            Self::InstallSignedRelease => "install the signed release",
        }
    }
}

/// A standing apply-lane failure for the build a surface is offering: how many times, why, and
/// whether it will happen again without the user. Constructed only through
/// [`ApplyTrouble::new`], which enforces the at-least-one-attempt rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApplyTrouble {
    /// Consecutive failed applies of the artifact on offer (the ledger's
    /// `apply_failures_for_target`, not the running-build-scoped escalation streak). Always
    /// `>= 1`.
    attempts: u32,
    /// The ledger's own `last_apply_error` prose, the raw `apply_failure=` value, kept verbatim
    /// for the log and the control verb; no rendering here prints it.
    reason: String,
    /// Whether the automatic lane will try again unaided.
    retry: ApplyRetry,
    /// How long until the scheduled retry, when the lane can say, so a six-hour stand-down does
    /// not read like a two-minute one. `None` keeps the bare promise (a latch the next
    /// background check releases).
    retry_in: Option<std::time::Duration>,
}

/// `(full clause, short clause)` for a reason this program does not recognise: true of every
/// string that reaches this slot (an apply attempted that did not complete) and no guess at a
/// mechanism ("it did not start" would be wrong for an adoption mismatch or a stale
/// preflight).
const GENERIC: (&str, &str) = ("it did not finish installing", "didn\u{2019}t finish");

/// The open tabs' records over what one handover carries, in a person's
/// words — the one capture refusal a record names the cause of
/// (`update_words::capture_refused_waiting`), since closing tabs moves it.
pub(crate) const TABS_OVER_HANDOVER: &str =
    "too many tabs were open to move to the new version at once";

/// The apply outcomes this program can name, as `(a substring of a real ledger reason, full
/// clause, short clause)`.
///
/// Keyed on the data, not on `UpdateHandoffOutcome`: only the readiness wait
/// (`app_update_handoff.rs`, `format!("handoff proof ended {outcome:?}")`) stamps a variant
/// name, and every other failure is free-form prose under `"overlap handoff failed safely:
/// {detail}"`. The rows are substrings from the producers, named above each group, and the
/// tests replay each literal (`REAL_LEDGER_REASONS`) through [`clauses`]. An unlisted reason
/// takes [`GENERIC`], so the apply lane can grow new prose without this table in lockstep.
///
/// First match wins. "structural activity revoked handoff before Commit" contains both
/// `activity revoked` and `before Commit` and is typed `ActivityRevoked`, so the activity rows
/// come first.
const CAUSES: &[(&str, &str, &str)] = &[
    // ── The successor's own verdict: `wait_handoff_ready` returns one of five variants and
    // `run_handoff_decision` stamps its Debug name, the only reasons that carry a type name.
    (
        "handoff proof ended ChildDied",
        "the new version did not finish starting",
        "didn\u{2019}t start",
    ),
    (
        "handoff proof ended TimedOut",
        "the new version did not finish starting in time",
        "too slow to start",
    ),
    (
        "handoff proof ended AdoptionMismatch",
        "the new version did not take over the open sessions",
        "didn\u{2019}t take over",
    ),
    (
        "handoff proof ended ActivityRevoked",
        "the terminal was too busy to hand over",
        "terminal too busy",
    ),
    (
        "handoff proof ended Rejected",
        "the handover was called off before it finished",
        "handover called off",
    ),
    // ── Activity revoked the attempt, worded three different ways by three
    // different points in the worker: "activity revoked handoff during physical
    // preparation", "structural activity revoked handoff before Commit",
    // "structural activity revoked the handoff before any descriptor was sent".
    (
        "activity revoked",
        "the terminal was too busy to hand over",
        "terminal too busy",
    ),
    // ── A successor was started and then never answered in time: "the launched
    // successor never claimed the handoff: {error}".
    (
        "never claimed the handoff",
        "the new version did not finish starting in time",
        "too slow to start",
    ),
    // ── The handover was called off between ProofReady and Commit: "event loop
    // closed before final handoff admission", "a handed-off PTY session closed
    // before Commit", "final handoff admission rejected before Commit",
    // "main-thread final handoff decision timed out".
    (
        "before Commit",
        "the handover was called off before it finished",
        "handover called off",
    ),
    (
        "final handoff admission",
        "the handover was called off before it finished",
        "handover called off",
    ),
    (
        "final handoff decision timed out",
        "the handover was called off before it finished",
        "handover called off",
    ),
    // ── The park's capture refused the desk: `CaptureRefusal`'s Display ("the park's capture
    // refused session 3: …" or "the park's capture refused: handoff layout could not be
    // committed canonically; …"), on both lanes. Ahead of the preparation rows: the layout
    // refusal also contains "handoff layout" and is no set-up failure (the lane waits for the
    // desk to change).
    (
        "the park's capture refused session",
        "a session\u{2019}s screen could not be captured for the handover",
        "couldn\u{2019}t capture a session",
    ),
    // A desk past a cap no update carries, which the refusal names
    // (`restore::RestoreManifest::why_not_whole`): ahead of the generic row,
    // as closing some is what moves it.
    (
        crate::restore::UPDATE_CAP_KEY,
        "more was open than an update can carry",
        "too much open",
    ),
    (
        "the park's capture refused",
        "the open tabs could not be captured for the handover",
        "couldn\u{2019}t capture the tabs",
    ),
    // ── This process could not hold the park long enough: the fork lane's worker gave up
    // waiting ("the outgoing process did not park within N s"), typed `TimedOut`. The terminal
    // never went quiet.
    (
        "did not park within",
        "the terminal was too busy to hand over",
        "terminal too busy",
    ),
    // ── More tabs than one descriptor message carries at the park (the user
    // opened more while the successor held): something a person can act on.
    (
        "descriptor message carries",
        TABS_OVER_HANDOVER,
        "too many tabs",
    ),
    // The same fact where the park reads the dialled claim's own limit (one message or the
    // chunked grant).
    (
        "descriptor grant carries",
        TABS_OVER_HANDOVER,
        "too many tabs",
    ),
    // The open tabs' records over the manifest's cap with every optional part shed
    // (`WriteOutgoingFailure::ManifestOverCap`): ahead of the "authenticated handoff manifest"
    // row it also contains, as closing tabs is what moves it.
    ("records are over the", TABS_OVER_HANDOVER, "too many tabs"),
    // ── Preparation, all of it before any successor existed
    // (`send_handoff_preparation_failure` for the candidate's pre-park verification;
    // `send_handoff_producer_failure` and the park's `ProducerFailed` stand-downs for this
    // process's writes and descriptors, each with its `io::ErrorKind`; the descriptor transfer
    // on the out-of-band lane). Nothing was started, so nothing "failed to start".
    // ── A check that did not finish: `aterm_update::PASSING_REFUSAL_KEY`, which the verifier
    // appends for a helper past its budget or ceiling, a helper the kernel would not start
    // just then, and the apply lock held past its wait. Ahead of every preparation row, the
    // installed-copy one included: it says nothing about either bundle, and the lane retries
    // it by itself.
    (
        aterm_update::PASSING_REFUSAL_KEY,
        "the new version could not be checked just then",
        "couldn\u{2019}t check it just then",
    ),
    // ── The installed copy cannot be the swap's rollback source:
    // `install::rollback_source_refusal`, both lanes. The clause states the failure, not that
    // the source is still broken; only a current `NeedsPerson` policy carries repair advice.
    (
        "cannot be the rollback source",
        "the installed copy could not be verified for replacement",
        "installed copy did not verify",
    ),
    // ── The install's folder cannot be written: `install::install_location_refusal`, asked
    // before the park (an admin-owned or MDM-managed /Applications run by a standard user).
    (
        aterm_update::INSTALL_LOCATION_REFUSAL_KEY,
        "aterm could not write to the folder it is installed in",
        "install folder not writable",
    ),
    (
        "does not run from an installed bundle",
        "aterm was not running from an installed copy the updater could replace",
        "no replaceable installed copy",
    ),
    (
        "pre-park verification",
        "the handover could not be set up",
        "couldn\u{2019}t be set up",
    ),
    (
        "handoff layout",
        "the handover could not be set up",
        "couldn\u{2019}t be set up",
    ),
    (
        "authenticated handoff manifest",
        "the handover could not be set up",
        "couldn\u{2019}t be set up",
    ),
    (
        "committed screen digest",
        "the handover could not be set up",
        "couldn\u{2019}t be set up",
    ),
    (
        "proof format",
        "the handover could not be set up",
        "couldn\u{2019}t be set up",
    ),
    (
        "adoption-proof channel",
        "the handover could not be set up",
        "couldn\u{2019}t be set up",
    ),
    (
        "handoff-commit channel",
        "the handover could not be set up",
        "couldn\u{2019}t be set up",
    ),
    (
        "handoff process could not start",
        "the handover could not be set up",
        "couldn\u{2019}t be set up",
    ),
    // The proof deadline ran out in this process before the grant was sent, after the
    // successor dialed.
    (
        "proof deadline passed before the grant was sent",
        "the terminal was too busy to hand over",
        "terminal too busy",
    ),
    (
        "descriptors could not be delivered",
        "the handover could not be set up",
        "couldn\u{2019}t be set up",
    ),
    (
        "child-only PTY descriptors",
        "the handover could not be set up",
        "couldn\u{2019}t be set up",
    ),
    (
        "would not answer fstat",
        "the handover could not be set up",
        "couldn\u{2019}t be set up",
    ),
    (
        "identity space is exhausted",
        "the handover could not be set up",
        "couldn\u{2019}t be set up",
    ),
    (
        "private control directory",
        "the handover could not be set up",
        "couldn\u{2019}t be set up",
    ),
    (
        "before the park",
        "the handover could not be set up",
        "couldn\u{2019}t be set up",
    ),
    // ── The outgoing process's own capture refused itself: `screen_digest_refs`
    // (`seamless.rs`) names the session, field and bound ("visible checkpoint set could not be
    // committed canonically: session 0: meta out of bounds at 55x149 with 0 carried line(s):
    // …"), and the park stands the held successor down.
    (
        "could not be committed canonically",
        "a session\u{2019}s screen could not be captured for the handover",
        "couldn\u{2019}t capture a session",
    ),
    // ── Not the GUI's lane: `aterm-update`'s boot-trial recovery writes this slot too
    // (`install.rs`, both arms end "disarmed the boot sentinel to keep updates possible"). The
    // update installed, would not prove itself across three launches, and the machine is back
    // on this build.
    (
        "disarmed the boot sentinel",
        "the new version would not run and aterm went back to this one",
        "rolled back",
    ),
];

/// `(full clause, short clause)` for a ledger reason; an unrecognised one takes [`GENERIC`],
/// never its own log prose (see [`CAUSES`]).
fn clauses(reason: &str) -> (String, String) {
    let (full, short) = named_cause(reason).unwrap_or(GENERIC);
    (full.to_string(), short.to_string())
}

/// The full clause for a reason this program can name, or `None`, for a record that already
/// says the attempt did not finish, where [`GENERIC`] would only repeat its title. The raw
/// reason stays in aterm.log.
pub(crate) fn named_clause(reason: &str) -> Option<&'static str> {
    named_cause(reason).map(|(full, _)| full)
}

/// The [`CAUSES`] row this reason matches, if any. One scan, shared by [`clauses`]
/// and [`ApplyTrouble::cause_is_named`], so "which clause do we say" and "did we
/// recognise it" can never answer differently.
fn named_cause(reason: &str) -> Option<(&'static str, &'static str)> {
    let reason = reason.trim();
    CAUSES
        .iter()
        .find(|(token, _, _)| reason.contains(token))
        .map(|(_, full, short)| (*full, *short))
}

/// "6 h" / "30 min" / "a minute" — a wait a sentence can contain, at the
/// precision a person plans around (a stand-down is hours; a retry, minutes).
fn about(wait: std::time::Duration) -> String {
    let secs = wait.as_secs();
    if secs >= 3600 {
        format!("{} h", secs.div_ceil(3600))
    } else if secs >= 90 {
        format!("{} min", secs.div_ceil(60))
    } else {
        "a minute".to_string()
    }
}

/// "once" / "twice" / "3 times": a count a sentence can contain.
fn times(attempts: u32) -> String {
    match attempts {
        1 => "once".to_string(),
        2 => "twice".to_string(),
        n => format!("{n} times"),
    }
}

impl ApplyTrouble {
    /// The standing trouble for a staged build, or `None` when there is none. Zero attempts is
    /// `None` (rule 2 of the module docs): a deferred or blocked stage advances no streak. The
    /// caller supplies the current retry policy, so a historical installed-source failure
    /// cannot override a later repair that restored scheduling.
    #[must_use]
    pub(crate) fn new(attempts: u32, reason: &str, retry: ApplyRetry) -> Option<Self> {
        (attempts > 0).then(|| Self {
            attempts,
            reason: reason.trim().to_string(),
            retry,
            retry_in: None,
        })
    }

    /// Name how long until the scheduled retry. Ignored unless the retry is
    /// [`ApplyRetry::Scheduled`]: a duration under any other promise would be a
    /// contradiction the sentence cannot render.
    #[must_use]
    pub(crate) fn with_retry_in(mut self, retry_in: Option<std::time::Duration>) -> Self {
        self.retry_in = retry_in.filter(|_| self.retry == ApplyRetry::Scheduled);
        self
    }

    /// Whether this program could name the cause, or fell back to [`GENERIC`]; a surface with a
    /// line to spend then points at the log, where the untranslated reason is.
    #[must_use]
    pub(crate) fn cause_is_named(&self) -> bool {
        named_cause(&self.reason).is_some()
    }

    /// The clause that says what happens next: whether the reader has to do anything.
    fn next_step(&self) -> String {
        match (self.retry, self.retry_in) {
            (ApplyRetry::Scheduled, Some(wait)) => {
                format!("It will try again by itself in about {}.", about(wait))
            }
            (ApplyRetry::Scheduled, None) => "It will try again by itself.".to_string(),
            (ApplyRetry::ManualOnly, _) => "It will not try again until you ask.".to_string(),
            (ApplyRetry::NewerRelease, _) => format!(
                "Starting it again would roll it back; {}.",
                crate::update_words::NEWER_RELEASE_REPLACES
            ),
            (ApplyRetry::NeedsPerson(remedy), _) => remedy.sentence().to_string(),
        }
    }

    /// Whether the standing reason is one only a person can clear
    /// (`aterm_update::refusal_needs_person`): the surfaces stop promising a retry and the
    /// auto-apply lane stops scheduling one.
    #[must_use]
    pub(crate) fn needs_person(reason: &str) -> bool {
        aterm_update::refusal_needs_person(reason)
    }

    /// The full sentence, for a detail row with a line to spend: attempts, cause, and what
    /// happens next.
    #[must_use]
    pub(crate) fn sentence(&self) -> String {
        let (full, _) = clauses(&self.reason);
        format!(
            "aterm tried to update {} and {full}. {}",
            times(self.attempts),
            self.next_step()
        )
    }

    /// The one-line tail for a menu row ("Install aterm vX — tried twice, didn't start; will
    /// try again"). The Version-menu item and its palette mirror are single lines and are the
    /// control that retries (a held build's row opens the details instead), so the tail ends
    /// on what happens next, in the update row's words, never "apply".
    #[must_use]
    pub(crate) fn row_tail(&self) -> String {
        let (_, short) = clauses(&self.reason);
        let next = match self.retry {
            ApplyRetry::Scheduled => "will try again",
            ApplyRetry::ManualOnly => "try again now",
            ApplyRetry::NewerRelease => crate::update_words::NEWER_RELEASE_REPLACES,
            ApplyRetry::NeedsPerson(remedy) => remedy.row_tail(),
        };
        format!("tried {}, {short}; {next}", times(self.attempts))
    }

    /// The phone-width detail: the same two facts with the sentence scaffolding
    /// removed.
    #[must_use]
    pub(crate) fn compact(&self) -> String {
        let (_, short) = clauses(&self.reason);
        let next = match self.retry {
            ApplyRetry::Scheduled => "will try again",
            ApplyRetry::ManualOnly => "ask to retry",
            ApplyRetry::NewerRelease => crate::update_words::NEWER_RELEASE_REPLACES,
            ApplyRetry::NeedsPerson(remedy) => remedy.row_tail(),
        };
        format!("Tried {}, {short} \u{b7} {next}.", times(self.attempts))
    }

    /// The tersest truthful rung, for a status card with ~16 characters (a compact host at 2x
    /// Dynamic Type). The count survives: it separates "downloaded, waiting for you" from
    /// "tried and failed".
    #[must_use]
    pub(crate) fn micro(&self) -> String {
        if self.attempts == 1 {
            "1 try failed.".to_string()
        } else {
            format!("{} tries failed.", self.attempts)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ApplyRetry, ApplyTrouble, GENERIC, PersonRemedy, clauses};

    /// Every reason this program can write into `last_apply_error`, copied from the producers.
    /// Up to the boot-trial pair, each is a `UpdateHandoffCompletion` `detail` from
    /// `app_update_handoff.rs`, which `App::finish_update_handoff` wraps in
    /// `"overlap handoff failed safely: "` before `record_apply_failure`; the last two come from
    /// `aterm-update`'s `install.rs` when a boot trial cannot be recovered.
    const REAL_LEDGER_REASONS: &[&str] = &[
        // `run_handoff_decision`: format!("handoff proof ended {proof_outcome:?}")
        // over the five variants `wait_handoff_ready` can return.
        "overlap handoff failed safely: handoff proof ended ChildDied",
        "overlap handoff failed safely: handoff proof ended TimedOut",
        "overlap handoff failed safely: handoff proof ended AdoptionMismatch",
        "overlap handoff failed safely: handoff proof ended ActivityRevoked",
        "overlap handoff failed safely: handoff proof ended Rejected",
        // The decision loop's own four rejections.
        "overlap handoff failed safely: event loop closed before final handoff admission",
        "overlap handoff failed safely: structural activity revoked handoff before Commit",
        "overlap handoff failed safely: a handed-off PTY session closed before Commit",
        "overlap handoff failed safely: final handoff admission rejected before Commit",
        "overlap handoff failed safely: main-thread final handoff decision timed out",
        // Activity, before the decision loop was ever reached.
        "overlap handoff failed safely: activity revoked handoff during physical preparation",
        "overlap handoff failed safely: structural activity revoked the handoff before any \
         descriptor was sent",
        // The out-of-band lane's two.
        "overlap handoff failed safely: the launched successor never claimed the handoff: \
         timed out after 15s",
        "overlap handoff failed safely: the handoff descriptors could not be delivered: \
         Broken pipe (os error 32)",
        // `send_handoff_preparation_failure`, every call site.
        "overlap handoff failed safely: installed bundle failed pre-park verification: \
         code object is not signed at all",
        "overlap handoff failed safely: installed bundle failed pre-park verification: \
         install location not writable: /Applications/aterm.app; this install cannot update \
         itself: move aterm.app to a folder you can write to (such as ~/Applications), or \
         have an administrator install the update",
        "overlap handoff failed safely: staged update failed pre-park verification: \
         staged build 9 is not newer than 9",
        "overlap handoff failed safely: could not persist the bounded handoff layout",
        "overlap handoff failed safely: could not write the authenticated handoff manifest",
        "overlap handoff failed safely: handoff screen carry did not match the committed \
         screen digest",
        "overlap handoff failed safely: could not write the attempt-bound handoff layout",
        "overlap handoff failed safely: handoff identity set of 4294967296 sessions exceeds \
         the proof format",
        "overlap handoff failed safely: could not create the adoption-proof channel",
        "overlap handoff failed safely: could not create the handoff-commit channel",
        "overlap handoff failed safely: handoff process could not start: \
         Resource temporarily unavailable (os error 35)",
        // `ProducerFailed`: this process's own writes, pipes and descriptors, each carrying
        // its `io::ErrorKind`, and the park's own stand-downs.
        "overlap handoff failed safely: could not write the authenticated handoff manifest \
         (storage full)",
        // `ManifestOverCap`, filed `CaptureRefused`.
        "overlap handoff failed safely: could not write the authenticated handoff manifest \
         (the open tabs' records are over the 4194304-byte limit one handover carries)",
        "overlap handoff failed safely: could not write the attempt-bound handoff layout \
         (write restore manifest: No space left on device (os error 28))",
        "overlap handoff failed safely: could not create the adoption-proof channel \
         (too many open files)",
        "overlap handoff failed safely: could not reserve child-only PTY descriptors \
         (too many open files)",
        "overlap handoff failed safely: more sessions opened than one descriptor message \
         carries",
        "overlap handoff failed safely: a handed-off PTY would not answer fstat, so the \
         out-of-band proof term cannot be computed",
        "overlap handoff failed safely: handoff activity identity space is exhausted",
        "overlap handoff failed safely: no private control directory to write the handoff \
         manifest into",
        "overlap handoff failed safely: event loop closed before the park",
        "overlap handoff failed safely: the attempt record was dropped before the park",
        // The fork lane's hold deadline, typed `TimedOut`.
        "overlap handoff failed safely: the outgoing process did not park within 120 s",
        // `CaptureRefused`: the launched lane's stand-down detail and the fork lane's
        // synchronous message (no prefix), both `CaptureRefusal`'s Display.
        "overlap handoff failed safely: the park's capture refused session 3: visible \
         checkpoint set could not be committed canonically: too many sessions; update \
         handoff stayed in place; the lane retries once the desk changes",
        "overlap handoff failed safely: the park's capture refused: handoff layout could not \
         be committed canonically; the lane retries once the desk changes",
        // A desk past a cap no update carries (`RestoreManifest::why_not_whole`).
        "overlap handoff failed safely: the park's capture refused: 65 windows are open, more \
         than the 64 an update can carry",
        "the park's capture refused: 16500 tabs are open, more than the 16384 (64 windows of \
         256) an update can carry",
        "the park's capture refused: tab 1 of window 1 has 300 panes, more than the 256 in one \
         tab an update can carry",
        "the park's capture refused session 7: visible checkpoint set could not be committed \
         canonically: duplicate local id 7; update handoff stayed in place",
        // `screen_digest_refs`, via the park: the refusal names the session, field and bound.
        "overlap handoff failed safely: visible checkpoint set could not be committed \
         canonically: session 0: meta out of bounds at 55x149 with 0 carried line(s): \
         saved_cursor_main.cursor_row=55 must be below rows=55",
        // A pre-park check that did not finish: the verifier's budget, and the apply lock a
        // sibling held.
        "overlap handoff failed safely: staged update failed pre-park verification: bundle \
         policy: codesign --verify (team-pinned) ran past this apply's verification budget; \
         treating as a rejection (a passing condition, not a verdict on the update)",
        "overlap handoff failed safely: installed bundle failed pre-park verification: \
         pre-verify lock: another process has held the update lock for more than 10s (a \
         passing condition, not a verdict on the update)",
        "staged update failed pre-park verification: the installed bundle at \
         /Applications/aterm.app could not be verified just then: bundle policy: spctl \
         assessment did not finish within 30s; treating as a rejection (a passing \
         condition, not a verdict on the update); the terminal was left untouched",
        // `aterm_update::install`, both boot-trial recovery arms.
        "update trial for build 1787699398 was unrecoverable across 3 launches of build \
         1787690000; disarmed the boot sentinel to keep updates possible",
        "trial recovery proof failed 3x (trial receipt is missing); disarmed the boot \
         sentinel to keep updates possible",
    ];

    /// No reason this program writes survives into any rendering, nor its most alarming words
    /// ("failed safely", "Commit").
    #[test]
    fn no_reason_this_program_writes_can_reach_a_surface_verbatim() {
        for reason in REAL_LEDGER_REASONS {
            let trouble = ApplyTrouble::new(2, reason, ApplyRetry::Scheduled).expect("trouble");
            assert!(
                trouble.cause_is_named(),
                "a reason this program writes must be one this program can NAME, or the \
                 table was written from the type instead of the data: {reason}"
            );
            for rendering in [
                trouble.sentence(),
                trouble.row_tail(),
                trouble.compact(),
                trouble.micro(),
            ] {
                for leak in [
                    "failed safely",
                    "handoff",
                    "Commit",
                    "ChildDied",
                    "TimedOut",
                    "AdoptionMismatch",
                    "ActivityRevoked",
                    "Rejected",
                    "PreparationFailed",
                    "sentinel",
                    "os error",
                ] {
                    assert!(
                        !rendering.contains(leak),
                        "{leak:?} is engineering text, not something a Version menu may \
                         say: {rendering:?} (from {reason:?})"
                    );
                }
            }
        }
    }

    /// Each real reason maps to the right cause, not merely a safe one (a table that matched
    /// nothing would still pass the test above).
    #[test]
    fn apply_lane_reasons_this_program_can_actually_write_map_to_the_right_cause() {
        let full = |reason: &str| clauses(reason).0;
        let short = |reason: &str| clauses(reason).1;

        assert_eq!(
            full("overlap handoff failed safely: handoff proof ended ChildDied"),
            "the new version did not finish starting"
        );
        assert_eq!(
            short("overlap handoff failed safely: handoff proof ended ChildDied"),
            "didn\u{2019}t start"
        );
        // A timeout ("the machine was loaded") and a death ("the successor could not run")
        // are different advice.
        assert_eq!(
            full("overlap handoff failed safely: handoff proof ended TimedOut"),
            "the new version did not finish starting in time"
        );
        assert_eq!(
            full(
                "overlap handoff failed safely: the launched successor never claimed the \
                 handoff: timed out"
            ),
            "the new version did not finish starting in time"
        );
        assert_eq!(
            full("overlap handoff failed safely: handoff proof ended AdoptionMismatch"),
            "the new version did not take over the open sessions"
        );
        // Activity wins over "before Commit": "structural activity revoked handoff before
        // Commit" carries both markers and is typed ActivityRevoked, and "your terminal was
        // busy" corrects itself.
        assert_eq!(
            full(
                "overlap handoff failed safely: structural activity revoked handoff before Commit"
            ),
            "the terminal was too busy to hand over"
        );
        assert_eq!(
            full(
                "overlap handoff failed safely: activity revoked handoff during physical \
                 preparation"
            ),
            "the terminal was too busy to hand over"
        );
        assert_eq!(
            full("overlap handoff failed safely: a handed-off PTY session closed before Commit"),
            "the handover was called off before it finished"
        );
        assert_eq!(
            full("overlap handoff failed safely: main-thread final handoff decision timed out"),
            "the handover was called off before it finished"
        );
        // Nothing was started on the preparation lane, so none of these borrows the "did not
        // start" wording.
        for prepared in [
            "overlap handoff failed safely: could not persist the bounded handoff layout",
            "overlap handoff failed safely: could not create the adoption-proof channel",
            "overlap handoff failed safely: handoff process could not start: too many threads",
            "overlap handoff failed safely: installed bundle failed pre-park verification: nope",
        ] {
            assert_eq!(
                full(prepared),
                "the handover could not be set up",
                "{prepared}"
            );
        }
        assert_eq!(
            full(
                "trial recovery proof failed 3x (receipt missing); disarmed the boot sentinel \
                 to keep updates possible"
            ),
            "the new version would not run and aterm went back to this one"
        );
        // This process's own failures are set-up failures, not a candidate that did not
        // start…
        for producer in [
            "overlap handoff failed safely: could not write the authenticated handoff manifest \
             (storage full)",
            "overlap handoff failed safely: could not reserve child-only PTY descriptors \
             (too many open files)",
            "overlap handoff failed safely: no private control directory to write the handoff \
             manifest into",
        ] {
            assert_eq!(
                full(producer),
                "the handover could not be set up",
                "{producer}"
            );
        }
        // …except the one a person can act on, and the hold that timed out.
        assert_eq!(
            short(
                "overlap handoff failed safely: more sessions opened than one descriptor \
                 message carries"
            ),
            "too many tabs"
        );
        assert_eq!(
            short(
                "overlap handoff failed safely: more sessions opened than the successor's \
                 descriptor grant carries (63 open, its rendezvous claim carries at most 62)"
            ),
            "too many tabs"
        );
        // The tabs' records over the manifest's cap: closing tabs moves it, so
        // it is not the "could not be set up" its manifest half would match.
        #[cfg(unix)]
        {
            let over_cap = format!(
                "overlap handoff failed safely: could not write the authenticated handoff \
                 manifest ({})",
                crate::seamless::WriteOutgoingFailure::ManifestOverCap
            );
            assert_eq!(full(&over_cap), super::TABS_OVER_HANDOVER);
            assert_eq!(short(&over_cap), "too many tabs");
        }
        assert_eq!(
            full("overlap handoff failed safely: the outgoing process did not park within 120 s"),
            "the terminal was too busy to hand over"
        );
        assert_eq!(
            full(
                "overlap handoff failed safely: the handoff descriptors could not be delivered: \
                 the handoff's proof deadline passed before the grant was sent (the successor \
                 had dialed; this process's preparation after the park outran it), so no \
                 descriptor left"
            ),
            "the terminal was too busy to hand over"
        );
        // A capture refusal says it could not capture: never "could not be set up" (which its
        // layout half matches) nor "too busy".
        assert_eq!(
            full(
                "overlap handoff failed safely: the park's capture refused session 3: visible \
                 checkpoint set could not be committed canonically: too many sessions"
            ),
            "a session\u{2019}s screen could not be captured for the handover"
        );
        assert_eq!(
            full(
                "overlap handoff failed safely: the park's capture refused: handoff layout could \
                 not be committed canonically"
            ),
            "the open tabs could not be captured for the handover"
        );
        // …and a desk past a cap says so, not that it could not be captured.
        assert_eq!(
            full(
                "overlap handoff failed safely: the park's capture refused: tab 1 of window 1 is \
                 split 33 levels deep, more than the 31 an update can carry"
            ),
            "more was open than an update can carry"
        );
    }

    /// An unrecognised reason degrades to one bounded generic clause and does not leak. The
    /// submission lane's free-form messages (`UpdateHandoffStartError::failed`, the
    /// preflight's verdicts) are deliberately not enumerated, so the contract is the fallback,
    /// and `cause_is_named` reports that the specific answer is only in the log.
    #[test]
    fn an_unrecognised_reason_degrades_to_a_safe_generic_clause() {
        for stranger in [
            "updater apply preflight became stale",
            "native update preflight returned Ready without safety evidence",
            "could not start updater worker: too many threads",
            "\u{1f4a5} PANIC at src/lib.rs:1: attempt to subtract with overflow",
        ] {
            assert_eq!(
                clauses(stranger),
                (GENERIC.0.to_string(), GENERIC.1.to_string())
            );
            let trouble = ApplyTrouble::new(1, stranger, ApplyRetry::ManualOnly).expect("trouble");
            assert!(
                !trouble.cause_is_named(),
                "an unlisted reason must ADMIT that it is unlisted, so the detail row \
                 knows to point at the log: {stranger}"
            );
            for rendering in [trouble.sentence(), trouble.row_tail(), trouble.compact()] {
                assert!(
                    !rendering.contains("preflight")
                        && !rendering.contains("PANIC")
                        && !rendering.contains("updater worker"),
                    "{rendering:?}"
                );
            }
        }
        // An empty reason takes the same clause, not a second, differently worded one.
        assert_eq!(
            clauses("   "),
            (GENERIC.0.to_string(), GENERIC.1.to_string())
        );
    }

    /// A CLEAN STAGE IS NOT A FAILURE. Zero attempts is the state of every update
    /// that downloaded and is waiting for a quiet window — deferrals and blocks are
    /// recorded as refusals and advance no streak — so it must produce no trouble at
    /// all, and therefore no failure wording anywhere.
    #[test]
    fn a_clean_stage_reads_as_no_trouble_at_all() {
        assert_eq!(ApplyTrouble::new(0, "", ApplyRetry::Scheduled), None);
        assert_eq!(
            ApplyTrouble::new(
                0,
                "overlap handoff failed safely: handoff proof ended ChildDied",
                ApplyRetry::ManualOnly,
            ),
            None,
            "a reason left over from a streak that has expired is not an attempt"
        );
        // NEGATIVE CONTROL, the field case (owner's machine, 2026-08-21): a stage
        // attempted twice is trouble, and every rendering — the tersest too —
        // says the attempt count, the fact that separates "waiting for you"
        // from "tried and failed".
        let tried = |n| {
            let t = ApplyTrouble::new(
                n,
                "overlap handoff failed safely: handoff proof ended ChildDied",
                ApplyRetry::Scheduled,
            )
            .expect("an attempt is trouble");
            [t.sentence(), t.row_tail(), t.compact(), t.micro()]
        };
        for (twice, thrice) in tried(2).into_iter().zip(tried(3)) {
            assert_ne!(twice, thrice, "the count is said");
        }
    }

    /// The two retry regimes are different sentences: one asks the reader to act, the other to
    /// do nothing.
    #[test]
    fn the_manual_only_latch_changes_what_the_reader_is_told_to_do() {
        let auto = ApplyTrouble::new(2, "handoff proof ended TimedOut", ApplyRetry::Scheduled)
            .expect("trouble");
        let manual = ApplyTrouble::new(2, "handoff proof ended TimedOut", ApplyRetry::ManualOnly)
            .expect("trouble");
        assert_ne!(auto.sentence(), manual.sentence());
        assert!(
            auto.sentence().contains("try again by itself"),
            "{}",
            auto.sentence()
        );
        assert!(
            manual.sentence().contains("until you ask"),
            "{}",
            manual.sentence()
        );
        assert!(
            manual.row_tail().ends_with("try again now"),
            "the manual-only row must name the action: {}",
            manual.row_tail()
        );
        assert!(
            !manual.row_tail().contains("apply") && !auto.row_tail().contains("apply"),
            "the Version menu's words are the update row's (install), never apply: {} / {}",
            manual.row_tail(),
            auto.row_tail()
        );
        assert!(
            !manual.row_tail().contains("restart") && !auto.row_tail().contains("restart"),
            "the action is an in-place install, never a restart: {} / {}",
            manual.row_tail(),
            auto.row_tail()
        );
    }

    /// THE STAND-DOWN HAS A HORIZON (2026-09-14, audit OBS-5): a scheduled retry
    /// that knows its wait says it at planning precision; one that does not keeps
    /// the bare promise; and a wait under any other promise is dropped, not
    /// rendered into a contradiction.
    #[test]
    fn a_scheduled_retry_says_how_long_when_it_knows() {
        use std::time::Duration;
        let reason = "handoff proof ended ChildDied";
        let sentence = |retry, wait| {
            ApplyTrouble::new(3, reason, retry)
                .expect("trouble")
                .with_retry_in(wait)
                .sentence()
        };
        let unknown = sentence(ApplyRetry::Scheduled, None);
        let six_hours = sentence(ApplyRetry::Scheduled, Some(Duration::from_secs(21_599)));
        assert_ne!(six_hours, unknown, "a known wait is said");
        assert!(six_hours.contains('6'), "{six_hours}");
        assert_eq!(
            sentence(ApplyRetry::ManualOnly, Some(Duration::from_secs(3600))),
            sentence(ApplyRetry::ManualOnly, None),
            "a wait under a manual-only promise is not rendered"
        );
    }

    /// THE 2026-09-14 INCIDENT, AT THE WORDS. A hand-built, ad-hoc-signed
    /// `/Applications/aterm.app` refused six automatic applies of v0.85.0 over ~8 h
    /// because it cannot be the rollback source the swap installs. After 368d6b9e0
    /// the ledger reason names the installed bundle (this is the exact string
    /// `start_unix_update_handoff` now mints from the cached verdict). The table maps
    /// it — by its `pre-park verification` token — to "the handover could not be set
    /// up", which is (a) true of every preparation failure and therefore says nothing
    /// about THIS one, and (b) paired with "It will try again by itself", a promise
    /// the lane keeps for a day of six-hour stand-downs while nothing can change:
    /// the boot-time fallback runs the same team-pinned `codesign` gate
    /// (`install.rs` step 7, "current installed rollback source is not verified") and
    /// refuses too. This is the one apply failure whose remedy is entirely the
    /// person's — put the signed release bundle back — and no surface says so.
    #[test]
    fn observability_audit_a_rollback_source_refusal_names_the_installed_copy_and_the_remedy() {
        let reason = "installed bundle failed pre-park verification: the installed bundle at \
                      /Applications/aterm.app cannot be the rollback source the swap installs: \
                      bundle policy: codesign --verify (team-pinned requirement) failed: \
                      /Applications/aterm.app: valid on disk\n/Applications/aterm.app: \
                      satisfies its Designated Requirement\ntest-requirement: code failed to \
                      satisfy specified code requirement(s); the terminal was left untouched";
        let trouble = ApplyTrouble::new(
            6,
            reason,
            ApplyRetry::NeedsPerson(PersonRemedy::of_refusal(reason)),
        )
        .expect("trouble");
        let (full, short) = clauses(reason);
        assert!(
            trouble.cause_is_named(),
            "a reason this program writes must be one it can name"
        );
        assert!(
            full.contains("installed") || full.contains("Applications"),
            "the cause must name the INSTALLED copy — the staged update is fine and the \
             generic preparation clause blames neither: {full}"
        );
        assert!(
            trouble.sentence().contains("Install the signed release"),
            "the current NeedsPerson next step must carry the remedy: {}",
            trouble.sentence()
        );
        assert!(
            short != "couldn\u{2019}t be set up",
            "the row tail must not read as a transient setup hiccup: {short}"
        );
        // Retrying cannot change the bytes in /Applications, so the sentence must not
        // end on the automatic lane's reassurance.
        let sentence = trouble.sentence();
        assert!(
            !sentence.contains("try again by itself"),
            "a structural refusal must not promise a retry that cannot succeed: {sentence}"
        );
    }

    /// Round seven, #3: an install whose folder cannot be written is refused before
    /// the park, as a person's to clear — and the remedy is to move the app or ask an
    /// administrator, never to reinstall the signed release (which would land in the
    /// same folder and be refused again).
    // The bundle apply lane and its ledger (`aterm_update::apply_lane_report`) are
    // macOS's: a Linux update replaces the executable on disk and hands nothing off.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_non_writable_install_location_names_its_own_remedy() {
        let reason = "overlap handoff failed safely: installed bundle failed pre-park \
                      verification: install location not writable: /Applications/aterm.app; \
                      this install cannot update itself: move aterm.app to a folder you can \
                      write to (such as ~/Applications), or have an administrator install the \
                      update";
        assert!(ApplyTrouble::needs_person(reason));
        let trouble = ApplyTrouble::new(
            1,
            reason,
            ApplyRetry::NeedsPerson(PersonRemedy::of_refusal(reason)),
        )
        .expect("trouble");
        assert!(trouble.cause_is_named());
        let (full, short) = clauses(reason);
        assert_eq!(
            full,
            "aterm could not write to the folder it is installed in"
        );
        assert_eq!(short, "install folder not writable");
        let sentence = trouble.sentence();
        assert!(sentence.contains("~/Applications"), "{sentence}");
        assert!(!sentence.contains("signed release"), "{sentence}");
        assert!(trouble.row_tail().ends_with("move aterm or ask an admin"));
        assert!(trouble.compact().contains("move aterm or ask an admin"));
    }

    /// The remedy is the standing block's, not the latest reason's: a person's retry can fail
    /// for an ordinary reason before the folder is asked, and the cause clause says so while
    /// the next step stays the block's remedy. Control: the installed copy's block over the
    /// same booking still says the signed release.
    #[test]
    fn a_standing_block_names_its_own_remedy_over_a_later_reason() {
        let passing = "overlap handoff failed safely: staged update failed pre-park \
                       verification: the staged update could not be verified just then: \
                       codesign --verify ran past this apply's verification budget; treating as \
                       a rejection (a passing condition, not a verdict on the update)";
        assert!(aterm_update::is_passing_refusal(passing));
        assert!(!ApplyTrouble::needs_person(passing));
        for (remedy, says, never) in [
            (
                PersonRemedy::MoveOrAskAdmin,
                (
                    "Move aterm to a folder you can write to",
                    "move aterm or ask an admin",
                ),
                "signed release",
            ),
            (
                PersonRemedy::InstallSignedRelease,
                (
                    "Install the signed release from the release DMG, then retry.",
                    "install the signed release",
                ),
                "Move aterm",
            ),
        ] {
            let trouble =
                ApplyTrouble::new(2, passing, ApplyRetry::NeedsPerson(remedy)).expect("trouble");
            let sentence = trouble.sentence();
            assert!(
                sentence.contains("could not be checked just then") && sentence.contains(says.0),
                "the last attempt's cause, then the block's remedy: {sentence}"
            );
            assert!(
                trouble.row_tail().ends_with(says.1),
                "{}",
                trouble.row_tail()
            );
            assert!(trouble.compact().contains(says.1), "{}", trouble.compact());
            for surface in [sentence, trouble.row_tail(), trouble.compact()] {
                assert!(!surface.contains(never), "{remedy:?}: {surface}");
                assert!(!surface.contains("by itself"), "{surface}");
            }
        }
    }

    /// A check that did not finish is named as a moment, never as the person's to clear, also
    /// in the older shape that wraps a passing error in the installed-source key.
    #[test]
    fn a_check_that_did_not_finish_is_named_as_a_moment_and_never_a_person_s() {
        let passing = "overlap handoff failed safely: installed bundle failed pre-park \
                       verification: the installed bundle at /Applications/aterm.app could \
                       not be verified just then: bundle policy: codesign --verify \
                       (team-pinned) ran past this apply's verification budget; treating as \
                       a rejection (a passing condition, not a verdict on the update)";
        let legacy = "installed bundle failed pre-park verification: the installed bundle at \
                      /Applications/aterm.app cannot be the rollback source the swap installs: \
                      bundle policy: spctl assessment did not finish within 30s; treating as \
                      a rejection (a passing condition, not a verdict on the update); this \
                      install cannot update itself";
        for reason in [passing, legacy] {
            assert!(aterm_update::is_passing_refusal(reason));
            assert!(
                !ApplyTrouble::needs_person(reason),
                "a moment is never a person's to clear: {reason}"
            );
            let (full, short) = clauses(reason);
            assert_eq!(full, "the new version could not be checked just then");
            assert_eq!(short, "couldn\u{2019}t check it just then");
            let trouble = ApplyTrouble::new(1, reason, ApplyRetry::Scheduled)
                .expect("trouble")
                .with_retry_in(Some(std::time::Duration::from_secs(599)));
            assert!(
                trouble
                    .sentence()
                    .ends_with("It will try again by itself in about 10 min."),
                "{}",
                trouble.sentence()
            );
        }
    }

    #[test]
    fn historical_installed_source_failure_only_requests_repair_while_currently_blocked() {
        for (reason, historical) in [
            (
                "the installed bundle cannot be the rollback source",
                "the installed copy could not be verified for replacement",
            ),
            (
                "aterm does not run from an installed bundle",
                "aterm was not running from an installed copy the updater could replace",
            ),
        ] {
            for retry in [
                ApplyRetry::NeedsPerson(PersonRemedy::InstallSignedRelease),
                ApplyRetry::Scheduled,
                ApplyRetry::ManualOnly,
                ApplyRetry::NewerRelease,
            ] {
                let trouble = ApplyTrouble::new(2, reason, retry).expect("historical failure");
                assert!(trouble.sentence().contains(historical));
                for surface in [trouble.sentence(), trouble.row_tail(), trouble.compact()] {
                    assert_eq!(
                        surface
                            .to_lowercase()
                            .contains("install the signed release"),
                        matches!(retry, ApplyRetry::NeedsPerson(_)),
                        "historical reason cannot override current policy: {surface}"
                    );
                }
                match retry {
                    ApplyRetry::Scheduled => {
                        assert!(trouble.sentence().ends_with("It will try again by itself."));
                        assert!(trouble.row_tail().ends_with("will try again"));
                    }
                    ApplyRetry::ManualOnly => assert!(
                        trouble
                            .sentence()
                            .ends_with("It will not try again until you ask.")
                    ),
                    ApplyRetry::NewerRelease => {
                        // No surface offers the start the boot trial rolls back.
                        for surface in [trouble.sentence(), trouble.row_tail(), trouble.compact()] {
                            assert!(
                                surface.contains(crate::update_words::NEWER_RELEASE_REPLACES)
                                    && !surface.contains("try again now")
                                    && !surface.contains("ask")
                                    && !surface.contains("retry"),
                                "{surface}"
                            );
                        }
                    }
                    ApplyRetry::NeedsPerson(_) => {
                        assert!(!trouble.sentence().contains("by itself"));
                    }
                }
            }
        }
    }
}
