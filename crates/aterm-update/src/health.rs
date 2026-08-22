// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The updater's SELF-HEALING ledger (`…/aterm/Updates/health.toml`): a durable record
//! of check failures by CLASS — the memory that lets a silent background updater
//! notice it is persistently broken and say so.
//!
//! Build 826 shipped with a broken download pipeline and reported every failure as
//! "transient": nothing distinguished "the network blipped" from "this binary can
//! never download again", so a permanently stranded client looked healthy. The ledger
//! closes that hole with PER-CLASS failure streaks:
//!
//! * `network` — the releases LIST itself failed: GitHub unreachable / auth broken /
//!   unparseable. Genuinely transient things land here.
//! * `pipeline` — release metadata was visible but an asset that provably exists
//!   could not be fetched. The network is fine, so the download machinery itself is
//!   suspect — the build-826 signature. A `pipeline` streak of [`PERSISTENT_AFTER`]
//!   is surfaced loudly ([`Health::is_persistent`] drives the status wording and a
//!   user notification).
//! * `manifest` — a manifest was FETCHED but rejected (bad signature / unparseable):
//!   a release-side or under-attack state, visible in status but not the pipeline's
//!   fault.
//! * `stage` — the artifact downloaded but failed verification/staging (also
//!   memoized by `FailedMark`).
//! * `apply` — a VERIFIED, STAGED build failed to become the running build: the
//!   seamless in-session handoff was refused/revoked/mismatched, the child died,
//!   or the swap+re-exec failed. This is the class that used to be missing (see
//!   SCOPE below); it is recorded by [`Health::record_apply_failure`] from the
//!   GUI's apply lane, and it is deliberately NOT cleared by a healthy download
//!   check — only by an apply that actually succeeds
//!   ([`Health::record_apply_success`]). A check succeeding says nothing about
//!   whether applying works, and conflating the two is exactly how a 100%-failing
//!   apply lane read green.
//!
//! The streaks are PER-CLASS (each cleared only by a fully healthy check), so an
//! interleaved network blip cannot reset a pipeline streak and suppress the
//! escalation. (An on-disk ledger from a pre-v0.26 build may carry the retired
//! rescue-path counters; unknown keys parse fine and are dropped on the next write.)
//!
//! # SCOPE — the apply lane is now covered too (2026-07-28)
//!
//! This ledger used to cover the CHECK/DOWNLOAD/STAGE lane ONLY, and said so. The
//! gap was not hypothetical: through 2026-07 this machine carried an all-zero
//! `health.toml` — a perfect score — while the seamless apply/handoff lane failed
//! 100% of the time across three releases and every update had to wait for a cold
//! launch. Downloading and staging really were healthy; the ledger was telling the
//! truth about the only thing it measured, and the dashboard read green on a
//! half-broken updater.
//!
//! The `apply` class closes it. Two rules keep it honest:
//!
//! 1. **A healthy CHECK does not clear it.** [`Health::record_success`] resets the
//!    acquisition streaks and leaves `apply_failures` alone, because "I can see
//!    and download the release" is not evidence that applying it works. Only
//!    [`Health::record_apply_success`] clears it.
//! 2. **It counts toward [`Health::is_persistent`].** A staged build that will not
//!    apply strands the machine exactly as surely as one that will not download,
//!    so it escalates to the same loud notification rather than sitting in a file.

