// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Software update model ([`UpdateState`], [`aterm_update::status`]).

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
    /// The stage is an installed-bundle activation (`StagedUpdate::is_installed_activation`).
    staged_activation: bool,
    /// The staged build's "what changed" notes, rendered from Markdown to clean lines.
    changelog: Vec<String>,
    /// Whether the native updater runs here.
    enabled: bool,
    /// "Check for updates automatically".
    automatic_checks: bool,
    /// The same key as saved, which the next launch reads ([`Self::automatic_checks`]).
    automatic_checks_saved: bool,
    /// This process's launch read of the key has no answer yet, and its checks start once it
    /// does (`aterm_update::automatic_kept`).
    automatic_checks_waiting: bool,
    /// The last updater outcome string (from the health ledger).
    outcome: String,
    /// The health ledger says this machine's updates are failing persistently.
    failing_persistent: bool,
    /// See [`crate::native_updater_service::UpdaterSnapshot::check_unanswered_since`].
    check_unanswered: bool,
    /// The most recent failing class (`apply`, `pipeline`, …).
    failing_kind: String,
    /// Consecutive apply failures.
    failing_applies: u32,
    /// Why those applies failed.
    apply_failure: String,
    /// Which artifact [`Self::apply_failure`] is about, and how many consecutive
    /// attempts on that artifact are behind it.
    apply_failure_build: u64,
    /// Consecutive failed attempts on the [`Self::apply_failure_build`] artifact.
    apply_attempts_for_stage: u32,
    /// Whether the automatic lane retries this artifact by itself or needs a person.
    apply_retry: ApplyRetry,
    /// The staged build is on hold.
    staged_on_hold: bool,
    /// Whether this launch has a bundle the updater could replace at all.
    installable: UpdateInstallability,
    /// The stranded verdict: checks complete but the release channel cannot be read.
    channel_unreadable: bool,
    /// A manual "Check for Updates" is running off-thread (shows "Checking…").
    checking: bool,
    /// When the last check completed ([`UpdateProjection::checked_line`]).
    checked_at: Option<i64>,
    /// The automatic lane's download in flight, which posts no live row: this page says it
    /// ([`Self::quiet_download`]).
    downloading: Option<QuietDownload>,
    /// A dev-marked copy ([`Self::with_dev_build`]).
    dev_build: Option<DevBuildPage>,
    /// Where this copy runs from.
    install_posture: Option<aterm_update::which_copy::InstallPosture>,
}

/// Detail under "This copy of aterm can't update itself." for a non-bundle copy.
pub(crate) const NOT_A_BUNDLE_DETAIL: &str =
    "It isn\u{2019}t an installed aterm.app. Install aterm from alab.systems to get updates.";

/// A dev-marked copy as the page says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DevBuildPage {
    pub(crate) standing: Option<aterm_update::dev_channel::DevLag>,
}

impl DevBuildPage {
    /// The headline: how far behind, when that is known and true.
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

    /// The detail: where it stands.
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

/// A download (or its check) nobody is waiting on — the automatic lane's — as the page says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QuietDownload {
    /// The version, for the record a failure writes.
    pub(crate) version: String,
    pub(crate) title: String,
    pub(crate) progress: String,
}

/// Owned, structured read projection shared with the native Settings `/updates` route.
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
    /// "Check for updates automatically" as this process runs it.
    pub(crate) automatic_checks: bool,
    pub(crate) outcome: String,
    pub(crate) checking: bool,
    /// The ledger's persistent-failure verdict.
    pub(crate) failing_persistent: bool,
    /// Persistent failure in the apply class.
    pub(crate) apply_is_failing: bool,
    /// The apply lane's standing trouble for the staged build.
    pub(crate) apply_trouble: Option<ApplyTrouble>,
    /// The staged build is on hold (`App::latched_activation_trial_spent`).
    pub(crate) staged_on_hold: bool,
    /// Whether a replacement could be installed here at all.
    pub(crate) installable: UpdateInstallability,
    /// This copy is dev-marked (`ship dev-app`).
    pub(crate) dev_build: bool,
    /// The stranded verdict (see [`UpdateState`]).
    pub(crate) channel_unreadable: bool,
    /// When the last check completed (Unix seconds), `None` before any has.
    pub(crate) checked_at: Option<i64>,
    /// The headline says a check stopped answering: nothing the ladder knows outranks it
    /// ([`crate::native_updater_service::UpdaterSnapshot::check_unanswered_since`]).
    pub(crate) check_unanswered: bool,
    pub(crate) headline: String,
    pub(crate) detail: Option<String>,
    /// Meaning of the automatic-checks switch ([`UpdateState::automatic_checks_detail`]).
    pub(crate) automatic_checks_note: Option<String>,
}

