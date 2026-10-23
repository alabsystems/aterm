// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The software update model: the one structured snapshot ([`UpdateState`], captured from the process-owned reducer, or in tests from [`aterm_update::status`]) that every update surface reads.

use crate::native_updater_service::UpdateInstallability;
use crate::update_apply_trouble::{ApplyRetry, ApplyTrouble};

/// The headline while nothing has read whether this copy can replace itself.
pub(crate) const UPDATE_UNOBSERVED_HEADLINE: &str = "Checking this copy\u{2026}";

/// The per-process Software Update snapshot every update surface reads.
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
    /// The stage is an installed-bundle activation
    /// (`StagedUpdate::is_installed_activation`): already at this app's path, never
    /// downloaded, so the detail does not say "downloaded".
    staged_activation: bool,
    /// The staged build's "what changed" notes, rendered from Markdown to clean lines.
    changelog: Vec<String>,
    /// Whether the native updater runs here (`aterm_update::enabled`: macOS and Linux —
    /// on Linux also only for an enrolled copy, `aterm_update::linux::status`).
    enabled: bool,
    /// "Check for updates automatically" (`[update] enabled`, default on) as this process
    /// reads it: whether its background checker looks for builds by itself.
    automatic_checks: bool,
    /// The same key as saved, which the next launch reads; where it differs from
    /// [`Self::automatic_checks`] the detail says what changes then.
    automatic_checks_saved: bool,
    /// The last updater outcome string (from the health ledger) — shown small.
    outcome: String,
    /// The health ledger says this machine's updates are failing persistently (a streak
    /// of one failure class, not a blip): the headline says so instead of "You're up to
    /// date", and `outcome` carries the ledger's own sentence with the cause.
    failing_persistent: bool,
    /// The failing class (`apply`, `pipeline`, …) when `failing_persistent`: the most
    /// recent one, which is why it cannot decide [`Self::apply_is_failing`].
    failing_kind: String,
    /// Consecutive apply failures. `>= PERSISTENT_AFTER` is the exact statement "the
    /// staged build will not start", whichever class failed last; any non-zero value is
    /// already news (the threshold decides how loudly the person is told, not whether).
    failing_applies: u32,
    /// Why those applies failed: the ledger's `last_apply_error`, the prose the control
    /// socket prints as `apply_failure=`.
    apply_failure: String,
    /// Which artifact [`Self::apply_failure`] is about (0 when unknown), and how many
    /// consecutive attempts on that artifact are behind it (the ledger's
    /// `last_apply_failure_target_build` / `apply_failures_for_target`).
    apply_failure_build: u64,
    /// See [`Self::apply_failure_build`].
    apply_attempts_for_stage: u32,
    /// Whether the automatic lane retries this artifact by itself or needs a person
    /// (`App::apply_retry_for`): event-loop state, not ledger state, and the half that
    /// decides whether the reader has to act.
    apply_retry: ApplyRetry,
    /// The staged build is on hold (`App::latched_activation_trial_spent`, the fact the
    /// Version menu's press refuses on): its next start is the one its boot trial rolls
    /// back.
    staged_on_hold: bool,
    /// Whether this launch has a bundle the updater could replace at all, as far
    /// as a read has observed.
    installable: UpdateInstallability,
    /// The stranded verdict: checks complete but the release channel cannot be
    /// read (401/403/404 with nothing to try, or a renamed repo).
    channel_unreadable: bool,
    /// A manual "Check for Updates" is running off-thread (shows "Checking…").
    checking: bool,
    /// When the last check completed (Unix seconds), `None` before any has: the page
    /// says "Checked 12 min ago" from it ([`UpdateProjection::checked_line`]).
    checked_at: Option<i64>,
    /// The automatic lane's download in flight, which the band does not show: the
    /// headline and detail say it.
    downloading: Option<QuietDownload>,
    /// A dev-marked copy (`ship dev-app`), which the updater leaves alone on
    /// purpose; `None` for every other copy ([`Self::with_dev_build`]).
    dev_build: Option<DevBuildPage>,
    /// Where this copy runs from (`aterm_update::which_copy::running_posture`),
    /// read once at launch: why a copy that can't update itself can't.
    install_posture: Option<aterm_update::which_copy::InstallPosture>,
}

/// The detail under "This copy of aterm can't update itself." for a copy that is not an app bundle at all: what it is, and how to get a copy that updates.
pub(crate) const NOT_A_BUNDLE_DETAIL: &str =
    "It isn\u{2019}t an installed aterm.app. Install aterm from alab.systems to get updates.";

