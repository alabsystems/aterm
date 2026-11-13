// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The apply lane's refusal, in words a person can act on.

/// Whether the automatic apply lane will still try this exact staged build by itself, or has stopped and waits to be asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ApplyRetry {
    /// The cause requires user action before any retry can succeed.
    NeedsPerson(PersonRemedy),
    /// The lane still intends this artifact.
    Scheduled,
    /// The manual-only latch holds with no lapse deadline (or automatic apply is switched off).
    ManualOnly,
    /// The stood-down build is the installed activation and its boot trial has no launch to spare (`App::latched_activation_trial_spent`).
    NewerRelease,
}

/// What a person does about a refusal only a person can clear, carried by [`ApplyRetry::NeedsPerson`] from the block that answered it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PersonRemedy {
    /// The install location's folder cannot be written by this user (`aterm_update::is_install_location_refusal`).
    MoveOrAskAdmin,
    /// The installed copy cannot be the swap's rollback source, or this process does not run from one.
    InstallSignedRelease,
}

impl PersonRemedy {
    /// The remedy for a user-clearable `reason` when no standing block is present.
    #[must_use]
    pub(crate) fn of_refusal(reason: &str) -> Self {
        if aterm_update::is_install_location_refusal(reason) {
            Self::MoveOrAskAdmin
        } else {
            Self::InstallSignedRelease
        }
    }

    /// The sentence, for a detail with a line to spend.
    #[must_use]
    pub(crate) const fn sentence(self) -> &'static str {
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
    pub(crate) const fn row_tail(self) -> &'static str {
        match self {
            Self::MoveOrAskAdmin => "move aterm or ask an admin",
            Self::InstallSignedRelease => "install the signed release",
        }
    }
}

/// A standing apply-lane failure for the build a surface is offering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApplyTrouble {
    /// Consecutive failed applies of the artifact on offer (the ledger's
    /// `apply_failures_for_target`, not the running-build-scoped escalation streak).
    attempts: u32,
    /// The ledger's own `last_apply_error` prose, the raw `apply_failure=` value, kept verbatim
    /// for the log and the control verb; no rendering here prints it.
    reason: String,
    /// Whether the automatic lane will try again unaided.
    retry: ApplyRetry,
    /// How long until the scheduled retry, when the lane can say, so a six-hour stand-down does
    /// not read like a two-minute one.
    retry_in: Option<std::time::Duration>,
}

/// `(full clause, short clause)` for a reason this program does not recognise: true of every
/// string that reaches this slot (an apply attempted that did not complete) and no guess at a
/// mechanism ("it did not start" would be wrong for an adoption mismatch or a stale
/// preflight).
const GENERIC: (&str, &str) = ("it did not finish installing", "didn\u{2019}t finish");

/// The open tabs' records over what one handover carries, in a person's words.
pub(crate) const TABS_OVER_HANDOVER: &str =
    "too many tabs were open to move to the new version at once";

/// The apply outcomes this program can name, as `(a substring of a real ledger reason, full
/// clause, short clause)`.
const CAUSES: &[(&str, &str, &str)] = &[
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
    (
        "activity revoked",
        "the terminal was too busy to hand over",
        "terminal too busy",
    ),
    (
        "never claimed the handoff",
        "the new version did not finish starting in time",
        "too slow to start",
    ),
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
    (
        "the park's capture refused session",
        "a session\u{2019}s screen could not be captured for the handover",
        "couldn\u{2019}t capture a session",
    ),
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
    (
        "did not park within",
        "the terminal was too busy to hand over",
        "terminal too busy",
    ),
    (
        "descriptor message carries",
        TABS_OVER_HANDOVER,
        "too many tabs",
    ),
    (
        "descriptor grant carries",
        TABS_OVER_HANDOVER,
        "too many tabs",
    ),
    ("records are over the", TABS_OVER_HANDOVER, "too many tabs"),
    (
        aterm_update::PASSING_REFUSAL_KEY,
        "the new version could not be checked just then",
        "couldn\u{2019}t check it just then",
    ),
    (
        "cannot be the rollback source",
        "the installed copy could not be verified for replacement",
        "installed copy did not verify",
    ),
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
    (
        "could not be committed canonically",
        "a session\u{2019}s screen could not be captured for the handover",
        "couldn\u{2019}t capture a session",
    ),
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

/// The full clause for a known reason, or `None` when [`GENERIC`] would repeat the title.
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
    /// The standing trouble for a staged build, or `None` when there is none.
    #[must_use]
    pub(crate) fn new(attempts: u32, reason: &str, retry: ApplyRetry) -> Option<Self> {
        (attempts > 0).then(|| Self {
            attempts,
            reason: reason.trim().to_string(),
            retry,
            retry_in: None,
        })
    }

    /// Name how long until the scheduled retry.
    #[must_use]
    pub(crate) fn with_retry_in(mut self, retry_in: Option<std::time::Duration>) -> Self {
        self.retry_in = retry_in.filter(|_| self.retry == ApplyRetry::Scheduled);
        self
    }

    /// Whether this program could name the cause, or fell back to [`GENERIC`].
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

    /// Whether the standing reason is one only a person can clear (`aterm_update::refusal_needs_person`).
    #[must_use]
    pub(crate) fn needs_person(reason: &str) -> bool {
        aterm_update::refusal_needs_person(reason)
    }

    /// The full sentence, for a detail row with a line to spend.
    #[must_use]
    pub(crate) fn sentence(&self) -> String {
        let (full, _) = clauses(&self.reason);
        format!(
            "aterm tried to update {} and {full}. {}",
            times(self.attempts),
            self.next_step()
        )
    }

    /// The one-line tail for a menu row ("Install aterm vX — tried twice, didn't start; will try again").
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

    /// The phone-width detail.
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

    /// The tersest truthful rung, for a status card with ~16 characters (a compact host at 2x Dynamic Type).
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
#[path = "update_apply_trouble_tests.rs"]
mod tests;