impl UpdateProjection {
    /// `Checked 12 min ago` — when the last check completed.
    pub(crate) fn checked_line(&self, now: i64) -> Option<String> {
        self.checked_at
            .map(|at| format!("Checked {}", ago_words(at, now)))
    }
}

/// How long before `now` the instant `at` was, as a person says it.
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
    /// Project the process-owned updater reducer into UI state.
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
            automatic_checks_waiting: false,
            outcome: snapshot.outcome.clone(),
            failing_persistent: snapshot.failing_persistent,
            check_unanswered: snapshot.check_unanswered_since.is_some(),
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
            automatic_checks_waiting: false,
            outcome: status.map(|s| s.outcome.clone()).unwrap_or_default(),
            failing_persistent: status.is_some_and(|s| s.enabled && s.is_failing_persistently()),
            check_unanswered: false,
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

    /// "Check for updates automatically" as this process runs it.
    pub(crate) const fn with_automatic_checks(mut self, running: bool, saved: bool) -> Self {
        self.automatic_checks = running;
        self.automatic_checks_saved = saved;
        self
    }

    /// This process's launch read of "Check for updates automatically" has no answer yet.
    pub(crate) const fn with_automatic_checks_waiting(mut self, waiting: bool) -> Self {
        self.automatic_checks_waiting = waiting;
        self
    }

    /// The automatic lane's download in flight (`App::update_quiet`).
    pub(crate) fn with_download(mut self, downloading: Option<QuietDownload>) -> Self {
        self.downloading = downloading;
        self
    }

    /// This copy is dev-marked (`App::dev_build`).
    pub(crate) fn with_dev_build(mut self, dev_build: Option<DevBuildPage>) -> Self {
        self.dev_build = dev_build;
        self
    }

    /// Where this copy runs from (`App::install_posture`).
    pub(crate) const fn with_install_posture(
        mut self,
        posture: Option<aterm_update::which_copy::InstallPosture>,
    ) -> Self {
        self.install_posture = posture;
        self
    }

    /// The staged build is on hold (`App::latched_activation_trial_spent`).
    pub(crate) const fn with_staged_on_hold(mut self, on_hold: bool) -> Self {
        self.staged_on_hold = on_hold;
        self
    }

    /// A build on offer that is on hold ([`Self::staged_on_hold`]).
    const fn on_hold(&self) -> bool {
        self.enabled && self.staged.is_some() && self.staged_on_hold
    }

    /// The quiet download, where the page may say it.
    fn quiet_download(&self) -> Option<&QuietDownload> {
        self.downloading.as_ref().filter(|_| {
            !self.linux_host
                && self.enabled
                && self.installable.is_installable()
                && self.staged.is_none()
                && !self.checking
        })
    }

    /// A stage this copy cannot apply is not a stage for this copy.
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

    /// A check gave up with no answer, on a `from_status` fixture.
    #[cfg(test)]
    pub(crate) const fn with_check_unanswered(mut self) -> Self {
        self.check_unanswered = true;
        self
    }

    /// Attach the apply lane's facts to a `from_status` fixture.
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
        let headline = self.headline();
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
            outcome: if self.linux_outcome_predates_silence() {
                String::new()
            } else {
                self.outcome.clone()
            },
            checking: self.checking,
            failing_persistent: self.failing_persistent && self.enabled && !self.checking,
            apply_is_failing: self.apply_is_failing() && self.enabled && !self.checking,
            apply_trouble: (!self.checking).then(|| self.apply_trouble()).flatten(),
            staged_on_hold: self.on_hold(),
            installable: self.installable,
            dev_build: self.dev_build.is_some(),
            channel_unreadable: self.channel_unreadable && self.enabled && !self.checking,
            checked_at: self.checked_at,
            check_unanswered: headline == crate::native_updater_service::CHECK_DID_NOT_ANSWER,
            headline,
            detail,
            automatic_checks_note,
        }
    }

    /// Only "Up to date — …" speaks for the channel on Linux, so it says nothing while a check has
    /// stopped answering. The ledger's other words stay: a stage rejected on this read, a rollback's,
    /// or an error's cause.
    fn linux_outcome_predates_silence(&self) -> bool {
        self.linux_host
            && self.check_unanswered
            && !self.checking
            && self.outcome.trim_start().starts_with("Up to date")
    }

    /// The persistent failure is in the apply class.
    const fn apply_is_failing(&self) -> bool {
        self.failing_applies >= aterm_update::PERSISTENT_AFTER
    }

    /// The Linux ladder's disk, enrollment and trouble arms, first match first.
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
            UPDATE_UNOBSERVED_HEADLINE.to_string()
        } else if self.linux_host {
            if let Some(fact) = self.linux_fact_headline() {
                fact.to_string()
            } else if self.check_unanswered {
                crate::native_updater_service::CHECK_DID_NOT_ANSWER.to_string()
            } else if !self.automatic_checks {
                "Automatic checks are off.".to_string()
            } else {
                "No update staged.".to_string()
            }
        } else if self.on_hold() {
            "Update on hold".to_string()
        } else if self.staged.is_some() && self.enabled && self.apply_is_failing() {
            "Update keeps failing to install.".to_string()
        } else if self.apply_trouble().is_some() {
            "Update didn\u{2019}t install.".to_string()
        } else if self.staged.is_some() {
            "Update ready".to_string()
        } else if let Some(quiet) = self.quiet_download() {
            format!("{}\u{2026}", quiet.title)
        } else if self.installable.cannot_install() {
            match &self.dev_build {
                Some(dev) => dev.headline(),
                None => "This copy of aterm can\u{2019}t update itself.".to_string(),
            }
        } else if !self.enabled {
            "aterm doesn\u{2019}t update itself on this platform.".to_string()
        } else if self.channel_unreadable {
            "aterm can\u{2019}t check for updates on this machine.".to_string()
        } else if self.failing_persistent {
            "Updates are failing on this machine.".to_string()
        } else if self.check_unanswered {
            crate::native_updater_service::CHECK_DID_NOT_ANSWER.to_string()
        } else if !self.automatic_checks {
            "Automatic checks are off.".to_string()
        } else {
            "You\u{2019}re up to date.".to_string()
        }
    }

    /// What `[update] enabled` means for this page while nothing is staged.
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
            (false, true) if self.automatic_checks_waiting => Some(format!(
                "Automatic checks start once aterm can read its settings \u{2014} until then, \
                 {manual} still checks now."
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

    /// The secondary detail line under the headline, in plain words.
    fn detail(&self) -> Option<String> {
        if self.linux_host {
            let outcome = if self.linux_outcome_predates_silence() {
                ""
            } else {
                self.outcome.trim().trim_end_matches('.')
            };
            // Under a fact's headline this is the only place left to say it.
            let unanswered = if self.check_unanswered
                && !self.checking
                && self.linux_fact_headline().is_some()
            {
                crate::native_updater_service::CHECK_DID_NOT_ANSWER.trim_end_matches('.')
            } else {
                ""
            };
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
            let detail = [unanswered, identity.as_str(), outcome, direction]
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

/// Where a trouble line sends the reader for the rest.
const LOG_POINTER: &str = " Details in Settings \u{25b8} Messages.";

/// A failing check class in a person's words.
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
#[path = "update_screen_tests.rs"]
mod tests;
