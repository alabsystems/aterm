// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE APPLY LANE'S REFUSAL, IN WORDS A PERSON CAN ACT ON.
//!
//! The updater already knew everything in this module. `aterm ctl update status`
//! prints `failing_applies=2 apply_failure="overlap handoff failed safely: handoff
//! proof ended ChildDied"`, and the health ledger has carried both since the apply
//! class was added. The WINDOW carried neither. Measured on the owner's own machine
//! (2026-08-21): build 1787699398 sat staged for hours behind an "Update to v0.56.0
//! — restart now" menu item while the engine had privately tried twice and watched
//! the successor die both times — the machine was 8x-oversubscribed at the time, so
//! the child was STARVED, not broken (fork-to-execve p99 goes from 6 ms to 206 ms
//! under that load; `aterm-pty/src/unix.rs`). When the load stopped the same builds
//! applied with no intervention at all.
//!
//! Nothing about that state was a mystery to the program, and nothing about it
//! reached the person looking at it. A surface that says "Update ready" while the
//! engine is on its second failed attempt is not merely incomplete — it is telling
//! the user the opposite of what it knows.
//!
//! # What this module is
//!
//! One value ([`ApplyTrouble`]) and one wording law. Every standing "an update is
//! available" affordance — the macOS Version menu, the command palette's Version
//! row, the native Settings `/updates` route, the update card — reads the same value
//! and renders it at its own width, so no surface can drift into claiming a stage is
//! merely waiting when the engine has already tried and failed.
//!
//! # Three rules the wording obeys
//!
//! 1. **No internal text ever reaches the user.** Not the enum name — `ChildDied` is
//!    a proof outcome, and "the new version did not finish starting" is the same
//!    fact addressed to the person who has to decide what to do about it — and not
//!    the ledger's own sentence either. THAT half is the one that is easy to get
//!    backwards: an unrecognised reason must degrade to a bounded generic clause,
//!    never fall through to its own prose, because the prose in that slot is
//!    engineering text ("overlap handoff failed safely: a handed-off PTY session
//!    closed before Commit") and a menu item that reads *the handoff failed safely*
//!    is worse than the plain "Update ready" it replaced. [`CAUSES`] is keyed on
//!    strings this program provably writes; everything else takes [`GENERIC`], and
//!    [`ApplyTrouble::cause_is_named`] tells a surface with room to spare that it
//!    should point at the log, where the untranslated text still is.
//! 2. **A stage waiting for a QUIET WINDOW is not this.** That state is a REFUSAL
//!    (`Deferred`/`Blocked` → `record_apply_refusal`), it deliberately advances no
//!    streak, and it must keep reading as "ready". [`ApplyTrouble::new`] returns
//!    `None` for zero attempts precisely so a patient updater cannot be painted as a
//!    broken one.
//! 3. **"It will retry" and "it will not" are different situations for the reader.**
//!    One of them needs them to act and the other explicitly does not, so
//!    [`ApplyRetry`] is carried, never inferred, and every rendering states it.

/// Whether the automatic apply lane is still going to try this exact staged build by
/// itself, or whether it has stopped and is waiting to be asked.
///
/// This is the half of the state a person cannot guess and MUST know: one of these
/// resolves itself while they keep working, and the other never will. The decision
/// belongs to the event loop (a live `AutoApplyIntent`, or an `AutoApplyManualOnly`
/// latch still carrying a lapse deadline, with automatic apply enabled in config), so
/// it is passed in rather than re-derived here — the same facts
/// `App::react_to_update_apply_outcome` already consults before deciding whether a
/// failure is even worth a pill.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ApplyRetry {
    /// The lane still intends this artifact: a live intent, or a latch with a lapse
    /// deadline, both of which `about_to_wait` comes back to. The honest advice is
    /// "nothing to do".
    ///
    /// The two carriers reach the loop differently and the wording is chosen to be
    /// true of BOTH. An intent's `retry_at` is folded into the event loop's own
    /// deadline (`fold_auto_apply_deadline`), so that one is genuinely armed to
    /// the second; a manual-only latch is released by
    /// `lapse_expired_auto_apply_manual_only`, which runs at the top of
    /// `arm_native_auto_apply` — i.e. the next time the lane is asked about this
    /// artifact at all, which on a perfectly idle machine is the next background
    /// check rather than the latch's own deadline. "By itself" covers both; "a wake
    /// is armed for this instant" would have been a promise only one of them keeps.
    Scheduled,
    /// The manual-only latch holds with no lapse deadline (or automatic apply is
    /// switched off): this build moves only when the person asks it to.
    ManualOnly,
}

