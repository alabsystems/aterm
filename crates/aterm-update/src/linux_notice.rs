// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! What the Linux background check tells the window about update health ([`crate::HealthNotify`]): a check lane that keeps failing, its later reasons, and its recovery, never an ordinary result.

/// The class a Linux check failure is announced under: the check half.
const CLASS: &str = "manifest";

/// How many passes in a row may find another aterm holding the copy's update lock
/// before it is announced ([`Record::Busy`]): two hours at the loop's one-minute
/// pass, longer than any bounded download of a release holds it.
const BUSY_ANNOUNCED_AFTER: u32 = 120;

/// The loop's pause between passes, in seconds (`crate::linux`'s loop sleeps it).
pub(crate) const PASS_SECS: u64 = 60;

/// How many passes in a row an installed build may wait for its first start before the
/// wait is said ([`InstallWatch`]): the hour after which the macOS lane says a newer build
/// that is not running ([`crate::PENDING_UPDATE_OVERDUE_SECS`]), at one pass a minute.
const START_WAIT_ANNOUNCED_AFTER: u64 = crate::PENDING_UPDATE_OVERDUE_SECS / PASS_SECS;

/// The install half's class, which its recovery names: the window says a crash loop's end
/// under its title, "Couldn't install updates" ([`crate::INSTALL_CRASH_LOOP_TITLE`]).
const INSTALL_CLASS: &str = "apply";

/// What one pass of the Linux check loop saw ([`Announcer::tick`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pass<'a> {
    /// The recorded outcome: a failure's reason while `failing_checks` is non-zero.
    pub(crate) outcome: &'a str,
    /// Consecutive failed checks on record.
    pub(crate) failing_checks: u32,
    /// When the last check was recorded (RFC 3339, empty for none).
    pub(crate) updated_at: &'a str,
    /// What the record made of this pass.
    pub(crate) record: Record,
    /// Whether this pass took the copy's update lock, which shows no other aterm held it then.
    pub(crate) took_lock: bool,
    /// What the copy's record says of its installed build ([`InstallWatch`]); `None` when the record could not be read, which judges nothing.
    pub(crate) install: Option<Install<'a>>,
    /// The words of each crash loop's end no window was told of yet, oldest first, which this pass took off the copy's record (`crate::linux`'s `take_unsaid_trial_ends`): once for the copy, whenever this process started, and only by a loop with a window to tell.
    pub(crate) trial_ends: &'a [String],
}

impl<'a> Pass<'a> {
    /// The pass the loop makes of a check's `status`, `record` and `took_lock`, and the crash loops' ends it took (`trial_ends`): built wherever `crate::linux` is, its one caller.
    #[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
    pub(crate) fn of(
        status: &'a crate::UpdateStatus,
        record: Record,
        took_lock: bool,
        trial_ends: &'a [String],
    ) -> Self {
        Self {
            outcome: &status.outcome,
            failing_checks: status.failing_checks,
            updated_at: &status.updated_at,
            record,
            took_lock,
            install: status.linux.as_ref().map(Install::of),
            trial_ends,
        }
    }
}

/// What one pass read of the copy's installed build ([`Pass::install`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Install<'a> {
    /// The installed version.
    pub(crate) version: &'a str,
    /// Whether it waits for a start to finish its trial (installed, not yet healthy: update checks wait with it).
    pub(crate) awaiting_start: bool,
    /// The starts of it counted on its trial; none finished while it still waits.
    pub(crate) starts: u32,
    /// Whether a start finished the installed build's trial (it is healthy).
    pub(crate) started: bool,
    /// Whether the copy refuses a newer build it rolled back from.
    pub(crate) rolled_back: bool,
}

impl<'a> Install<'a> {
    #[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
    fn of(linux: &'a crate::LinuxUpdateStatus) -> Self {
        let trial_installed = linux.trial_phase.as_deref() == Some("Installed");
        Self {
            version: &linux.installed_version,
            awaiting_start: trial_installed && !linux.trial_healthy,
            starts: linux.trial_starts,
            started: linux.trial_phase.is_some() && linux.trial_healthy,
            rolled_back: linux.refused_newer,
        }
    }
}

/// Whether the record took a pass's result ([`Pass::record`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Record {
    /// The record holds this pass's result: a success, a recorded failure, or a copy whose updates are off (the remedy's sentence).
    Kept,
    /// This pass failed and the record did not take the failure: `failing_checks` is the count from before it.
    Lost,
    /// Another aterm held the copy's update lock (or changed its enrollment under this pass): nothing was checked, and nothing is judged.
    Busy,
}

/// The loop's memory of what it told the window.
#[derive(Debug)]
pub(crate) struct Announcer {
    /// The clock at the loop's start (RFC 3339): a streak last recorded before it
    /// is not announced.
    started: String,
    /// The body the window shows, said last; `None` = nothing, or a recovery
    /// answered it.
    shown: Option<String>,
    /// The check half's failure announced and not yet answered, kept while the
    /// lock's notice is shown over it ([`BUSY_BODY`]).
    failure: Option<Failure>,
    /// The lock's notice is what the window shows: the lock coming free answers it,
    /// and only it.
    busy_shown: bool,
    /// Consecutive passes whose failure the record did not take.
    unrecorded: u32,
    /// Consecutive passes that found another aterm holding the copy's update lock.
    busy: u32,
}

