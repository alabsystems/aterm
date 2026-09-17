// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SOFTWARE UPDATE MODEL: the ONE structured snapshot ([`UpdateState`], captured
//! from [`aterm_update::status`] or from the process-owned reducer) that every update
//! surface reads. [`UpdateState::projection`] is what the native Settings `/updates`
//! route paints and what `controls update` serializes from that route's compiled
//! semantic frame.
//!
//! The own-rendered floating card that used to paint this model here was DELETED on
//! 2026-09-15, under the owner's 2026-09-07 direction — *"don't use the overlay box.
//! instead, use the pulldown slot UX area that is used for atpkg"* — which moved the
//! whole update lane onto the status bar (`status_bars`) and the native route. The
//! tab-strip ↻ icon, the App-menu "Software Update…" item, the toolbar ↻ button and the
//! update bar's press all land on the one-click apply
//! (`App::apply_update_or_details`) or on that route.

use crate::update_apply_trouble::{ApplyRetry, ApplyTrouble};

/// The per-process Software Update snapshot every update surface reads. A SNAPSHOT of
/// the updater state captured when the route opens (or after a check), so what is painted
/// never reads a half-written ledger. `checking` reflects a manual check in flight (set
/// true when the user presses Check, cleared when the refresh lands).
pub(crate) struct UpdateState {
    /// The running build's version string (e.g. `0.5.14`).
    current_version: String,
    /// The running build number.
    current_build: u64,
    /// A strictly-newer staged build `(build, version)`, if one is ready to apply.
    staged: Option<(u64, String)>,
    /// The staged build's "what changed" notes, rendered from Markdown to clean lines.
    changelog: Vec<String>,
    /// Whether the in-app updater is enabled on this platform / by config.
    enabled: bool,
    /// The last updater outcome string (from the health ledger) — shown small.
    outcome: String,
    /// The health ledger says this Mac's updates are FAILING PERSISTENTLY (a streak
    /// of one failure class, not a blip): the headline says so instead of "You're
    /// up to date", and `outcome` carries the ledger's own sentence with the cause.
    failing_persistent: bool,
    /// The failing CLASS (`apply`, `pipeline`, …) when `failing_persistent` — the
    /// MOST RECENT one, which is why it cannot decide [`Self::apply_is_failing`].
    failing_kind: String,
    /// Consecutive APPLY failures. `>= PERSISTENT_AFTER` is the exact statement
    /// "the staged build will not start", independent of which class happened to
    /// fail last (2026-08-19 round-4 skeptics).
    ///
    /// ANY NON-ZERO VALUE IS ALREADY NEWS. The escalation threshold decides how
    /// LOUDLY the machine complains; it was never meant to decide whether the person
    /// gets told at all. On the owner's machine (2026-08-21) this sat at 2 — one
    /// short of `PERSISTENT_AFTER` — for hours, so every surface keyed on the
    /// threshold went on painting a plain "Update ready" over an engine that had
    /// tried twice and watched the successor die both times.
    failing_applies: u32,
    /// WHY those applies failed: the ledger's `last_apply_error`, the same prose the
    /// control socket prints as `apply_failure=`. Rendered into human words by
    /// [`ApplyTrouble`]; never shown raw.
    apply_failure: String,
    /// WHICH ARTIFACT [`Self::apply_failure`] is about (0 when unknown), and how many
    /// consecutive attempts on THAT artifact are behind it — the ledger's
    /// `last_apply_failure_target_build` / `apply_failures_for_target`.
    ///
    /// [`Self::failing_applies`] cannot stand in: it expires on the RUNNING build, so
    /// a stage that failed twice hands its count and its reason to whatever newer
    /// artifact replaces it, and the page would decorate a never-attempted download
    /// with someone else's failures.
    apply_failure_build: u64,
    /// See [`Self::apply_failure_build`].
    apply_attempts_for_stage: u32,
    /// Whether the automatic lane still intends to retry this artifact by itself.
    /// Event-loop state (`App::automatic_apply_retry_scheduled`), not ledger state —
    /// and the half of the story that decides whether the reader has to act.
    apply_retry: ApplyRetry,
    /// This launch has a bundle the updater could replace at all.
    installable: bool,
    /// The STRANDED verdict: checks complete but the release channel cannot be
    /// read (401/403/404 with nothing to try, or a renamed repo). Deliberately
    /// records ZERO ledger failures, so `failing_persistent` can never carry it —
    /// without this field the headline said "You're up to date." at a machine
    /// that will never update (round-11 audit). The explanation rides `outcome`.
    channel_unreadable: bool,
    /// A manual "Check for Updates" is running off-thread (shows "Checking…").
    checking: bool,
}

