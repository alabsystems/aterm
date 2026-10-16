// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The updater's SELF-HEALING ledger (`…/aterm/Updates/health.toml`): a durable record
//! of check failures by CLASS — the memory that lets a silent background updater
//! notice it is persistently broken and say so. Without classes, "the network
//! blipped" and "this binary can never download again" look the same.
//!
//! * `network` — the channel pointer read (`HEAD …/releases/latest/download/
//!   aterm-appcast.toml`) failed: unreachable, a refused redirect, an unexpected
//!   status. Genuinely transient things land here.
//! * `pipeline` — release metadata was visible but an asset that provably exists
//!   could not be fetched: the download machinery itself is suspect.
//! * `manifest` — a manifest was FETCHED but rejected (bad signature / unparseable).
//! * `stage` — the artifact downloaded but failed verification/staging (also
//!   memoized by `FailedMark`).
//! * `apply` — a VERIFIED, STAGED build failed to become the running build (handoff
//!   refused/revoked/mismatched, the child died, the swap or re-exec failed).
//!   Recorded by [`Health::record_apply_failure`] from the GUI's apply lane.
//!
//! Streaks are PER-CLASS, so an interleaved network blip cannot reset a pipeline
//! streak and suppress the escalation. Unknown keys from older ledgers parse and are
//! dropped on the next write.
//!
//! Two rules keep the `apply` class honest:
//!
//! 1. **A healthy CHECK does not clear it.** [`Health::record_success`] resets the
//!    acquisition streaks and leaves `apply_failures` alone: "I can download the
//!    release" is no evidence that applying it works. Only
//!    [`Health::record_apply_success`] clears it.
//! 2. **It counts toward [`Health::is_persistent`].** A staged build that will not
//!    apply strands the machine as surely as one that will not download.

use aterm_update_core::FileLock;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// How long a ledger write waits for `health.toml.lock` before proceeding unlocked.
/// Every legitimate holder is one read→mutate→write; five seconds is
/// hundreds of them, and past it the holder is stopped or wedged.
const HEALTH_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Consecutive failures of one class at which the state may be called PERSISTENT:
/// the status wording stops saying "deferred", the update screen headlines it, and
/// the GUI raises a notification.
///
/// Three checks in a row, AND, for the classes a check books, a streak that spans
/// [`PERSISTENT_SPAN`] on that class's own clock ([`Health::persistent_class`]). A
/// count alone is a duration only at one cadence: three failures took half an hour at
/// the old ten-minute interval and take six minutes at the two-minute one, inside a
/// short GitHub incident. Long enough that a blip never escalates, short enough that a
/// broken pipeline is named within the hour.
pub use crate::{PERSISTENT_AFTER, PERSISTENT_SPAN};

/// The durable health record. All fields default so an absent/corrupt file reads as
/// "healthy" (fail-open here is correct: health is observability, never a gate).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Health {
    /// Consecutive `network`-class failures; cleared only by a healthy check.
    #[serde(default)]
    pub network_failures: u32,
    /// Consecutive `pipeline`-class failures; cleared only by a healthy check.
    /// This is the streak [`Self::is_persistent`] watches.
    #[serde(default)]
    pub pipeline_failures: u32,
    /// Consecutive `manifest`-class failures (fetched but rejected).
    #[serde(default)]
    pub manifest_failures: u32,
    /// Consecutive `stage`-class failures (downloaded but failed to verify/stage).
    #[serde(default)]
    pub stage_failures: u32,
    /// Consecutive `apply`-class failures: a verified, staged build that did not
    /// become the running build (handoff refused/revoked/mismatched, child died,
    /// swap or re-exec failed). Cleared ONLY by a successful apply, never by a
    /// healthy download check (see the module docs).
    #[serde(default)]
    pub apply_failures: u32,
    /// RFC3339 UTC at which each class's CURRENT streak began (empty when that
    /// class is not failing) — the class's own clock, read via
    /// [`Self::class_since`].
    ///
    /// [`Self::failing_since`] is any-class by definition: it stamps the moment the
    /// ledger stopped being clean and does not move again until it is. A per-class
    /// COUNT printed beside it would splice two different failures into one sentence
    /// (one class's count, another's date).
    #[serde(default)]
    pub network_since: String,
    /// See [`Self::network_since`].
    #[serde(default)]
    pub pipeline_since: String,
    /// See [`Self::network_since`].
    #[serde(default)]
    pub manifest_since: String,
    /// See [`Self::network_since`].
    #[serde(default)]
    pub stage_since: String,
    /// See [`Self::network_since`].
    #[serde(default)]
    pub apply_since: String,
    /// Class of the MOST RECENT failure (`""` when healthy).
    #[serde(default)]
    pub kind: String,
    /// RFC3339 UTC of the first failure of the current unhealthy period (any class).
    #[serde(default)]
    pub failing_since: String,
    /// RFC3339 UTC of the most recent failure.
    #[serde(default)]
    pub last_failure_at: String,
    /// The most recent failure's error text (truncated), for `update status`.
    #[serde(default)]
    pub last_error: String,
    /// The most recent APPLY-lane failure's reason, kept separately from
    /// [`Self::last_error`].
    ///
    /// The two lanes interleave: a handoff can fail at 12:00 and a network blip
    /// land at 12:01, and a single `last_error` would leave the surviving apply
    /// streak described by an unrelated DNS message. Keeping the apply reason
    /// apart lets [`Self::record_success`] restore an honest `kind`/`last_error`
    /// when it clears the acquisition streaks and the apply streak survives.
    #[serde(default)]
    pub last_apply_error: String,
    /// The most recent DIAGNOSIS-class failure's reason (`pipeline`,
    /// `manifest`, `stage`), kept apart from [`Self::last_error`] for the same
    /// reason `last_apply_error` is: a network blip on a standing stage streak
    /// must not leave that streak described by a DNS error. Whenever the ledger
    /// re-labels itself to a standing diagnosis class it restores this.
    #[serde(default)]
    pub last_diagnosis_error: String,
    /// RFC3339 UTC of [`Self::last_diagnosis_error`] (empty when there is none): the
    /// newest failure of the standing diagnosis class, the end of the span
    /// [`Self::persistent_class`] measures. [`Self::last_failure_at`] is any-class: a
    /// DNS blip hours after three quick `pipeline` failures would stretch that streak
    /// past [`PERSISTENT_SPAN`] with no new evidence about the pipeline.
    #[serde(default)]
    pub last_diagnosis_at: String,
    /// RFC3339 UTC of [`Self::last_apply_error`] (empty when there is none).
    ///
    /// [`Self::last_failure_at`] is any-class: a DNS blip would re-date an apply
    /// streak that last failed days earlier. The apply lane owns its own clock.
    #[serde(default)]
    pub last_apply_failure_at: String,
    /// The build that was RUNNING when the last apply failure was recorded
    /// (0 when unknown/none) — the apply-streak twin of
    /// [`Self::last_apply_refusal_build`], and the expiry key
    /// [`Self::expire_stale_apply_streak`] reads: an apply streak is only
    /// ever the claim "THIS running build cannot be replaced through the
    /// lane", and once a DIFFERENT build is running the machine has moved —
    /// through the channel, a manual install, or a boot swap — so the claim
    /// is proven stale. Without this, a manually-updated healthy machine would
    /// carry `persistent=true` forever.
    #[serde(default)]
    pub last_apply_failure_build: u64,
    /// The build the failed apply was TRYING TO REACH (0 when unknown) — the
    /// staged artifact's own identity, as distinct from
    /// [`Self::last_apply_failure_build`], which names the build that was
    /// RUNNING.
    ///
    /// `apply_failures` is expiry-bound to the RUNNING build, so staging a NEWER
    /// artifact resets nothing; this field lets a reader ask whether that streak is
    /// about the build on offer, so a fresh download never inherits "tried twice".
    #[serde(default)]
    pub last_apply_failure_target_build: u64,
    /// Consecutive failed applies OF [`Self::last_apply_failure_target_build`],
    /// reset the moment a different target is attempted.
    ///
    /// [`Self::apply_failures`] is the ESCALATION streak and is deliberately
    /// running-build-scoped ("this machine cannot be moved through the lane"), so
    /// it must not reset when a new artifact appears or a machine whose every
    /// stage fails once would never escalate. "aterm tried to update twice" is a
    /// claim about the ARTIFACT ON OFFER. Two counts because there are two questions.
    #[serde(default)]
    pub apply_failures_for_target: u32,
    /// The most recent apply-lane REFUSAL: a verdict that stopped an apply
    /// BEFORE it could fail, in the words of whichever caller refused it.
    ///
    /// Deliberately not a streak and deliberately absent from
    /// [`Self::total_failures`]/[`Self::is_persistent`]: a refusal is a normal,
    /// self-correcting state, and counting one would manufacture a persistent-
    /// failure escalation out of ordinary terminal use. Still recorded, because a
    /// refusal that records nothing is indistinguishable from an updater that never
    /// ran.
    #[serde(default)]
    pub last_apply_refusal: String,
    /// RFC3339 UTC of [`Self::last_apply_refusal`] (empty when there is none).
    /// A refusal without a time cannot be told apart from a stale one.
    #[serde(default)]
    pub last_apply_refusal_at: String,
    /// The build that was RUNNING when [`Self::last_apply_refusal`] was recorded
    /// (0 when there is none).
    ///
    /// A successful in-session apply never returns to clear anything — it execs
    /// into the new image — so without this the last refusal would follow the
    /// machine into the build that proves it was overcome, and `update status`
    /// would explain a fixed problem forever. A refusal is only ever about the
    /// build that could not move; readers drop it once a different build is
    /// running.
    #[serde(default)]
    pub last_apply_refusal_build: u64,
    /// The newer build this machine has been WAITING ON — staged in `Updates/`,
    /// or already installed under a running image that is older — and
    /// [`Self::pending_since`], when it was first seen waiting. `0` when
    /// nothing newer is waiting. Written by [`Self::note_pending_update`].
    ///
    /// Unlike `ready.toml`'s `staged_at`, which every re-stage rewrites, this clock
    /// survives re-stages: past [`crate::PENDING_UPDATE_OVERDUE_SECS`] the loud
    /// notice fires.
    #[serde(default)]
    pub pending_build: u64,
    /// RFC3339 UTC at which [`Self::pending_build`] was first seen waiting
    /// (empty when nothing is).
    #[serde(default)]
    pub pending_since: String,
    /// What the seamless handoff's capture cost the last time it was TIMED — a
    /// dry run of the park's own capture, taken while the successor booted with
    /// every reader live — in microseconds (0 when none has been measured).
    /// Written by [`Self::record_handoff_capture`].
    ///
    /// The GUI seeds the first freeze rung from measurements — `clamp(1.5 × the
    /// larger of this and [`Self::handoff_park_us`], 20 ms, 80 ms)` — and this is
    /// where they outlive the process that took them
    /// ([`crate::handoff_capture_prior`]). A PRIOR, never a gate: an absent or
    /// corrupt value is the 20 ms default.
    #[serde(default)]
    pub handoff_capture_us: u64,
    /// How many sessions the timed capture of [`Self::handoff_capture_us`]
    /// carried — the desk it describes, so a reader can tell a number taken
    /// over two tabs from one taken over twenty.
    #[serde(default)]
    pub handoff_capture_sessions: u32,
    /// The first automatic freeze rung [`Self::handoff_capture_us`] seeded, in
    /// milliseconds (0 when none has been measured) — what `update status`
    /// prints as `freeze_seed_ms=`.
    #[serde(default)]
    pub handoff_freeze_seed_ms: u64,
    /// The build that was RUNNING when [`Self::handoff_capture_us`] was
    /// measured (0 when unknown): the capture is that build's code over that
    /// moment's desk.
    #[serde(default)]
    pub handoff_capture_build: u64,
    /// RFC3339 UTC of [`Self::handoff_capture_us`] (empty when there is none).
    #[serde(default)]
    pub handoff_capture_at: String,
    /// What the seamless handoff's REAL park cost the last time one landed —
    /// from the instant the readers were told to stop until the capture, the
    /// layout and both digests were done: everything the freeze budget is spent
    /// on — in microseconds (0 when none has been recorded). Written by
    /// [`Self::record_handoff_park`].
    ///
    /// The dry run of [`Self::handoff_capture_us`] cannot time the part of the
    /// park that IS the freeze — stopping the readers — and that part is most of it
    /// (measured 2026-09-26: a 1.1 ms capture inside a ~20 ms park). So the next
    /// update seeds its first rung from the larger of the two.
    #[serde(default)]
    pub handoff_park_us: u64,
    /// The build that was RUNNING when [`Self::handoff_park_us`] was recorded
    /// (0 when unknown).
    #[serde(default)]
    pub handoff_park_build: u64,
    /// RFC3339 UTC of [`Self::handoff_park_us`] (empty when there is none).
    #[serde(default)]
    pub handoff_park_at: String,
}