/// A dev-marked copy as the page says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DevBuildPage {
    pub(crate) standing: Option<aterm_update::dev_channel::DevLag>,
}

impl DevBuildPage {
    /// The headline: how far behind, when that is known and true; else that it is a
    /// dev build.
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
            "{standing}The updater leaves a dev build alone: rebuild it with `targo \
             --unverified ship dev-app` in its checkout, or use the release (tools/install.sh \
             puts it in Applications)."
        )
    }
}

/// A download (or its check) nobody is waiting on — the automatic lane's — as the page says it: it takes no row on the band, so this page is where its progress is seen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QuietDownload {
    /// The version, for the record a failure writes.
    pub(crate) version: String,
    pub(crate) title: String,
    pub(crate) progress: String,
}

/// Owned, structured read projection shared with the native Settings `/updates` route: it keeps the updater service state private without parsing the `controls update` serialization.
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
    /// "Check for updates automatically" as this process runs it (a saved flip applies next launch): the full headline and the compact rungs key "off" on it, so no squeeze of the page calls it "Current" while nothing checks by itself.
    pub(crate) automatic_checks: bool,
    pub(crate) outcome: String,
    pub(crate) checking: bool,
    /// The ledger's persistent-failure verdict — the surfaces style the headline as a warning and never say "current" while it holds.
    pub(crate) failing_persistent: bool,
    /// …and specifically in the apply class: the staged build will not start.
    pub(crate) apply_is_failing: bool,
    /// The apply lane's standing trouble for the staged build: how many attempts failed, why, and whether the lane will try again unaided.
    pub(crate) apply_trouble: Option<ApplyTrouble>,
    /// The staged build is on hold (`App::latched_activation_trial_spent`): its next start is the one its boot trial rolls back, so the page says it is on hold at every width, and its button sends nothing and says so.
    pub(crate) staged_on_hold: bool,
    /// Whether a replacement could be installed here at all — false for a copy run from the mounted disk image, a Gatekeeper-translocated download, or a dev-marked build.
    pub(crate) installable: UpdateInstallability,
    /// This copy is dev-marked (`ship dev-app`): `installable` is false on purpose, so every card that would say "can't update" or "reinstall" says it is a dev build instead.
    pub(crate) dev_build: bool,
    /// The stranded verdict (see [`UpdateState`]): the compact card must say "Can't check", never "Current", while the channel is unreadable.
    pub(crate) channel_unreadable: bool,
    /// When the last check completed (Unix seconds), `None` before any has.
    pub(crate) checked_at: Option<i64>,
    pub(crate) headline: String,
    pub(crate) detail: Option<String>,
    /// macOS: what the automatic-checks switch means for this process where it is not simply on ([`UpdateState::automatic_checks_detail`]), said after when the last check ran.
    pub(crate) automatic_checks_note: Option<String>,
}

impl UpdateProjection {
    /// `Checked 12 min ago` — when the last check completed, said relative to `now` (Unix seconds); `None` before any check has.
    pub(crate) fn checked_line(&self, now: i64) -> Option<String> {
        self.checked_at
            .map(|at| format!("Checked {}", ago_words(at, now)))
    }
}