/// Owned, structured read projection shared with the native Settings `/updates`
/// route.  It keeps the updater service state private while avoiding brittle
/// parsing of the legacy `controls update` serialization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UpdateProjection {
    pub(crate) current_version: String,
    pub(crate) current_build: u64,
    pub(crate) staged: Option<(u64, String)>,
    pub(crate) changelog: Vec<String>,
    pub(crate) enabled: bool,
    pub(crate) outcome: String,
    pub(crate) checking: bool,
    /// The ledger's persistent-failure verdict — the surfaces style the headline
    /// as a warning and never say "current" while it holds.
    pub(crate) failing_persistent: bool,
    /// …and specifically in the APPLY class: the staged build will not start. An
    /// acquisition class (`manifest`, `pipeline`, …) says nothing about the stage
    /// in hand, so the "not applying" wording is reserved for this.
    ///
    /// This is the ESCALATION verdict — the loud one — and it is no longer what the
    /// wording keys on: [`Self::apply_trouble`] is, because a person is entitled to
    /// know about the FIRST failed apply and this field cannot report it. Retained
    /// because "the streak crossed `PERSISTENT_AFTER`" remains a distinct and useful
    /// fact (it is what earns the log pointer in the detail line), and because a
    /// surface that wanted to style the escalated case differently would need it.
    pub(crate) apply_is_failing: bool,
    /// The apply lane's STANDING trouble for the staged build, when there is any:
    /// how many attempts failed, why, and whether the lane will try again unaided.
    ///
    /// Distinct from [`Self::apply_is_failing`], which is the ESCALATION verdict
    /// (`>= PERSISTENT_AFTER`) and therefore silent about the first two failures.
    /// This is `Some` from the FIRST one, because "aterm tried once and the new
    /// version did not finish starting" is already something the person looking at
    /// an "update available" affordance is entitled to know. `None` for a stage that
    /// has simply not been attempted yet — a deferral or a block records a refusal
    /// and advances no streak, and that state must keep reading as ready.
    pub(crate) apply_trouble: Option<ApplyTrouble>,
    /// Whether a replacement could be installed here AT ALL — false for a copy run
    /// from the mounted disk image, a Gatekeeper-translocated download, or a
    /// dev-marked build. EVERY surface on the page has to read it. Round five gave
    /// the headline an honest sentence and left the rest of the page alone, so the
    /// release-notes card went on printing "aterm X is the latest build" directly
    /// under "This copy of aterm can't update itself" — a claim about a release
    /// list this process never fetched (2026-08-19 round-6 audit).
    pub(crate) installable: bool,
    /// The stranded verdict (see [`UpdateState`]): the compact card must say
    /// "Can't check", never "Current", while the channel is unreadable.
    pub(crate) channel_unreadable: bool,
    pub(crate) headline: String,
    pub(crate) detail: Option<String>,
}