use aterm_update_core::FileLock;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Consecutive `pipeline` failures at which the state is called PERSISTENT: the
/// status wording stops saying "deferred" and the GUI raises a notification.
/// (Re-exported from the crate root so cross-platform status consumers share the
/// one threshold.)
///
/// This 3 was chosen against a 6h check interval, where it meant "≈18h — long
/// enough to skip a flaky day". **That interval is retired.** The cadence is now
/// 75s (`ATERM_UPDATE_INTERVAL_SECS`, `spawn_background_check`), so 3 consecutive
/// failures is ≈4 minutes: the threshold now means "three checks in a row", and it
/// escalates far sooner than the original rationale intended. It has not been
/// re-tuned for the new cadence — revisit it against 75s rather than trusting the
/// old "skip a flaky day" reading.
pub use crate::PERSISTENT_AFTER;

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
    /// healthy download check — see the module SCOPE note.
    #[serde(default)]
    pub apply_failures: u32,
    /// RFC3339 UTC at which each class's CURRENT streak began (empty when that
    /// class is not failing) — the class's own clock, read via
    /// [`Self::class_since`].
    ///
    /// [`Self::failing_since`] is any-class by definition: it stamps the moment the
    /// ledger stopped being clean and does not move again until it is. Reporting a
    /// per-class streak COUNT beside it therefore splices two different failures
    /// into one sentence. Observed in the field on 2026-08-17: a machine carrying a
    /// long-standing `apply` streak from 08-11 met a `manifest` failure on 08-15 and
    /// `update status` read "FAILING (6 consecutive checks since 2026-08-11…)" — the
    /// count from one class, the date from another, describing a problem as four
    /// days older than it was and sending the diagnosis down the wrong lane.
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
    /// RFC3339 UTC of [`Self::last_apply_error`] (empty when there is none).
    ///
    /// [`Self::last_failure_at`] is any-class: a DNS blip at 21:53 re-dates a
    /// ledger whose apply lane last actually failed days earlier, and `update
    /// status` then presents a long-dead streak as fresh (observed on m3,
    /// 2026-08-14: the "last failure" was a `curl` resolve error, not an
    /// apply). The apply lane owns its own clock.
    #[serde(default)]
    pub last_apply_failure_at: String,
    /// The build that was RUNNING when the last apply failure was recorded
    /// (0 when unknown/none) — the apply-streak twin of
    /// [`Self::last_apply_refusal_build`], and the expiry key
    /// [`Self::expire_stale_apply_streak`] reads: an apply streak is only
    /// ever the claim "THIS running build cannot be replaced through the
    /// lane", and once a DIFFERENT build is running the machine has moved —
    /// through the channel, a manual install, or a boot swap — so the claim
    /// is proven stale. Without this, a manually-updated healthy machine
    /// carried `persistent=true` forever (m3: 2,191 counted failures on an
    /// up-to-date install).
    #[serde(default)]
    pub last_apply_failure_build: u64,
    /// The most recent apply-lane REFUSAL: a verdict that stopped an apply
    /// BEFORE it could fail, in the words of whichever caller refused it.
    ///
    /// Deliberately not a streak and deliberately absent from
    /// [`Self::total_failures`]/[`Self::is_persistent`]: a refusal is a normal,
    /// self-correcting state, and counting one would manufacture a persistent-
    /// failure escalation out of ordinary terminal use. That reasoning is why
    /// refusals were recorded NOWHERE — which is the defect this field closes.
    /// In the field, `update apply` answered "OK apply requested", the reducer
    /// refused it, and `update status` went on reporting a healthy updater with
    /// a staged build for hours: a refusal that records nothing is
    /// indistinguishable from an updater that never ran at all.
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
}

