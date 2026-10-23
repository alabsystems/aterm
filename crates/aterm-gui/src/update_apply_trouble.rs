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

/// A standing apply-lane failure for the build a surface is offering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApplyTrouble {
    /// Consecutive failed applies of the artifact on offer (the ledger's
    attempts: u32,
    /// The ledger's own `last_apply_error` prose, the raw `apply_failure=` value, kept verbatim
    reason: String,
    /// Whether the automatic lane will try again unaided.
    retry: ApplyRetry,
    /// How long until the scheduled retry, when the lane can say, so a six-hour stand-down does
    retry_in: Option<std::time::Duration>,
}

/// `(full clause, short clause)` for a reason this program does not recognise: true of every
const GENERIC: (&str, &str) = ("it did not finish installing", "didn\u{2019}t finish");

/// The open tabs' records over what one handover carries, in a person's words.
pub(crate) const TABS_OVER_HANDOVER: &str =
    "too many tabs were open to move to the new version at once";

/// The apply outcomes this program can name, as `(a substring of a real ledger reason, full
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
fn clauses(reason: &str) -> (String, String) {
    let (full, short) = named_cause(reason).unwrap_or(GENERIC);
    (full.to_string(), short.to_string())
}

/// The full clause for a known reason, or `None` when [`GENERIC`] would repeat the title.
pub(crate) fn named_clause(reason: &str) -> Option<&'static str> {
    named_cause(reason).map(|(full, _)| full)
}

/// The [`CAUSES`] row this reason matches, if any. One scan, shared by [`clauses`]
fn named_cause(reason: &str) -> Option<(&'static str, &'static str)> {
    let reason = reason.trim();
    CAUSES
        .iter()
        .find(|(token, _, _)| reason.contains(token))
        .map(|(_, full, short)| (*full, *short))
}

/// "6 h" / "30 min" / "a minute" — a wait a sentence can contain, at the
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
mod tests {
    use super::{ApplyRetry, ApplyTrouble, GENERIC, PersonRemedy, clauses};

    /// Every reason this program can write into `last_apply_error`, copied from the producers.
    const REAL_LEDGER_REASONS: &[&str] = &[
        "overlap handoff failed safely: handoff proof ended ChildDied",
        "overlap handoff failed safely: handoff proof ended TimedOut",
        "overlap handoff failed safely: handoff proof ended AdoptionMismatch",
        "overlap handoff failed safely: handoff proof ended ActivityRevoked",
        "overlap handoff failed safely: handoff proof ended Rejected",
        "overlap handoff failed safely: event loop closed before final handoff admission",
        "overlap handoff failed safely: structural activity revoked handoff before Commit",
        "overlap handoff failed safely: a handed-off PTY session closed before Commit",
        "overlap handoff failed safely: final handoff admission rejected before Commit",
        "overlap handoff failed safely: main-thread final handoff decision timed out",
        "overlap handoff failed safely: activity revoked handoff during physical preparation",
        "overlap handoff failed safely: structural activity revoked the handoff before any \
         descriptor was sent",
        "overlap handoff failed safely: the launched successor never claimed the handoff: \
         timed out after 15s",
        "overlap handoff failed safely: the handoff descriptors could not be delivered: \
         Broken pipe (os error 32)",
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
        "overlap handoff failed safely: could not write the authenticated handoff manifest \
         (storage full)",
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
        "overlap handoff failed safely: the outgoing process did not park within 120 s",
        "overlap handoff failed safely: the park's capture refused session 3: visible \
         checkpoint set could not be committed canonically: too many sessions; update \
         handoff stayed in place; the lane retries once the desk changes",
        "overlap handoff failed safely: the park's capture refused: handoff layout could not \
         be committed canonically; the lane retries once the desk changes",
        "overlap handoff failed safely: the park's capture refused: 65 windows are open, more \
         than the 64 an update can carry",
        "the park's capture refused: 16500 tabs are open, more than the 16384 (64 windows of \
         256) an update can carry",
        "the park's capture refused: tab 1 of window 1 has 300 panes, more than the 256 in one \
         tab an update can carry",
        "the park's capture refused session 7: visible checkpoint set could not be committed \
         canonically: duplicate local id 7; update handoff stayed in place",
        "overlap handoff failed safely: visible checkpoint set could not be committed \
         canonically: session 0: meta out of bounds at 55x149 with 0 carried line(s): \
         saved_cursor_main.cursor_row=55 must be below rows=55",
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
        "update trial for build 1787699398 was unrecoverable across 3 launches of build \
         1787690000; disarmed the boot sentinel to keep updates possible",
        "trial recovery proof failed 3x (trial receipt is missing); disarmed the boot \
         sentinel to keep updates possible",
    ];

    /// No reason this program writes survives into any rendering, nor its most alarming words
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
        assert_eq!(
            full(
                "overlap handoff failed safely: the park's capture refused: tab 1 of window 1 is \
                 split 33 levels deep, more than the 31 an update can carry"
            ),
            "more was open than an update can carry"
        );
    }

    /// An unrecognised reason degrades to one bounded generic clause and does not leak. The
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
        assert_eq!(
            clauses("   "),
            (GENERIC.0.to_string(), GENERIC.1.to_string())
        );
    }

    /// A CLEAN STAGE IS NOT A FAILURE. Zero attempts is the state of every update
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
        let sentence = trouble.sentence();
        assert!(
            !sentence.contains("try again by itself"),
            "a structural refusal must not promise a retry that cannot succeed: {sentence}"
        );
    }

    /// an install whose folder cannot be written is refused before
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