impl UpdateState {
    /// Project the process-owned updater reducer into UI state. This is the shipping
    /// path: it is deliberately memory-only so opening Settings, querying
    /// introspection, and repainting can never parse a ledger or launch a bundle
    /// metadata probe on the event-loop thread.
    ///
    /// `apply_retry` is the ONE fact the reducer cannot supply: whether the automatic
    /// lane still intends this artifact at all
    /// (`App::automatic_apply_retry_scheduled`). It is passed rather than defaulted
    /// because guessing it wrong is worse than not saying: telling somebody an update
    /// will retry itself when it will not is how a machine sits un-updated for hours.
    pub(crate) fn from_service(
        snapshot: &crate::native_updater_service::UpdaterSnapshot,
        checking: bool,
        apply_retry: ApplyRetry,
    ) -> Self {
        let staged = snapshot.staged.as_ref().and_then(|staged| {
            (snapshot.enabled && staged.build > snapshot.current_build)
                .then(|| (staged.build, staged.version.clone()))
        });
        let changelog = snapshot
            .staged
            .as_ref()
            .filter(|_| staged.is_some())
            .and_then(|staged| staged.changelog.as_deref())
            .map(str::trim)
            .filter(|notes| !notes.is_empty())
            .map(|notes| {
                crate::markdown::to_plain_text(notes)
                    .lines()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Self {
            current_version: snapshot.current_version.clone(),
            current_build: snapshot.current_build,
            staged,
            changelog,
            enabled: snapshot.enabled,
            outcome: snapshot.outcome.clone(),
            failing_persistent: snapshot.failing_persistent,
            failing_kind: snapshot.failing_kind.clone(),
            failing_applies: snapshot.failing_applies,
            apply_failure: snapshot.apply_failure.clone(),
            apply_failure_build: snapshot.apply_failure_build,
            apply_attempts_for_stage: snapshot.apply_failures_for_target,
            apply_retry,
            installable: snapshot.installable,
            channel_unreadable: snapshot.channel_unreadable,
            checking,
        }
        .without_inapplicable_stage()
    }

    /// Snapshot the current updater state. `status` is `aterm_update::status(build)` —
    /// `None` on a platform with no updater (then the card says so). Pure: no I/O.
    #[cfg(test)]
    pub(crate) fn from_status(
        current_build: u64,
        current_version: &str,
        status: Option<&aterm_update::UpdateStatus>,
        checking: bool,
    ) -> Self {
        // "Ready" iff the staged build is strictly newer than what is ACTUALLY RUNNING
        // (`current_build`, from `build_info`) — not merely newer than the ledger's own
        // `current_build` snapshot, which can lag on a machine that staged-then-relaunched.
        let staged = status.and_then(|s| {
            let b = s.staged_build?;
            (b > current_build).then(|| {
                (
                    b,
                    s.staged_version.clone().unwrap_or_else(|| "?".to_string()),
                )
            })
        });
        let changelog = status
            .filter(|_| staged.is_some())
            .and_then(|s| s.changelog.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|md| {
                crate::markdown::to_plain_text(md)
                    .lines()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Self {
            current_version: current_version.to_string(),
            current_build,
            staged,
            changelog,
            enabled: status.map(|s| s.enabled).unwrap_or(false),
            outcome: status.map(|s| s.outcome.clone()).unwrap_or_default(),
            failing_persistent: status.is_some_and(|s| s.enabled && s.is_failing_persistently()),
            failing_kind: status.map(|s| s.failing_kind.clone()).unwrap_or_default(),
            failing_applies: status.map_or(0, |s| s.failing_applies),
            // `aterm_update::UpdateStatus` is the CHECK lane's projection and
            // deliberately carries no apply-lane prose; the shipping path reads it
            // beside the status (`app_native::durable_update_status`). Tests that
            // want a cause attach one with [`Self::with_apply_lane`].
            apply_failure: String::new(),
            apply_failure_build: 0,
            apply_attempts_for_stage: 0,
            // The conservative default: claiming a retry that is not scheduled is the
            // one error that costs the reader real time.
            apply_retry: ApplyRetry::ManualOnly,
            installable: status.is_none_or(|s| s.installable),
            channel_unreadable: status.is_some_and(|s| s.enabled && s.channel_unreadable),
            checking,
        }
        .without_inapplicable_stage()
    }

    /// A stage this copy CANNOT APPLY is not a stage for this copy.
    ///
    /// `aterm_update::status()` reads a HOME-keyed Updates directory, so every copy
    /// of the app for one user shares ONE ready.toml — while `installable` is
    /// per-copy (`bundle::resolve()`). `staged.is_some() && !installable` is
    /// therefore a REACHABLE state: the user's installed /Applications copy stages
    /// build N+1, and they open an older aterm.app from a mounted DMG or a
    /// translocated download. Every surface tests `staged` first, so that window
    /// headlined "Update ready", printed the full release notes for a build it could
    /// never install, and offered an enabled Install button whose apply lane returns
    /// NotApplicable — the exact over-claim the `installable` work removed
    /// everywhere else (2026-08-19 round-7 audit).
    ///
    /// Clearing it here fixes every surface at once, including the default action,
    /// rather than adding a `!installable` guard to each and waiting for the next one
    /// to be written without it.
    fn without_inapplicable_stage(mut self) -> Self {
        if !self.installable {
            self.staged = None;
            self.changelog.clear();
            // The ledger's own sentence describes the check the INSTALLED copy ran;
            // this copy has never run one, and "up to date" under a headline saying
            // it can never update is the same contradiction one row lower.
            self.outcome.clear();
        }
        self
    }

    /// Attach the apply lane's own facts to a `from_status` fixture: the ledger
    /// reason behind the streak, and whether the loop still intends to retry.
    ///
    /// Only tests need this. The shipping constructor takes both from the reducer
    /// snapshot and the event loop, which is where they actually live.
    #[cfg(test)]
    pub(crate) fn with_apply_lane(mut self, reason: &str, retry: ApplyRetry) -> Self {
        self.apply_failure = reason.to_string();
        self.apply_retry = retry;
        // The artifact-scoped pair the shipping constructor copies out of the ledger.
        // A fixture that set the reason but left the target at 0 would silently
        // produce NO trouble, so it is derived from the fixture's own stage rather
        // than left to the caller to remember.
        self.apply_failure_build = self.staged.as_ref().map_or(0, |(build, _)| *build);
        self.apply_attempts_for_stage = self.failing_applies;
        self
    }

    /// The apply lane's standing trouble for the STAGED build, in the form every
    /// surface renders.
    ///
    /// Gated on `staged` because trouble is only ever about a build actually on
    /// offer: with nothing staged there is no affordance to qualify, and a leftover
    /// streak would be describing a stage the user cannot see. Gated on `enabled` for
    /// the same reason the failure headlines are — a disabled updater's ledger is
    /// history, not a live report.
    pub(crate) fn apply_trouble(&self) -> Option<ApplyTrouble> {
        if !self.enabled {
            return None;
        }
        // Only the artifact the failures were actually ABOUT may wear them; see
        // [`Self::apply_failure_build`]. This also subsumes the old `staged.is_none()`
        // guard — with nothing on offer there is no build to match.
        let (staged_build, _) = self.staged.as_ref()?;
        if self.apply_failure_build != *staged_build {
            return None;
        }
        ApplyTrouble::new(
            self.apply_attempts_for_stage,
            &self.apply_failure,
            self.apply_retry,
        )
    }

    /// Snapshot the exact state the existing update card paints for native tab
    /// presentation and semantic introspection.
    pub(crate) fn projection(&self) -> UpdateProjection {
        UpdateProjection {
            current_version: self.current_version.clone(),
            current_build: self.current_build,
            staged: self.staged.clone(),
            changelog: self.changelog.clone(),
            enabled: self.enabled,
            outcome: self.outcome.clone(),
            checking: self.checking,
            failing_persistent: self.failing_persistent && self.enabled && !self.checking,
            apply_is_failing: self.apply_is_failing() && self.enabled && !self.checking,
            // Suppressed mid-check for the same reason `apply_is_failing` is: a check
            // in flight owns the surface, and a verdict from before it started must
            // not argue with the "Checking…" the user is looking at.
            apply_trouble: (!self.checking).then(|| self.apply_trouble()).flatten(),
            installable: self.installable,
            channel_unreadable: self.channel_unreadable && self.enabled && !self.checking,
            headline: self.headline(),
            detail: self.detail(),
        }
    }

    /// The primary status headline (the big semibold line under the current build). The
    /// version specifics live in [`Self::detail`], so the headline stays a short, strong
    /// statement — "Update ready", not a full version sentence.
    /// The persistent failure is in the APPLY class — the staged build will not
    /// start — as opposed to an acquisition class (`manifest`, `pipeline`,
    /// `network`, `stage`), which is about fetching the NEXT build.
    fn apply_is_failing(&self) -> bool {
        // NOT `failing_kind == "apply"`: that is the last class to fail, so a machine
        // whose apply lane escalated but whose latest failure was a network blip
        // would hide it, and a single apply failure under an escalated `pipeline`
        // streak would wrongly claim it. The apply streak itself is the statement.
        self.failing_applies >= aterm_update::PERSISTENT_AFTER
    }

    fn headline(&self) -> String {
        if self.checking {
            "Checking for updates\u{2026}".to_string()
        } else if self.staged.is_some() && self.enabled && self.apply_is_failing() {
            // A stage that is persistently FAILING TO APPLY (the APPLY class: the
            // handoff keeps ending badly) is not "Update ready" — the health notice
            // points the user here, and this line must not contradict it. Only the
            // apply class earns this sentence: a `manifest`/`pipeline` streak is
            // about ACQUIRING the NEXT build and says nothing about the stage in
            // hand, which is verified and will install (round-4 audit).
            "Update ready, but it keeps failing to apply.".to_string()
        } else if self.apply_trouble().is_some() {
            // BELOW THE ESCALATION THRESHOLD IS STILL NOT "READY". `PERSISTENT_AFTER`
            // decides when the machine complains LOUDLY; it was never meant to decide
            // whether the person is told at all, and treating it that way is the
            // defect this arm closes. Measured on the owner's machine (2026-08-21):
            // `failing_applies=2` for hours under a plain "Update ready", while the
            // engine had watched the successor die twice. The count and the cause
            // ride the detail line below.
            "Update ready, but applying it failed.".to_string()
        } else if self.staged.is_some() {
            "Update ready".to_string()
        } else if !self.installable {
            // NOT "You're up to date": this copy cannot be replaced at all — it is
            // running from the mounted disk image, from a Gatekeeper-translocated
            // location, or from a dev-marked install, so no check thread ever starts
            // and every ledger field below is the pristine default of a machine that
            // structurally cannot update (2026-08-19 round-5 audit).
            "This copy of aterm can\u{2019}t update itself.".to_string()
        } else if !self.enabled {
            "Automatic updates are off.".to_string()
        } else if self.channel_unreadable {
            // A permanently stranded machine — the channel cannot be read, and the
            // state deliberately records ZERO ledger failures, so the
            // failing_persistent arm below can never catch it. Before this arm the
            // headline was "You're up to date." over an outcome that says "will
            // NEVER receive an update until it is fixed" (round-11 audit).
            "aterm can\u{2019}t check for updates on this machine.".to_string()
        } else if self.failing_persistent {
            // NEVER "up to date" while the ledger says otherwise: for eight hours on
            // 2026-08-18 every check was rejected publisher-side and this line said
            // "You're up to date." The cause rides in `outcome` (the same sentence
            // `aterm ctl update status` prints), shown as the detail below.
            "Updates are failing on this machine.".to_string()
        } else {
            "You\u{2019}re up to date.".to_string()
        }
    }

    /// The secondary detail line under the headline: the staged build's version + number
    /// (`Some` only when a build is ready), rendered small under the accent headline.
    fn detail(&self) -> Option<String> {
        if !self.installable {
            return Some(
                "Move aterm.app to your Applications folder and open it from there. \
                 A copy running from a disk image, a quarantined download, or a local \
                 build is never replaced in place."
                    .to_string(),
            );
        }
        if let Some((b, v)) = self.staged.as_ref() {
            // Not the ledger `outcome`: the check lane rewrites that every cycle with
            // the healthy "staged … ready to apply" sentence while a stage is held.
            // The durable fact is the class that is failing.
            // "every attempt to start it has failed" named neither HOW MANY attempts
            // nor WHY, so a reader could not tell a starved child from a successor
            // that refuses to boot — which on 2026-08-21 was exactly the difference
            // that mattered: the same builds applied unaided once the machine's load
            // dropped. The trouble sentence carries both, plus whether anything is
            // still scheduled to happen without the user.
            // `!self.checking` for the same reason `headline()` has it, and for the
            // reason `projection()` suppresses `apply_trouble` mid-check: a check in
            // flight owns the surface. Without it the three fields of ONE projection
            // contradicted each other — `apply_trouble: None`, headline "Checking for
            // updates…", and this line still explaining a failure from before the
            // check started.
            if let Some(trouble) = self.apply_trouble().filter(|_| !self.checking) {
                // The log pointer earns its characters in exactly two states: the
                // ESCALATED one (the full attempt history is worth chasing), and the
                // one where this program could not NAME the cause — there the
                // untranslated ledger reason exists only in the log, and dropping the
                // pointer would be dropping the only explanation the machine has.
                let tail = if self.apply_is_failing() || !trouble.cause_is_named() {
                    " See aterm.log."
                } else {
                    ""
                };
                return Some(format!(
                    "Version {v} \u{00b7} build {b} \u{00b7} {}{tail}",
                    trouble.sentence()
                ));
            }
            if self.enabled && !self.checking && self.apply_is_failing() {
                // THE ESCALATION WITHOUT AN ARTIFACT TO PIN IT TO. `apply_is_failing`
                // reads the running-build-scoped streak, which outlives the artifact
                // it was recorded against; the trouble sentence above needs the
                // ledger to NAME this build and a ledger written before that field
                // existed does not. Losing the count and the cause there is a real
                // loss, but silently falling through to the acquisition wording below
                // ("update CHECKS are failing") would have been a lie about which
                // lane is broken.
                return Some(format!(
                    "Version {v} \u{00b7} build {b} \u{00b7} every attempt to start it has \
                     failed; see aterm.log"
                ));
            }
            if self.enabled && self.failing_persistent {
                // The class is named only when it is the one that ESCALATED. A single
                // apply failure under an escalated `pipeline` streak leaves
                // `failing_kind = "apply"` — naming it there told the user their
                // staged build would not start when nothing of the kind had been
                // established (2026-08-19 round-5 audit).
                let named = match self.failing_kind.as_str() {
                    "apply" | "" => String::new(),
                    kind => format!(" ({kind})"),
                };
                return Some(format!(
                    "Version {v} \u{00b7} build {b} \u{00b7} ready to install; but update \
                     CHECKS are failing{named}, so newer builds may not arrive — see aterm.log"
                ));
            }
            return Some(format!("Version {v} \u{00b7} build {b}"));
        }
        if (self.failing_persistent || self.channel_unreadable) && !self.checking && self.enabled {
            // For the stranded state the outcome IS the full explanation — cause,
            // every indistinguishable alternative, and the copy-pasteable remedy —
            // rewritten on every check so it cannot go stale.
            let cause = self.outcome.trim();
            return Some(if cause.is_empty() {
                "Every recent check failed the same way; run `aterm ctl update status` for the ledger.".to_string()
            } else {
                cause.to_string()
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staged_status() -> aterm_update::UpdateStatus {
        aterm_update::UpdateStatus {
            enabled: true,
            current_build: 828,
            staged_build: Some(830),
            staged_version: Some("0.5.15".to_string()),
            staged_commit: Some("deadbeefcafe".to_string()),
            staged_dmg_sha256: Some("ab".repeat(32)),
            changelog: Some("### Features\n- **DSU**: hot-swap\n- faster startup".to_string()),
            outcome: "ok".to_string(),
            updated_at: String::new(),
            failing_checks: 0,
            failing_kind: String::new(),
            failing_applies: 0,
            installable: true,
            failing_since: String::new(),
            failing_persistent: false,
            rescues: 0,
            failing_checks_kind: String::new(),
            channel_unreadable: false,
        }
    }

    /// The projection is what every native surface reads, and it used to DROP
    /// `installable` — which is how a page whose headline said "This copy of aterm
    /// can't update itself" carried a card underneath saying the running version
    /// was the latest build (2026-08-19 round-6 audit).
    /// The stage ledger is HOME-keyed and shared by every copy of the app; a copy
    /// that cannot replace itself must not present the installed copy's staged build
    /// as its own. Before this, that window headlined "Update ready" with notes and
    /// an enabled Install button (2026-08-19 round-7 audit).
    #[test]
    fn a_stage_this_copy_cannot_apply_is_not_shown_as_ready() {
        let mut st = staged_status();
        st.installable = false;
        let p = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
        assert!(
            p.staged.is_none(),
            "a stage it cannot apply is not this copy's"
        );
        assert!(
            p.changelog.is_empty(),
            "…nor are that build's release notes"
        );
        assert!(p.outcome.is_empty(), "…nor the check verdict it never ran");
        assert_eq!(p.headline, "This copy of aterm can\u{2019}t update itself.");

        // The same ledger on an ordinary install is still a ready update.
        st.installable = true;
        let ok = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
        assert_eq!(ok.staged, Some((830, "0.5.15".to_string())));
        assert_eq!(ok.headline, "Update ready");
    }

    #[test]
    fn the_projection_carries_whether_this_copy_can_be_replaced_at_all() {
        let mut st = staged_status();
        st.staged_build = None;
        st.staged_version = None;
        st.staged_dmg_sha256 = None;
        st.changelog = None;
        st.installable = false;
        let projected = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
        assert!(
            !projected.installable,
            "a copy with no replaceable bundle must project that fact, not just headline it"
        );
        st.installable = true;
        assert!(
            UpdateState::from_status(828, "0.5.14", Some(&st), false)
                .projection()
                .installable,
            "an ordinary install must stay installable"
        );
    }

    /// A staged build whose apply keeps failing persistently is not "Update ready":
    /// the headline says so and the detail names the failing class, not the
    /// churned ledger outcome.
    /// A copy that structurally CANNOT be replaced — run from the mounted DMG, a
    /// Gatekeeper-translocated download, or a dev-marked install — must say so. No
    /// check thread ever starts in that state, so every ledger field is the pristine
    /// default and the panel otherwise reported the confident "You're up to date" of
    /// a machine that will never update (2026-08-19 round-5 audit).
    #[test]
    fn a_copy_that_cannot_replace_itself_says_so_instead_of_up_to_date() {
        let mut st = staged_status();
        st.staged_build = None;
        st.staged_version = None;
        st.staged_dmg_sha256 = None;
        st.changelog = None;
        st.installable = false;
        let s = UpdateState::from_status(828, "0.5.14", Some(&st), false);
        let p = s.projection();
        assert_eq!(p.headline, "This copy of aterm can\u{2019}t update itself.");
        let detail = p.detail.expect("detail");
        assert!(
            detail.contains("Applications"),
            "…and how to fix it: {detail}"
        );
        // The same state WITH a replaceable bundle is the ordinary healthy line.
        st.installable = true;
        let ok = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
        assert_eq!(ok.headline, "You\u{2019}re up to date.");
    }

    #[test]
    fn a_staged_build_that_keeps_failing_to_apply_says_so() {
        let mut st = staged_status();
        st.failing_applies = 3;
        st.failing_kind = "apply".to_string();
        st.failing_persistent = true;
        st.outcome = "staged 0.5.15 (build 830) — verified and ready to apply".to_string();
        let s = UpdateState::from_status(828, "0.5.14", Some(&st), false).with_apply_lane(
            "overlap handoff failed safely: handoff proof ended ChildDied",
            ApplyRetry::ManualOnly,
        );
        let p = s.projection();
        assert!(
            p.failing_persistent,
            "the projection no longer masks a failure while staged"
        );
        assert_eq!(p.headline, "Update ready, but it keeps failing to apply.");
        let detail = p.detail.expect("detail");
        assert!(
            detail.contains("build 830") && detail.contains("3 times"),
            "the escalated detail names how many attempts died: {detail}"
        );
        assert!(
            !detail.contains("ready to apply"),
            "not the ledger's healthy sentence: {detail}"
        );
        assert!(
            detail.contains("aterm.log"),
            "the escalated case keeps its pointer at the full history: {detail}"
        );

        // AN ACQUISITION-CLASS streak is a different statement: the stage in hand is
        // fine and will install; what is failing is fetching the NEXT build.
        let mut acquiring = staged_status();
        acquiring.failing_checks = 3;
        acquiring.failing_kind = "manifest".to_string();
        acquiring.failing_persistent = true;
        let s = UpdateState::from_status(828, "0.5.14", Some(&acquiring), false);
        let p = s.projection();
        assert!(p.failing_persistent && !p.apply_is_failing);
        assert_eq!(p.headline, "Update ready");
        let detail = p.detail.expect("detail");
        assert!(
            detail.contains("CHECKS are failing") && detail.contains("manifest"),
            "{detail}"
        );
    }

    /// THE DEFECT, ON THE SURFACE THAT HID IT (owner's machine, 2026-08-21).
    ///
    /// `aterm ctl update status` was reporting `staged_version=0.56.0
    /// relaunch_ready=true failing_applies=2 apply_failure="overlap handoff failed
    /// safely: handoff proof ended ChildDied"`, and the window said "Update ready"
    /// for hours. TWO is below `PERSISTENT_AFTER`, so every surface keyed on the
    /// escalation verdict stayed silent through exactly the window in which a person
    /// would have wanted to know — and a person who did not know could not tell
    /// "downloaded, waiting for you" from "tried twice, the handoff died both times".
    ///
    /// So the assertion is on the SURFACED STRING, and on both halves of it: the
    /// number of attempts, and a cause in words rather than the proof-outcome enum
    /// name. (What actually happened that night: the machine was 8x-oversubscribed,
    /// the child was STARVED rather than broken, and the identical builds applied
    /// unaided once the load dropped — which a reader can only reason about if the
    /// window tells them the successor kept dying.)
    #[test]
    fn a_stage_that_already_failed_twice_names_the_count_and_a_human_cause() {
        let mut st = staged_status();
        st.failing_applies = 2;
        // The check lane keeps rewriting its healthy sentence while a stage is held;
        // that is precisely why it cannot be the only thing on the page.
        st.outcome = "staged 0.5.15 (build 830) \u{2014} verified and ready to apply".to_string();
        let p = UpdateState::from_status(828, "0.5.14", Some(&st), false)
            .with_apply_lane(
                "overlap handoff failed safely: handoff proof ended ChildDied",
                ApplyRetry::Scheduled,
            )
            .projection();

        assert!(
            !p.apply_is_failing,
            "two failures is BELOW the escalation threshold — which is exactly the \
             window in which the old surface said nothing at all"
        );
        assert!(
            p.apply_trouble.is_some(),
            "…and exactly the window this projection now has to speak in"
        );
        assert_ne!(
            p.headline, "Update ready",
            "a build the engine has already failed to start twice is not simply ready"
        );

        let detail = p.detail.expect("a staged build always has a detail line");
        assert!(
            detail.contains("twice"),
            "the surfaced string must name the ATTEMPT COUNT: {detail}"
        );
        assert!(
            detail.contains("did not finish starting"),
            "…and the CAUSE in human words: {detail}"
        );
        assert!(
            !detail.contains("ChildDied"),
            "\"handoff proof ended ChildDied\" is the register of a log line, not of a \
             window: {detail}"
        );
        assert!(
            detail.contains("try again by itself"),
            "…and whether the person has to do anything: {detail}"
        );
    }

    /// A STAGED UPDATE WAITING FOR A QUIET WINDOW IS A DIFFERENT SENTENCE.
    ///
    /// Deferrals and blocks are recorded as REFUSALS and advance no streak, so a
    /// patient updater carries `failing_applies == 0` — and must read exactly as it
    /// did before this work. The stale reason below is deliberate: the ledger's
    /// `last_apply_error` slot is not expiry-bound the way the streak is, so "a
    /// reason with no attempts behind it" is a genuinely reachable state and must
    /// produce no failure wording anywhere.
    #[test]
    fn a_clean_staged_update_does_not_read_as_a_failure() {
        let st = staged_status();
        assert_eq!(st.failing_applies, 0, "the fixture is a clean stage");
        let p = UpdateState::from_status(828, "0.5.14", Some(&st), false)
            .with_apply_lane(
                "overlap handoff failed safely: handoff proof ended ChildDied",
                ApplyRetry::Scheduled,
            )
            .projection();

        assert!(
            p.apply_trouble.is_none(),
            "a reason with no attempts behind it is not an attempt"
        );
        assert_eq!(p.headline, "Update ready");
        let detail = p.detail.expect("detail");
        assert_eq!(detail, "Version 0.5.15 \u{00b7} build 830");
        for alarming in ["tried", "failed", "did not", "retry"] {
            assert!(
                !detail.contains(alarming),
                "a stage that has simply not been attempted must not read as a \
                 failure ({alarming:?} in {detail:?})"
            );
        }
    }

    /// "IT WILL FIX ITSELF" AND "IT WILL NOT" ARE DIFFERENT SITUATIONS FOR THE READER.
    ///
    /// One of them resolves while they keep working and the other never will, so the
    /// manual-only latch has to reach the words. Same failure, same count, same
    /// cause — only the scheduling state differs, and the surfaced string must differ
    /// with it.
    #[test]
    fn the_surface_says_whether_the_update_will_retry_without_the_user() {
        let mut st = staged_status();
        st.failing_applies = 2;
        let detail_for = |retry| {
            UpdateState::from_status(828, "0.5.14", Some(&st), false)
                .with_apply_lane("handoff proof ended ChildDied", retry)
                .projection()
                .detail
                .expect("detail")
        };
        let scheduled = detail_for(ApplyRetry::Scheduled);
        let manual = detail_for(ApplyRetry::ManualOnly);
        assert_ne!(scheduled, manual);
        assert!(
            scheduled.contains("try again by itself"),
            "a scheduled retry tells the reader to do nothing: {scheduled}"
        );
        assert!(
            manual.contains("not try again until you ask"),
            "a latched lane tells the reader it is now up to them: {manual}"
        );
    }

    /// THE SOFTWARE UPDATE PAGE ON 2026-09-14. The pull-down said "Run `aterm-ctl
    /// update status` … — see Settings ▸ Software Update", and this is what Settings
    /// showed for six failed applies of a staged v0.85.0 refused by an ad-hoc-signed
    /// `/Applications/aterm.app`: "Update ready, but it keeps failing to apply." over
    /// "aterm tried to update 6 times and the handover could not be set up. It will
    /// try again by itself. See aterm.log". While the App still reports a current
    /// installed-source block, the page must carry that repair advice. A historical
    /// failure on its own cannot override later repaired scheduling.
    #[test]
    fn observability_audit_the_page_tells_the_owner_to_reinstall_over_an_unsigned_install() {
        let mut st = staged_status();
        st.failing_applies = 6;
        st.failing_kind = "apply".to_string();
        st.failing_persistent = true;
        let p = UpdateState::from_status(828, "0.5.14", Some(&st), false)
            .with_apply_lane(
                "installed bundle failed pre-park verification: the installed bundle at \
                 /Applications/aterm.app cannot be the rollback source the swap installs: \
                 bundle policy: codesign --verify (team-pinned requirement) failed; the \
                 terminal was left untouched",
                ApplyRetry::NeedsPerson,
            )
            .projection();
        let detail = p.detail.expect("detail");
        assert!(
            detail.contains("the installed copy could not be verified for replacement"),
            "the page must name the refused installed copy: {detail}"
        );
        assert!(
            detail.contains("Install the signed release from the release DMG, then retry."),
            "a current installed-source block must carry its remedy: {detail}"
        );
        assert!(
            !detail.contains("try again by itself"),
            "a current installed-source block cannot promise an automatic retry: {detail}"
        );
        assert!(
            !detail.contains("could not be set up"),
            "not the generic preparation clause — the installed copy is the cause: {detail}"
        );
    }

    #[test]
    fn snapshot_renders_staged_update_notes() {
        let st = staged_status();
        let s = UpdateState::from_status(828, "0.5.14", Some(&st), false);
        assert!(s.staged.is_some());
        // Markdown is rendered to clean lines (no raw `###`/`**`).
        assert!(s.changelog.iter().any(|l| l.contains("DSU: hot-swap")));
        assert!(
            !s.changelog
                .iter()
                .any(|l| l.contains('#') || l.contains("**"))
        );
        // The headline is the short statement; the version lives in the detail line.
        assert_eq!(s.headline(), "Update ready");
        assert!(s.detail().unwrap().contains("0.5.15"));
    }
}