/// How long before `now` the instant `at` was, as a person says it: `just now`, `12 min ago`, `3 h ago`, `2 days ago` (Unix seconds both).
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
    /// Project the process-owned updater reducer into UI state: memory-only, so opening Settings, introspection and repainting never parse a ledger or probe a bundle on the event-loop thread.
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
            staged_on_hold: false,
            installable: snapshot.installable,
            channel_unreadable: snapshot.channel_unreadable,
            checking,
            checked_at: snapshot.checked_at,
            downloading: None,
            dev_build: None,
            install_posture: None,
        }
        .without_inapplicable_stage()
    }

    /// Snapshot the current updater state.
    #[cfg(test)]
    pub(crate) fn from_status(
        current_build: u64,
        current_version: &str,
        status: Option<&aterm_update::UpdateStatus>,
        checking: bool,
    ) -> Self {
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
            apply_failure: String::new(),
            apply_failure_build: 0,
            apply_attempts_for_stage: 0,
            apply_retry: ApplyRetry::ManualOnly,
            staged_on_hold: false,
            installable: status.map_or(UpdateInstallability::Unobserved, |s| {
                UpdateInstallability::observed(s.installable)
            }),
            channel_unreadable: status.is_some_and(|s| s.enabled && s.channel_unreadable),
            checking,
            checked_at: status
                .and_then(|s| aterm_update_core::pkg_check::rfc3339_to_unix(&s.updated_at)),
            downloading: None,
            dev_build: None,
            install_posture: None,
        }
        .without_inapplicable_stage()
    }

    /// "Check for updates automatically" as this process runs it (`running`: the value `aterm_update::automatic` read at launch) and as saved (`saved`: `app_config::update_checks_automatic`, which the next launch reads).
    pub(crate) const fn with_automatic_checks(mut self, running: bool, saved: bool) -> Self {
        self.automatic_checks = running;
        self.automatic_checks_saved = saved;
        self
    }

    /// The automatic lane's download in flight (`App::update_quiet`): no row says it, so the page does.
    pub(crate) fn with_download(mut self, downloading: Option<QuietDownload>) -> Self {
        self.downloading = downloading;
        self
    }

    /// This copy is dev-marked (`App::dev_build`): the page says so, and where it stands, instead of the move-it remedy.
    pub(crate) fn with_dev_build(mut self, dev_build: Option<DevBuildPage>) -> Self {
        self.dev_build = dev_build;
        self
    }

    /// Where this copy runs from (`App::install_posture`): a copy that can't update itself is told its one cause and remedy, not a guess between three.
    pub(crate) const fn with_install_posture(
        mut self,
        posture: Option<aterm_update::which_copy::InstallPosture>,
    ) -> Self {
        self.install_posture = posture;
        self
    }

    /// The staged build is on hold (`App::latched_activation_trial_spent`): the page says so, and its button starts nothing.
    pub(crate) const fn with_staged_on_hold(mut self, on_hold: bool) -> Self {
        self.staged_on_hold = on_hold;
        self
    }

    /// A build on offer that is on hold ([`Self::staged_on_hold`]).
    const fn on_hold(&self) -> bool {
        self.enabled && self.staged.is_some() && self.staged_on_hold
    }

    /// The quiet download, where the page may say it: macOS, an installable copy
    /// with the updater on, nothing staged, and no check a person started (whose
    /// own words and row own the surface).
    fn quiet_download(&self) -> Option<&QuietDownload> {
        self.downloading.as_ref().filter(|_| {
            !self.linux_host
                && self.enabled
                && self.installable.is_installable()
                && self.staged.is_none()
                && !self.checking
        })
    }

    /// A stage this copy cannot apply is not a stage for this copy. `aterm_update::status()`
    /// reads a HOME-keyed Updates directory, so every copy for one user shares one
    /// ready.toml, while `installable` is per-copy (`bundle::resolve()`): an older aterm.app
    /// opened from a DMG would otherwise headline the installed copy's stage and offer an
    /// Install whose apply returns `NotApplicable`.
    fn without_inapplicable_stage(mut self) -> Self {
        if self.linux_host {
            self.staged = None;
            self.changelog.clear();
            return self;
        }
        if !self.installable.is_installable() {
            self.staged = None;
            self.changelog.clear();
            self.outcome.clear();
            self.checked_at = None;
        }
        self
    }

    /// Attach the apply lane's facts to a `from_status` fixture: the ledger reason behind the streak and whether the loop still intends to retry.
    #[cfg(test)]
    pub(crate) fn with_apply_lane(mut self, reason: &str, retry: ApplyRetry) -> Self {
        self.apply_failure = reason.to_string();
        self.apply_retry = retry;
        self.apply_failure_build = self.staged.as_ref().map_or(0, |(build, _)| *build);
        self.apply_attempts_for_stage = self.failing_applies;
        self
    }

    /// The apply lane's standing trouble for the staged build, in the form every surface renders.
    pub(crate) fn apply_trouble(&self) -> Option<ApplyTrouble> {
        if !self.enabled {
            return None;
        }
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

    /// The projection every native update surface paints and semantic introspection serializes.
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
            apply_trouble: (!self.checking).then(|| self.apply_trouble()).flatten(),
            staged_on_hold: self.on_hold(),
            installable: self.installable,
            dev_build: self.dev_build.is_some(),
            channel_unreadable: self.channel_unreadable && self.enabled && !self.checking,
            checked_at: self.checked_at,
            headline: self.headline(),
            detail,
            automatic_checks_note,
        }
    }

    /// The persistent failure is in the apply class (the staged build will not start), as
    /// opposed to an acquisition class (`manifest`, `pipeline`, `network`, `stage`).
    const fn apply_is_failing(&self) -> bool {
        // Not `failing_kind == "apply"`: that is the last class to fail, so a machine
        // whose apply lane escalated but whose latest failure was a network blip
        // would hide it, and a single apply failure under an escalated `pipeline`
        // streak would wrongly claim it. The apply streak itself is the statement.
        self.failing_applies >= aterm_update::PERSISTENT_AFTER
    }

    /// The Linux ladder's disk, enrollment and trouble arms, first match first: every
    /// headline that outranks this process's checker.
    fn linux_fact_headline(&self) -> Option<&'static str> {
        let linux = self.linux.as_ref();
        if self.installable.cannot_install() {
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

    /// The primary status headline: a short statement ("Update ready"); the version
    /// specifics live in [`Self::detail`].
    fn headline(&self) -> String {
        if self.checking {
            "Checking for updates\u{2026}".to_string()
        } else if self.installable == UpdateInstallability::Unobserved {
            // Nothing has read this copy yet (the startup or first-open reconcile is
            // in flight): it may be an installed copy or one that can't replace
            // itself, so the page claims neither — no "up to date", no "can't update
            // itself", no timing for the switches.
            UPDATE_UNOBSERVED_HEADLINE.to_string()
        } else if self.linux_host {
            if let Some(fact) = self.linux_fact_headline() {
                fact.to_string()
            } else if !self.automatic_checks {
                // The Linux updater reads `[update] enabled` as macOS's does (once per
                // process, gating only its background checker), so the idle arm speaks for
                // the checker this process runs. Every Linux arm above outranks it.
                "Automatic checks are off.".to_string()
            } else {
                "No update staged.".to_string()
            }
        } else if self.on_hold() {
            // Not "ready": no surface starts a build on hold (the band's record
            // says "is on hold" too). The count and the cause, when the ledger
            // still has them, ride the detail line below.
            "Update on hold".to_string()
        } else if self.staged.is_some() && self.enabled && self.apply_is_failing() {
            // Only the apply class earns this: the health notice points here and this line
            // must not contradict it. A `manifest`/`pipeline` streak is about acquiring the
            // next build; the stage in hand is verified and will install.
            "Update keeps failing to install.".to_string()
        } else if self.apply_trouble().is_some() {
            // The count and the cause ride the detail line.
            "Update didn\u{2019}t install.".to_string()
        } else if self.staged.is_some() {
            "Update ready".to_string()
        } else if let Some(quiet) = self.quiet_download() {
            // The automatic lane's download takes no band row, so this is where it is seen.
            format!("{}\u{2026}", quiet.title)
        } else if self.installable.cannot_install() {
            match &self.dev_build {
                Some(dev) => dev.headline(),
                None => "This copy of aterm can\u{2019}t update itself.".to_string(),
            }
        } else if !self.enabled {
            // No native updater on this platform (Linux answers in its own ladder above),
            // so the line does not call the updater macOS-only. The Settings switch is not
            // this: off, it stops only the background checker (the arm below).
            "aterm doesn\u{2019}t update itself on this platform.".to_string()
        } else if self.channel_unreadable {
            // Stranded: the channel cannot be read and the state records zero ledger
            // failures, so the `failing_persistent` arm below never catches it.
            "aterm can\u{2019}t check for updates on this machine.".to_string()
        } else if self.failing_persistent {
            // Never "up to date" while the ledger says otherwise. The cause, in words, is the
            // detail; the ledger's own sentence is `aterm ctl update status`'s and the log's.
            "Updates are failing on this machine.".to_string()
        } else if !self.automatic_checks {
            // Nothing in this process looks by itself, so "up to date" would describe a
            // check that is not running. Short: the medium workbench gives the hero line
            // ~320pt, and "Automatic update checks are off." ellipsized there.
            "Automatic checks are off.".to_string()
        } else {
            "You\u{2019}re up to date.".to_string()
        }
    }

    /// What `[update] enabled` means for this page while nothing is staged: this process
    /// read the key at launch, so a saved value that differs applies next launch; the
    /// sentence says which way and what still works (the switch stops only the background
    /// checker).
    fn automatic_checks_detail(&self) -> Option<String> {
        if !self.enabled || self.staged.is_some() {
            return None;
        }
        if self.linux_host && (self.linux.is_none() || self.linux_fact_headline().is_some()) {
            return None;
        }
        let manual = if self.linux_host {
            "`aterm update check`"
        } else {
            "Update to Latest Now"
        };
        match (self.automatic_checks, self.automatic_checks_saved) {
            (true, true) => None,
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
    /// offer, what is wrong and what happens next.
    fn detail(&self) -> Option<String> {
        if self.linux_host {
            let outcome = self.outcome.trim().trim_end_matches('.');
            let own_facts = self.linux_error || outcome.is_empty();
            let direction = if own_facts
                && self.installable.is_installable()
                && self
                    .linux
                    .as_ref()
                    .is_some_and(|status| status.staged_build.is_some())
            {
                "Run `aterm update apply` to install it."
            } else {
                ""
            };
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
                        // installed build is the trial's); mid-install or mid-rollback
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
            let detail = match self.automatic_checks_detail() {
                Some(checks) if detail.is_empty() => checks,
                Some(checks) => format!("{checks} {detail}"),
                None => detail,
            };
            return (!detail.is_empty()).then_some(detail);
        }
        if self.installable == UpdateInstallability::Unobserved {
            // The headline says the copy is being read; no detail claims anything
            // about it before that read lands.
            return None;
        }
        if self.installable.cannot_install() {
            use aterm_update::which_copy::InstallPosture;
            return match (&self.dev_build, self.install_posture) {
                (Some(dev), _) => Some(dev.detail()),
                (None, Some(InstallPosture::NotABundle)) => Some(NOT_A_BUNDLE_DETAIL.to_string()),
                (None, posture) => posture.and_then(InstallPosture::remedy).map(str::to_string),
            };
        }
        if let Some((_, v)) = self.staged.as_ref() {
            // `!self.checking` as in `projection()`: a check in flight owns the surface.
            if let Some(trouble) = self.apply_trouble().filter(|_| !self.checking) {
                let tail = if self.apply_is_failing() || !trouble.cause_is_named() {
                    LOG_POINTER
                } else {
                    ""
                };
                return Some(format!("Version {v} \u{00b7} {}{tail}", trouble.sentence()));
            }
            if self.enabled && !self.checking && self.apply_is_failing() {
                return Some(format!(
                    "Version {v} \u{00b7} every try to install it failed.{LOG_POINTER}"
                ));
            }
            if self.on_hold() {
                return Some(format!(
                    "Version {v} is on hold. Starting it again would roll it back; {}.",
                    crate::update_words::NEWER_RELEASE_REPLACES
                ));
            }
            if self.enabled && self.failing_persistent {
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
            return Some(if quiet.progress.is_empty() {
                "Checking the download.".to_string()
            } else {
                format!("{} downloaded.", quiet.progress)
            });
        }
        if self.channel_unreadable && !self.checking && self.enabled {
            let cause = self.outcome.trim();
            return Some(if cause.is_empty() {
                format!("The release channel can\u{2019}t be read.{LOG_POINTER}")
            } else {
                cause.to_string()
            });
        }
        if self.failing_persistent && !self.checking && self.enabled {
            // A build that predates the channel's keys (the one `manifest` sentence that
            // says "reinstall") can never check again: the one check failure a person must
            // act on, so its remedy is on the page. Every other class clears by itself.
            if self.failing_kind == "manifest" && self.outcome.contains("reinstall") {
                return Some(
                    "This copy of aterm is too old to check for updates. Reinstall aterm \
                     from the current release (drag it from the release DMG)."
                        .to_string(),
                );
            }
            let trouble = if self.failing_kind == "apply" {
                "The last updates didn\u{2019}t install".to_string()
            } else {
                crate::update_words::capitalize_first(check_trouble_words(&self.failing_kind))
            };
            return Some(format!("{trouble}; aterm keeps trying.{LOG_POINTER}"));
        }
        None
    }
}

/// Where a trouble line sends the reader for the rest: the page's own log link.
const LOG_POINTER: &str = " Details in Settings \u{25b8} Messages.";

/// A failing check class (`network` / `manifest` / `pipeline` / `stage`, the updater's
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
            installed_version: String::new(),
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
    const LINUX_HELD: &str = "aterm 0.5.15 is downloaded \u{2014} `aterm update apply` installs it \
         (\u{201c}Install updates automatically\u{201d} is off)";
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
        assert_eq!(detail, LINUX_HELD);
        assert!(!detail.contains("build 8"), "{detail}");
        assert!(projection.staged.is_none());
        assert_eq!(projection.linux.as_ref().unwrap().staged_build, Some(830));
        assert!(
            crate::native_settings::compact_update_detail_minimum(&projection)
                .contains("aterm update apply")
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
        assert_eq!(projection.detail.as_deref(), Some(LINUX_PENDING));
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
        assert!(
            detail.contains("0.5.15")
                && detail.contains("signature verification refused the new manifest")
                && detail.contains("`aterm update apply`"),
            "{detail}"
        );
        state.outcome = "Another aterm is updating this copy right now.".into();
        assert!(
            !state.projection().detail.unwrap().contains(".."),
            "one sentence ends once"
        );
        assert!(projection.linux_attention);
    }

    #[test]
    fn linux_update_status_unenrolled_copy_keeps_exact_reason_and_linux_advice() {
        let mut state = linux_state(None, 0);
        state.enabled = false;
        state.installable = UpdateInstallability::NotInstallable;
        state.outcome = "installed prefix is group writable before a private ancestor".into();
        let state = state.without_inapplicable_stage();
        let projection = state.projection();
        let detail = projection.detail.as_deref().unwrap();
        assert!(detail.contains("group writable"));
        assert!(!detail.contains("aterm update enable"), "{detail}");
        assert!(!detail.contains("Applications"));
        assert!(!detail.contains(".app"));
        assert_eq!(projection.outcome, state.outcome);
    }

    /// The Linux checker reads `[update] enabled` once per process
    /// (`aterm_update::automatic()`), as macOS's does, and a typed `aterm update check`
    /// ignores it: the idle headline follows the running value, the detail names a saved
    /// flip and the terminal lane, and every disk, enrollment or trouble fact outranks it.
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
        assert!(
            !on.detail.as_deref().unwrap().contains("Automatic checks"),
            "{:?}",
            on.detail
        );

        let off = page(idle.clone(), 0, false, false);
        let detail = off.detail.expect("the Linux detail");
        assert!(detail.contains("`aterm update check`"), "{detail}");
        assert!(
            detail.ends_with(on.detail.as_deref().unwrap()),
            "the Linux detail follows whole: {detail}"
        );
        assert!(!detail.contains("Update to Latest Now"), "{detail}");
        assert!(!detail.contains(".."), "one sentence ends once: {detail}");

        let starting = page(idle.clone(), 0, false, true);
        let starting = starting.detail.expect("the Linux detail");
        assert!(starting.contains("`aterm update check`"), "{starting}");
        assert_ne!(starting, detail, "a saved flip is its own detail");

        let stopping = page(idle.clone(), 0, true, false);
        assert!(
            stopping
                .detail
                .as_deref()
                .unwrap()
                .starts_with("Automatic checks stop next launch"),
            "{:?}",
            stopping.detail
        );

        let downloaded = page(linux_facts(), 0, false, false);
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
        assert!(
            !attention
                .detail
                .as_deref()
                .unwrap()
                .contains("Automatic checks"),
            "{:?}",
            attention.detail
        );
        let mut trial = idle.clone();
        trial.installed_build = 830;
        trial.trial_phase = Some("Installed".into());
        trial.trial_starts = 1;
        let mut installed = idle;
        installed.installed_build = 830;
        for (facts, headline, fact) in [
            (trial, "a pending trial", LINUX_PENDING),
            (installed, "a newer install", "aterm 0.5.15 is installed"),
        ] {
            for (running, saved) in [(false, false), (false, true), (true, false)] {
                let mut state = linux_state(Some(facts.clone()), 0);
                state.outcome = fact.into();
                let projection = state.with_automatic_checks(running, saved).projection();
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

    /// "Check for updates automatically" off stops only the background checker: the page
    /// never claims "You're up to date" over it and says a check still runs when asked; a
    /// staged build is still "Update ready", and real trouble still outranks the switch.
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
        assert_eq!(off.detail, None);
        assert!(
            off.automatic_checks_note.is_some(),
            "the page says what is off"
        );
        let on = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
        assert_eq!((on.detail, on.automatic_checks_note), (None, None));
        let staged = UpdateState::from_status(828, "0.5.14", Some(&staged_status()), false)
            .with_automatic_checks(false, false)
            .projection();
        assert!(staged.staged.is_some());
        st.channel_unreadable = true;
        st.outcome = "the channel is unreadable".to_string();
        let stranded = UpdateState::from_status(828, "0.5.14", Some(&st), false)
            .with_automatic_checks(false, false)
            .projection();
        assert_eq!(
            stranded.detail.as_deref(),
            Some("the channel is unreadable")
        );
        assert_eq!(stranded.automatic_checks_note, None);
    }

    /// The headline speaks for the checker this process runs, which read `[update]
    /// enabled` at launch: flipped off, it keeps running until quit, so the headline stays
    /// and the detail says checks stop next launch; flipped on over an off launch, nothing
    /// checks until the next one, so the headline stays "off" and the detail says so.
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
        let off = page(false, false)
            .automatic_checks_note
            .expect("off is named");
        let stopping = page(true, false)
            .automatic_checks_note
            .expect("the pending stop is named");
        let starting = page(false, true)
            .automatic_checks_note
            .expect("the pending start is named");
        assert!(
            off != stopping && off != starting && stopping != starting,
            "each state is its own sentence: {off:?} / {stopping:?} / {starting:?}"
        );
        assert_eq!(page(true, true).automatic_checks_note, None);
    }

    /// The stage ledger is HOME-keyed and shared by every copy of the app: a copy that
    /// cannot replace itself shows neither the installed copy's stage, its notes, nor its
    /// check verdict.
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

        st.installable = true;
        let ok = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
        assert_eq!(ok.staged, Some((830, "0.5.15".to_string())));
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
            projected.installable.cannot_install(),
            "a copy with no replaceable bundle must project that fact, not just headline it"
        );
        st.installable = true;
        assert!(
            UpdateState::from_status(828, "0.5.14", Some(&st), false)
                .projection()
                .installable
                .is_installable(),
            "an ordinary install must stay installable"
        );
    }

    /// A copy that cannot be replaced (the mounted DMG, a translocated download, a plain
    /// binary) never starts a check, so its ledger fields are pristine defaults: the page
    /// says it can't update itself, with the one cause's remedy, never "up to date".
    #[test]
    fn a_copy_that_cannot_replace_itself_says_so_instead_of_up_to_date() {
        let mut st = staged_status();
        st.staged_build = None;
        st.staged_version = None;
        st.staged_dmg_sha256 = None;
        st.changelog = None;
        st.installable = false;
        use aterm_update::which_copy::InstallPosture;
        let page = |posture: Option<InstallPosture>| {
            UpdateState::from_status(828, "0.5.14", Some(&st), false)
                .with_install_posture(posture)
                .projection()
        };
        let p = page(Some(InstallPosture::MountedImage));
        assert_eq!(p.detail.as_deref(), InstallPosture::MountedImage.remedy());
        assert_eq!(
            page(Some(InstallPosture::Translocated)).detail.as_deref(),
            InstallPosture::Translocated.remedy()
        );
        let binary = page(Some(InstallPosture::NotABundle)).detail;
        assert_eq!(binary.as_deref(), Some(super::NOT_A_BUNDLE_DETAIL));
        assert_eq!(page(None).detail, None);
    }

    /// A dev-marked copy cannot be replaced either, but on purpose: the page says it is a
    /// dev build, how far behind the newest release it is once the dev channel has read
    /// that, and what moves it, never "move it to Applications".
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
        let detail = unread.detail.expect("detail");
        assert!(
            detail.starts_with("The updater leaves a dev build alone"),
            "{detail}"
        );
        let behind = page(Some(DevLag::Behind {
            latest: "v0.93.0".into(),
            releases: Some(2),
        }));
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
        assert!(
            current
                .detail
                .as_deref()
                .is_some_and(|d| d.starts_with("It is at aterm v0.91.0, the newest release. ")),
            "{:?}",
            current.detail
        );
        assert!(behind.dev_build);
        let plain = UpdateState::from_status(828, "0.91.0", Some(&st), false)
            .with_install_posture(Some(aterm_update::which_copy::InstallPosture::MountedImage))
            .projection();
        assert!(!plain.dev_build);
        assert!(
            plain
                .detail
                .is_some_and(|d| d.contains("Drag aterm to Applications"))
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

        let mut acquiring = staged_status();
        acquiring.failing_checks = 3;
        acquiring.failing_kind = "manifest".to_string();
        acquiring.failing_persistent = true;
        let s = UpdateState::from_status(828, "0.5.14", Some(&acquiring), false);
        let p = s.projection();
        assert!(p.failing_persistent && !p.apply_is_failing);
        let detail = p.detail.expect("detail");
        assert!(
            detail.contains("0.5.15") && detail.contains("Messages"),
            "the stage in hand is still ready; the log has the rest: {detail}"
        );
    }

    /// Deferrals and blocks record refusals and advance no streak, and `last_apply_error`
    /// is not expiry-bound like the streak: a reason with no attempts behind it is
    /// reachable and reads as a plain ready stage.
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
        let detail = p.detail.expect("detail");
        assert!(detail.contains("0.5.15"), "{detail}");
        for alarming in ["tried", "failed", "did not", "retry"] {
            assert!(
                !detail.contains(alarming),
                "a stage that has simply not been attempted must not read as a \
                 failure ({alarming:?} in {detail:?})"
            );
        }
    }

    /// An installed-bundle activation (a newer bundle already at this app's path, staged
    /// under its activation digest) is not called downloaded; a real download stage still
    /// is (the negative control).
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
            snapshot.installable = UpdateInstallability::Installable;
            snapshot.staged = Some(StagedUpdate {
                build: 830,
                version: "0.5.15".to_string(),
                commit: Some(commit.to_string()),
                dmg_sha256,
                changelog: None,
                generation: 1,
            });
            let p = UpdateState::from_service(&snapshot, false, ApplyRetry::Scheduled).projection();
            p.detail
        };

        let activated = detail_for(installed_activation_digest(830, commit)).expect("detail");
        assert!(!activated.contains("downloaded"), "{activated}");
        let downloaded = detail_for("ab".repeat(32)).expect("detail");
        assert!(downloaded.contains("downloaded"), "{downloaded}");
    }

    /// Same failure, count and cause: only whether a retry is scheduled differs, and the
    /// words differ with it.
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

    /// A current installed-source block (an ad-hoc-signed `/Applications/aterm.app`
    /// refused as the rollback source) names the refused copy and its remedy, and never
    /// promises an automatic retry.
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
                ApplyRetry::NeedsPerson(
                    crate::update_apply_trouble::PersonRemedy::InstallSignedRelease,
                ),
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

    /// A healthy page carries no detail of its own: the surface paints when the last
    /// check completed, relative to now, never the updater's decision sentence.
    #[test]
    fn a_healthy_page_says_when_it_last_checked() {
        let mut st = staged_status();
        st.staged_build = None;
        st.staged_version = None;
        st.outcome = "up to date (latest release build 828) \u{b7} checks every 30 min".to_string();
        st.updated_at = "2026-09-23T12:00:00Z".to_string();
        let p = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
        assert_eq!(p.detail, None, "nothing to add beyond when it last checked");
        let checked = aterm_update_core::pkg_check::rfc3339_to_unix(&st.updated_at).unwrap();
        assert_eq!(p.checked_at, Some(checked));
        assert!(
            p.checked_line(checked + 12 * 60 + 5)
                .is_some_and(|line| line.contains(&ago_words(checked, checked + 12 * 60 + 5)))
        );
        st.updated_at = String::new();
        let p = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
        assert_eq!(p.checked_line(checked), None);
    }

    /// A persistent check failure is said in words, with where the rest is: the class
    /// the ledger names, that aterm keeps trying, and Settings ▸ Messages — never the
    /// ledger's "FAILING (3 consecutive checks since …)" sentence.
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
        let detail = p.detail.expect("detail");
        assert!(
            detail.contains("Messages") && !detail.contains("FAILING") && !detail.contains("curl"),
            "words, and where the rest is: {detail}"
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
        st.failing_kind = "apply".to_string();
        st.failing_applies = aterm_update::PERSISTENT_AFTER;
        st.outcome = "staged build did not apply: this install cannot update itself: \
                      reinstall the signed release"
            .to_string();
        let p = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
        let detail = p.detail.expect("detail");
        assert!(
            !detail.starts_with("This copy of aterm is too old") && detail.contains("Messages"),
            "{detail}"
        );
    }

    #[test]
    fn snapshot_renders_staged_update_notes() {
        let st = staged_status();
        let s = UpdateState::from_status(828, "0.5.14", Some(&st), false);
        assert!(s.staged.is_some());
        assert!(s.changelog.iter().any(|l| l.contains("DSU: hot-swap")));
        assert!(
            !s.changelog
                .iter()
                .any(|l| l.contains('#') || l.contains("**"))
        );
        assert!(s.detail().unwrap().contains("0.5.15"));
    }
}
