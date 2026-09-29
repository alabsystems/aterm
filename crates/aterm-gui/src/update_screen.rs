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
//! whole update lane onto the message band (`update_words`) and the native route. The
//! tab-strip ↻ icon, the App-menu "Software Update…" item, the toolbar ↻ button and the
//! update bar's press all land on the one-click apply
//! (`App::apply_update_or_details`) or on that route.

use crate::update_apply_trouble::{ApplyRetry, ApplyTrouble};

/// The per-process Software Update snapshot every update surface reads. A SNAPSHOT of
/// the updater state captured when the route opens (or after a check), so what is painted
/// never reads a half-written ledger. `checking` reflects a manual check in flight (set
/// true when the user presses Check, cleared when the refresh lands).
pub(crate) struct UpdateState {
    linux_host: bool,
    linux: Option<aterm_update::LinuxUpdateStatus>,
    linux_error: bool,
    /// The running build's version string (e.g. `0.5.14`).
    current_version: String,
    /// The running build number.
    current_build: u64,
    /// A strictly-newer staged build `(build, version)`, if one is ready to apply.
    staged: Option<(u64, String)>,
    /// The stage is an installed-bundle ACTIVATION
    /// (`StagedUpdate::is_installed_activation`): already at this app's path, never
    /// downloaded, so the detail does not say "downloaded".
    staged_activation: bool,
    /// The staged build's "what changed" notes, rendered from Markdown to clean lines.
    changelog: Vec<String>,
    /// Whether the native updater runs here (`aterm_update::enabled`: macOS and Linux —
    /// on Linux also only for an enrolled copy, `aterm_update::linux::status`).
    enabled: bool,
    /// "Check for updates automatically" (`[update] enabled`, default on) as THIS PROCESS
    /// reads it: whether its background checker looks for builds by itself. The updater
    /// reads the key once per process, so this — not the saved value — decides the
    /// headline. Off, a check a person asks for and Update Now still work.
    automatic_checks: bool,
    /// The same key as SAVED, which the next launch reads; where it differs from
    /// [`Self::automatic_checks`] the detail says what changes then.
    automatic_checks_saved: bool,
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
    /// When the last check completed (Unix seconds), `None` before any has: the page
    /// says "Checked 12 min ago" from it ([`UpdateProjection::checked_line`]).
    checked_at: Option<i64>,
    /// The automatic lane's download in flight, which the band does not show
    /// (design ruling 220): the headline and detail say it.
    downloading: Option<QuietDownload>,
    /// A DEV-MARKED copy (`tools/dev-app.sh`), which the updater leaves alone on
    /// purpose; `None` for every other copy ([`Self::with_dev_build`], gap #30).
    dev_build: Option<DevBuildPage>,
}

/// A dev-marked copy as the page says it (gap #30). The updater leaves it alone on
/// purpose, so the "move it to Applications" remedy the other copies that cannot
/// update get would be wrong for it (the CLI learned that in 2dead72c9); and where it
/// stands against the public channel is the news the page carries for it —
/// `standing` is the dev-channel watch's last reading
/// (`aterm_update::dev_channel`), `None` before it has read one (automatic checks off,
/// or the channel unreachable, say nothing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DevBuildPage {
    pub(crate) standing: Option<aterm_update::dev_channel::DevLag>,
}

impl DevBuildPage {
    /// The headline: how far behind, when that is known and true; else that it is a
    /// dev build. Short, like every headline on the page.
    fn headline(&self) -> String {
        use aterm_update::dev_channel::DevLag;
        match &self.standing {
            Some(DevLag::Behind {
                releases: Some(1), ..
            }) => "This dev build is 1 release behind.".to_string(),
            Some(DevLag::Behind {
                releases: Some(n), ..
            }) => format!("This dev build is {n} releases behind."),
            Some(DevLag::Behind { latest, .. }) => {
                format!("This dev build is older than aterm {latest}.")
            }
            _ => "This is a dev build.".to_string(),
        }
    }

    /// The detail: where it stands, in the dev channel's words, then why nothing moves
    /// it and what does.
    fn detail(&self) -> String {
        let standing = self
            .standing
            .as_ref()
            .map(|lag| format!("It is {}. ", lag.words()))
            .unwrap_or_default();
        format!(
            "{standing}The updater leaves a dev build alone: rebuild it with tools/dev-app.sh, \
             or use the release (tools/install.sh puts it in Applications)."
        )
    }
}

/// A download (or its check) nobody is waiting on — the automatic lane's — as the
/// page says it (design ruling 220): it takes no row on the band, so this page is
/// where its progress is seen. `title` is the flow's own phase title
/// (`Downloading aterm v0.92.0`), `progress` the bytes so far (`45 MB / 74 MB`, or
/// `45 MB` with no total; empty while it is checked).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QuietDownload {
    /// The version, for the record a failure writes.
    pub(crate) version: String,
    pub(crate) title: String,
    pub(crate) progress: String,
}

/// Owned, structured read projection shared with the native Settings `/updates`
/// route.  It keeps the updater service state private while avoiding brittle
/// parsing of the legacy `controls update` serialization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UpdateProjection {
    pub(crate) linux_host: bool,
    pub(crate) linux: Option<aterm_update::LinuxUpdateStatus>,
    pub(crate) linux_attention: bool,
    pub(crate) current_version: String,
    pub(crate) current_build: u64,
    pub(crate) staged: Option<(u64, String)>,
    pub(crate) changelog: Vec<String>,
    pub(crate) enabled: bool,
    /// "Check for updates automatically" as THIS process runs it — the value the full
    /// headline keys "Automatic checks are off." on (a saved flip applies next launch).
    /// The compact rungs read it too, so no squeeze of the page calls it "Current" while
    /// nothing checks by itself (2026-09-24 post-push review).
    pub(crate) automatic_checks: bool,
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
    /// This copy is DEV-MARKED (`tools/dev-app.sh`, gap #30): `installable` is false on
    /// purpose, so every card that would say "can't update" or "reinstall" says it is a
    /// dev build instead.
    pub(crate) dev_build: bool,
    /// The stranded verdict (see [`UpdateState`]): the compact card must say
    /// "Can't check", never "Current", while the channel is unreadable.
    pub(crate) channel_unreadable: bool,
    /// When the last check completed (Unix seconds), `None` before any has.
    pub(crate) checked_at: Option<i64>,
    pub(crate) headline: String,
    pub(crate) detail: Option<String>,
    /// macOS: what the automatic-checks switch means for this process where it is not
    /// simply on ([`UpdateState::automatic_checks_detail`]), said AFTER when the last
    /// check ran — never instead of it (2026-09-24). `None` while [`Self::detail`] holds
    /// a stage or trouble, and always on Linux, whose detail leads with it.
    pub(crate) automatic_checks_note: Option<String>,
}