impl Health {
    /// Read the ledger; absent/corrupt ⇒ healthy default (observability, not a gate).
    /// Non-UTF-8 IS a corrupt ledger, deliberately folded to the healthy default.
    #[must_use]
    pub fn read(path: &Path) -> Self {
        crate::read_ledger_text(path)
            .and_then(|t| toml::from_str(&t).ok())
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
    /// move to an update?" versus "can it fetch one?"), and summing the two made the
    /// `failing=` field double-count: a machine with two network blips beside a
    /// standing apply streak of 7 printed `failing=9:network failing_applies=7` —
    /// nine consecutive failures of class `network` when two checks had failed —
    /// sending a reader after an acquisition fault that did not exist.
    #[must_use]
    pub fn acquisition_failures(&self) -> u32 {
        self.network_failures
            .saturating_add(self.pipeline_failures)
            .saturating_add(self.manifest_failures)
            .saturating_add(self.stage_failures)
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
    /// Callers need the CLASS, not just the boolean: the count and the sentence a
    /// user is shown have to come from the streak that actually escalated. Reporting
    /// `pipeline_failures` for an `apply`-class escalation printed "0 consecutive
    /// checks … cannot be downloaded" — a nonsense count attached to the wrong lane —
    /// because a healthy check zeroes the acquisition streaks and deliberately
    /// preserves only `apply` ([`Self::record_success`]).
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
        .find(|(_, n)| *n >= PERSISTENT_AFTER)
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
    ///
    /// Omitting `manifest` and `stage` is not hypothetical. On 2026-07-25 a machine
    /// carried `manifest_failures = 597` — an unsigned release under a pinned channel,
    /// ~13 hours — and nothing escalated, because this predicate watched only
    /// `pipeline` and `apply`. That is precisely the silent stranding this ledger
    /// exists to prevent; see the regression test below.
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
            }
            *count = count.saturating_add(1);
        }
        h.kind = kind.to_string();
        if h.failing_since.is_empty() {
            h.failing_since = now.clone();
        }
        h.last_failure_at = now;
        // Cap the stored error so a pathological message can't bloat the ledger.
        h.last_error = error.chars().take(400).collect();
    }

    /// Record a fully-healthy CHECK: every ACQUISITION streak clears.
    ///
    /// `apply_failures` deliberately survives. A check proves the machine can see,
    /// fetch, verify and stage a release; it proves nothing about whether that
    /// staged build can be made to run, which is a different lane with different
    /// failure modes. Clearing it here would recreate the exact blindness this
    /// class was added to remove — every 75 s check would wipe the evidence that
    /// the handoff never completes. Use [`Self::record_apply_success`] for that.
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
            h.kind = String::new();
            h.failing_since = String::new();
            h.last_failure_at = String::new();
            h.last_error = String::new();
        } else {
            // The acquisition streaks cleared but the machine still cannot APPLY.
            // Re-describe the ledger in terms of the failure that is actually
            // still standing, or `update status` would report the apply streak
            // under whatever transient network message happened to land last.
            h.kind = "apply".to_string();
            h.last_error = h.last_apply_error.clone();
        }
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
    ///
    /// Same shape, and the same reasoning, as `record_success` preserving the apply
    /// streak — see this module's SCOPE note.
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
        if h.total_failures() == 0 {
            h.kind = String::new();
            h.failing_since = String::new();
            h.last_failure_at = String::new();
            h.last_error = String::new();
        } else if let Some(standing) = h.standing_acquisition_class() {
            h.kind = standing.to_string();
        } else {
            // Only the apply streak survived; describe the ledger by IT, exactly as
            // `record_success` does, or status would explain a standing apply failure
            // with whatever acquisition message happened to land last.
            h.kind = "apply".to_string();
            h.last_error = h.last_apply_error.clone();
        }
        h.write(path);
        h
    }

    /// Record that a staged build FAILED to become the running build, as
    /// observed by `current_build`. `reason` is the typed handoff/apply
    /// outcome (e.g. `ChildDied`, `AdoptionMismatch`, `ActivityRevoked`,
    /// `re-exec failed`), stored for `update status`.
    pub fn record_apply_failure(path: &Path, current_build: u64, reason: &str) -> Self {
        // ONE LOCK SCOPE, deliberately. This used to call `record_failure` (which
        // locks, writes and releases) and then re-lock to mirror the reason. Between
        // the two scopes another writer could land — `expire_stale_apply_streak` runs
        // from the check lane on its own cadence — read the just-incremented ledger,
        // decide the streak was recorded by a different build, zero it, and write. The
        // increment was then durably gone, and the apply lane under-counted exactly
        // when it was failing often enough to interleave.
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        Self::advance_failure_streak(&mut h, "apply", reason);
        // Mirror the reason into the apply-owned slot so a later acquisition-lane
        // failure cannot overwrite the description of a still-standing apply streak.
        h.last_apply_error = reason.chars().take(400).collect();
        h.last_apply_failure_at = crate::install::now_rfc3339();
        h.last_apply_failure_build = current_build;
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
    /// prove it FRESH, and the demo-blocking failure mode is a stale streak
    /// presented as standing (the reverse error self-corrects: a real
    /// failing lane re-records within one apply attempt).
    pub fn expire_stale_apply_streak(path: &Path, current_build: u64) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if h.apply_failures == 0 || h.last_apply_failure_build == current_build {
            return h;
        }
        h.apply_failures = 0;
        h.apply_since = String::new();
        h.last_apply_error = String::new();
        h.last_apply_failure_at = String::new();
        h.last_apply_failure_build = 0;
        if h.total_failures() == 0 {
            h.kind = String::new();
            h.failing_since = String::new();
            h.last_failure_at = String::new();
            h.last_error = String::new();
        } else if h.kind == "apply" {
            // Some acquisition streak still stands; let it own the headline. NAMING it
            // matters: `kind == ""` is the documented HEALTHY sentinel, so blanking it
            // here reported a healthy class on a machine with a live streak, and
            // `update status` printed a non-zero `failing=` beside no class at all.
            h.kind = h
                .standing_acquisition_class()
                .unwrap_or_default()
                .to_string();
            h.last_error = String::new();
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
        h.last_apply_refusal = reason.chars().take(400).collect();
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

    /// Record that an apply actually succeeded — the staged build is now the
    /// running build. Clears the `apply` streak only; the acquisition streaks are
    /// owned by [`Self::record_success`].
    pub fn record_apply_success(path: &Path) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        // An apply that actually went through answers every standing apply-lane
        // question, refusals included: a "the terminal was busy" left behind by an
        // earlier attempt must not outlive the attempt that succeeded.
        if h.apply_failures == 0 && h.last_apply_refusal.is_empty() {
            return h;
        }
        h.clear_apply_refusal();
        if h.apply_failures > 0 {
            h.apply_failures = 0;
            h.apply_since = String::new();
            h.last_apply_error = String::new();
            if h.total_failures() == 0 {
                h.kind = String::new();
                h.failing_since = String::new();
                h.last_failure_at = String::new();
                h.last_error = String::new();
            } else if h.kind == "apply" {
                // Acquisition failures are still standing; stop describing the ledger
                // by the apply failure that just resolved — and NAME the one that is
                // still standing, because `kind == ""` means healthy.
                h.kind = h
                    .standing_acquisition_class()
                    .unwrap_or_default()
                    .to_string();
            }
        }
        h.write(path);
        h
    }

    /// Best-effort sibling lock (`…/health.toml.lock`) guarding a whole read→mutate→
    /// write of the ledger. `None` on failure: the caller then proceeds unlocked —
    /// health is observability, never a gate, so a missed lock must never drop the
    /// update. Held for the lifetime of the returned guard (i.e. the record_* call).
    fn lock(path: &Path) -> Option<FileLock> {
        FileLock::acquire(&path.with_extension("toml.lock")).ok()
    }

    /// Best-effort atomic write (temp + rename), mirroring `status::record`.
    fn write(&self, path: &Path) {
        use std::sync::atomic::{AtomicU64, Ordering};
        let Ok(text) = toml::to_string(self) else {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Per-TEST scratch dir (tests share one process, so a pid-keyed dir would race
    /// across the parallel test threads).
    fn tmp(name: &str) -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("aterm-health-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("health.toml")
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
        Health::record_apply_failure(&p, 9, "ActivityRevoked");
        let h = Health::read(&p);
        assert_eq!((h.network_failures, h.stage_failures, h.apply_failures), (1, 1, 1));

        let h = Health::record_acquisition_success(&p);
        assert_eq!(h.network_failures, 0, "acquisition streak clears");
        assert!(h.network_since.is_empty(), "and so does its clock");
        assert_eq!(h.stage_failures, 1, "the skipped lane keeps its streak");
        assert!(!h.stage_since.is_empty(), "and its clock");
        assert_eq!(h.apply_failures, 1, "a check never vouches for the apply lane");
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
        Health::record_apply_failure(&p, 11, "AdoptionMismatch");
        assert_eq!(Health::read(&p).kind, "apply");

        let h = Health::record_apply_success(&p);
        assert_eq!(h.apply_failures, 0);
        assert_eq!(h.manifest_failures, 1, "the acquisition streak survives");
        assert_eq!(h.kind, "manifest", "and is NAMED, not blanked to healthy");

        // The expiry path is the same rule from the other direction.
        let p = tmp("apply-expire-names-standing");
        Health::record_failure(&p, "pipeline", "asset fetch failed");
        Health::record_apply_failure(&p, 11, "ChildDied");
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
        let h = Health::record_apply_failure(&p, 77, "AdoptionMismatch");
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
        Health::record_apply_failure(&p, 5, "ActivityRevoked");
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
        assert!(h.manifest_since.is_empty(), "the field is new; old files lack it");
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
        Health::record_apply_failure(&p, 77, "AdoptionMismatch");
        Health::record_apply_failure(&p, 77, "ActivityRevoked");
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
        Health::record_apply_failure(&p, 77, "ChildDied");
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

    /// An apply streak recorded by a DIFFERENT running build is proven stale
    /// and expires; the same build's streak stands. The m3 field case: 2,191
    /// counted failures presented as `persistent=true` on a machine that had
    /// long since moved to the fixed build by manual install.
    #[test]
    fn an_apply_streak_expires_once_a_different_build_is_running() {
        let p = tmp("apply-expiry");
        for _ in 0..PERSISTENT_AFTER {
            Health::record_apply_failure(&p, 77, "handoff proof ended TimedOut");
        }
        let h = Health::read(&p);
        assert!(h.is_persistent());
        assert_eq!(h.last_apply_failure_build, 77);
        assert!(!h.last_apply_failure_at.is_empty(), "the lane owns its clock");
        // Same build still running: the streak is fresh evidence — stands.
        Health::expire_stale_apply_streak(&p, 77);
        assert!(Health::read(&p).is_persistent(), "same build ⇒ still standing");
        // A different build is running: the machine moved, the claim is stale.
        Health::expire_stale_apply_streak(&p, 78);
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 0, "stale streak expires");
        assert!(!h.is_persistent());
        assert!(h.last_apply_error.is_empty());
        assert!(h.last_apply_failure_at.is_empty());
        assert_eq!(h.last_apply_failure_build, 0);
        assert!(h.kind.is_empty(), "an expired lane stops owning the headline");
        // An interleaved acquisition failure keeps ITS OWN streak through the
        // apply expiry — the lanes never launder each other.
        Health::record_failure(&p, "network", "dns");
        Health::record_apply_failure(&p, 78, "ChildDied");
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
            Health::record_apply_failure(&p, 77, "ChildDied");
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

    /// THE 2026-07-25 INCIDENT, pinned. A machine carried `manifest_failures = 597`
    /// with `last_error = "authoritative update v0.61 is unsigned under the pinned
    /// channel"` for ~13 hours and NOTHING escalated, because `is_persistent` watched
    /// only `pipeline` and `apply`. The class cannot heal on the client — an unsigned
    /// release stays unsigned until the publisher republishes — so a streak of it
    /// means this Mac can never update again, which is the loudest thing this ledger
    /// can be asked to say.
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
        let h = Health::read(&p);
        assert!(h.network_failures >= PERSISTENT_AFTER);
        assert!(
            !h.is_persistent(),
            "a flaky network must not tell the user their updater is broken"
        );
        assert_eq!(h.persistent_class(), None);
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
}