/// The longest error text the ledger stores, in characters.
const MAX_STORED_ERROR_CHARS: usize = 400;

/// `error` cut to [`MAX_STORED_ERROR_CHARS`] — WITHOUT CUTTING OFF WHAT IT IS. A
/// passing refusal carries its key ([`crate::PASSING_REFUSAL_KEY`]) at the END of its
/// text, and readers ([`crate::is_passing_refusal`]) tell a moment from a verdict by
/// it. Such a text keeps its head and its key, with the cut marked.
fn capped_error(error: &str) -> String {
    if error.chars().count() <= MAX_STORED_ERROR_CHARS {
        return error.to_string();
    }
    if !crate::is_passing_refusal(error) {
        return error.chars().take(MAX_STORED_ERROR_CHARS).collect();
    }
    let tail = format!(" \u{2026} ({})", crate::PASSING_REFUSAL_KEY);
    let head = MAX_STORED_ERROR_CHARS.saturating_sub(tail.chars().count());
    let mut kept: String = error.chars().take(head).collect();
    kept.push_str(&tail);
    kept
}

impl Health {
    /// Read the ledger; absent/corrupt ⇒ healthy default (observability, not a gate).
    /// Non-UTF-8 IS a corrupt ledger, deliberately folded to the healthy default.
    #[must_use]
    pub fn read(path: &Path) -> Self {
        crate::read_ledger_text(path)
            .and_then(|t| aterm_toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Total consecutive failed checks across classes (the `failing=` status field).
    #[must_use]
    pub fn total_failures(&self) -> u32 {
        self.network_failures
            .saturating_add(self.pipeline_failures)
            .saturating_add(self.manifest_failures)
            .saturating_add(self.stage_failures)
            .saturating_add(self.apply_failures)
    }

    /// Consecutive failed checks of the ACQUISITION classes only — network,
    /// pipeline, manifest, stage — which is what `failing_checks` means.
    ///
    /// The apply streak is deliberately excluded. It is reported separately as
    /// `failing_applies` because it answers a different question ("can this machine
    /// move to an update?" versus "can it fetch one?").
    ///
    /// Not a sum across classes either: it is the STANDING class's own count
    /// ([`Self::standing_acquisition_class`], the class the `:kind` suffix names), so
    /// the number and the label describe one streak.
    #[must_use]
    pub fn acquisition_failures(&self) -> u32 {
        match self.standing_acquisition_class() {
            Some("pipeline") => self.pipeline_failures,
            Some("manifest") => self.manifest_failures,
            Some("stage") => self.stage_failures,
            Some("network") => self.network_failures,
            _ => 0,
        }
    }

    /// The `(streak, start-of-streak)` pair a failure class owns, or `None` for an
    /// unrecognised kind. Both halves move together — that is the point: a count
    /// without its own clock is what produced the spliced status line
    /// [`Self::network_since`] documents.
    fn class_streak_mut(&mut self, kind: &str) -> Option<(&mut u32, &mut String)> {
        match kind {
            "network" => Some((&mut self.network_failures, &mut self.network_since)),
            "pipeline" => Some((&mut self.pipeline_failures, &mut self.pipeline_since)),
            "manifest" => Some((&mut self.manifest_failures, &mut self.manifest_since)),
            "stage" => Some((&mut self.stage_failures, &mut self.stage_since)),
            "apply" => Some((&mut self.apply_failures, &mut self.apply_since)),
            _ => None,
        }
    }

    /// When THIS class's current streak began — the timestamp to print beside that
    /// class's count.
    ///
    /// Falls back to [`Self::failing_since`] when the class carries no stamp, which
    /// covers both an unrecognised kind and a ledger written before per-class clocks
    /// existed. The fallback is the any-class answer such a ledger has always given,
    /// so an in-place upgrade never prints an empty date; the class stamp appears on
    /// the next failure that starts a streak.
    #[must_use]
    pub fn class_since(&self, kind: &str) -> &str {
        let since = match kind {
            "network" => &self.network_since,
            "pipeline" => &self.pipeline_since,
            "manifest" => &self.manifest_since,
            "stage" => &self.stage_since,
            "apply" => &self.apply_since,
            _ => &self.failing_since,
        };
        if since.is_empty() {
            &self.failing_since
        } else {
            since
        }
    }

    /// The class whose streak has crossed [`PERSISTENT_AFTER`], with its own count —
    /// or `None` while nothing is persistently failing.
    ///
    /// A class a CHECK books (`pipeline`, `manifest`, `stage`) must also have failed
    /// for [`PERSISTENT_SPAN`]: from its streak's start ([`Self::class_since`]) to its
    /// newest failure ([`Self::last_diagnosis_at`]; a ledger an older build wrote has
    /// only the any-class [`Self::last_failure_at`]). Measured between two failures,
    /// never against the clock, so a laptop asleep on a short streak wakes to the same
    /// verdict it slept on. `apply` is counted in attempts the apply lane paces on its
    /// own clock, and its count alone escalates.
    ///
    /// Callers need the CLASS, not just the boolean: the count and the sentence a
    /// user is shown have to come from the streak that actually escalated.
    ///
    /// Order is severity-of-diagnosis, not precedence: the earlier a class sits in
    /// the acquire→apply pipeline, the more it explains, so it wins the report.
    #[must_use]
    pub fn persistent_class(&self) -> Option<(&'static str, u32)> {
        [
            ("pipeline", self.pipeline_failures),
            ("manifest", self.manifest_failures),
            ("stage", self.stage_failures),
            ("apply", self.apply_failures),
        ]
        .into_iter()
        .find(|&(class, n)| n >= PERSISTENT_AFTER && self.spans_persistently(class))
    }

    /// Whether `class`'s streak has lasted [`PERSISTENT_SPAN`] (see
    /// [`Self::persistent_class`]). An unreadable stamp has not.
    fn spans_persistently(&self, class: &str) -> bool {
        if class == "apply" {
            return true;
        }
        let newest = if self.last_diagnosis_at.is_empty() {
            &self.last_failure_at
        } else {
            &self.last_diagnosis_at
        };
        crate::install::rfc3339_delta_secs(self.class_since(class), newest)
            .is_some_and(|span| span >= PERSISTENT_SPAN.as_secs())
    }

    /// Whether the ledger shows a PERSISTENT failure of a lane that strands the
    /// machine, for [`PERSISTENT_AFTER`]+ consecutive attempts. Per-class streaks
    /// mean an interleaved network blip cannot reset any of them.
    ///
    /// EVERY class that cannot heal itself on the client counts, because the
    /// user-visible symptom is identical in all of them — the machine does not move
    /// to the new version:
    ///
    /// * `pipeline` — releases visible, downloads impossible;
    /// * `manifest` — the authoritative release cannot be trusted (unsigned under a
    ///   pinned channel, bad signature, unparseable). Retrying cannot fix it: it
    ///   stays broken until the PUBLISHER republishes;
    /// * `stage` — the artifact downloads but will not verify or become a bundle;
    /// * `apply` — a verified build staged but never able to become the running one.
    ///
    /// `network` is deliberately excluded: it is the one genuinely transient class
    /// (GitHub unreachable, auth blip), and it already slows the cadence rather than
    /// alarming the user.
    #[must_use]
    pub fn is_persistent(&self) -> bool {
        self.persistent_class().is_some()
    }

    /// Record one failed check of `kind` (`network` / `pipeline` / `manifest` /
    /// `stage`); unknown kinds count only toward `kind`/timestamps. Returns the
    /// updated record.
    pub fn record_failure(path: &Path, kind: &str, error: &str) -> Self {
        // Serialize the whole read→mutate→write against the same-pid sibling in this
        // process (the background loop vs. a manual `update check`), which the temp
        // file's pid key cannot separate. Best-effort: health is observability, never
        // a gate — if the lock can't be taken we proceed unlocked rather than drop the
        // update.
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        Self::advance_failure_streak(&mut h, kind, error);
        h.write(path);
        h
    }

    /// The streak arithmetic shared by [`Self::record_failure`] and
    /// [`Self::record_apply_failure`], applied to an ALREADY-LOCKED, already-read
    /// record so a caller that must do more under the same lock can.
    fn advance_failure_streak(h: &mut Self, kind: &str, error: &str) {
        let now = crate::install::now_rfc3339();
        // The class's clock starts when ITS streak does. An unknown kind still
        // counts toward `kind`/timestamps below, exactly as before.
        if let Some((count, since)) = h.class_streak_mut(kind) {
            if *count == 0 {
                *since = now.clone();
            } else if since.is_empty() {
                // A COUNT WITHOUT ITS CLOCK is a streak this ledger cannot vouch
                // for (written by a build without per-class clocks). It restarts
                // HERE, dated now, so the count and the date describe one streak.
                *count = 0;
                *since = now.clone();
            }
            *count = count.saturating_add(1);
        }
        // ONE CHECK, ONE DIAGNOSIS. `pipeline` (the manifest could not be fetched),
        // `manifest` (it was fetched and rejected) and `stage` (it was accepted and
        // the artifact would not stage) are mutually exclusive verdicts about the
        // same check, so a failure of one of them means the check did NOT fail the
        // other two — their streaks are no longer consecutive and are cleared here,
        // with their clocks. `network` is left
        // alone on purpose: it is weather, and the module's standing rule is that
        // a blip must not reset a standing diagnosis (nor does a diagnosis need to
        // forget a blip); `apply` is a different lane and is never touched by a
        // check ([`Self::record_success`]).
        if matches!(kind, "pipeline" | "manifest" | "stage") {
            for sibling in ["pipeline", "manifest", "stage"] {
                if sibling != kind
                    && let Some((count, since)) = h.class_streak_mut(sibling)
                {
                    *count = 0;
                    since.clear();
                }
            }
        }
        h.kind = kind.to_string();
        if h.failing_since.is_empty() {
            h.failing_since = now.clone();
        }
        h.last_failure_at = now;
        // Cap the stored error so a pathological message can't bloat the ledger —
        // keeping what it SAYS it is ([`capped_error`]).
        h.last_error = capped_error(error);
        if matches!(kind, "pipeline" | "manifest" | "stage") {
            h.last_diagnosis_error = h.last_error.clone();
            h.last_diagnosis_at = h.last_failure_at.clone();
        }
    }

    /// Re-describe the ledger by the class that still stands, with THAT
    /// class's own reason: the apply lane's for `apply`, the diagnosis lane's
    /// for `pipeline`/`manifest`/`stage`, nothing for a bare network blip
    /// (weather has no diagnosis). Every writer that clears some streaks and
    /// leaves others goes through here.
    fn relabel_by_standing(h: &mut Self) {
        if h.total_failures() == 0 {
            h.kind = String::new();
            h.failing_since = String::new();
            h.last_failure_at = String::new();
            h.last_error = String::new();
            h.last_diagnosis_error = String::new();
            h.last_diagnosis_at = String::new();
            return;
        }
        match h.standing_acquisition_class() {
            Some(standing @ ("pipeline" | "manifest" | "stage")) => {
                h.kind = standing.to_string();
                if !h.last_diagnosis_error.is_empty() {
                    h.last_error = h.last_diagnosis_error.clone();
                }
            }
            Some(standing) => {
                // `network` alone: weather, described by whatever it said.
                h.kind = standing.to_string();
            }
            None => {
                // Only the apply streak survived; describe the ledger by IT.
                h.kind = "apply".to_string();
                h.last_error = h.last_apply_error.clone();
            }
        }
    }

    /// Record a fully-healthy CHECK: every ACQUISITION streak clears.
    ///
    /// `apply_failures` deliberately survives. A check proves the machine can see,
    /// fetch, verify and stage a release; it proves nothing about whether that
    /// staged build can be made to run, which is a different lane with different
    /// failure modes. Use [`Self::record_apply_success`] for that.
    pub fn record_success(path: &Path) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        // No-op skip: a healthy check on an already-clean ledger must not rewrite the
        // file every interval (mirrors `Floor::bump_and_write`).
        if h.total_failures() == 0
            && h.kind.is_empty()
            && h.failing_since.is_empty()
            && h.last_failure_at.is_empty()
            && h.last_error.is_empty()
        {
            return h;
        }
        let apply_streak_survives = h.apply_failures;
        h.network_failures = 0;
        h.pipeline_failures = 0;
        h.manifest_failures = 0;
        h.stage_failures = 0;
        h.network_since = String::new();
        h.pipeline_since = String::new();
        h.manifest_since = String::new();
        h.stage_since = String::new();
        h.apply_failures = apply_streak_survives;
        if apply_streak_survives == 0 {
            h.apply_since = String::new();
        }
        // Re-describe the ledger by what still stands — the apply streak, with
        // ITS reason, or nothing — or `update status` would report the apply
        // streak under whatever transient network message happened to land last.
        h.last_diagnosis_error = String::new();
        h.last_diagnosis_at = String::new();
        Self::relabel_by_standing(&mut h);
        h.write(path);
        h
    }

    /// The acquisition class whose streak still stands, in the same
    /// severity-of-diagnosis order [`Self::persistent_class`] uses — or `None` when
    /// every acquisition streak is clear.
    ///
    /// Exists because `""` is the documented HEALTHY sentinel for [`Self::kind`], so
    /// blanking that field while a streak is still standing reports a healthy class on
    /// an unhealthy machine.
    #[must_use]
    pub(crate) fn standing_acquisition_class(&self) -> Option<&'static str> {
        [
            ("pipeline", self.pipeline_failures),
            ("manifest", self.manifest_failures),
            ("stage", self.stage_failures),
            ("network", self.network_failures),
        ]
        .into_iter()
        .find(|(_, n)| *n > 0)
        .map(|(class, _)| class)
    }