impl UpdateProjection {
    /// `Checked 12 min ago` — when the last check completed, said relative to `now`
    /// (Unix seconds); `None` before any check has. The healthy page's one status line:
    /// the updater's own decision sentence stays in `aterm ctl update status` and the log.
    pub(crate) fn checked_line(&self, now: i64) -> Option<String> {
        self.checked_at
            .map(|at| format!("Checked {}", ago_words(at, now)))
    }
}

/// How long before `now` the instant `at` was, as a person says it: `just now`, `12 min
/// ago`, `3 h ago`, `2 days ago` (Unix seconds both). A time ahead of `now` — a clock set
/// back since — is `just now`, never a negative age.
pub(crate) fn ago_words(at: i64, now: i64) -> String {
    let age = now.saturating_sub(at);
    if age < 60 {
        "just now".to_string()
    } else if age < 3600 {
        format!("{} min ago", age / 60)
    } else if age < 86_400 {
        format!("{} h ago", age / 3600)
    } else if age < 2 * 86_400 {
        "1 day ago".to_string()
    } else {
        format!("{} days ago", age / 86_400)
    }
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
        let staged_activation = staged.is_some()
            && snapshot
                .staged
                .as_ref()
                .is_some_and(crate::native_updater_service::StagedUpdate::is_installed_activation);
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
            linux_host: snapshot.linux_host,
            linux: snapshot.linux.clone(),
            linux_error: snapshot.error.is_some(),
            current_version: snapshot.current_version.clone(),
            current_build: snapshot.current_build,
            staged,
            staged_activation,
            changelog,
            enabled: snapshot.enabled,
            automatic_checks: true,
            automatic_checks_saved: true,
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
            checked_at: snapshot.checked_at,
            downloading: None,
            dev_build: None,
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
            linux_host: status.is_some_and(|s| s.linux.is_some()),
            linux: status.and_then(|s| s.linux.clone()),
            linux_error: status.is_some_and(|s| s.failing_checks > 0),
            current_version: current_version.to_string(),
            current_build,
            staged,
            staged_activation: false,
            changelog,
            enabled: status.map(|s| s.enabled).unwrap_or(false),
            automatic_checks: true,
            automatic_checks_saved: true,
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
            checked_at: status
                .and_then(|s| aterm_update_core::pkg_check::rfc3339_to_unix(&s.updated_at)),
            downloading: None,
            dev_build: None,
        }
        .without_inapplicable_stage()
    }

    /// "Check for updates automatically" as this process runs it (`running`: the value
    /// `aterm_update::automatic` read at launch) and as saved (`saved`:
    /// `app_config::update_checks_automatic`, which the next launch reads). The shipping
    /// path passes an App field and the in-memory config: no read on the event loop.
    pub(crate) fn with_automatic_checks(mut self, running: bool, saved: bool) -> Self {
        self.automatic_checks = running;
        self.automatic_checks_saved = saved;
        self
    }

    /// The automatic lane's download in flight (`App::update_quiet`, design ruling
    /// 220): no row says it, so the page does.
    pub(crate) fn with_download(mut self, downloading: Option<QuietDownload>) -> Self {
        self.downloading = downloading;
        self
    }

    /// This copy is dev-marked (`App::dev_build`, gap #30): the page says so, and where
    /// it stands, instead of the move-it remedy.
    pub(crate) fn with_dev_build(mut self, dev_build: Option<DevBuildPage>) -> Self {
        self.dev_build = dev_build;
        self
    }

    /// The quiet download, where the page may say it: macOS, an installable copy
    /// with the updater on, nothing staged, and no check a person started (whose
    /// own words and row own the surface).
    fn quiet_download(&self) -> Option<&QuietDownload> {
        self.downloading.as_ref().filter(|_| {
            !self.linux_host
                && self.enabled
                && self.installable
                && self.staged.is_none()
                && !self.checking
        })
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
        if self.linux_host {
            // Linux staging does not mint macOS live-session handoff authority.
            // Its outcome is per-copy and remains diagnostic even unenrolled.
            self.staged = None;
            self.changelog.clear();
            return self;
        }
        if !self.installable {
            self.staged = None;
            self.changelog.clear();
            // The ledger's own sentence describes the check the INSTALLED copy ran;
            // this copy has never run one, and "up to date" under a headline saying
            // it can never update is the same contradiction one row lower — and so is
            // when that check ran.
            self.outcome.clear();
            self.checked_at = None;
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
        let detail = self.detail();
        let automatic_checks_note = (detail.is_none() && !self.linux_host)
            .then(|| self.automatic_checks_detail())
            .flatten();
        UpdateProjection {
            linux_host: self.linux_host,
            linux: self.linux.clone(),
            linux_attention: self.linux_host
                && self.enabled
                && !self.checking
                && (self.linux_error || self.failing_persistent || self.channel_unreadable),
            current_version: self.current_version.clone(),
            current_build: self.current_build,
            staged: self.staged.clone(),
            changelog: self.changelog.clone(),
            enabled: self.enabled,
            automatic_checks: self.automatic_checks,
            outcome: self.outcome.clone(),
            checking: self.checking,
            failing_persistent: self.failing_persistent && self.enabled && !self.checking,
            apply_is_failing: self.apply_is_failing() && self.enabled && !self.checking,
            // Suppressed mid-check for the same reason `apply_is_failing` is: a check
            // in flight owns the surface, and a verdict from before it started must
            // not argue with the "Checking…" the user is looking at.
            apply_trouble: (!self.checking).then(|| self.apply_trouble()).flatten(),
            installable: self.installable,
            dev_build: self.dev_build.is_some(),
            channel_unreadable: self.channel_unreadable && self.enabled && !self.checking,
            checked_at: self.checked_at,
            headline: self.headline(),
            detail,
            automatic_checks_note,
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

    /// The Linux ladder's DISK, ENROLLMENT and TROUBLE arms, first match first: every
    /// headline that outranks what this process's checker is doing. `None` is the idle
    /// copy, where the headline speaks for the checker. One ladder serves the headline
    /// and [`Self::automatic_checks_detail`], which stays silent wherever this answers —
    /// so the switch's timing can never push the fact under a headline out of the
    /// status card's three painted lines (2026-09-24 review: "Startup pending." over a
    /// detail that painted the timing sentence and ellipsized the trial it named).
    fn linux_fact_headline(&self) -> Option<&'static str> {
        let linux = self.linux.as_ref();
        if !self.installable {
            Some("Update unavailable.")
        } else if !self.enabled {
            Some("Updates are off.")
        } else if self.linux_error || self.failing_persistent || self.channel_unreadable {
            Some("Update needs attention.")
        } else if linux.is_some_and(|status| status.staged_build.is_some()) {
            Some("Update downloaded.")
        } else if linux.is_some_and(|status| status.trial_phase.is_some() && !status.trial_healthy)
        {
            Some("Startup pending.")
        } else if linux.is_some_and(|status| status.installed_build > self.current_build) {
            Some("New build installed.")
        } else {
            None
        }
    }

    fn headline(&self) -> String {
        if self.checking {
            "Checking for updates\u{2026}".to_string()
        } else if self.linux_host {
            if let Some(fact) = self.linux_fact_headline() {
                fact.to_string()
            } else if !self.automatic_checks {
                // The Linux updater reads `[update] enabled` exactly as macOS's does —
                // once per process, gating only its background checker — so the idle
                // arm speaks for the checker THIS process runs, as the macOS ladder's
                // does below: "No update staged." over a copy nothing is checking for
                // would read as a verdict. Every Linux arm above is a disk, enrollment
                // or trouble fact and outranks it.
                "Automatic checks are off.".to_string()
            } else {
                "No update staged.".to_string()
            }
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
        } else if let Some(quiet) = self.quiet_download() {
            // The automatic lane's download: the band stays quiet (design ruling
            // 220), so this is where it is seen — `Downloading aterm v0.92.0…`.
            format!("{}\u{2026}", quiet.title)
        } else if !self.installable {
            // NOT "You're up to date": this copy cannot be replaced at all — it is
            // running from the mounted disk image, from a Gatekeeper-translocated
            // location, or from a dev-marked install, so no check thread ever starts
            // and every ledger field below is the pristine default of a machine that
            // structurally cannot update (2026-08-19 round-5 audit). A dev build is
            // that on purpose, and is told where it stands instead (gap #30).
            match &self.dev_build {
                Some(dev) => dev.headline(),
                None => "This copy of aterm can\u{2019}t update itself.".to_string(),
            }
        } else if !self.enabled {
            // No native updater runs on this platform at all — macOS and Linux have one
            // (Linux answers in its own ladder above), so this is neither, and the line
            // does not call the updater macOS-only. The Settings switch is not this:
            // off, it stops only the background checker — the arm below.
            "aterm doesn\u{2019}t update itself on this platform.".to_string()
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
            // "You're up to date." The cause, in words, is the detail below; the ledger's
            // own sentence is `aterm ctl update status`'s and the log's.
            "Updates are failing on this machine.".to_string()
        } else if !self.automatic_checks {
            // NOT "You're up to date": nothing in this process looks by itself, so the
            // claim would be about a check that is not running — whatever the switch
            // now says (a saved flip applies next launch; the detail says so). Real
            // trouble above still outranks it. Short: the medium workbench gives the hero
            // line ~320pt, and "Automatic update checks are off." ellipsized there.
            "Automatic checks are off.".to_string()
        } else {
            "You\u{2019}re up to date.".to_string()
        }
    }

    /// What `[update] enabled` means for this page while no build is staged: this process
    /// reads the key once, so a saved value that differs from the one running applies
    /// next launch, and the page says which way. It stops only the background checker,
    /// so each sentence says what still works. Neutral about the switch's wording, which
    /// the card fits to its width.
    ///
    /// Both native updaters read the key the same way (`aterm_update::automatic`), so the
    /// sentence is the same on Linux; only the manual lane it names differs — the Linux
    /// page has no Update to Latest Now button, and `aterm update check` checks whatever the
    /// switch says. Silent while a build is staged or downloaded, or while trouble owns
    /// the detail, as on macOS — and on Linux while a startup trial is pending or a newer
    /// build is installed ([`Self::linux_fact_headline`]).
    fn automatic_checks_detail(&self) -> Option<String> {
        if !self.enabled || self.staged.is_some() {
            return None;
        }
        // On Linux the sentence belongs to the idle copy alone: every arm of
        // `linux_fact_headline` — a download, a pending startup trial, a newly installed
        // build, trouble — is a fact the three painted lines owe the reader first.
        if self.linux_host && (self.linux.is_none() || self.linux_fact_headline().is_some()) {
            return None;
        }
        // The manual lane each page really has: the Settings page's one button on
        // macOS (it checks when nothing is staged), the terminal verb on Linux.
        let manual = if self.linux_host {
            "`aterm update check`"
        } else {
            "Update to Latest Now"
        };
        match (self.automatic_checks, self.automatic_checks_saved) {
            (true, true) => None,
            // The switch's timing is the card's to say (its caption and the row's
            // "Applies next launch"), not this sentence's.
            (false, false) => Some(format!(
                "Nothing checks for updates by itself \u{2014} {manual} still checks now."
            )),
            (false, true) => Some(format!(
                "Automatic checks start next launch \u{2014} until then, {manual} still \
                 checks now."
            )),
            (true, false) => Some(
                "Automatic checks stop next launch \u{2014} until then, aterm still checks \
                 for updates by itself."
                    .to_string(),
            ),
        }
    }

    /// The secondary detail line under the headline, in plain words: the version on
    /// offer, what is wrong and what happens next. `None` for a healthy macOS page with
    /// nothing staged — the surface says when the last check ran instead
    /// ([`UpdateProjection::checked_line`]), followed by what the automatic-checks switch
    /// means for this process ([`UpdateProjection::automatic_checks_note`]). Build numbers
    /// belong in About; the updater's own decision sentences stay in `aterm ctl update
    /// status` and the log, which the page links to (Settings ▸ Messages).
    fn detail(&self) -> Option<String> {
        if self.linux_host {
            // The ledger's own sentence names the version and the one next step — a
            // download and the verb that installs it, an install and the start of aterm
            // (a window or a terminal session) that finishes it, `aterm update enable` for a copy not enrolled — so a
            // healthy page shows it alone. A failure's sentence names neither: only
            // beside one (or where there is no sentence) does the page add what the
            // headline names and the one step, and a copy that cannot update gets no
            // step (every cause there, an unsafe install prefix or a worker that did not
            // start, is one `enable` cannot fix).
            let outcome = self.outcome.trim().trim_end_matches('.');
            let own_facts = self.linux_error || outcome.is_empty();
            let direction = if own_facts
                && self.installable
                && self
                    .linux
                    .as_ref()
                    .is_some_and(|status| status.staged_build.is_some())
            {
                "Run `aterm update apply` to install it."
            } else {
                ""
            };
            // What the headline names, version first; the running build is About's.
            let identity = self
                .linux
                .as_ref()
                .filter(|_| own_facts)
                .map(|status| {
                    let mut facts = Vec::new();
                    if let Some(build) = status.staged_build {
                        facts.push(match &status.staged_version {
                            Some(version) => format!("Version {version} is downloaded"),
                            None => format!("Build {build} is downloaded"),
                        });
                    }
                    if status.trial_phase.is_some() && !status.trial_healthy {
                        // Only once the new executable is in place (`Installed`: the
                        // installed build IS the trial's); mid-install or mid-rollback
                        // nothing waits for a launch yet.
                        if status.trial_phase.as_deref() == Some("Installed") {
                            facts.push(format!(
                                "Build {} waits for a clean launch",
                                status.installed_build
                            ));
                        }
                    } else if status.installed_build > self.current_build {
                        facts.push(format!("Build {} is installed", status.installed_build));
                    }
                    facts.join(". ")
                })
                .unwrap_or_default();
            let detail = [identity.as_str(), outcome, direction]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(". ");
            // The checker's timing, as on macOS — and FIRST: the status card paints this
            // slot three lines deep and ellipsizes the rest, and main's Linux sentences
            // alone fill those lines at every width, so a timing sentence appended after
            // them never painted (seen in the 2026-09-24 capture). The whole string stays
            // the slot's semantic value.
            let detail = match self.automatic_checks_detail() {
                Some(checks) if detail.is_empty() => checks,
                Some(checks) => format!("{checks} {detail}"),
                None => detail,
            };
            return (!detail.is_empty()).then_some(detail);
        }
        if !self.installable {
            return Some(match &self.dev_build {
                Some(dev) => dev.detail(),
                None => "Move aterm.app to your Applications folder and open it from there. \
                         A copy running from a disk image, a quarantined download, or a \
                         local build is never replaced in place."
                    .to_string(),
            });
        }
        if let Some((_, v)) = self.staged.as_ref() {
            // Not the ledger `outcome`: the check lane rewrites that every cycle with
            // the healthy "staged … ready to apply" sentence while a stage is held.
            // The trouble sentence carries how many attempts failed, why, and whether
            // anything is still scheduled to happen without the user — on 2026-08-21
            // the same builds applied unaided once the machine's load dropped, and only
            // the count and the cause told that apart from a successor that will not
            // boot. `!self.checking` for the reason `projection()` suppresses
            // `apply_trouble` mid-check: a check in flight owns the surface.
            if let Some(trouble) = self.apply_trouble().filter(|_| !self.checking) {
                // The log pointer earns its characters in exactly two states: the
                // ESCALATED one (the full attempt history is worth chasing), and the
                // one where this program could not NAME the cause — there the
                // untranslated ledger reason exists only in the log.
                let tail = if self.apply_is_failing() || !trouble.cause_is_named() {
                    LOG_POINTER
                } else {
                    ""
                };
                return Some(format!("Version {v} \u{00b7} {}{tail}", trouble.sentence()));
            }
            if self.enabled && !self.checking && self.apply_is_failing() {
                // THE ESCALATION WITHOUT AN ARTIFACT TO PIN IT TO: `apply_is_failing`
                // reads the running-build-scoped streak, which outlives the artifact it
                // was recorded against, and a ledger written before the target field
                // existed cannot name this build. Falling through to the check wording
                // below would be a lie about which half is broken.
                return Some(format!(
                    "Version {v} \u{00b7} every try to install it failed.{LOG_POINTER}"
                ));
            }
            if self.enabled && self.failing_persistent {
                // The stage in hand is fine and will install; what is failing is
                // finding the NEXT one. The class is named only when it is not the
                // apply lane's (a single apply failure under an escalated `pipeline`
                // streak leaves `failing_kind = "apply"`, 2026-08-19 round-5 audit).
                let why = match self.failing_kind.as_str() {
                    "apply" | "" => String::new(),
                    kind => format!(": {}", check_trouble_words(kind)),
                };
                return Some(format!(
                    "Version {v} is ready to install, but newer updates can\u{2019}t be \
                     checked right now{why}.{LOG_POINTER}"
                ));
            }
            if self.staged_activation {
                return Some(format!("Version {v} is ready to install."));
            }
            return Some(format!("Version {v} is downloaded and ready to install."));
        }
        if let Some(quiet) = self.quiet_download() {
            // How far it has got (ruling 220): the bytes so far, or — while it
            // is checked — only that. How it installs is the staged page's to
            // say, once there is a build to install.
            return Some(if quiet.progress.is_empty() {
                "Checking the download.".to_string()
            } else {
                format!("{} downloaded.", quiet.progress)
            });
        }
        if self.channel_unreadable && !self.checking && self.enabled {
            // For the stranded state the outcome IS the explanation — the cause and the
            // copy-pasteable remedy only an operator can apply — rewritten on every
            // check so it cannot go stale.
            let cause = self.outcome.trim();
            return Some(if cause.is_empty() {
                format!("The release channel can\u{2019}t be read.{LOG_POINTER}")
            } else {
                cause.to_string()
            });
        }
        if self.failing_persistent && !self.checking && self.enabled {
            // A build that predates the channel's keys can never check again — the one
            // CHECK failure a person has to act on, and the only `manifest` sentence
            // that prescribes a reinstall — so its remedy is on the page. Every other
            // class clears by itself, and its record is on Settings ▸ Messages.
            if self.failing_kind == "manifest" && self.outcome.contains("reinstall") {
                return Some(
                    "This copy of aterm is too old to check for updates. Reinstall aterm \
                     from the current release (drag it from the release DMG)."
                        .to_string(),
                );
            }
            // An install streak with no build left to pin it to (its stage is gone):
            // not a check failure, whatever the class words would say.
            let trouble = if self.failing_kind == "apply" {
                "The last updates didn\u{2019}t install".to_string()
            } else {
                sentence_start(check_trouble_words(&self.failing_kind))
            };
            return Some(format!("{trouble}; aterm keeps trying.{LOG_POINTER}"));
        }
        // After the trouble above, as the headline has it: a failing or stranded
        // channel's cause is worth more than the switch's timing, which rides
        // `automatic_checks_note` after the last check's time.
        None
    }
}

/// Where a trouble line sends the reader for the rest: the page's own log link.
const LOG_POINTER: &str = " Details in Settings \u{25b8} Messages.";

/// A failing check CLASS (`network` / `manifest` / `pipeline` / `stage`, the updater's
/// health-ledger names) in a person's words.
fn check_trouble_words(kind: &str) -> &'static str {
    match kind {
        "network" => "the update server can\u{2019}t be reached",
        "manifest" => "the newest release couldn\u{2019}t be verified",
        "pipeline" => "downloads aren\u{2019}t finishing",
        "stage" => "a download couldn\u{2019}t be prepared",
        _ => "update checks keep failing",
    }
}