/// A failure of the check half the loop announced ([`Announcer::failure`]).
#[derive(Debug)]
struct Failure {
    /// The body said for it, last.
    body: String,
    /// The record's last check (`updated_at`) when it was said: only a check recorded
    /// since then can say the checks work again.
    at: String,
}

/// What a lock held past [`BUSY_ANNOUNCED_AFTER`] passes says.
const BUSY_BODY: &str = "Update lock held by another aterm for over 2 h; checks, installs \
    and update starts wait. A stopped `aterm update` (Ctrl-Z) holds it until resumed (`fg`) or ended.";

impl Announcer {
    pub(crate) fn new(started: String) -> Self {
        Self {
            started,
            shown: None,
            failure: None,
            busy_shown: false,
            unrecorded: 0,
            busy: 0,
        }
    }

    /// The `(title, body)` owed to the window for `pass`, or `None`.
    pub(crate) fn tick(&mut self, pass: Pass<'_>) -> Option<(String, String)> {
        if pass.record == Record::Busy {
            self.busy = self.busy.saturating_add(1);
            if self.busy < BUSY_ANNOUNCED_AFTER || self.busy_shown {
                return None;
            }
            self.busy_shown = true;
            return self.show(BUSY_BODY.to_string(), true);
        }
        let mut lock_freed = false;
        if pass.took_lock {
            self.busy = 0;
            lock_freed = std::mem::take(&mut self.busy_shown);
        }
        self.unrecorded = match pass.record {
            Record::Lost => self.unrecorded.saturating_add(1),
            Record::Kept | Record::Busy => 0,
        };
        let failing_checks = pass.failing_checks.max(self.unrecorded);
        if failing_checks >= crate::PERSISTENT_AFTER {
            let news = self.unrecorded >= crate::PERSISTENT_AFTER
                || pass.updated_at >= self.started.as_str();
            let body = failing_body(
                (self.unrecorded == 0).then_some(pass.failing_checks),
                pass.outcome,
            );
            let said = self.show(body.clone(), news);
            if said.is_some() {
                self.failure = Some(Failure {
                    body,
                    at: pass.updated_at.to_string(),
                });
            }
            return said;
        }
        let answered = match &self.failure {
            Some(failure) => failing_checks == 0 && pass.updated_at != failure.at,
            None => lock_freed,
        };
        if answered {
            self.failure = None;
            self.shown = None;
            return Some((crate::HEALTH_RECOVERED_TITLE.to_string(), CLASS.to_string()));
        }
        match &self.failure {
            Some(failure) if lock_freed => {
                let body = failure.body.clone();
                self.show(body, true)
            }
            _ => None,
        }
    }

    /// Whether the check half has nothing said and unanswered (no failure, no lock's notice), so a recovery that reads as the whole ledger healing may be sent ([`InstallWatch::tick`]'s `quiet`).
    #[inline]
    pub(crate) fn quiet(&self) -> bool {
        self.shown.is_none() && self.failure.is_none()
    }

    /// Show `body` (`news`: a first announcement is owed), or restate it when it
    /// differs from what the window shows; `None` when it is shown already.
    fn show(&mut self, body: String, news: bool) -> Option<(String, String)> {
        let title = match &self.shown {
            None if news => crate::health_failing_title(CLASS).to_string(),
            Some(shown) if *shown != body => crate::HEALTH_RESTATED_TITLE.to_string(),
            _ => return None,
        };
        self.shown = Some(body.clone());
        Some((title, body))
    }
}

/// THE INSTALL HALF'S WATCH.
#[derive(Debug, Default)]
pub(crate) struct InstallWatch {
    /// Consecutive passes the installed build waited for a start.
    waiting: u64,
    /// The wait was said ([`crate::INSTALL_AWAITS_START_TITLE`]) and is not yet over.
    wait_said: bool,
    /// A crash loop's end (a rollback, or a build kept) was said, and no started newer
    /// build has answered it yet.
    failure_said: bool,
}