    /// Record a check that proved ACQUISITION works but deliberately did not attempt a
    /// stage — the `FailedMark` backoff path, where a known-bad candidate is being
    /// skipped on purpose.
    ///
    /// Clears network/pipeline/manifest and their clocks, and preserves BOTH
    /// `stage_failures` and `apply_failures` (with their clocks). Preserving the stage
    /// streak is the whole point: the reason this check skipped the download is the
    /// memo that streak represents, so a full [`Self::record_success`] here would erase
    /// the evidence of the very thing being backed off from, and the machine would
    /// report a clean stage lane for as long as it kept refusing to use it.
    pub fn record_acquisition_success(path: &Path) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if h.network_failures == 0 && h.pipeline_failures == 0 && h.manifest_failures == 0 {
            return h;
        }
        h.network_failures = 0;
        h.pipeline_failures = 0;
        h.manifest_failures = 0;
        h.network_since = String::new();
        h.pipeline_since = String::new();
        h.manifest_since = String::new();
        Self::relabel_by_standing(&mut h);
        h.write(path);
        h
    }

    /// Record that a staged build FAILED to become the running build, as
    /// observed by `current_build`. `reason` is the typed handoff/apply
    /// outcome (e.g. `ChildDied`, `AdoptionMismatch`, `ActivityRevoked`,
    /// `re-exec failed`), stored for `update status`.
    ///
    /// `target_build` is the artifact the attempt was TRYING to reach (0 when the
    /// caller genuinely does not know — a debug seam, or a stage already consumed).
    /// It keys [`Self::apply_failures_for_target`], which is the count any surface
    /// offering that build may quote; see that field for why the escalation streak
    /// cannot answer the same question.
    pub fn record_apply_failure(
        path: &Path,
        current_build: u64,
        target_build: u64,
        reason: &str,
    ) -> Self {
        // ONE LOCK SCOPE: between two scopes `expire_stale_apply_streak` (check
        // lane) could read the just-incremented ledger, zero the streak as another
        // build's, and lose the increment.
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        Self::advance_failure_streak(&mut h, "apply", reason);
        // Mirror the reason into the apply-owned slot so a later acquisition-lane
        // failure cannot overwrite the description of a still-standing apply streak.
        h.last_apply_error = capped_error(reason);
        h.last_apply_failure_at = crate::install::now_rfc3339();
        h.last_apply_failure_build = current_build;
        // A DIFFERENT ARTIFACT STARTS ITS OWN COUNT.
        if h.last_apply_failure_target_build != target_build {
            h.apply_failures_for_target = 0;
        }
        h.apply_failures_for_target = h.apply_failures_for_target.saturating_add(1);
        h.last_apply_failure_target_build = target_build;
        // A terminal verdict supersedes whatever refusal preceded it: leaving the
        // old "the terminal was busy" standing beside a hard failure would offer
        // an operator two competing answers to one question.
        h.clear_apply_refusal();
        h.write(path);
        h
    }

    /// Expire an apply streak PROVEN STALE by the running build (see
    /// [`Self::last_apply_failure_build`]): the streak was recorded by a
    /// different build, so "cannot be made to run" describes a machine state
    /// that no longer exists. Wired into the check lane, so a machine that
    /// moved by any means heals within one check interval. A streak whose
    /// recording build is unknown (0 — a pre-field ledger) expires too: the
    /// machine cannot prove it stale, but a pre-field ledger also cannot
    /// prove it FRESH, and the worse failure mode is a stale streak
    /// presented as standing (the reverse error self-corrects: a real
    /// failing lane re-records within one apply attempt).
    pub fn expire_stale_apply_streak(path: &Path, current_build: u64) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if h.apply_failures == 0 || h.last_apply_failure_build == current_build {
            return h;
        }
        // ONLY A NEWER BUILD PROVES THE MACHINE MOVED. A process whose image is
        // OLDER than the recorder (a terminal session launched before a seamless
        // self-update) knows nothing of the window's apply lane and must not zero
        // its live streak. A pre-field ledger (recorder 0) still expires.
        if h.last_apply_failure_build != 0 && current_build < h.last_apply_failure_build {
            return h;
        }
        h.apply_failures = 0;
        h.apply_since = String::new();
        h.last_apply_error = String::new();
        h.last_apply_failure_at = String::new();
        h.last_apply_failure_build = 0;
        h.apply_failures_for_target = 0;
        h.last_apply_failure_target_build = 0;
        if h.total_failures() == 0 || h.kind == "apply" {
            // Some acquisition streak may still stand; let it own the headline
            // WITH ITS OWN REASON (`kind == ""` means healthy).
            Self::relabel_by_standing(&mut h);
        }
        h.write(path);
        h
    }

    /// Record one apply-lane REFUSAL — a block/deferral that stopped an apply
    /// before it became a failure — as observed by `current_build`. `reason` must
    /// say WHAT refused and WHY, since it is the whole answer an operator gets to
    /// "the build is staged, so why is it not running?".
    ///
    /// Every streak is untouched by construction (see [`Self::last_apply_refusal`]);
    /// this only replaces the standing explanation, so a refusal can never
    /// escalate to the persistent-failure notification on its own.
    pub fn record_apply_refusal(path: &Path, current_build: u64, reason: &str) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        h.last_apply_refusal = capped_error(reason);
        h.last_apply_refusal_at = crate::install::now_rfc3339();
        h.last_apply_refusal_build = current_build;
        h.write(path);
        h
    }

    /// Whether the stored refusal is still the answer for `current_build`. A
    /// refusal recorded by a build that is no longer running was overcome by
    /// definition — most often by the very apply that execed away without
    /// returning to clear it.
    #[must_use]
    pub fn apply_refusal_applies_to(&self, current_build: u64) -> bool {
        !self.last_apply_refusal.is_empty() && self.last_apply_refusal_build == current_build
    }

    fn clear_apply_refusal(&mut self) {
        self.last_apply_refusal = String::new();
        self.last_apply_refusal_at = String::new();
        self.last_apply_refusal_build = 0;
    }

    /// Withdraw the standing apply refusal when it carries `key` — a refusal whose
    /// cause the caller has just answered, such as the swap's wait for a roster
    /// confirmation (`crate::manifest::ROSTER_UNCONFIRMED_KEY`), which the
    /// confirming check ends. Any other refusal is untouched, and so is every
    /// streak. No write when nothing matches.
    pub fn withdraw_apply_refusal_containing(path: &Path, key: &str) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if key.is_empty() || !h.last_apply_refusal.contains(key) {
            return h;
        }
        h.clear_apply_refusal();
        h.write(path);
        h
    }

    /// Record that an apply actually succeeded — the staged build is now the
    /// running build. Clears the `apply` streak only; the acquisition streaks are
    /// owned by [`Self::record_success`].
    pub fn record_apply_success(path: &Path) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        // An apply that actually went through answers every standing apply-lane
        // question, refusals included: a "the terminal was busy" left behind by an
        // earlier attempt must not outlive the attempt that succeeded.
        if h.apply_failures == 0
            && h.apply_failures_for_target == 0
            && h.last_apply_refusal.is_empty()
        {
            return h;
        }
        h.clear_apply_refusal();
        // The artifact-scoped count answers to the same success: whatever build just
        // became the running build, no standing attempt survives it.
        h.apply_failures_for_target = 0;
        h.last_apply_failure_target_build = 0;
        if h.apply_failures > 0 {
            h.apply_failures = 0;
            h.apply_since = String::new();
            h.last_apply_error = String::new();
            // The dated stamp and its build go with the streak they dated.
            h.last_apply_failure_at = String::new();
            h.last_apply_failure_build = 0;
            if h.total_failures() == 0 || h.kind == "apply" {
                // Acquisition failures may still stand; stop describing the ledger
                // by the apply failure that just resolved — and NAME the one that
                // is still standing, with its own reason, because `kind == ""`
                // means healthy.
                Self::relabel_by_standing(&mut h);
            }
        }
        h.write(path);
        h
    }

    /// Note which newer build is waiting for this machine, as `current_build`
    /// observes it (`observed`: the newest of the staged marker and an installed
    /// bundle newer than the running image; `None` when this observation found
    /// neither), and return the ledger with [`Self::pending_build`] /
    /// [`Self::pending_since`] brought up to date.
    ///
    /// THE FIRST SIGHTING WINS, which is the whole point: re-observing the same
    /// build — every check cycle, every re-stage of the same release — never
    /// moves the clock. A DIFFERENT newer build starts its own. The record is
    /// dropped only once it is PROVEN moot — the running build has caught up
    /// with it — never on an observation that merely found nothing: the stage
    /// is retired for a moment during the very swap that strands a machine
    /// (the candidate boot-applies it), and absence of evidence resetting the
    /// clock is how a stranded machine would stay forever "just staged".
    pub fn note_pending_update(path: &Path, current_build: u64, observed: Option<u64>) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        let observed = observed.filter(|build| *build > current_build);
        let changed = match observed {
            Some(build) if build != h.pending_build || h.pending_since.is_empty() => {
                h.pending_build = build;
                h.pending_since = crate::install::now_rfc3339();
                true
            }
            Some(_) => false,
            None if h.pending_build != 0 && h.pending_build <= current_build => {
                h.pending_build = 0;
                h.pending_since = String::new();
                true
            }
            None => false,
        };
        if changed {
            h.write(path);
        }
        h
    }

    /// Restart the pending clock for `build`, which could not be installed until now
    /// and now can: its roster authorization had gone stale (a day with no check,
    /// most often the app closed) and a check has just confirmed it — and return the
    /// ledger. The overdue notice promises that "a newer build
    /// has been here an hour and is not running" is a stuck update; a stage no lane
    /// was allowed to install says nothing about the lanes, so the hour counts from
    /// when one was. A clock keyed to another build is left alone.
    pub fn restart_pending_update(path: &Path, build: u64) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if h.pending_build == build && build != 0 {
            h.pending_since = crate::install::now_rfc3339();
            h.write(path);
        }
        h
    }

    /// Drop the pending clock for `build`, which was WITHDRAWN — quarantined or
    /// yanked, and so never going to be applied (the caller judges that) — and return
    /// the ledger. A clock another observation has since re-keyed to a different build
    /// is left alone.
    pub fn forget_pending_update(path: &Path, build: u64) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if h.pending_build == build {
            h.pending_build = 0;
            h.pending_since = String::new();
            h.write(path);
        }
        h
    }

    /// Record one TIMED handoff capture: `capture_us` microseconds to
    /// capture `sessions` session(s), measured by `current_build`, which seeded
    /// a first freeze rung of `freeze_seed_ms`. The newest measurement replaces
    /// the last one outright — it is a prior about THIS desk and THIS build,
    /// and an older, cheaper desk is exactly the number that made the rung
    /// miss. A zero measurement records nothing (it cannot be told from "never
    /// measured"); every streak and every other slot is untouched.
    pub fn record_handoff_capture(
        path: &Path,
        current_build: u64,
        capture_us: u64,
        sessions: u32,
        freeze_seed_ms: u64,
    ) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if capture_us == 0 {
            return h;
        }
        h.handoff_capture_us = capture_us;
        h.handoff_capture_sessions = sessions;
        h.handoff_freeze_seed_ms = freeze_seed_ms;
        h.handoff_capture_build = current_build;
        h.handoff_capture_at = crate::install::now_rfc3339();
        h.write(path);
        h
    }

    /// Record what one LANDED seamless park cost `current_build`: `park_us`
    /// microseconds from the readers' stop to a capture ready to hand over
    /// (see [`Self::handoff_park_us`]). Same rules as
    /// [`Self::record_handoff_capture`]: the newest replaces the last, a zero
    /// records nothing, and no streak or standing slot moves.
    pub fn record_handoff_park(path: &Path, current_build: u64, park_us: u64) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if park_us == 0 {
            return h;
        }
        h.handoff_park_us = park_us;
        h.handoff_park_build = current_build;
        h.handoff_park_at = crate::install::now_rfc3339();
        h.write(path);
        h
    }

    /// Best-effort sibling lock (`…/health.toml.lock`) guarding a whole read→mutate→
    /// write of the ledger. `None` on failure: the caller then proceeds unlocked —
    /// health is observability, never a gate, so a missed lock must never drop the
    /// update. Held for the lifetime of the returned guard (i.e. the record_* call).
    ///
    /// BOUNDED: the check loop records here every cycle, and a blocking `flock`
    /// against a holder stopped mid-write would park it forever. Every
    /// real holder is one read→mutate→write, milliseconds long; a holder past
    /// [`HEALTH_LOCK_WAIT`] is wedged, and the unlocked fallback above applies.
    fn lock(path: &Path) -> Option<FileLock> {
        FileLock::acquire_within(&path.with_extension("toml.lock"), HEALTH_LOCK_WAIT).ok()
    }

    /// Best-effort atomic write (temp + rename), mirroring `status::record`.
    fn write(&self, path: &Path) {
        use std::sync::atomic::{AtomicU64, Ordering};
        let Ok(text) = aterm_toml::to_string(self) else {
            return;
        };
        // Unique per INVOCATION (pid + a process-wide counter), not just per process:
        // two record_* calls in one process (background loop + manual `update check`)
        // must never stage through the same temp path and clobber each other's write.
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let tmp = path.with_extension(format!(
            "toml.{}-{}.tmp",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// TEST SEAM: move every streak clock of the ledger at `path` back by `by`, as if
/// each standing streak had begun that much earlier — the span
/// [`Health::persistent_class`] measures, without waiting [`PERSISTENT_SPAN`] for it.
/// The newest-failure stamps stay where they are.
#[cfg(test)]
pub(crate) fn backdate_streaks(path: &Path, by: std::time::Duration) {
    let mut h = Health::read(path);
    for since in [
        &mut h.network_since,
        &mut h.pipeline_since,
        &mut h.manifest_since,
        &mut h.stage_since,
        &mut h.apply_since,
        &mut h.failing_since,
    ] {
        if let Some(at) = aterm_types::rfc3339::parse_utc(since)
            .and_then(|at| u64::try_from(at).ok())
            .and_then(|at| at.checked_sub(by.as_secs()))
        {
            *since = aterm_types::rfc3339::format_rfc3339(at);
        }
    }
    h.write(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `health.toml` in a per-TEST scratch dir (tests share one process, so a
    /// pid-keyed dir would race across the parallel test threads). Dereferences to
    /// the file's path; the dir goes when this does.
    struct Scratch {
        file: std::path::PathBuf,
        _dir: aterm_tempfile::TempDir,
    }

    impl std::ops::Deref for Scratch {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.file
        }
    }

    impl AsRef<Path> for Scratch {
        fn as_ref(&self) -> &Path {
            &self.file
        }
    }

    fn tmp(name: &str) -> Scratch {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-health-test-{name}-"))
            .tempdir()
            .unwrap();
        Scratch {
            file: dir.path().join("health.toml"),
            _dir: dir,
        }
    }

    /// A TIMED HANDOFF CAPTURE IS A PRIOR: it survives a re-read, the newest
    /// measurement replaces the last outright, a zero reading
    /// records nothing, and no streak or standing slot moves for it.
    #[test]
    fn a_timed_handoff_capture_is_kept_as_a_prior_and_moves_no_streak() {
        let p = tmp("handoff-capture");
        Health::record_apply_failure(&p, 9, 909, "ActivityRevoked");
        Health::record_apply_refusal(&p, 9, "the terminal was busy");
        let recorded = Health::record_handoff_capture(&p, 9, 22_000, 2, 33);
        let read = Health::read(&p);
        for h in [&recorded, &read] {
            assert_eq!(
                (
                    h.handoff_capture_us,
                    h.handoff_capture_sessions,
                    h.handoff_freeze_seed_ms,
                    h.handoff_capture_build
                ),
                (22_000, 2, 33, 9)
            );
            assert!(
                !h.handoff_capture_at.is_empty(),
                "a measurement carries its time"
            );
            assert_eq!(h.apply_failures, 1, "no streak moves");
            assert_eq!(h.apply_failures_for_target, 1);
            assert_eq!(h.last_apply_refusal, "the terminal was busy");
            assert!(!h.is_persistent());
        }
        let newer = Health::record_handoff_capture(&p, 10, 9_000, 1, 20);
        assert_eq!(
            (
                newer.handoff_capture_us,
                newer.handoff_capture_sessions,
                newer.handoff_capture_build
            ),
            (9_000, 1, 10),
            "the newest measurement replaces the last, cheaper or not"
        );
        let zero = Health::record_handoff_capture(&p, 11, 0, 3, 20);
        assert_eq!(
            (zero.handoff_capture_us, zero.handoff_capture_build),
            (9_000, 10),
            "a zero reading cannot be told from none, so it records nothing"
        );

        // The LANDED park is its own slot, on the same rules, and neither slot
        // moves the other.
        let parked = Health::record_handoff_park(&p, 12, 22_000);
        assert_eq!(
            (parked.handoff_park_us, parked.handoff_park_build),
            (22_000, 12)
        );
        assert!(!parked.handoff_park_at.is_empty());
        assert_eq!(
            parked.handoff_capture_us, 9_000,
            "the dry run's slot is untouched"
        );
        let zero = Health::record_handoff_park(&p, 13, 0);
        assert_eq!(
            (zero.handoff_park_us, zero.handoff_park_build),
            (22_000, 12)
        );
        let read = Health::read(&p);
        assert_eq!(
            (
                read.handoff_park_us,
                read.handoff_capture_us,
                read.apply_failures
            ),
            (22_000, 9_000, 1)
        );
    }

    /// The stage-backoff check proves ACQUISITION works while deliberately declining to
    /// download, so it must not vouch for the lane it just refused to exercise. A full
    /// `record_success` there erases the very streak the `FailedMark` memo exists
    /// because of, and the machine then reports a clean stage lane for as long as it
    /// keeps skipping the candidate.
    #[test]
    fn an_acquisition_only_success_preserves_the_stage_and_apply_streaks() {
        let p = tmp("acquisition-success");
        Health::record_failure(&p, "network", "dns");
        Health::record_failure(&p, "stage", "bundle would not verify");
        Health::record_apply_failure(&p, 9, 900 + 9, "ActivityRevoked");
        let h = Health::read(&p);
        assert_eq!(
            (h.network_failures, h.stage_failures, h.apply_failures),
            (1, 1, 1)
        );

        let h = Health::record_acquisition_success(&p);
        assert_eq!(h.network_failures, 0, "acquisition streak clears");
        assert!(h.network_since.is_empty(), "and so does its clock");
        assert_eq!(h.stage_failures, 1, "the skipped lane keeps its streak");
        assert!(!h.stage_since.is_empty(), "and its clock");
        assert_eq!(
            h.apply_failures, 1,
            "a check never vouches for the apply lane"
        );
        assert_eq!(h.kind, "stage", "the standing streak owns the headline");
    }

    /// `kind == ""` is the documented HEALTHY sentinel, so the apply-lane clears must
    /// never blank it while an acquisition streak is still standing — that reported a
    /// healthy class on an unhealthy machine, and `update status` printed a non-zero
    /// `failing=` beside no class at all.
    #[test]
    fn clearing_the_apply_lane_names_the_streak_that_is_still_standing() {
        let p = tmp("apply-clear-names-standing");
        Health::record_failure(&p, "manifest", "bad signature");
        Health::record_apply_failure(&p, 11, 900 + 11, "AdoptionMismatch");
        assert_eq!(Health::read(&p).kind, "apply");

        let h = Health::record_apply_success(&p);
        assert_eq!(h.apply_failures, 0);
        assert_eq!(h.manifest_failures, 1, "the acquisition streak survives");
        assert_eq!(h.kind, "manifest", "and is NAMED, not blanked to healthy");

        // The expiry path is the same rule from the other direction.
        let p = tmp("apply-expire-names-standing");
        Health::record_failure(&p, "pipeline", "asset fetch failed");
        Health::record_apply_failure(&p, 11, 900 + 11, "ChildDied");
        let h = Health::expire_stale_apply_streak(&p, 12);
        assert_eq!(h.apply_failures, 0, "a streak from another build is stale");
        assert_eq!(h.pipeline_failures, 1);
        assert_eq!(h.kind, "pipeline", "named, not blanked");
    }

    /// One lock scope: the apply-lane increment and the apply-owned reason must land
    /// together. They used to be two locked writes with a window between them, and
    /// `expire_stale_apply_streak` landing in that window durably dropped the
    /// increment.
    #[test]
    fn an_apply_failure_records_its_streak_and_its_reason_together() {
        let p = tmp("apply-failure-atomic");
        let h = Health::record_apply_failure(&p, 77, 900 + 77, "AdoptionMismatch");
        assert_eq!(h.apply_failures, 1);
        assert_eq!(h.kind, "apply");
        assert_eq!(h.last_apply_error, "AdoptionMismatch");
        assert_eq!(h.last_apply_failure_build, 77);
        assert!(!h.apply_since.is_empty(), "the class clock started");
        // The returned record must equal what a reader sees on disk — i.e. the whole
        // mutation was one write, not a partial one another writer could interleave.
        let reread = Health::read(&p);
        assert_eq!(reread.apply_failures, h.apply_failures);
        assert_eq!(reread.last_apply_error, h.last_apply_error);
        assert_eq!(reread.last_apply_failure_build, h.last_apply_failure_build);
    }

    /// THE SPLICE THIS LEDGER USED TO REPORT. `failing_since` is any-class: it
    /// stamps when the ledger stopped being clean and does not move again until it
    /// is clean. A machine that had been failing to APPLY for days, and only later
    /// met a bad manifest, therefore had its MANIFEST streak dated to the APPLY
    /// streak's start — `update status` read "FAILING (6 consecutive checks since
    /// <four days before the manifest problem existed>)". Each class owns its clock.
    #[test]
    fn each_failure_class_carries_its_own_clock() {
        let p = tmp("per-class-clock");
        Health::record_apply_failure(&p, 5, 900 + 5, "ActivityRevoked");
        let after_apply = Health::read(&p);
        assert_eq!(after_apply.apply_failures, 1);
        assert!(!after_apply.failing_since.is_empty());
        let unhealthy_since = after_apply.failing_since.clone();
        assert_eq!(after_apply.class_since("apply"), unhealthy_since);

        // The manifest lane breaks LATER. The any-class stamp must not move (the
        // ledger never became clean), and the manifest class starts its own clock.
        Health::record_failure(&p, "manifest", "bad signature");
        let h = Health::read(&p);
        assert_eq!(
            h.failing_since, unhealthy_since,
            "the any-class stamp is sticky until the ledger is clean"
        );
        assert_eq!(h.manifest_failures, 1);
        assert_eq!(
            h.class_since("apply"),
            unhealthy_since,
            "the apply streak still dates from when IT started"
        );
        assert!(
            !h.manifest_since.is_empty(),
            "the manifest class started its own clock"
        );

        // A healthy check clears the acquisition class AND its clock; the apply
        // streak and its clock survive, because a check vouches for neither.
        Health::record_success(&p);
        let h = Health::read(&p);
        assert_eq!(h.manifest_failures, 0);
        assert!(
            h.manifest_since.is_empty(),
            "a cleared class must not keep a start time"
        );
        assert_eq!(h.apply_failures, 1);
        assert_eq!(h.class_since("apply"), unhealthy_since);

        // A real apply success clears the last class and its clock.
        Health::record_apply_success(&p);
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 0);
        assert!(h.apply_since.is_empty());
    }

    /// A ledger written BEFORE per-class clocks existed carries no class stamp.
    /// Reading one must not print an empty date: it falls back to the any-class
    /// answer such a ledger has always given, so an in-place upgrade is silent.
    #[test]
    fn a_pre_upgrade_ledger_falls_back_to_the_any_class_clock() {
        let h = Health {
            manifest_failures: 6,
            failing_since: "2026-08-11T07:16:17Z".to_string(),
            ..Health::default()
        };
        assert!(
            h.manifest_since.is_empty(),
            "the field is new; old files lack it"
        );
        assert_eq!(h.class_since("manifest"), "2026-08-11T07:16:17Z");
        assert_eq!(
            h.class_since("not-a-class"),
            "2026-08-11T07:16:17Z",
            "an unrecognised kind falls back too"
        );
    }

    /// THE regression this class exists for. Through 2026-07 the apply lane failed
    /// 100% of the time while the ledger read all-zero, because only the download
    /// lane was recorded and every healthy check cleared everything. Assert the two
    /// halves of the fix: apply failures are recorded, and a healthy CHECK does not
    /// erase them.
    #[test]
    fn a_healthy_check_never_clears_the_apply_streak() {
        let p = tmp("apply-survives-check");
        Health::record_apply_failure(&p, 77, 900 + 77, "AdoptionMismatch");
        Health::record_apply_failure(&p, 77, 900 + 77, "ActivityRevoked");
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 2);
        assert_eq!(h.kind, "apply");
        assert_eq!(h.last_error, "ActivityRevoked");

        // A perfectly healthy download check comes in. It must clear the
        // acquisition streaks and LEAVE the apply streak alone — otherwise the
        // 75 s cadence wipes the evidence before anyone can read it.
        Health::record_failure(&p, "network", "dns");
        Health::record_success(&p);
        let h = Health::read(&p);
        assert_eq!(h.network_failures, 0, "acquisition streak clears");
        assert_eq!(
            h.apply_failures, 2,
            "a healthy check must NOT vouch for the apply lane"
        );
        assert_eq!(
            h.kind, "apply",
            "the standing failure is still the apply one"
        );
        assert_eq!(
            h.last_error, "ActivityRevoked",
            "the standing streak must be described by ITS reason, not the network blip"
        );

        // Only a real apply success clears it.
        Health::record_apply_success(&p);
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 0);
        assert_eq!(h.total_failures(), 0);
        assert!(h.kind.is_empty(), "fully healthy ledger reports no class");
    }

    /// A refusal is the answer to "the build is staged, so why is it not
    /// running?" — it must be durable, must NOT manufacture a failure streak (or
    /// ordinary terminal use would escalate to the persistent-failure
    /// notification), and must not outlive the apply that finally succeeds.
    #[test]
    fn a_refusal_is_recorded_without_inventing_a_failure_streak() {
        let p = tmp("apply-refusal");
        let quiet = "terminal input/output is still inside the quiet epoch";
        Health::record_apply_refusal(&p, 812, quiet);
        let h = Health::read(&p);
        assert_eq!(h.last_apply_refusal, quiet);
        assert!(!h.last_apply_refusal_at.is_empty(), "a refusal is timed");
        assert!(h.apply_refusal_applies_to(812));
        assert_eq!(h.total_failures(), 0, "a refusal is not a failure");
        assert!(!h.is_persistent(), "a refusal never escalates on its own");
        assert!(
            h.kind.is_empty(),
            "a refusal does not claim a failure class"
        );

        // A successful in-session apply execs away and never returns to clear the
        // slot, so the running build is what retires a refusal: build 813 is proof
        // that whatever stopped 812 was overcome.
        assert!(
            !h.apply_refusal_applies_to(813),
            "a refusal must not follow the machine into the build that overcame it"
        );

        // A healthy CHECK must leave it alone for the same reason it leaves the
        // apply streak alone: downloading proves nothing about applying.
        Health::record_failure(&p, "network", "dns");
        Health::record_success(&p);
        assert_eq!(Health::read(&p).last_apply_refusal, quiet);

        // A hard failure supersedes it — one standing answer, never two.
        Health::record_apply_failure(&p, 77, 900 + 77, "ChildDied");
        let h = Health::read(&p);
        assert!(!h.apply_refusal_applies_to(812));
        assert!(h.last_apply_refusal.is_empty());
        assert!(h.last_apply_refusal_at.is_empty());
        assert_eq!(h.last_apply_error, "ChildDied");

        // And a success clears the whole lane, refusal slot included.
        Health::record_apply_refusal(&p, 812, "updater work is in flight");
        Health::record_apply_success(&p);
        let h = Health::read(&p);
        assert!(h.last_apply_refusal.is_empty());
        assert!(h.last_apply_refusal_at.is_empty());
        assert_eq!(h.last_apply_refusal_build, 0);
        assert_eq!(h.apply_failures, 0);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// TWO COUNTS, BECAUSE THERE ARE TWO QUESTIONS.
    ///
    /// `apply_failures` answers "can this machine be moved through the lane at all?"
    /// and is expiry-bound to the RUNNING build, so it deliberately survives a new
    /// download — otherwise a machine whose every stage fails once would never
    /// escalate. But a window offering a build says "aterm tried to update twice",
    /// which is a claim about THAT ARTIFACT, and answering it from the escalation
    /// streak told a person their fresh download had already been attempted. The
    /// artifact-scoped pair below is what a surface may quote.
    #[test]
    fn the_artifact_scoped_count_restarts_with_the_artifact_and_the_escalation_does_not() {
        let p = tmp("apply-target-scope");
        Health::record_apply_failure(&p, 77, 500, "handoff proof ended ChildDied");
        Health::record_apply_failure(&p, 77, 500, "handoff proof ended ChildDied");
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 2);
        assert_eq!(h.apply_failures_for_target, 2);
        assert_eq!(h.last_apply_failure_target_build, 500);

        // A NEWER ARTIFACT IS A NEW QUESTION. The escalation streak keeps counting
        // (the machine still has not moved); the artifact's own count starts over.
        Health::record_apply_failure(&p, 77, 501, "handoff proof ended TimedOut");
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 3, "the machine still has not moved");
        assert_eq!(
            h.apply_failures_for_target, 1,
            "…but THIS build has been tried exactly once"
        );
        assert_eq!(h.last_apply_failure_target_build, 501);

        // Both expire together when the running build proves the streak stale.
        Health::expire_stale_apply_streak(&p, 78);
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 0);
        assert_eq!(h.apply_failures_for_target, 0);
        assert_eq!(h.last_apply_failure_target_build, 0);

        // …and an apply that actually lands clears the artifact-scoped pair even
        // when it is the only thing standing.
        Health::record_apply_failure(&p, 78, 900, "handoff proof ended ChildDied");
        Health::record_apply_success(&p);
        let h = Health::read(&p);
        assert_eq!(h.apply_failures_for_target, 0);
        assert_eq!(h.last_apply_failure_target_build, 0);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// An apply streak recorded by a DIFFERENT running build is proven stale
    /// and expires; the same build's streak stands. The m3 field case: 2,191
    /// counted failures presented as `persistent=true` on a machine that had
    /// long since moved to the fixed build by manual install.
    #[test]
    fn an_apply_streak_expires_once_a_different_build_is_running() {
        let p = tmp("apply-expiry");
        for _ in 0..PERSISTENT_AFTER {
            Health::record_apply_failure(&p, 77, 900 + 77, "handoff proof ended TimedOut");
        }
        let h = Health::read(&p);
        assert!(h.is_persistent());
        assert_eq!(h.last_apply_failure_build, 77);
        assert!(
            !h.last_apply_failure_at.is_empty(),
            "the lane owns its clock"
        );
        // Same build still running: the streak is fresh evidence — stands.
        Health::expire_stale_apply_streak(&p, 77);
        assert!(
            Health::read(&p).is_persistent(),
            "same build ⇒ still standing"
        );
        // A different build is running: the machine moved, the claim is stale.
        Health::expire_stale_apply_streak(&p, 78);
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 0, "stale streak expires");
        assert!(!h.is_persistent());
        assert!(h.last_apply_error.is_empty());
        assert!(h.last_apply_failure_at.is_empty());
        assert_eq!(h.last_apply_failure_build, 0);
        assert!(
            h.kind.is_empty(),
            "an expired lane stops owning the headline"
        );
        // An interleaved acquisition failure keeps ITS OWN streak through the
        // apply expiry — the lanes never launder each other.
        Health::record_failure(&p, "network", "dns");
        Health::record_apply_failure(&p, 78, 900 + 78, "ChildDied");
        Health::expire_stale_apply_streak(&p, 79);
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 0);
        assert_eq!(h.network_failures, 1, "the network streak survives");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// A stranded apply lane must escalate as loudly as a stranded download lane:
    /// the user-visible symptom (machine does not move to the new version) is the
    /// same, so it drives the same persistent-failure notification.
    #[test]
    fn a_persistent_apply_streak_is_surfaced_like_a_persistent_pipeline_streak() {
        let p = tmp("apply-persistent");
        for _ in 0..PERSISTENT_AFTER {
            Health::record_apply_failure(&p, 77, 900 + 77, "ChildDied");
        }
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, PERSISTENT_AFTER);
        assert!(
            h.is_persistent(),
            "a staged build that never applies must escalate, not sit in a file"
        );
        // And a healthy check does not silence it.
        Health::record_success(&p);
        assert!(Health::read(&p).is_persistent());
    }

    /// A MANIFEST STREAK ESCALATES. The class cannot heal on the client — an unsigned
    /// release stays unsigned until the publisher republishes — so a streak of it
    /// means this Mac cannot update.
    #[test]
    fn a_manifest_streak_escalates_and_names_its_own_class() {
        let p = tmp("manifest-persistent");
        for _ in 0..PERSISTENT_AFTER {
            Health::record_failure(
                &p,
                "manifest",
                "authoritative update v0.61 is unsigned under the pinned channel",
            );
        }
        backdate_streaks(&p, PERSISTENT_SPAN);
        let h = Health::read(&p);
        assert_eq!(h.manifest_failures, PERSISTENT_AFTER);
        assert!(
            h.is_persistent(),
            "an untrustworthy authoritative release strands the machine exactly as \
             surely as one that cannot be downloaded"
        );
        // The notification takes its count and its sentence from HERE, so the class
        // must be named and the count must be its own — never `pipeline_failures`,
        // which is 0 in this state and produced "0 consecutive checks".
        assert_eq!(h.persistent_class(), Some(("manifest", PERSISTENT_AFTER)));
        assert_eq!(
            h.pipeline_failures, 0,
            "the incident's pipeline lane was fine"
        );
    }

    /// `stage` strands too: the artifact arrives and then refuses to become a bundle.
    #[test]
    fn a_stage_streak_escalates_and_names_its_own_class() {
        let p = tmp("stage-persistent");
        for _ in 0..PERSISTENT_AFTER {
            Health::record_failure(&p, "stage", "sha256 mismatch");
        }
        backdate_streaks(&p, PERSISTENT_SPAN);
        let h = Health::read(&p);
        assert!(h.is_persistent());
        assert_eq!(h.persistent_class(), Some(("stage", PERSISTENT_AFTER)));
    }

    /// `network` is the one class that must NOT alarm: it is genuinely transient and
    /// is already answered by slowing the cadence.
    #[test]
    fn a_network_streak_never_escalates() {
        let p = tmp("network-not-persistent");
        for _ in 0..(PERSISTENT_AFTER * 4) {
            Health::record_failure(&p, "network", "dns");
        }
        backdate_streaks(&p, PERSISTENT_SPAN * 4);
        let h = Health::read(&p);
        assert!(h.network_failures >= PERSISTENT_AFTER);
        assert!(
            !h.is_persistent(),
            "a flaky network must not tell the user their updater is broken"
        );
        assert_eq!(h.persistent_class(), None);
    }

    /// THREE FAILED CHECKS ARE PERSISTENT ONLY ONCE THEY SPAN HALF AN HOUR. At the
    /// two-minute cadence three failures of a check class come 0, 2 and 6 minutes
    /// apart — a GitHub incident or a slow asset upload, not the broken lane the
    /// status, the update screen's headline and the loud notice promise. The same
    /// three spanning 30 minutes escalate. The span ends at the class's own newest
    /// failure: a DNS blip hours later (weather, which never resets a standing
    /// diagnosis) moves only the any-class stamp and stretches nothing. A ledger an
    /// older build wrote, with no `last_diagnosis_at`, is measured to its
    /// `last_failure_at`. `apply` counts attempts the apply lane paces itself, and
    /// three quick ones still escalate.
    #[test]
    fn a_check_class_streak_is_persistent_only_once_it_spans_half_an_hour() {
        let streak = |class: &str, count: u32, newest: &str| {
            let mut h = Health {
                last_failure_at: newest.to_string(),
                last_diagnosis_at: newest.to_string(),
                kind: class.to_string(),
                ..Health::default()
            };
            let (n, since) = h.class_streak_mut(class).expect("a ledger class");
            *n = count;
            *since = "2026-10-15T10:00:00Z".to_string();
            h
        };
        for class in ["pipeline", "manifest", "stage"] {
            let quick = streak(class, PERSISTENT_AFTER, "2026-10-15T10:06:00Z");
            assert_eq!(
                quick.persistent_class(),
                None,
                "{class}: three inside 7 minutes"
            );
            let long = streak(class, PERSISTENT_AFTER, "2026-10-15T10:30:00Z");
            assert_eq!(
                long.persistent_class(),
                Some((class, PERSISTENT_AFTER)),
                "{class}: three spanning 30 minutes"
            );
            // NEGATIVE CONTROL: the span alone is not a streak.
            let few = streak(class, PERSISTENT_AFTER - 1, "2026-10-15T14:00:00Z");
            assert_eq!(few.persistent_class(), None, "{class}: two in four hours");
            // A blip hours after a quick streak moves only `last_failure_at`.
            let mut blipped = quick.clone();
            blipped.network_failures = 1;
            blipped.last_failure_at = "2026-10-15T18:00:00Z".to_string();
            assert_eq!(
                blipped.persistent_class(),
                None,
                "{class}: weather stretches nothing"
            );
            // An older build's ledger: the any-class stamp is all there is.
            let mut older = long.clone();
            older.last_diagnosis_at.clear();
            assert_eq!(older.persistent_class(), Some((class, PERSISTENT_AFTER)));
            older.last_failure_at = "2026-10-15T10:06:00Z".to_string();
            assert_eq!(older.persistent_class(), None);
        }
        let apply = streak("apply", PERSISTENT_AFTER, "2026-10-15T10:06:00Z");
        assert_eq!(apply.persistent_class(), Some(("apply", PERSISTENT_AFTER)));

        // Through the writer: three booked at once are not persistent, the same three
        // once their streak began half an hour earlier are, and a success ends it.
        let p = tmp("persistent-span");
        for _ in 0..PERSISTENT_AFTER {
            Health::record_failure(&p, "pipeline", "asset fetch failed");
        }
        let h = Health::read(&p);
        assert_eq!(h.pipeline_failures, PERSISTENT_AFTER);
        assert!(!h.is_persistent(), "{h:?}");
        assert_eq!(
            h.last_diagnosis_at, h.last_failure_at,
            "the class's own clock"
        );
        backdate_streaks(&p, PERSISTENT_SPAN);
        assert_eq!(Health::read(&p).persistent_class(), Some(("pipeline", 3)));
        let h = Health::record_success(&p);
        assert!(
            h.last_diagnosis_at.is_empty() && !h.is_persistent(),
            "{h:?}"
        );
    }

    #[test]
    fn failure_streaks_count_per_class() {
        let p = tmp("streaks");
        assert!(
            !Health::read(&p).is_persistent(),
            "absent ledger reads healthy"
        );
        Health::record_failure(&p, "network", "dns");
        Health::record_failure(&p, "network", "dns");
        let h = Health::read(&p);
        assert_eq!((h.network_failures, h.kind.as_str()), (2, "network"));
        assert!(
            !h.is_persistent(),
            "network failures never escalate to persistent"
        );

        Health::record_failure(&p, "pipeline", "asset fetch failed");
        Health::record_failure(&p, "pipeline", "asset fetch failed");
        // An interleaved network blip must NOT reset the pipeline streak (or a flaky
        // network could suppress the stranded-client escalation forever).
        Health::record_failure(&p, "network", "blip");
        backdate_streaks(&p, PERSISTENT_SPAN);
        let h = Health::record_failure(&p, "pipeline", "asset fetch failed");
        assert_eq!(h.pipeline_failures, PERSISTENT_AFTER);
        assert!(
            h.is_persistent(),
            "3 pipeline failures are persistent despite the blip"
        );
        assert_eq!(h.total_failures(), 3 + 3);
        assert!(!h.failing_since.is_empty() && !h.last_error.is_empty());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn success_clears_all_failure_streaks() {
        let p = tmp("success");
        Health::record_failure(&p, "pipeline", "x");
        Health::record_failure(&p, "stage", "y");
        let h = Health::record_success(&p);
        assert_eq!(h.total_failures(), 0);
        assert!(h.kind.is_empty() && h.last_error.is_empty() && h.failing_since.is_empty());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn success_on_clean_ledger_does_not_write() {
        let p = tmp("clean-noop");
        // Absent ledger is already fully clean: a healthy check must not create it.
        assert!(!p.exists());
        let h = Health::record_success(&p);
        assert_eq!(h.total_failures(), 0);
        assert!(
            !p.exists(),
            "success on an already-clean ledger must not write the file"
        );

        // A clean ledger that DOES exist (a streak just cleared by a success) is
        // likewise not rewritten: capture the mtime and assert it is unchanged
        // across a second success.
        Health::record_failure(&p, "network", "blip");
        Health::record_success(&p);
        assert!(p.exists());
        let before = std::fs::metadata(&p).unwrap().modified().unwrap();
        let h = Health::record_success(&p);
        assert_eq!(h.total_failures(), 0);
        let after = std::fs::metadata(&p).unwrap().modified().unwrap();
        assert_eq!(
            before, after,
            "success on an already-clean ledger must not rewrite it"
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn corrupt_ledger_reads_healthy_and_error_is_capped() {
        let p = tmp("corrupt");
        std::fs::write(&p, "not = [valid").unwrap();
        assert_eq!(Health::read(&p).total_failures(), 0);
        let long = "e".repeat(2000);
        let h = Health::record_failure(&p, "pipeline", &long);
        assert_eq!(h.last_error.len(), 400, "stored error text is capped");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// A PASSING REFUSAL LONGER THAN THE CAP IS STILL A PASSING REFUSAL WHEN STORED.
    /// `Mounted::attach` joins four timed-out `hdiutil attach` attempts into one
    /// ~456-character message whose passing key is at the end.
    #[test]
    fn a_long_passing_refusal_keeps_its_key_in_the_ledger() {
        let p = tmp("long-passing");
        let attempt = "hdiutil attach did not finish within 120s; treating as a failure";
        let error = crate::verify::mark_passing(format!(
            "hdiutil attach failed after 4 attempts (/Users//someone/Library/Application \
             Support/aterm/Updates/download/aterm-0.93.0.dmg, 52428800 bytes): {}",
            [attempt; 4].join(" | ")
        ));
        assert!(error.chars().count() > 400, "{}", error.len());
        assert!(crate::is_passing_refusal(&error));
        for _ in 0..crate::PERSISTENT_AFTER {
            Health::record_failure(&p, "stage", &error);
        }
        let h = Health::read(&p);
        assert!(h.last_error.chars().count() <= 400, "still capped");
        assert!(
            h.last_error
                .starts_with("hdiutil attach failed after 4 attempts")
        );
        assert!(crate::is_passing_refusal(&h.last_error), "{}", h.last_error);
        let (_, body) = crate::persistent_failure_notice("stage", h.stage_failures, &h, 1);
        assert!(
            body.contains("could not finish") && !body.contains("will not verify"),
            "{body}"
        );
        // A network blip on top of the standing stage streak overwrites `last_error`;
        // the notice describes the STAGE streak by the stage class's own reason.
        let h = Health::record_failure(&p, "network", "could not resolve the host");
        assert!(h.stage_failures >= crate::PERSISTENT_AFTER, "{h:?}");
        let (_, body) = crate::persistent_failure_notice("stage", h.stage_failures, &h, 1);
        assert!(
            body.contains("could not finish") && body.contains("hdiutil"),
            "{body}"
        );
        // The apply lane's slots keep it the same way.
        let h = Health::record_apply_failure(&p, 1, 2, &error);
        assert!(crate::is_passing_refusal(&h.last_apply_error));
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// NO SPLICED COUNT: a ledger carrying a dead, clock-less `manifest` streak of 999
    /// meets this build's `pipeline` failures. A pipeline failure CLEARS its sibling
    /// diagnoses, the reported count is the standing class's own, and a clock-less
    /// count restarts under a clock of its own.
    #[test]
    fn a_pipeline_failure_clears_its_sibling_diagnoses_and_the_count_is_its_own() {
        let p = tmp("sibling-clear");
        // The inherited ledger: a big manifest streak with no per-class clock, and a
        // pipeline count with no clock either (both written by an older build).
        let legacy = Health {
            manifest_failures: 999,
            pipeline_failures: 19,
            kind: "pipeline".to_string(),
            failing_since: "2026-08-27T22:04:36Z".to_string(),
            ..Health::default()
        };
        legacy.write(&p);
        let h = Health::record_failure(&p, "pipeline", "HTTP 404 on the appcast");
        assert_eq!(h.manifest_failures, 0, "the sibling diagnosis is retired");
        assert!(h.manifest_since.is_empty());
        assert_eq!(
            h.pipeline_failures, 1,
            "a clock-less count is another build's; this ledger's streak starts at one"
        );
        assert!(
            !h.pipeline_since.is_empty() && h.pipeline_since != "2026-08-27T22:04:36Z",
            "the pipeline clock is its own, not the any-class fallback: {}",
            h.pipeline_since
        );
        assert_eq!(
            h.acquisition_failures(),
            1,
            "the count is the standing class's"
        );
        assert_eq!(h.standing_acquisition_class(), Some("pipeline"));

        // A network blip still resets nothing (the module's standing rule)…
        Health::record_failure(&p, "pipeline", "again");
        let h = Health::record_failure(&p, "network", "dns");
        assert_eq!(h.pipeline_failures, 2);
        assert_eq!(
            h.acquisition_failures(),
            2,
            "the pipeline streak outranks the blip and is reported alone, not summed"
        );
        // …and the COUNT and the CLASS the status line prints name the same
        // streak: `kind` records the LAST failure (the blip), so a renderer that
        // paired `acquisition_failures()` with `kind` would print `2:network` —
        // two failures of a class that failed once. The standing class is the
        // label that goes with the count; `UpdateStatus` renders from that pair
        // (`failing_checks` / `failing_checks_kind`), never from `kind`.
        assert_eq!(h.kind, "network", "the last failure was the blip");
        assert_eq!(
            h.standing_acquisition_class(),
            Some("pipeline"),
            "the label beside the count is the standing class, not the last blip"
        );
        assert_eq!(h.network_failures, 1);
        assert_ne!(
            (h.acquisition_failures(), h.kind.as_str()),
            (h.network_failures, "network"),
            "count and kind must not be read as one pair"
        );
        // …and a manifest verdict retires the pipeline streak, because the manifest
        // was evidently fetched this time.
        let h = Health::record_failure(&p, "manifest", "bad signature");
        assert_eq!((h.pipeline_failures, h.manifest_failures), (0, 1));
        assert!(h.pipeline_since.is_empty());
        assert_eq!(h.acquisition_failures(), 1);
        assert_eq!(
            h.network_failures, 1,
            "weather is neither reset nor summed in"
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// NO SPLICED REASON. Counts and clocks are per class, but there is ONE
    /// `last_error`, and it belongs to whichever failure landed LAST. Every reader
    /// pairs the STANDING class with it: `UpdateStatus` prints `failing_checks_kind`
    /// beside `last_error`, and the loud notice says "updates download but will not
    /// verify or install (<last_error>)". A network blip on top of a standing stage
    /// streak therefore describes a bundle that will not verify with a DNS error —
    /// and the acquisition-only success the backoff path records EVERY cycle after
    /// it (`stage_backoff` → `record_acquisition_success`) re-labels the ledger
    /// `stage` while leaving the blip's text in place, so the splice becomes the
    /// standing description for the life of the memo (up to 24 h).
    #[test]
    fn a_standing_diagnosis_keeps_its_own_reason_across_a_blip() {
        let p = tmp("diagnosis-reason");
        let stage_reason =
            "staged bundle failed verification: codesign --verify (team-pinned requirement) failed";
        Health::record_failure(&p, "stage", stage_reason);
        Health::record_failure(&p, "stage", stage_reason);
        Health::record_failure(&p, "stage", stage_reason);
        backdate_streaks(&p, PERSISTENT_SPAN);
        let h = Health::record_failure(&p, "network", "curl: (6) could not resolve host");
        assert_eq!(h.standing_acquisition_class(), Some("stage"));
        assert_eq!(h.persistent_class(), Some(("stage", 3)));

        // The backed-off check that follows: acquisition worked, the memo skipped the
        // download. The ledger is re-described by the standing class …
        let h = Health::record_acquisition_success(&p);
        assert_eq!(h.kind, "stage");
        assert_eq!(h.stage_failures, 3);
        // … and the reason printed beside that class must be the stage lane's own,
        // not the blip that happened to land last.
        assert!(
            h.last_error.contains("codesign"),
            "the standing stage diagnosis is described by a network blip: {:?}",
            h.last_error
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// The same splice from the apply-expiry side: `expire_stale_apply_streak` blanks
    /// `last_error` while NAMING a standing acquisition class, so the status line
    /// would read `failing=1:stage` with no reason at all.
    #[test]
    fn expiring_a_stale_apply_streak_leaves_the_standing_diagnosis_its_reason() {
        let p = tmp("expire-keeps-reason");
        let stage_reason = "ditto zip extract failed (exit status: 1)";
        Health::record_failure(&p, "stage", stage_reason);
        Health::record_apply_failure(&p, 11, 900 + 11, "ChildDied");
        assert_eq!(Health::read(&p).kind, "apply");
        let h = Health::expire_stale_apply_streak(&p, 12);
        assert_eq!(h.apply_failures, 0);
        assert_eq!(h.kind, "stage", "named, not blanked");
        assert!(
            h.last_error.contains("ditto"),
            "a named standing class must carry its reason, not an empty string: {:?}",
            h.last_error
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// THE PENDING CLOCK SURVIVES RE-STAGES (`ready.toml`'s `staged_at` does not).
    /// The first sighting
    /// of a build wins; a different newer build starts its own clock; an
    /// observation that merely found nothing (the stage retired for the very swap
    /// that strands a machine) keeps it; only a running build that caught up
    /// drops it.
    #[test]
    fn the_pending_clock_is_the_first_sighting_and_survives_a_restage() {
        let p = tmp("pending-clock");
        let first = Health::note_pending_update(&p, 10, Some(11));
        assert_eq!(first.pending_build, 11);
        assert!(
            !first.pending_since.is_empty(),
            "the first sighting is stamped"
        );
        // Back-date the stamp so a rewrite would be visible.
        let mut aged = Health::read(&p);
        aged.pending_since = "2026-09-22T00:10:45Z".to_string();
        aged.write(&p);
        // The same build observed again (a re-stage, the next check cycle).
        let again = Health::note_pending_update(&p, 10, Some(11));
        assert_eq!(
            again.pending_since, "2026-09-22T00:10:45Z",
            "re-observing the same build must never move its clock"
        );
        // The stage vanished for a moment (the candidate's own swap retired it):
        // no evidence the machine moved, so the clock stands.
        let gone = Health::note_pending_update(&p, 10, None);
        assert_eq!(
            (gone.pending_build, gone.pending_since.as_str()),
            (11, "2026-09-22T00:10:45Z")
        );
        // An observation no newer than the running build is not a pending one.
        let stale = Health::note_pending_update(&p, 10, Some(10));
        assert_eq!(
            stale.pending_build, 11,
            "a non-newer observation changes nothing"
        );
        // A different, newer build is a new wait with its own clock.
        let newer = Health::note_pending_update(&p, 10, Some(12));
        assert_eq!(newer.pending_build, 12);
        assert_ne!(newer.pending_since, "2026-09-22T00:10:45Z");
        // The running build caught up: the record is moot and is dropped.
        let landed = Health::note_pending_update(&p, 12, None);
        assert_eq!(
            (landed.pending_build, landed.pending_since.as_str()),
            (0, "")
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