/// `the newest …` → `The newest …`.
fn sentence_start(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux_state(linux: Option<aterm_update::LinuxUpdateStatus>, failures: u32) -> UpdateState {
        let mut status = staged_status();
        status.linux = linux;
        status.staged_build = None;
        status.staged_version = None;
        status.staged_commit = None;
        status.staged_dmg_sha256 = None;
        status.failing_checks = failures;
        status.outcome = "authenticated Linux status receipt".into();
        let mut state = UpdateState::from_status(828, "0.5.14", Some(&status), false);
        state.linux_host = true;
        state
    }

    fn linux_facts() -> aterm_update::LinuxUpdateStatus {
        aterm_update::LinuxUpdateStatus {
            installed_build: 828,
            staged_build: Some(830),
            staged_version: Some("0.5.15".into()),
            staged_commit: Some("ab".repeat(20)),
            trial_phase: None,
            trial_starts: 0,
            trial_healthy: false,
            refused_newer: false,
        }
    }

    /// The held download's ledger sentence (`aterm_update::linux`).
    const LINUX_HELD: &str = "aterm 0.5.15 is downloaded \u{2014} `aterm update apply` installs it (`[update] \
         auto_apply` is off)";
    /// A replaced executable's ledger sentence while it waits for its first start.
    const LINUX_PENDING: &str = "aterm 0.5.15 is installed \u{2014} the next aterm you start \
                                 finishes it; existing sessions continue unchanged";

    #[test]
    fn linux_update_status_download_is_read_only_and_directs_manual_apply() {
        let mut state = linux_state(Some(linux_facts()), 0);
        state.outcome = LINUX_HELD.into();
        let projection = state.projection();
        assert!(projection.headline.contains("downloaded"));
        let detail = projection.detail.as_deref().unwrap();
        // The ledger's sentence names the version and the verb, once each; build
        // numbers are About's.
        assert_eq!(detail, LINUX_HELD);
        assert!(!detail.contains("build 8"), "{detail}");
        assert!(projection.staged.is_none());
        assert_eq!(projection.linux.as_ref().unwrap().staged_build, Some(830));
        assert_eq!(
            crate::native_settings::compact_update_headline(&projection),
            "Downloaded"
        );
        // The tersest rung keeps the command whole: `update apply` alone runs nothing.
        assert_eq!(
            crate::native_settings::compact_update_detail_minimum(&projection),
            "aterm update apply"
        );
    }

    #[test]
    fn linux_update_status_pending_trial_never_claims_current() {
        let mut facts = linux_facts();
        facts.staged_build = None;
        facts.staged_version = None;
        facts.staged_commit = None;
        facts.installed_build = 830;
        facts.trial_phase = Some("Installed".into());
        facts.trial_starts = 1;
        let mut state = linux_state(Some(facts), 0);
        state.outcome = LINUX_PENDING.into();
        let projection = state.projection();
        assert_eq!(projection.headline, "Startup pending.");
        assert_eq!(projection.detail.as_deref(), Some(LINUX_PENDING));
        assert_eq!(
            crate::native_settings::compact_update_headline(&projection),
            "Awaiting startup"
        );
        // Beside a failure the page names the pending install itself; mid-install and
        // mid-rollback nothing waits for a launch yet.
        let failing = linux_state(state.linux.clone(), 1).projection().detail;
        assert!(
            failing
                .as_deref()
                .unwrap_or_default()
                .starts_with("Build 830 waits for a clean launch."),
            "{failing:?}"
        );
        for phase in ["Prepared", "RollbackPrepared"] {
            let mut facts = state.linux.clone().expect("the Linux facts");
            facts.trial_phase = Some(phase.into());
            let detail = linux_state(Some(facts), 1).projection().detail;
            assert!(
                !detail
                    .as_deref()
                    .unwrap_or_default()
                    .contains("clean launch"),
                "{phase}: {detail:?}"
            );
        }
    }

    #[test]
    fn linux_update_status_first_error_remains_visible_with_a_download() {
        let mut state = linux_state(Some(linux_facts()), 1);
        state.outcome = "signature verification refused the new manifest".into();
        let projection = state.projection();
        assert!(projection.headline.contains("attention"));
        let detail = projection.detail.as_deref().unwrap();
        // The failure names neither the download nor the step: the page adds both.
        assert_eq!(
            detail,
            "Version 0.5.15 is downloaded. signature verification refused the new manifest. \
             Run `aterm update apply` to install it."
        );
        // A sentence that ends itself is not ended twice.
        state.outcome = "Another aterm is updating this copy right now.".into();
        assert!(
            !state.projection().detail.unwrap().contains(".."),
            "one sentence ends once"
        );
        assert!(projection.linux_attention);
        assert_eq!(
            crate::native_settings::compact_update_headline(&projection),
            "Needs attention"
        );
    }

    #[test]
    fn linux_update_status_unenrolled_copy_keeps_exact_reason_and_linux_advice() {
        let mut state = linux_state(None, 0);
        state.enabled = false;
        state.installable = false;
        state.outcome = "installed prefix is group writable before a private ancestor".into();
        let state = state.without_inapplicable_stage();
        let projection = state.projection();
        let detail = projection.detail.as_deref().unwrap();
        assert!(detail.contains("group writable"));
        // `aterm update enable` fails with this same error: it is not the remedy.
        assert!(!detail.contains("aterm update enable"), "{detail}");
        assert!(!detail.contains("Applications"));
        assert!(!detail.contains(".app"));
        assert_eq!(projection.outcome, state.outcome);
        assert_eq!(
            crate::native_settings::compact_update_headline(&projection),
            "Unavailable"
        );
    }

    /// THE LINUX PAGE SPEAKS FOR THE CHECKER THIS PROCESS RUNS, TOO (2026-09-24 merge of
    /// the two-switch card with the Linux updater). `aterm_update::linux` gates its
    /// background checker on `aterm_update::automatic()` — the same once-per-process read
    /// of `[update] enabled` macOS uses — and a typed `aterm update check` never consults
    /// it. So the idle headline follows the running value, the detail names a saved flip
    /// as next launch in both directions and the manual lane Linux actually has (no Check
    /// for Updates button there), and every disk, enrollment or trouble fact still
    /// outranks the switch.
    #[test]
    fn linux_automatic_checks_follow_the_running_checker_and_name_the_terminal_lane() {
        let mut idle = linux_facts();
        idle.staged_build = None;
        idle.staged_version = None;
        idle.staged_commit = None;
        let page = |facts: aterm_update::LinuxUpdateStatus, failures, running, saved| {
            linux_state(Some(facts), failures)
                .with_automatic_checks(running, saved)
                .projection()
        };
        let on = page(idle.clone(), 0, true, true);
        assert_eq!(on.headline, "No update staged.");
        assert!(
            !on.detail.as_deref().unwrap().contains("Automatic checks"),
            "{:?}",
            on.detail
        );

        let off = page(idle.clone(), 0, false, false);
        assert_eq!(off.headline, "Automatic checks are off.");
        let detail = off.detail.expect("the Linux detail");
        // First, where the three painted lines of the slot show it; main's Linux
        // sentences follow whole.
        assert!(
            detail.starts_with(
                "Nothing checks for updates by itself \u{2014} `aterm update check` still \
                 checks now. "
            ),
            "{detail}"
        );
        assert!(
            detail.ends_with(on.detail.as_deref().unwrap()),
            "the Linux detail follows whole: {detail}"
        );
        assert!(!detail.contains("Update to Latest Now"), "{detail}");
        assert!(!detail.contains(".."), "one sentence ends once: {detail}");

        let starting = page(idle.clone(), 0, false, true);
        assert_eq!(starting.headline, "Automatic checks are off.");
        assert!(
            starting.detail.as_deref().unwrap().starts_with(
                "Automatic checks start next launch \u{2014} until then, `aterm update \
                 check` still checks now. "
            ),
            "{:?}",
            starting.detail
        );

        let stopping = page(idle.clone(), 0, true, false);
        assert_eq!(stopping.headline, "No update staged.");
        assert!(
            stopping
                .detail
                .as_deref()
                .unwrap()
                .starts_with("Automatic checks stop next launch"),
            "{:?}",
            stopping.detail
        );

        // A download on disk and trouble outrank the switch, headline and detail alike.
        let downloaded = page(linux_facts(), 0, false, false);
        assert_eq!(downloaded.headline, "Update downloaded.");
        assert!(
            !downloaded
                .detail
                .as_deref()
                .unwrap()
                .contains("Automatic checks"),
            "{:?}",
            downloaded.detail
        );
        let attention = page(idle.clone(), 1, false, false);
        assert_eq!(attention.headline, "Update needs attention.");
        assert!(
            !attention
                .detail
                .as_deref()
                .unwrap()
                .contains("Automatic checks"),
            "{:?}",
            attention.detail
        );
        // So do a startup trial still pending and a build installed since this one
        // started (2026-09-24 review): the detail opens with the facts its headline names,
        // never with the switch's timing, in the three lines the status card paints.
        let mut trial = idle.clone();
        trial.installed_build = 830;
        trial.trial_phase = Some("Installed".into());
        trial.trial_starts = 1;
        let mut installed = idle;
        installed.installed_build = 830;
        for (facts, headline, fact) in [
            (trial, "Startup pending.", LINUX_PENDING),
            (
                installed,
                "New build installed.",
                "aterm 0.5.15 is installed",
            ),
        ] {
            for (running, saved) in [(false, false), (false, true), (true, false)] {
                let mut state = linux_state(Some(facts.clone()), 0);
                state.outcome = fact.into();
                let projection = state.with_automatic_checks(running, saved).projection();
                assert_eq!(projection.headline, headline);
                let detail = projection.detail.expect("the Linux detail");
                assert!(detail.starts_with(fact), "{headline} {detail}");
                assert!(
                    !detail.contains("healthy") && !detail.contains("starts 1"),
                    "no raw trial fields: {headline} {detail}"
                );
                assert!(
                    !detail.contains("Automatic checks") && !detail.contains("Nothing checks"),
                    "{headline} {detail}"
                );
            }
        }
    }

    /// Where no native updater runs (neither macOS nor Linux) the projection says so
    /// without calling the updater macOS-only: Linux has one.
    #[test]
    fn no_native_updater_is_not_called_macos_only() {
        let mut status = staged_status();
        status.enabled = false;
        status.staged_build = None;
        status.staged_version = None;
        let projection = UpdateState::from_status(828, "0.5.14", Some(&status), false).projection();
        assert!(!projection.linux_host);
        assert_eq!(
            projection.headline,
            "aterm doesn\u{2019}t update itself on this platform."
        );
        assert!(!projection.headline.contains("macOS"));
    }

    fn staged_status() -> aterm_update::UpdateStatus {
        aterm_update::UpdateStatus {
            linux: None,
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
            failing_checks_kind: String::new(),
            channel_unreadable: false,
        }
    }

    /// "CHECK FOR UPDATES AUTOMATICALLY" OFF IS NOT "UPDATES OFF" (2026-09-23 review).
    /// The switch stops the background checker only: the page never claims "You're up
    /// to date" over a machine nothing is checking, and says a check still runs when
    /// asked; a staged build is still "Update ready", and real trouble still outranks
    /// the switch — in the detail as in the headline. Before, the switch fed the
    /// updater's `enabled` and a clicked Check for Updates was refused as "disabled on
    /// this build".
    #[test]
    fn automatic_checks_off_is_named_and_leaves_the_manual_lane_standing() {
        let mut st = staged_status();
        st.staged_build = None;
        st.staged_version = None;
        let off = UpdateState::from_status(828, "0.5.14", Some(&st), false)
            .with_automatic_checks(false, false)
            .projection();
        assert!(
            off.enabled,
            "the updater still runs here: a person's check works"
        );
        assert_eq!(off.headline, "Automatic checks are off.");
        // The detail slot keeps the last check's time; the switch's sentence follows it.
        assert_eq!(off.detail, None);
        let detail = off
            .automatic_checks_note
            .expect("the page says what is off");
        assert!(
            detail.starts_with("Nothing checks for updates by itself"),
            "{detail}"
        );
        assert!(
            detail.contains("Update to Latest Now still checks"),
            "{detail}"
        );
        let on = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
        assert_eq!(on.headline, "You\u{2019}re up to date.");
        assert_eq!((on.detail, on.automatic_checks_note), (None, None));
        // A build a person checked for and staged is still ready to install.
        let staged = UpdateState::from_status(828, "0.5.14", Some(&staged_status()), false)
            .with_automatic_checks(false, false)
            .projection();
        assert_eq!(staged.headline, "Update ready");
        // A stranded channel outranks the switch, headline and detail alike.
        st.channel_unreadable = true;
        st.outcome = "the channel is unreadable".to_string();
        let stranded = UpdateState::from_status(828, "0.5.14", Some(&st), false)
            .with_automatic_checks(false, false)
            .projection();
        assert_eq!(
            stranded.headline,
            "aterm can\u{2019}t check for updates on this machine."
        );
        assert_eq!(
            stranded.detail.as_deref(),
            Some("the channel is unreadable")
        );
        assert_eq!(stranded.automatic_checks_note, None);
    }

    /// THE PAGE SPEAKS FOR THIS PROCESS, NOT THE SWITCH (2026-09-23 review). The updater
    /// reads `[update] enabled` once per process, and the switch now sits on this page, so
    /// one click must not make the headline describe a checker that is not what runs:
    /// turned off, the checker keeps running until quit (and can still stage and, with
    /// Install on, install), so the headline stays the running one's and the detail says
    /// checks stop next launch; turned on over an off launch, nothing checks until the
    /// next one, so the headline stays "off" and the detail says when they start.
    #[test]
    fn a_saved_flip_of_automatic_checks_is_named_as_next_launch_in_both_directions() {
        let mut st = staged_status();
        st.staged_build = None;
        st.staged_version = None;
        let page = |running, saved| {
            UpdateState::from_status(828, "0.5.14", Some(&st), false)
                .with_automatic_checks(running, saved)
                .projection()
        };
        let stopping = page(true, false);
        assert_eq!(stopping.headline, "You\u{2019}re up to date.");
        let detail = stopping
            .automatic_checks_note
            .expect("the pending stop is named");
        assert!(
            detail.starts_with("Automatic checks stop next launch"),
            "{detail}"
        );
        assert!(
            detail.contains("still checks for updates by itself"),
            "{detail}"
        );
        let starting = page(false, true);
        assert_eq!(starting.headline, "Automatic checks are off.");
        let detail = starting
            .automatic_checks_note
            .expect("the pending start is named");
        assert!(
            detail.starts_with("Automatic checks start next launch"),
            "{detail}"
        );
        assert!(
            detail.contains("Update to Latest Now still checks now"),
            "{detail}"
        );
        // Neither sentence names the switch by one of the wordings the card fits to its
        // width.
        for detail in [
            page(false, false).automatic_checks_note,
            page(false, true).automatic_checks_note,
            page(true, false).automatic_checks_note,
        ] {
            let detail = detail.expect("a sentence");
            assert!(
                !detail.contains("Check for updates automatically"),
                "{detail}"
            );
            assert!(!detail.contains("Check automatically"), "{detail}");
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

    /// A DEV BUILD IS TOLD IT IS ONE, AND WHERE IT STANDS (gap #30): a dev-marked copy
    /// cannot be replaced either, but on purpose — "move it to Applications" is wrong
    /// for it. The page says it is a dev build, how far behind the newest release it is
    /// once the dev channel has read that, and what moves it. A copy that is not
    /// dev-marked keeps the move-it remedy.
    #[test]
    fn a_dev_build_is_told_where_it_stands_not_to_move() {
        use aterm_update::dev_channel::DevLag;
        let mut st = staged_status();
        st.staged_build = None;
        st.staged_version = None;
        st.staged_dmg_sha256 = None;
        st.changelog = None;
        st.installable = false;
        let page = |standing: Option<DevLag>| {
            UpdateState::from_status(828, "0.91.0", Some(&st), false)
                .with_dev_build(Some(DevBuildPage { standing }))
                .projection()
        };
        let unread = page(None);
        assert_eq!(unread.headline, "This is a dev build.");
        let detail = unread.detail.expect("detail");
        assert!(
            detail.starts_with("The updater leaves a dev build alone"),
            "{detail}"
        );
        assert!(detail.contains("tools/dev-app.sh"), "{detail}");
        let behind = page(Some(DevLag::Behind {
            latest: "v0.93.0".into(),
            releases: Some(2),
        }));
        assert_eq!(behind.headline, "This dev build is 2 releases behind.");
        assert!(
            behind
                .detail
                .as_deref()
                .is_some_and(|d| d.starts_with("It is 2 releases behind aterm v0.93.0. ")),
            "{:?}",
            behind.detail
        );
        assert_eq!(
            page(Some(DevLag::Behind {
                latest: "v0.93.0".into(),
                releases: Some(1),
            }))
            .headline,
            "This dev build is 1 release behind."
        );
        assert_eq!(
            page(Some(DevLag::Behind {
                latest: "v1.0.0".into(),
                releases: None,
            }))
            .headline,
            "This dev build is older than aterm v1.0.0."
        );
        let current = page(Some(DevLag::Current {
            latest: "v0.91.0".into(),
        }));
        assert_eq!(current.headline, "This is a dev build.");
        assert!(
            current
                .detail
                .as_deref()
                .is_some_and(|d| d.starts_with("It is at aterm v0.91.0, the newest release. ")),
            "{:?}",
            current.detail
        );
        // Every compact card reads the same mark: "Dev build", never "Can't update".
        assert!(behind.dev_build);
        assert_eq!(
            crate::native_settings::compact_update_headline(&behind),
            "Dev build"
        );
        // The control: the same copy without the dev mark keeps the move-it remedy.
        let plain = UpdateState::from_status(828, "0.91.0", Some(&st), false).projection();
        assert!(!plain.dev_build);
        assert_eq!(
            crate::native_settings::compact_update_headline(&plain),
            "Can\u{2019}t update"
        );
        assert_eq!(
            plain.headline,
            "This copy of aterm can\u{2019}t update itself."
        );
        assert!(
            plain
                .detail
                .is_some_and(|d| d.contains("Applications folder"))
        );
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
            detail.contains("0.5.15") && detail.contains("3 times"),
            "the escalated detail names the version and how many attempts died: {detail}"
        );
        assert!(
            !detail.contains("ready to apply"),
            "not the ledger's healthy sentence: {detail}"
        );
        assert!(
            detail.contains("Settings \u{25b8} Messages"),
            "the escalated case keeps its pointer at the full history: {detail}"
        );
        assert!(
            !detail.contains("build 830"),
            "build numbers belong in About: {detail}"
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
        assert_eq!(
            detail,
            "Version 0.5.15 is ready to install, but newer updates can\u{2019}t be checked \
             right now: the newest release couldn\u{2019}t be verified. Details in \
             Settings \u{25b8} Messages."
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
        assert_eq!(detail, "Version 0.5.15 is downloaded and ready to install.");
        for alarming in ["tried", "failed", "did not", "retry"] {
            assert!(
                !detail.contains(alarming),
                "a stage that has simply not been attempted must not read as a \
                 failure ({alarming:?} in {detail:?})"
            );
        }
    }

    /// AN INSTALLED-BUNDLE ACTIVATION WAS NEVER DOWNLOADED (2026-09-27 audit). The
    /// reducer imports a newer bundle already at this app's path — the cutter writing
    /// into its own bundle, a dragged-in `.app` — as a stage whose digest is the
    /// activation identity. The page's "Update ready" detail must not call it
    /// downloaded; a real download stage still says so (the negative control).
    #[test]
    fn an_installed_activation_is_not_called_downloaded() {
        use crate::native_updater_service::{
            NativeUpdaterService, StagedUpdate, installed_activation_digest,
        };
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let detail_for = |dmg_sha256: String| {
            let mut snapshot = NativeUpdaterService::new(828, "0.5.14", true)
                .snapshot()
                .clone();
            snapshot.staged = Some(StagedUpdate {
                build: 830,
                version: "0.5.15".to_string(),
                commit: Some(commit.to_string()),
                dmg_sha256,
                changelog: None,
                generation: 1,
            });
            let p = UpdateState::from_service(&snapshot, false, ApplyRetry::Scheduled).projection();
            assert_eq!(p.headline, "Update ready");
            p.detail
        };

        assert_eq!(
            detail_for(installed_activation_digest(830, commit)).as_deref(),
            Some("Version 0.5.15 is ready to install.")
        );
        assert_eq!(
            detail_for("ab".repeat(32)).as_deref(),
            Some("Version 0.5.15 is downloaded and ready to install.")
        );
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

    /// A HEALTHY PAGE SAYS WHEN IT LAST CHECKED, NOT HOW (2026-09-23 audit, SB-07): the
    /// updater's "up to date (latest release build N) — checking over the unmetered web
    /// lane …" sentence was painted under "You're up to date." as three wrapped lines of
    /// jargon. The healthy projection carries no detail of its own; the surface paints
    /// the check time, relative, from the ledger's completed-check stamp.
    #[test]
    fn a_healthy_page_says_when_it_last_checked() {
        let mut st = staged_status();
        st.staged_build = None;
        st.staged_version = None;
        st.outcome = "up to date (latest release build 828) \u{b7} checks every 30 min".to_string();
        st.updated_at = "2026-09-23T12:00:00Z".to_string();
        let p = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
        assert_eq!(p.headline, "You\u{2019}re up to date.");
        assert_eq!(p.detail, None, "nothing to add beyond when it last checked");
        let checked = aterm_update_core::pkg_check::rfc3339_to_unix(&st.updated_at).unwrap();
        assert_eq!(p.checked_at, Some(checked));
        assert_eq!(
            p.checked_line(checked + 12 * 60 + 5).as_deref(),
            Some("Checked 12 min ago")
        );
        // Never checked: nothing to say, and the surface says so in its own words.
        st.updated_at = String::new();
        let p = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
        assert_eq!(p.checked_line(checked), None);
    }

    #[test]
    fn ago_words_are_what_a_person_says() {
        for (age, words) in [
            (-30, "just now"),
            (0, "just now"),
            (59, "just now"),
            (60, "1 min ago"),
            (12 * 60 + 59, "12 min ago"),
            (3 * 3600 + 1, "3 h ago"),
            (86_400 + 5, "1 day ago"),
            (5 * 86_400, "5 days ago"),
        ] {
            assert_eq!(ago_words(1_000_000 - age, 1_000_000), words, "age {age}");
        }
    }

    /// A PERSISTENT CHECK FAILURE IS SAID IN WORDS, WITH WHERE THE REST IS: the class
    /// the ledger names, that aterm keeps trying, and Settings ▸ Messages — never the
    /// ledger's "FAILING (3 consecutive checks since …)" sentence. The one failure a
    /// person must act on, a build too old to verify the channel, keeps its remedy.
    #[test]
    fn a_persistent_check_failure_is_plain_and_points_at_the_log() {
        let mut st = staged_status();
        st.staged_build = None;
        st.staged_version = None;
        st.failing_checks = 3;
        st.failing_kind = "network".to_string();
        st.failing_persistent = true;
        st.outcome = "FAILING (3 consecutive checks since 2026-09-23T10:00:00Z): network: \
                      curl exit 6"
            .to_string();
        let p = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
        assert_eq!(p.headline, "Updates are failing on this machine.");
        assert_eq!(
            p.detail.as_deref(),
            Some(
                "The update server can\u{2019}t be reached; aterm keeps trying. Details in \
                 Settings \u{25b8} Messages."
            )
        );
        st.failing_kind = "manifest".to_string();
        st.outcome = "FAILING (3 consecutive checks since 2026-09-23T10:00:00Z): this build's \
                      trust anchor cannot verify the channel's releases — reinstall aterm from \
                      the current release"
            .to_string();
        let p = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
        let detail = p.detail.expect("detail");
        assert!(
            detail.starts_with("This copy of aterm is too old"),
            "{detail}"
        );
        assert!(detail.contains("Reinstall aterm"), "{detail}");
        // An INSTALL streak whose own remedy says "reinstall" is not a too-old copy, and
        // not a check failure either.
        st.failing_kind = "apply".to_string();
        st.failing_applies = aterm_update::PERSISTENT_AFTER;
        st.outcome = "staged build did not apply: this install cannot update itself: \
                      reinstall the signed release"
            .to_string();
        let p = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
        assert_eq!(
            p.detail.as_deref(),
            Some(
                "The last updates didn\u{2019}t install; aterm keeps trying. Details in \
                 Settings \u{25b8} Messages."
            )
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
