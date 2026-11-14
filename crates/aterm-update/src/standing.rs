// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where this copy's updater stands: the one decision `aterm update status` prints its line by
//! and a fleet survey says its standing by ([`UpdateStatus::arm`]), so the two never disagree.

use aterm_types::update_standing::{UpdateFact, UpdateStanding, build_name, why_word};

use crate::{ApplyLaneReport, HoldView, LinuxUpdateStatus, UpdateStatus};

/// The arm of the status decision a status falls in, with what that arm names, first match
/// first ([`UpdateStatus::arm`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusArm<'a> {
    /// The updater does not run for this copy.
    Off,
    /// A Linux copy downloaded a build newer than the running one; `aterm update apply`
    /// installs it.
    LinuxDownloaded {
        /// The downloaded build.
        build: u64,
        /// Its version, where the stage recorded one.
        version: Option<&'a str>,
    },
    /// A Linux copy installed a newer build and waits for an aterm to start from it.
    Installed(&'a LinuxUpdateStatus),
    /// This copy cannot update itself (a dev build, a copy outside an installed app).
    Cannot,
    /// A live hold on a newer build: it outranks a download of the build it holds.
    Held(&'a HoldView),
    /// The staged build's exact bytes, which a window here saw fail to start: while that holds,
    /// no start without a window installs them ([`UpdateStatus::start_failure`]).
    StartFailed(&'a HoldView),
    /// A newer build is downloaded and the apply lane's standing refusal stands.
    Refused {
        /// The downloaded build.
        build: u64,
        /// Its version, where the stage recorded one.
        version: Option<&'a str>,
        /// The refusal, trimmed and not empty.
        refusal: &'a str,
    },
    /// A newer build is downloaded and failed to install ([`staged_tries`]).
    NotInstalling {
        /// The downloaded build.
        build: u64,
        /// Its version, where the stage recorded one.
        version: Option<&'a str>,
        /// The failed installs of that build, at least one.
        tries: u32,
    },
    /// A newer build is downloaded, not installed yet.
    Downloaded {
        /// The downloaded build.
        build: u64,
        /// Its version, where the stage recorded one.
        version: Option<&'a str>,
    },
    /// The last completed check could not read the release channel.
    Unreadable,
    /// A build that predates the channel's keys, whose ledger prescribes a reinstall.
    TooOld,
    /// A stranding failure of `class` persists.
    Failing {
        /// The failure's class ([`UpdateStatus::failing_class`]).
        class: &'a str,
    },
    /// The last check did not finish.
    CheckFailed {
        /// The failure's class ([`UpdateStatus::failing_class`]).
        class: &'a str,
    },
    /// A lapsed hold: the build it held is tried again.
    Retrying(&'a HoldView),
    /// A newer release is offered and not downloaded yet.
    Behind,
    /// Up to date as of the check that completed `at` (Unix seconds).
    Current {
        /// When the last check completed.
        at: i64,
    },
    /// No check has completed.
    Unchecked,
}

impl StatusArm<'_> {
    /// The arm's word on the fleet's wire.
    #[must_use]
    pub const fn standing(&self) -> UpdateStanding {
        match self {
            Self::Off => UpdateStanding::Off,
            Self::LinuxDownloaded { .. } | Self::Downloaded { .. } => UpdateStanding::Downloaded,
            Self::Installed(_) => UpdateStanding::Installed,
            Self::Cannot => UpdateStanding::Cannot,
            Self::Held(_) | Self::StartFailed(_) => UpdateStanding::Held,
            Self::Refused { .. } => UpdateStanding::Refused,
            Self::NotInstalling { .. } => UpdateStanding::NotInstalling,
            Self::Unreadable => UpdateStanding::Unreadable,
            Self::TooOld => UpdateStanding::TooOld,
            Self::Failing { .. } => UpdateStanding::Failing,
            Self::CheckFailed { .. } => UpdateStanding::CheckFailed,
            Self::Retrying(_) => UpdateStanding::Retrying,
            Self::Behind => UpdateStanding::Behind,
            Self::Current { .. } => UpdateStanding::Current,
            Self::Unchecked => UpdateStanding::Unchecked,
        }
    }
}

/// How many times the staged build `staged` failed to install, as `lane` counts them: its
/// failures where they were trying to reach that very build, else none, so a superseded build's
/// tries never mark a fresh download failed.
#[must_use]
pub fn staged_tries(staged: u64, lane: Option<&ApplyLaneReport>) -> u32 {
    lane.filter(|lane| staged > 0 && lane.last_failure_target_build == staged)
        .map_or(0, |lane| lane.failures_for_target)
}

impl UpdateStatus {
    /// Whether a Linux copy installed a build and waits for an aterm to start from it: its trial
    /// is `Installed` and not proven healthy yet.
    #[must_use]
    pub fn linux_install_waits_for_a_start(&self) -> bool {
        self.linux.as_ref().is_some_and(|native| {
            native.trial_phase.as_deref() == Some("Installed") && !native.trial_healthy
        })
    }

    /// The class of the failure the status stands on: the standing check streak's, else the
    /// most recent failure's.
    #[must_use]
    pub fn failing_class(&self) -> &str {
        if self.failing_checks_kind.is_empty() {
            self.failing_kind.as_str()
        } else {
            self.failing_checks_kind.as_str()
        }
    }

    /// Where the updater of the running `build` stands, with the apply lane's `lane` report: the
    /// first arm that matches, in this order. Off; a Linux build downloaded; a Linux install
    /// waiting for a start; a copy that cannot update itself; a live hold; a live window-start
    /// failure of the newer staged build's bytes; a newer build staged (its refusal, then its
    /// failed installs, then the download itself); an unreadable channel; a persistent failure (a
    /// manifest one that prescribes a reinstall is too old); a failed check; a lapsed hold; a
    /// newer offer; a completed check; none.
    #[must_use]
    pub fn arm<'a>(&'a self, build: u64, lane: Option<&'a ApplyLaneReport>) -> StatusArm<'a> {
        if !self.enabled {
            return StatusArm::Off;
        }
        if let Some((staged, version)) = self
            .linux
            .as_ref()
            .and_then(|native| {
                native
                    .staged_build
                    .map(|b| (b, native.staged_version.as_deref()))
            })
            .filter(|(b, _)| *b > build)
        {
            return StatusArm::LinuxDownloaded {
                build: staged,
                version,
            };
        }
        if let Some(native) = self
            .linux
            .as_ref()
            .filter(|_| self.linux_install_waits_for_a_start())
        {
            return StatusArm::Installed(native);
        }
        if !self.installable {
            return StatusArm::Cannot;
        }
        if let Some(hold) = self.hold.as_ref().filter(|hold| hold.live) {
            return StatusArm::Held(hold);
        }
        if let Some(failed) = self.start_failure.as_ref().filter(|failed| {
            failed.live && failed.build > build && self.staged_build == Some(failed.build)
        }) {
            return StatusArm::StartFailed(failed);
        }
        if let Some(staged) = self.staged_build.filter(|staged| *staged > build) {
            let version = self.staged_version.as_deref();
            if let Some(refusal) = lane
                .map(|lane| lane.last_refusal.trim())
                .filter(|refusal| !refusal.is_empty())
            {
                return StatusArm::Refused {
                    build: staged,
                    version,
                    refusal,
                };
            }
            let tries = staged_tries(staged, lane);
            return if tries > 0 {
                StatusArm::NotInstalling {
                    build: staged,
                    version,
                    tries,
                }
            } else {
                StatusArm::Downloaded {
                    build: staged,
                    version,
                }
            };
        }
        if self.channel_unreadable {
            return StatusArm::Unreadable;
        }
        let class = self.failing_class();
        if self.failing_persistent {
            // The one failure a person has to act on: a build that predates the channel's keys,
            // whose ledger sentence prescribes the reinstall.
            if class == "manifest" && self.outcome.contains("reinstall") {
                return StatusArm::TooOld;
            }
            return StatusArm::Failing { class };
        }
        if self.failing_checks > 0 {
            return StatusArm::CheckFailed { class };
        }
        if let Some(hold) = &self.hold {
            return StatusArm::Retrying(hold);
        }
        if self.behind {
            return StatusArm::Behind;
        }
        aterm_update_core::pkg_check::rfc3339_to_unix(&self.updated_at)
            .map_or(StatusArm::Unchecked, |at| StatusArm::Current { at })
    }

    /// [`Self::arm`]'s word.
    #[must_use]
    pub fn standing(&self, build: u64, lane: Option<&ApplyLaneReport>) -> UpdateStanding {
        self.arm(build, lane).standing()
    }

    /// What the updater of the running `build` tells its fleet ([`UpdateFact`]): [`Self::arm`]'s
    /// word, when its last check completed, whether automatic checks are on (`checks`, `None`
    /// where no read of the setting answered), and the structured facts its arm names (the build
    /// it names, by [`build_name`]; a live hold's or window-start failure's lapse; its cause or a
    /// failure's class, as one of the closed [`aterm_types::update_standing::WHY_WORDS`]; the
    /// tries). None of the status's free text is read: not its outcome, nor a hold's, a refusal's
    /// or a failure's reason, nor its changelog, commit or digest.
    #[must_use]
    pub fn fleet_fact(
        &self,
        build: u64,
        lane: Option<&ApplyLaneReport>,
        checks: Option<bool>,
    ) -> UpdateFact {
        let arm = self.arm(build, lane);
        let mut fact = UpdateFact::of(arm.standing());
        fact.checked_at = aterm_update_core::pkg_check::rfc3339_to_unix(&self.updated_at)
            .and_then(|at| u64::try_from(at).ok());
        fact.checks = checks;
        match arm {
            StatusArm::LinuxDownloaded { build, version }
            | StatusArm::Downloaded { build, version }
            | StatusArm::Refused { build, version, .. } => {
                fact.to = Some(build_name(build, version));
            }
            StatusArm::NotInstalling {
                build,
                version,
                tries,
            } => {
                fact.to = Some(build_name(build, version));
                fact.tries = Some(tries);
            }
            StatusArm::Installed(native) => {
                fact.to = Some(build_name(
                    native.installed_build,
                    Some(native.installed_version.as_str()),
                ));
            }
            StatusArm::Held(hold) | StatusArm::StartFailed(hold) => {
                fact.to = Some(build_name(hold.build, Some(&hold.version)));
                fact.until = Some(hold.until);
                fact.why = why_word(hold.cause.word());
                fact.tries = Some(u32::from(hold.tries));
            }
            StatusArm::Retrying(hold) => {
                fact.to = Some(build_name(hold.build, Some(&hold.version)));
                fact.why = why_word(hold.cause.word());
            }
            StatusArm::Failing { class } | StatusArm::CheckFailed { class } => {
                fact.why = why_word(class);
            }
            StatusArm::Behind => {
                fact.to = self
                    .offered
                    .filter(|offered| *offered > build)
                    .map(|offered| build_name(offered, None));
            }
            StatusArm::Off
            | StatusArm::Cannot
            | StatusArm::Unreadable
            | StatusArm::TooOld
            | StatusArm::Current { .. }
            | StatusArm::Unchecked => {}
        }
        fact
    }
}

/// What this copy's updater tells its fleet now, from a process that runs build `running`
/// ([`UpdateStatus::fleet_fact`]): the standing `aterm update status` on this machine gives. So it
/// is decided for the build that command runs, the bundle installed in this copy's place where
/// there is one ([`crate::installed_bundle_build`]), which an update may have moved past a window
/// still on `running`, and from the reads that command makes, each a machine's ledger or file
/// that every process reads alike: the status, the apply lane's report, and the automatic-checks
/// setting as saved ([`crate::automatic_as_saved`]). No network, no lock. Where the status cannot
/// be read, `unread` on a system whose updater keeps one, `off` on any other but Windows. Windows
/// keeps no status: there it is the standing against the build staged there, which that command
/// prints its lines by ([`crate::windows::Here::fleet_fact`]).
#[must_use]
pub fn fleet_fact(running: u64) -> UpdateFact {
    #[cfg(windows)]
    {
        crate::windows::fleet_fact(running)
    }
    #[cfg(not(windows))]
    {
        fleet_fact_of(
            running,
            crate::installed_bundle_build(),
            crate::status,
            crate::apply_lane_report,
            crate::automatic_as_saved,
        )
    }
}

/// [`fleet_fact`] over its reads: the build installed in this copy's place (`installed`), and the
/// readers of the status, the apply lane's report and the automatic-checks setting.
#[cfg(any(not(windows), test))]
fn fleet_fact_of(
    running: u64,
    installed: Option<u64>,
    status: impl FnOnce(u64) -> Option<UpdateStatus>,
    lane: impl FnOnce(u64) -> Option<ApplyLaneReport>,
    checks: impl FnOnce() -> Option<bool>,
) -> UpdateFact {
    let build = installed.unwrap_or(running);
    match status(build) {
        Some(status) => status.fleet_fact(build, lane(build).as_ref(), checks()),
        None if cfg!(any(target_os = "macos", target_os = "linux")) => {
            UpdateFact::of(UpdateStanding::Unread)
        }
        None => UpdateFact::of(UpdateStanding::Off),
    }
}

#[cfg(test)]
#[path = "standing_tests.rs"]
mod tests;