/// A STANDING apply-lane failure for the build a surface is currently offering.
///
/// Constructed only through [`ApplyTrouble::new`], which enforces the "at least one
/// real attempt" rule. The three fields are exactly the three things the field report
/// says were missing from the window: how many times, why, and whether it will happen
/// again without the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApplyTrouble {
    /// Consecutive failed applies OF THE ARTIFACT ON OFFER — the ledger's
    /// `apply_failures_for_target`, not the running-build-scoped escalation streak.
    /// Always `>= 1`.
    attempts: u32,
    /// The ledger's own `last_apply_error` prose — the raw `apply_failure=` value.
    /// Kept verbatim so the mapping to human words happens at render time and the
    /// untranslated text stays available to the log and the control verb; no
    /// rendering here ever prints it.
    reason: String,
    /// Whether the automatic lane will try again unaided.
    retry: ApplyRetry,
}

/// `(full clause, short clause)` for a reason this program does not recognise.
///
/// TRUE OF EVERY MEMBER OF THE SET, which is what makes it safe to say when the
/// specific answer is unavailable: every string that reaches this slot is an apply
/// that was attempted and did not complete. It is deliberately not a guess at a
/// mechanism — "it did not start" would be wrong for an adoption mismatch and for a
/// preflight that went stale before anything was spawned.
const GENERIC: (&str, &str) = ("it did not finish applying", "didn\u{2019}t finish");

/// The apply outcomes this program can actually name, as
/// `(a substring of a real ledger reason, full clause, short clause)`.
///
/// # Keyed on the DATA, because the type never gets here
///
/// The obvious table — one row per `UpdateHandoffOutcome` variant, matched on its
/// `Debug` spelling — is wrong, and wrong in the direction that ships a regression.
/// Exactly ONE producer stamps a variant name into the string that reaches the
/// ledger (`app_update_handoff.rs`, the readiness wait: `format!("handoff proof
/// ended {outcome:?}")`). Every other failure arrives as free-form prose wrapped in
/// `format!("overlap handoff failed safely: {detail}")`, so a `contains("Rejected")`
/// row is dead code while the four commonest rejections — the ones spelled "final
/// handoff admission rejected before Commit" and friends — fall through to whatever
/// the fallback does. When that fallback was "show the reason verbatim", the Version
/// menu read "⬆️ Update to v0.56.0 — tried twice, overlap handoff failed safely: a
/// handed-off PTY session closed before Commit; retrying on its own".
///
/// So the rows below are keyed on substrings taken from the producers themselves,
/// each named in the comment above its group, and this module's tests replay every
/// one of those literals (`REAL_LEDGER_REASONS`) through [`clauses`]. An unlisted
/// reason is not a bug — it takes [`GENERIC`] and the surface points at the log —
/// which is what makes it safe for the apply lane to grow new failure prose without
/// this table being updated in lockstep.
///
/// FIRST MATCH WINS, and the order encodes one real overlap: "structural activity
/// revoked handoff before Commit" contains both `activity revoked` and `before
/// Commit`, and its typed outcome is `ActivityRevoked`, so the activity rows come
/// first.
const CAUSES: &[(&str, &str, &str)] = &[
    // ── The successor's own verdict. `wait_handoff_ready` returns one of five
    // variants and `run_handoff_decision` stamps its Debug name; these are the only
    // reasons in the whole set that carry a type name at all.
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
    // ── Preparation, all of it BEFORE any successor existed
    // (`send_handoff_preparation_failure`, plus the descriptor transfer on the
    // out-of-band lane). Nothing was started, so nothing "failed to start".
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
        "descriptors could not be delivered",
        "the handover could not be set up",
        "couldn\u{2019}t be set up",
    ),
    // ── Not the GUI's lane at all: `aterm-update`'s own boot-trial recovery writes
    // this slot too (`install.rs`, both arms end "disarmed the boot sentinel to keep
    // updates possible"). The update was installed, would not prove itself across
    // three launches, and the machine is back on this build.
    (
        "disarmed the boot sentinel",
        "the new version would not run and aterm went back to this one",
        "rolled back",
    ),
];