impl InstallWatch {
    /// The `(title, body)` notices owed to the window for `pass`, in the order they are sent; `quiet` is whether the check half has nothing unanswered ([`Announcer::quiet`]) after this pass.
    pub(crate) fn tick(&mut self, pass: Pass<'_>, quiet: bool) -> Vec<(String, String)> {
        let mut owed = Vec::new();
        if let Some(install) = pass.install {
            if self.wait_said && !install.awaiting_start {
                self.wait_said = false;
                owed.push((
                    crate::INSTALL_WAIT_OVER_TITLE.to_string(),
                    install.version.to_string(),
                ));
            }
            if install.awaiting_start {
                self.waiting = self.waiting.saturating_add(1);
                if self.waiting >= START_WAIT_ANNOUNCED_AFTER && !self.wait_said {
                    self.wait_said = true;
                    owed.push((
                        crate::INSTALL_AWAITS_START_TITLE.to_string(),
                        start_wait_body(install.version, install.starts),
                    ));
                }
            } else {
                self.waiting = 0;
            }
        }
        if !pass.trial_ends.is_empty() {
            self.failure_said = true;
            owed.extend(
                pass.trial_ends
                    .iter()
                    .map(|body| (crate::INSTALL_CRASH_LOOP_TITLE.to_string(), body.clone())),
            );
        } else if self.failure_said
            && quiet
            && pass
                .install
                .is_some_and(|install| install.started && !install.rolled_back)
        {
            self.failure_said = false;
            owed.push((
                crate::HEALTH_RECOVERED_TITLE.to_string(),
                INSTALL_CLASS.to_string(),
            ));
        }
        owed
    }
}

/// What an install waiting past [`START_WAIT_ANNOUNCED_AFTER`] passes says: what waits,
/// how many of its starts began, what ends it, and what waits with it.
fn start_wait_body(version: &str, starts: u32) -> String {
    let begun = match starts {
        0 => "has not started yet".to_string(),
        1 => "its one start has not finished".to_string(),
        n => format!("none of its {n} starts has finished"),
    };
    format!(
        "aterm {version} was installed over an hour ago and {begun}: the next aterm you start \
         finishes the update, and update checks wait until then."
    )
}

/// What a crash loop's rollback says ([`InstallWatch`]), from the facts alone: the build that failed (`failed`, its version) and the one the copy restored (`restored`, as `crate::linux` names it: `aterm 0.10.0`, or `the previous copy`).
pub(crate) fn rollback_body(failed: &str, restored: &str) -> String {
    format!(
        "aterm {failed} didn\u{2019}t start properly, so {restored} is back and aterm \
         {failed} won\u{2019}t install again."
    )
}

/// What a crash loop no rollback could answer says, from the facts alone: the build that stays.
pub(crate) fn kept_body(kept: &str) -> String {
    format!(
        "aterm {kept} didn\u{2019}t start properly, and the build before it can\u{2019}t come \
         back, so aterm {kept} stays until a newer release installs over it."
    )
}

/// The loop's memory of the outcome it last reported: a changed outcome is logged and cues the window to re-read the copy's state (`crate::CheckedNotify`), since the health notice above stays silent on a healthy pass.
#[derive(Debug, Default)]
pub(crate) struct OutcomeWatch {
    /// The outcome last reported; `None` before the first pass.
    last: Option<String>,
}

impl OutcomeWatch {
    /// Whether `outcome` is a change to report: the first pass's always is.
    pub(crate) fn changed(&mut self, outcome: &str) -> bool {
        if self.last.as_deref() == Some(outcome) {
            return false;
        }
        self.last = Some(outcome.to_string());
        true
    }
}

/// What one pass of the Linux check loop tells the window (`crate::linux`'s loop hands every pass to [`Reporter::report`]): a changed outcome cues a re-read ([`OutcomeWatch`], `on_checked`), and a notice owed by [`Announcer`] or [`InstallWatch`] goes through `notify`.
#[derive(Debug)]
pub(crate) struct Reporter {
    outcome: OutcomeWatch,
    announcer: Announcer,
    install: InstallWatch,
}

impl Reporter {
    /// The loop's reporter; `started` is the loop's start ([`Announcer::new`]).
    pub(crate) fn new(started: String) -> Self {
        Self {
            outcome: OutcomeWatch::default(),
            announcer: Announcer::new(started),
            install: InstallWatch::default(),
        }
    }

    /// Tell the window what `pass` changed: `on_checked` when its outcome changed (the first pass's always is), then the notice it owes, if any.
    pub(crate) fn report(
        &mut self,
        pass: Pass<'_>,
        on_checked: Option<&crate::CheckedNotify>,
        notify: Option<&crate::HealthNotify>,
    ) {
        if self.outcome.changed(pass.outcome) {
            aterm_log::info!("aterm-update: {}", pass.outcome);
            if let Some(on_checked) = on_checked {
                on_checked();
            }
        }
        let mut owed: Vec<_> = self.announcer.tick(pass).into_iter().collect();
        owed.extend(self.install.tick(pass, self.announcer.quiet()));
        if let Some(notify) = notify {
            for (title, body) in owed {
                notify(title, body);
            }
        }
    }
}

/// The body of a failing or restated announcement: the count and the reason, in the
/// macOS notice's shape.
fn failing_body(failing_checks: Option<u32>, outcome: &str) -> String {
    let reason = outcome.trim_end_matches('.');
    match failing_checks {
        Some(count) => format!("{count} failed checks in a row: {reason}."),
        None => format!("{reason}."),
    }
}

#[cfg(test)]
#[path = "linux_notice_tests.rs"]
mod tests;