/// `(full clause, short clause)` for a ledger reason.
///
/// An unrecognised reason takes [`GENERIC`]. It does NOT fall through to its own
/// text: this slot holds engineering prose written for a log ("overlap handoff failed
/// safely: …", "updater apply preflight became stale"), and the surfaces that render
/// it are a macOS menu item, a palette row and a one-line Settings detail. See
/// [`CAUSES`].
fn clauses(reason: &str) -> (String, String) {
    let (full, short) = named_cause(reason).unwrap_or(GENERIC);
    (full.to_string(), short.to_string())
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

/// "once" / "twice" / "3 times" — a count a sentence can contain.
///
/// The first two have words in English and the rest do not; writing "1 times" in a
/// line the user reads is the kind of thing that makes a program feel unattended.
fn times(attempts: u32) -> String {
    match attempts {
        1 => "once".to_string(),
        2 => "twice".to_string(),
        n => format!("{n} times"),
    }
}

impl ApplyTrouble {
    /// The standing trouble for a staged build, or `None` when there is none.
    ///
    /// `None` for zero attempts is the whole of rule 2 in the module docs: a stage
    /// that has been DEFERRED (waiting for a quiet window) or BLOCKED advances no
    /// streak — those are recorded as refusals — so zero attempts really does mean
    /// "nothing has gone wrong yet", and that state must keep reading as ready.
    #[must_use]
    pub(crate) fn new(attempts: u32, reason: &str, retry: ApplyRetry) -> Option<Self> {
        (attempts > 0).then(|| Self {
            attempts,
            reason: reason.trim().to_string(),
            retry,
        })
    }

    /// Whether this program could name the cause, or fell back to [`GENERIC`].
    ///
    /// A surface with a line to spend uses it to point at the log — which is where
    /// the untranslated reason still is — so degrading safely never means silently
    /// dropping the only explanation the machine has.
    #[must_use]
    pub(crate) fn cause_is_named(&self) -> bool {
        named_cause(&self.reason).is_some()
    }

    /// The clause that says what happens NEXT — the half that decides whether the
    /// reader has to do anything.
    fn next_step(&self) -> &'static str {
        match self.retry {
            ApplyRetry::Scheduled => "It will try again by itself.",
            ApplyRetry::ManualOnly => "It will not try again until you ask.",
        }
    }

    /// The full sentence, for a detail row that has a line to spend: attempts, cause,
    /// and what happens next.
    ///
    /// This is the string the field report was missing. Reading it, the owner would
    /// have known at a glance that the two invisible attempts had happened and that
    /// the successor was dying rather than being refused.
    #[must_use]
    pub(crate) fn sentence(&self) -> String {
        let (full, _) = clauses(&self.reason);
        format!(
            "aterm tried to update {} and {full}. {}",
            times(self.attempts),
            self.next_step()
        )
    }

    /// The one-line tail for a MENU ROW: a Version-menu item and the palette row that
    /// mirrors it are single lines with no room for a sentence, and both are also the
    /// control that retries, so the tail ends on what pressing it does.
    #[must_use]
    pub(crate) fn row_tail(&self) -> String {
        let (_, short) = clauses(&self.reason);
        let next = match self.retry {
            ApplyRetry::Scheduled => "retrying on its own",
            ApplyRetry::ManualOnly => "restart now to retry",
        };
        format!("tried {}, {short}; {next}", times(self.attempts))
    }

    /// The phone-width detail: the same two facts with the sentence scaffolding
    /// removed.
    #[must_use]
    pub(crate) fn compact(&self) -> String {
        let (_, short) = clauses(&self.reason);
        let next = match self.retry {
            ApplyRetry::Scheduled => "retrying",
            ApplyRetry::ManualOnly => "ask to retry",
        };
        format!("Tried {}, {short} \u{b7} {next}.", times(self.attempts))
    }

    /// The tersest truthful rung, for a status card that has ~16 characters (a
    /// compact host at 2x Dynamic Type). The COUNT survives to the last rung: it is
    /// the single fact that separates "downloaded, waiting for you" from "tried and
    /// failed", which is the whole point of this module.
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
    use super::{ApplyRetry, ApplyTrouble, GENERIC, clauses, times};

    /// EVERY REASON THIS PROGRAM CAN ACTUALLY WRITE INTO `last_apply_error`, copied
    /// from the producers.
    ///
    /// The list is the point of the test below, so it is worth saying where each
    /// line comes from: everything up to the boot-trial pair is a
    /// `UpdateHandoffCompletion` `detail` from `app_update_handoff.rs`, which
    /// `App::finish_update_handoff` wraps in `"overlap handoff failed safely: "`
    /// before it reaches `record_apply_failure`; the last two are written by
    /// `aterm-update`'s own `install.rs` when a boot trial cannot be recovered.
    ///
    /// A table written from the TYPE instead of from this data had six rows, two of
    /// which (`PreparationFailed`, `Rejected`) could never match anything, and let
    /// nine of these strings through verbatim into a menu item.
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
        "overlap handoff failed safely: staged update failed pre-park verification: \
         staged build 9 is not newer than 9",
        "overlap handoff failed safely: could not persist the bounded handoff layout",
        "overlap handoff failed safely: could not write the authenticated handoff manifest",
        "overlap handoff failed safely: handoff screen carry did not match the committed \
         screen digest",
        "overlap handoff failed safely: could not write the attempt-bound handoff layout",
        "overlap handoff failed safely: handoff identity set exceeds the proof format",
        "overlap handoff failed safely: could not create the adoption-proof channel",
        "overlap handoff failed safely: could not create the handoff-commit channel",
        "overlap handoff failed safely: handoff process could not start: \
         Resource temporarily unavailable (os error 35)",
        // `aterm_update::install`, both boot-trial recovery arms.
        "update trial for build 1787699398 was unrecoverable across 3 launches of build \
         1787690000; disarmed the boot sentinel to keep updates possible",
        "trial recovery proof failed 3x (trial receipt is missing); disarmed the boot \
         sentinel to keep updates possible",
    ];

    /// THE REGRESSION THIS TABLE EXISTS TO PREVENT: internal prose in a menu item.
    ///
    /// Every string above is one this program provably writes. None of them may
    /// survive into any rendering, and the two that a reader would find most
    /// alarming — "failed safely" (a sentence that tells you the failure succeeded)
    /// and "Commit" — are named explicitly because they are what the verbatim
    /// fallback actually put on screen.
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

    /// …and each of them maps to the RIGHT cause, not merely to a safe one.
    ///
    /// Degrading to the generic clause is the safety net, so a table that matched
    /// nothing at all would still pass the test above. This is the one that says the
    /// net is not carrying the whole load.
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
        // A TIMEOUT AND A DEATH ARE DIFFERENT ADVICE: one says "the machine was
        // loaded", the other says "the successor could not run at all".
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
        // ACTIVITY WINS OVER "before Commit". "structural activity revoked handoff
        // before Commit" contains both markers and its typed outcome is
        // ActivityRevoked — the reader's situation is "your terminal was busy",
        // which is self-correcting, not "something refused the handover".
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
        // NOTHING WAS EVER STARTED on the preparation lane, so none of these may
        // borrow the "did not start" wording.
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
    }

    /// AN UNRECOGNISED REASON DEGRADES; IT DOES NOT LEAK.
    ///
    /// The submission lane alone has dozens of free-form messages
    /// (`UpdateHandoffStartError::failed`, the preflight's own verdicts), and it is
    /// deliberately not enumerated here — new ones will be written by people who
    /// have never read this module. So the contract is the fallback: whatever
    /// arrives, the surfaces say one bounded generic thing, and
    /// `cause_is_named` reports that the specific answer is only in the log.
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
        // An EMPTY reason is the same situation with even less to go on, and takes
        // the same clause rather than a second, differently-worded one.
        assert_eq!(
            clauses("   "),
            (GENERIC.0.to_string(), GENERIC.1.to_string())
        );
    }

    /// THE FIELD CASE, END TO END (owner's machine, 2026-08-21): a staged 0.56.0 that
    /// had been attempted twice, both times ending `ChildDied`, with the automatic
    /// lane still scheduled. Every surfaced rendering must name the ATTEMPT COUNT and
    /// a HUMAN cause; none may leak the proof-outcome enum name.
    #[test]
    fn a_twice_failed_stage_names_the_count_and_a_human_cause_everywhere() {
        let trouble = ApplyTrouble::new(
            2,
            "overlap handoff failed safely: handoff proof ended ChildDied",
            ApplyRetry::Scheduled,
        )
        .expect("two attempts is trouble");

        let sentence = trouble.sentence();
        assert_eq!(
            sentence,
            "aterm tried to update twice and the new version did not finish starting. \
             It will try again by itself."
        );
        assert_eq!(
            trouble.row_tail(),
            "tried twice, didn\u{2019}t start; retrying on its own"
        );
        assert_eq!(
            trouble.compact(),
            "Tried twice, didn\u{2019}t start \u{b7} retrying."
        );
        // The count reaches even the tersest rung — it is the fact that separates
        // "waiting for you" from "tried twice and failed".
        assert_eq!(trouble.micro(), "2 tries failed.");
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
    }

    /// The two retry regimes are DIFFERENT SENTENCES, because one of them requires
    /// the reader to act and the other requires them to do nothing.
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
            manual.row_tail().contains("restart now"),
            "the manual-only row must name the action: {}",
            manual.row_tail()
        );
    }

    /// English has words for one and two; "1 times" in a line a person reads is the
    /// tell of an unattended program.
    #[test]
    fn small_counts_are_words_and_larger_ones_are_numerals() {
        assert_eq!(times(1), "once");
        assert_eq!(times(2), "twice");
        assert_eq!(times(3), "3 times");
        assert_eq!(times(11), "11 times");
        // …and the tersest rung, which cannot spend a word on "once", still agrees
        // with itself about singular and plural.
        let micro = |n| {
            ApplyTrouble::new(n, "handoff proof ended ChildDied", ApplyRetry::Scheduled)
                .expect("trouble")
                .micro()
        };
        assert_eq!(micro(1), "1 try failed.");
        assert_eq!(micro(2), "2 tries failed.");
    }
}
