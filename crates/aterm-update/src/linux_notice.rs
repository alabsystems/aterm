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
mod tests {
    use super::*;
    use std::slice::from_ref;

    /// A healthy pass that changes the outcome owes no notice but still cues the window,
    /// through the loop's own step ([`Reporter::report`]); the same outcome again does not.
    #[test]
    fn a_changed_outcome_cues_the_window_with_no_notice_owed() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let cues = Arc::new(AtomicUsize::new(0));
        let notices = Arc::new(AtomicUsize::new(0));
        let on_checked: crate::CheckedNotify = Box::new({
            let cues = Arc::clone(&cues);
            move || {
                cues.fetch_add(1, Ordering::SeqCst);
            }
        });
        let notify: crate::HealthNotify = Box::new({
            let notices = Arc::clone(&notices);
            move |_, _| {
                notices.fetch_add(1, Ordering::SeqCst);
            }
        });
        let mut reporter = Reporter::new(START.into());
        let mut cued = Vec::new();
        for (text, at) in [
            ("Up to date", "2026-09-28T10:01:00Z"),
            ("aterm 0.99.0 is installed", "2026-09-28T10:02:00Z"),
            ("aterm 0.99.0 is installed", "2026-09-28T10:03:00Z"),
        ] {
            let before = cues.load(Ordering::SeqCst);
            reporter.report(pass(text, 0, at), Some(&on_checked), Some(&notify));
            cued.push(cues.load(Ordering::SeqCst) > before);
        }
        assert_eq!(
            cued,
            [true, true, false],
            "the first pass and the install are cues; the same outcome again is not"
        );
        assert_eq!(notices.load(Ordering::SeqCst), 0, "no notice is owed");
        let failing = "No space left on device (os error 28)";
        reporter.report(
            pass(failing, crate::PERSISTENT_AFTER, "2026-09-28T10:04:00Z"),
            Some(&on_checked),
            Some(&notify),
        );
        assert_eq!(notices.load(Ordering::SeqCst), 1);
        assert_eq!(cues.load(Ordering::SeqCst), 3);
    }

    const START: &str = "2026-09-28T10:00:00Z";

    fn pass<'a>(outcome: &'a str, failing_checks: u32, updated_at: &'a str) -> Pass<'a> {
        Pass {
            outcome,
            failing_checks,
            updated_at,
            record: Record::Kept,
            took_lock: true,
            install: None,
            trial_ends: &[],
        }
    }

    /// A pass over a copy whose installed `version` waits for a start (`awaiting`), was
    /// started (`started`), or refuses a newer build it rolled back (`rolled_back`), with
    /// no start counted.
    fn over<'a>(
        outcome: &'a str,
        version: &'a str,
        (awaiting_start, started, rolled_back): (bool, bool, bool),
    ) -> Pass<'a> {
        Pass {
            install: Some(Install {
                version,
                awaiting_start,
                starts: 0,
                started,
                rolled_back,
            }),
            ..pass(outcome, 0, START)
        }
    }

    /// `pass` taking crash loops' ends off the record, in the words `words`.
    fn ending<'a>(pass: Pass<'a>, words: &'a [String]) -> Pass<'a> {
        Pass {
            trial_ends: words,
            ..pass
        }
    }

    /// The notices `watch` owes for `pass` while the check half is quiet.
    fn quiet(watch: &mut InstallWatch, pass: Pass<'_>) -> Vec<(String, String)> {
        watch.tick(pass, true)
    }

    fn notice(title: &str, body: &str) -> (String, String) {
        (title.to_string(), body.to_string())
    }

    const WAITING: (bool, bool, bool) = (true, false, false);
    const STARTED: (bool, bool, bool) = (false, true, false);
    const ROLLED_BACK: (bool, bool, bool) = (false, false, true);
    const NOTHING: (bool, bool, bool) = (false, false, false);

    /// THE WAIT IS SAID ONCE, AT ITS DEADLINE, AS NO FAILURE, AND ITS END IS SAID (U21 and
    /// its review).
    #[test]
    fn an_install_waiting_for_a_start_is_said_at_its_deadline_and_the_start_answers_it() {
        let waiting = "aterm 0.11.0 is installed";
        let mut watch = InstallWatch::default();
        for _ in 1..START_WAIT_ANNOUNCED_AFTER {
            assert_eq!(quiet(&mut watch, over(waiting, "0.11.0", WAITING)), []);
        }
        assert_eq!(quiet(&mut watch, pass("unreadable record", 1, START)), []);
        let said = quiet(&mut watch, over(waiting, "0.11.0", WAITING));
        let [(title, body)] = said.as_slice() else {
            panic!("said once, at the hour: {said:?}");
        };
        assert_eq!(title, crate::INSTALL_AWAITS_START_TITLE);
        assert_eq!(crate::health_failing_class(title), None, "no failure");
        assert!(
            body.contains("0.11.0")
                && body.contains("has not started yet")
                && body.contains("update checks wait"),
            "{body}"
        );
        assert_eq!(
            quiet(&mut watch, over(waiting, "0.11.0", WAITING)),
            [],
            "once"
        );
        assert_eq!(
            quiet(&mut watch, over(waiting, "0.11.0", STARTED)),
            [notice(crate::INSTALL_WAIT_OVER_TITLE, "0.11.0")],
            "the start ends the wait, as no recovery"
        );
        assert_eq!(quiet(&mut watch, over(waiting, "0.11.0", STARTED)), []);

        let mut begun = InstallWatch::default();
        let mut two = over(waiting, "0.11.0", WAITING);
        if let Some(install) = two.install.as_mut() {
            install.starts = 2;
        }
        let said = (0..START_WAIT_ANNOUNCED_AFTER)
            .flat_map(|_| quiet(&mut begun, two))
            .collect::<Vec<_>>();
        assert!(
            matches!(said.as_slice(), [(_, body)] if body.contains("none of its 2 starts has finished")),
            "{said:?}"
        );
    }

    /// A CRASH LOOP'S END IS SAID FROM THE RECORD, AND A NEWER STARTED INSTALL ANSWERS IT
    /// (U21 and its reviews).
    #[test]
    fn a_crash_loops_end_is_said_from_the_record_and_a_newer_started_install_answers_it() {
        let rolled = "aterm 0.11.0 didn\u{2019}t start properly in 3 launches, so aterm 0.10.0 \
                      is back and aterm 0.11.0 won\u{2019}t install again";
        let crash = crate::INSTALL_CRASH_LOOP_TITLE;
        let words = rollback_body("0.11.0", "aterm 0.10.0");
        let mut watch = InstallWatch::default();
        assert_eq!(
            quiet(
                &mut watch,
                ending(over(rolled, "0.10.0", ROLLED_BACK), from_ref(&words))
            ),
            [notice(crash, &words)],
            "an end found at the first pass is said"
        );
        assert_eq!(
            quiet(&mut watch, over(rolled, "0.10.0", ROLLED_BACK)),
            [],
            "once: the record no longer holds it"
        );
        assert_eq!(
            quiet(
                &mut watch,
                over("aterm 0.12.0 is installed", "0.12.0", WAITING)
            ),
            [],
            "a newer install waiting for its start answers nothing yet"
        );
        assert_eq!(
            quiet(
                &mut watch,
                over("aterm 0.12.0 is installed", "0.12.0", STARTED)
            ),
            [notice(crate::HEALTH_RECOVERED_TITLE, INSTALL_CLASS)]
        );

        let mut busy_pass = InstallWatch::default();
        let held = Pass {
            record: Record::Busy,
            failing_checks: 1,
            ..over("Another aterm is updating this copy", "0.10.0", ROLLED_BACK)
        };
        assert_eq!(
            quiet(&mut busy_pass, ending(held, from_ref(&words))),
            [notice(crash, &words)],
            "the record's words, never the lock's"
        );

        let mut after_wait = InstallWatch::default();
        for _ in 0..START_WAIT_ANNOUNCED_AFTER {
            quiet(
                &mut after_wait,
                over("aterm 0.11.0 is installed", "0.11.0", WAITING),
            );
        }
        assert_eq!(
            quiet(
                &mut after_wait,
                ending(over(rolled, "0.10.0", ROLLED_BACK), from_ref(&words))
            ),
            [
                notice(crate::INSTALL_WAIT_OVER_TITLE, "0.10.0"),
                notice(crash, &words),
            ],
            "the wait is over, and the rollback is the install half's failure"
        );
        quiet(
            &mut after_wait,
            over("aterm 0.12.0 is installed", "0.12.0", WAITING),
        );
        let again = rollback_body("0.12.0", "aterm 0.10.0");
        assert_eq!(
            quiet(
                &mut after_wait,
                ending(over(rolled, "0.10.0", ROLLED_BACK), from_ref(&again))
            ),
            [notice(crash, &again)],
            "a second crash loop before an answer is said as its own"
        );
        let both = [
            rollback_body("0.13.0", "aterm 0.10.0"),
            rollback_body("0.14.0", "aterm 0.10.0"),
        ];
        assert_eq!(
            quiet(
                &mut after_wait,
                ending(over(rolled, "0.10.0", ROLLED_BACK), &both)
            ),
            [notice(crash, &both[0]), notice(crash, &both[1])],
            "two ends that waited for a window are each said, oldest first"
        );

        let mut asked = InstallWatch::default();
        for _ in 0..START_WAIT_ANNOUNCED_AFTER {
            quiet(
                &mut asked,
                over("aterm 0.11.0 is installed", "0.11.0", WAITING),
            );
        }
        assert_eq!(
            quiet(
                &mut asked,
                over("aterm 0.10.0 is back", "0.10.0", ROLLED_BACK)
            ),
            [notice(crate::INSTALL_WAIT_OVER_TITLE, "0.10.0")],
            "a person's rollback ends the wait and is no failure"
        );
    }

    /// A failed pass the record did not take, under the lock (a save the full or
    /// read-only folder refused): its count is the one from before.
    fn unrecorded<'a>(outcome: &'a str, failing_checks: u32, updated_at: &'a str) -> Pass<'a> {
        Pass {
            record: Record::Lost,
            ..pass(outcome, failing_checks, updated_at)
        }
    }

    /// A failed pass that never reached the lock: the settings or the record would
    /// not read, so the record did not take it either.
    fn before_the_lock<'a>(outcome: &'a str, failing_checks: u32, updated_at: &'a str) -> Pass<'a> {
        Pass {
            took_lock: false,
            ..unrecorded(outcome, failing_checks, updated_at)
        }
    }

    /// A pass that found another aterm updating the copy: nothing was checked.
    fn busy<'a>(outcome: &'a str, failing_checks: u32, updated_at: &'a str) -> Pass<'a> {
        Pass {
            record: Record::Busy,
            took_lock: false,
            ..pass(outcome, failing_checks, updated_at)
        }
    }

    /// A pass that failed before reaching the lock neither answers the lock's notice nor
    /// restarts its count; the first pass that takes the lock answers it.
    #[test]
    fn a_pass_that_failed_before_the_lock_neither_answers_nor_restarts_its_notice() {
        let held = "Another aterm is updating this copy right now; try again when it finishes";
        let stalled = "current update settings could not be read";
        let before = "2026-09-28T09:00:00Z";
        let recovered = Some((crate::HEALTH_RECOVERED_TITLE.to_string(), CLASS.to_string()));

        let mut notice = Announcer::new(START.into());
        let said: Vec<_> = (0..BUSY_ANNOUNCED_AFTER)
            .filter_map(|_| notice.tick(busy(held, 0, before)))
            .collect();
        assert_eq!(said.len(), 1, "PRECONDITION: the lock's notice is said");
        assert_eq!(
            notice.tick(before_the_lock(stalled, 1, before)),
            None,
            "a pass that never reached the lock does not say it came free"
        );
        assert_eq!(notice.tick(busy(held, 0, before)), None, "still said once");
        assert_eq!(
            notice.tick(pass("Up to date", 0, before)),
            recovered,
            "the first pass that took the lock answers the notice"
        );

        let mut notice = Announcer::new(START.into());
        for n in 1..BUSY_ANNOUNCED_AFTER {
            assert_eq!(notice.tick(busy(held, 0, before)), None, "busy pass {n}");
        }
        assert_eq!(notice.tick(before_the_lock(stalled, 1, before)), None);
        notice
            .tick(busy(held, 0, before))
            .expect("the bound's last busy pass, a failure before the lock between");

        let mut notice = Announcer::new(START.into());
        for _ in 0..BUSY_ANNOUNCED_AFTER {
            notice.tick(busy(held, 0, before));
        }
        let full = "No space left on device (os error 28)";
        assert_eq!(notice.tick(unrecorded(full, 1, before)), recovered);
        for n in 1..BUSY_ANNOUNCED_AFTER {
            assert_eq!(notice.tick(busy(held, 0, before)), None, "busy pass {n}");
        }
    }

    /// Busy passes announce nothing and neither add to nor end the loop's own streak.
    #[test]
    fn another_aterms_long_update_announces_nothing() {
        let mut notice = Announcer::new(START.into());
        let held = "Another aterm is updating this copy right now; try again when it finishes";
        let before = "2026-09-28T09:00:00Z";
        for n in 0..10 {
            assert_eq!(notice.tick(busy(held, 1, before)), None, "busy pass {n}");
        }
        assert_eq!(notice.tick(pass("Up to date", 0, before)), None);
        let full = "No space left on device (os error 28)";
        assert_eq!(notice.tick(unrecorded(full, 1, before)), None);
        assert_eq!(notice.tick(busy(held, 1, before)), None);
        assert_eq!(notice.tick(unrecorded(full, 1, before)), None);
        assert_eq!(notice.tick(busy(held, 1, before)), None);
        assert!(
            notice.tick(unrecorded(full, 1, before)).is_some(),
            "the third unrecorded failure, busy passes between"
        );
    }

    /// A lock held past [`BUSY_ANNOUNCED_AFTER`] passes (e.g. `aterm update check` stopped
    /// with Ctrl-Z) is announced once, and its release is the recovery.
    #[test]
    fn a_lock_held_past_its_bound_is_said_once_and_its_release_recovers() {
        let mut notice = Announcer::new(START.into());
        let held = "Another aterm is updating this copy right now; try again when it finishes";
        let before = "2026-09-28T09:00:00Z";
        let said: Vec<_> = (0..24 * 60)
            .filter_map(|_| notice.tick(busy(held, 0, before)))
            .collect();
        assert_eq!(
            said.len(),
            1,
            "a lock busy for a whole day reaches the window once"
        );
        assert_eq!(crate::health_failing_class(&said[0].0), Some(CLASS));
        assert_eq!(
            notice.tick(pass("Up to date", 0, before)),
            Some((crate::HEALTH_RECOVERED_TITLE.to_string(), CLASS.to_string())),
            "the lock came free: no check need have run"
        );
        let mut notice = Announcer::new(START.into());
        for _ in 0..2 {
            for n in 1..BUSY_ANNOUNCED_AFTER {
                assert_eq!(notice.tick(busy(held, 0, before)), None, "busy pass {n}");
            }
            assert_eq!(notice.tick(pass("Up to date", 0, before)), None);
        }
        let mut notice = Announcer::new(START.into());
        for _ in 0..BUSY_ANNOUNCED_AFTER {
            notice.tick(busy(held, 0, before));
        }
        let (title, body) = notice
            .tick(pass("network down", 3, "2026-09-28T12:00:00Z"))
            .expect("the failing checks replace the lock's notice");
        assert_eq!(title, crate::HEALTH_RESTATED_TITLE);
        assert!(body.starts_with('3'), "the streak's count: {body}");
    }

    /// An install zeroes the failure count with no check run: the recovery waits for a
    /// check recorded after the announcement.
    #[test]
    fn a_zeroed_count_with_no_check_since_is_no_recovery() {
        let mut notice = Announcer::new(START.into());
        let failed = "2026-09-28T10:30:00Z";
        assert!(
            notice
                .tick(pass("machine roster signature refused", 3, failed))
                .is_some()
        );
        let installed = "aterm 0.12.0 is installed \u{2014} the next aterm you start finishes it";
        for _ in 0..30 {
            assert_eq!(
                notice.tick(pass(installed, 0, failed)),
                None,
                "no check ran"
            );
        }
        assert_eq!(
            notice.tick(pass(
                "machine roster signature refused",
                1,
                "2026-09-28T11:30:00Z"
            )),
            None
        );
        assert_eq!(
            notice.tick(pass("Up to date", 0, "2026-09-28T12:30:00Z")),
            Some((crate::HEALTH_RECOVERED_TITLE.to_string(), CLASS.to_string())),
            "a check recorded since the announcement"
        );
    }

    /// The lock coming free does not answer a failure said under its notice: the failure
    /// is said again, and only a check recorded since answers it.
    #[test]
    fn the_lock_coming_free_does_not_answer_a_failure_under_its_notice() {
        let mut notice = Announcer::new(START.into());
        let failed = "2026-09-28T10:30:00Z";
        let (_, failure) = notice
            .tick(pass("machine roster signature refused", 3, failed))
            .expect("announced");
        let held = "Another aterm is updating this copy right now; try again when it finishes";
        let said: Vec<_> = (0..BUSY_ANNOUNCED_AFTER)
            .filter_map(|_| notice.tick(busy(held, 3, failed)))
            .collect();
        assert_eq!(said.len(), 1, "the lock's notice, once");
        assert_eq!(
            said[0].0,
            crate::HEALTH_RESTATED_TITLE,
            "said over the failure"
        );
        let installed = "aterm 0.12.0 is installed \u{2014} the next aterm you start finishes it";
        assert_eq!(
            notice.tick(pass(installed, 0, failed)),
            Some((crate::HEALTH_RESTATED_TITLE.to_string(), failure)),
            "the lock came free and no check ran: the failure stands"
        );
        for _ in 0..30 {
            assert_eq!(notice.tick(pass(installed, 0, failed)), None, "said once");
        }
        assert_eq!(
            notice.tick(pass("Up to date", 0, "2026-09-28T13:00:00Z")),
            Some((crate::HEALTH_RECOVERED_TITLE.to_string(), CLASS.to_string())),
            "a check recorded since the failure was said"
        );
        let mut notice = Announcer::new(START.into());
        assert!(notice.tick(pass("network down", 3, failed)).is_some());
        for _ in 0..BUSY_ANNOUNCED_AFTER {
            notice.tick(busy(held, 3, failed));
        }
        assert_eq!(
            notice.tick(pass("Up to date", 0, "2026-09-28T13:00:00Z")),
            Some((crate::HEALTH_RECOVERED_TITLE.to_string(), CLASS.to_string()))
        );
    }

    /// One unrecorded failure does not announce a streak recorded before this launch.
    #[test]
    fn one_unrecorded_blip_is_not_news_of_an_old_streak() {
        let mut notice = Announcer::new(START.into());
        let before = "2026-09-28T09:00:00Z";
        let blip = "Read-only file system (os error 30)";
        assert_eq!(notice.tick(unrecorded(blip, 5, before)), None);
        assert_eq!(notice.tick(unrecorded(blip, 5, before)), None);
        let (_, body) = notice
            .tick(unrecorded(blip, 5, before))
            .expect("three of its own in a row are");
        assert!(!body.contains("failed checks"), "5 is the record's: {body}");
    }

    /// Once the record stops taking failures (the folder turns read-only), the body names
    /// no stale count.
    #[test]
    fn a_streak_the_record_stopped_taking_names_no_stale_count() {
        let mut notice = Announcer::new(START.into());
        let later = "2026-09-28T10:30:00Z";
        let (_, body) = notice
            .tick(pass("network down", 5, later))
            .expect("a recorded streak of this launch is announced");
        assert!(body.starts_with('5'), "{body}");
        let read_only = "Read-only file system (os error 30)";
        let (title, body) = notice
            .tick(unrecorded(read_only, 5, later))
            .expect("the reason changed");
        assert_eq!(title, crate::HEALTH_RESTATED_TITLE);
        assert!(!body.contains('5'), "no stale count: {body}");
        for n in 0..8 {
            assert_eq!(
                notice.tick(unrecorded(read_only, 5, later)),
                None,
                "unrecorded pass {n}"
            );
        }
    }

    /// The loop counts failures the record cannot hold (a full disk): the third in a row is
    /// announced, and any pass the record took restarts the count.
    #[test]
    fn a_failure_the_record_cannot_hold_is_counted_by_the_loop() {
        let mut notice = Announcer::new(START.into());
        let full = "No space left on device (os error 28)";
        let before = "2026-09-28T09:00:00Z";
        assert_eq!(notice.tick(unrecorded(full, 1, before)), None);
        assert_eq!(notice.tick(unrecorded(full, 1, before)), None);
        let (title, body) = notice
            .tick(unrecorded(full, 1, before))
            .expect("the third unrecorded failure in a row is announced");
        assert_eq!(crate::health_failing_class(&title), Some(CLASS));
        assert!(!body.contains("failed checks"), "{body}");
        assert_eq!(notice.tick(unrecorded(full, 1, before)), None, "said once");
        assert_eq!(notice.tick(unrecorded(full, 1, before)), None, "said once");
        assert_eq!(
            notice.tick(pass("Up to date", 0, "2026-09-28T10:30:00Z")),
            Some((crate::HEALTH_RECOVERED_TITLE.to_string(), CLASS.to_string()))
        );
        let mut notice = Announcer::new(START.into());
        let later = "2026-09-28T10:30:00Z";
        assert_eq!(notice.tick(unrecorded(full, 0, before)), None);
        assert_eq!(notice.tick(unrecorded(full, 0, before)), None);
        assert_eq!(notice.tick(pass("network down", 1, later)), None);
        assert_eq!(notice.tick(unrecorded(full, 1, later)), None);
        assert_eq!(
            notice.tick(unrecorded(full, 1, later)),
            None,
            "two in a row"
        );
    }

    /// Only a persistent failure is announced: three in a row under the failing title, a
    /// changed reason or count restated quietly, then the recovery naming its class.
    #[test]
    fn only_a_persistent_failure_is_announced_and_its_recovery_follows() {
        let mut notice = Announcer::new(START.into());
        let t = |minute: u32| format!("2026-09-28T10:{minute:02}:00Z");
        let (t1, t2, t3, t4, t5, t6) = (t(1), t(2), t(3), t(4), t(5), t(6));
        assert_eq!(
            notice.tick(pass(
                "Up to date \u{2014} the newest release is aterm 0.99.0",
                0,
                &t1
            )),
            None
        );
        assert_eq!(
            notice.tick(pass("aterm 0.100.0 is installed", 0, &t1)),
            None
        );
        assert_eq!(notice.tick(pass("network down", 1, &t2)), None, "a blip");
        let (title, body) = notice
            .tick(pass("machine roster signature refused", 3, &t3))
            .expect("a persistent failure is announced");
        assert_eq!(
            crate::health_failing_class(&title),
            Some(CLASS),
            "the window reads the title back to the check half: {title}"
        );
        assert!(body.starts_with('3'), "{body}");
        assert_eq!(
            notice.tick(pass("machine roster signature refused", 3, &t4)),
            None,
            "a pass that checked nothing (not due) says nothing twice"
        );
        let (title, body) = notice
            .tick(pass("machine roster signature refused", 4, &t4))
            .expect("a moved count is restated, as the macOS lane restates it");
        assert_eq!(title, crate::HEALTH_RESTATED_TITLE);
        assert!(body.starts_with('4'), "{body}");
        let (title, _) = notice
            .tick(pass("below the signed minimum-build floor", 5, &t5))
            .expect("a changed reason is restated");
        assert_eq!(title, crate::HEALTH_RESTATED_TITLE);
        assert_eq!(
            notice.tick(pass("Up to date", 0, &t6)),
            Some((crate::HEALTH_RECOVERED_TITLE.to_string(), CLASS.to_string()))
        );
        assert_eq!(notice.tick(pass("Up to date", 0, &t6)), None);
        let (title, _) = notice
            .tick(pass("network down", 3, &t6))
            .expect("a new episode");
        assert_eq!(crate::health_failing_class(&title), Some(CLASS));
    }

    /// A streak recorded before this launch is not re-announced; its next failed check is.
    #[test]
    fn a_streak_from_before_this_launch_waits_for_its_next_failed_check() {
        let mut notice = Announcer::new(START.into());
        assert_eq!(
            notice.tick(pass("network down", 4, "2026-09-28T09:00:00Z")),
            None
        );
        assert!(
            notice
                .tick(pass("network down", 5, "2026-09-28T10:30:00Z"))
                .is_some()
        );
        let mut quiet = Announcer::new(START.into());
        assert_eq!(
            quiet.tick(pass("Up to date", 0, "2026-09-28T10:30:00Z")),
            None
        );
    }

    /// What `reporter` told the window over `passes` (its `notify`).
    fn reported<'a>(
        reporter: &mut Reporter,
        passes: impl IntoIterator<Item = Pass<'a>>,
    ) -> Vec<(String, String)> {
        use std::sync::{Arc, Mutex};
        let said = Arc::new(Mutex::new(Vec::new()));
        let notify: crate::HealthNotify = Box::new({
            let said = Arc::clone(&said);
            move |title, body| said.lock().unwrap().push((title, body))
        });
        for pass in passes {
            reporter.report(pass, None, Some(&notify));
        }
        said.lock().unwrap().clone()
    }

    /// THE INSTALL HALF'S RECOVERY WAITS FOR THE CHECK HALF (the review of
    /// , U21).
    #[test]
    fn the_install_halfs_recovery_waits_for_the_check_half() {
        let rolled = "aterm 0.11.0 didn\u{2019}t start properly in 3 launches, so aterm 0.10.0 \
                      is back and aterm 0.11.0 won\u{2019}t install again";
        let words = rollback_body("0.11.0", "aterm 0.10.0");
        let failed_at = "2026-09-28T11:00:00Z";
        let checked_at = "2026-09-28T12:00:00Z";
        let mut reporter = Reporter::new(START.into());
        let said = reported(
            &mut reporter,
            [
                over("Up to date", "0.10.0", NOTHING),
                ending(over(rolled, "0.10.0", ROLLED_BACK), from_ref(&words)),
                Pass {
                    failing_checks: crate::PERSISTENT_AFTER,
                    updated_at: failed_at,
                    ..over("network unreachable", "0.10.0", ROLLED_BACK)
                },
            ],
        );
        assert!(
            said.iter()
                .any(|(title, _)| title == crate::INSTALL_CRASH_LOOP_TITLE),
            "PRECONDITION: the rollback was said: {said:?}"
        );
        assert!(
            said.iter()
                .any(|(title, _)| crate::health_failing_class(title) == Some(CLASS)),
            "PRECONDITION: the check half's failure was said: {said:?}"
        );
        let recovery = |class: &str| (crate::HEALTH_RECOVERED_TITLE.to_string(), class.to_string());
        let unanswered = reported(
            &mut reporter,
            [Pass {
                updated_at: failed_at,
                ..over("aterm 0.12.0 is installed", "0.12.0", STARTED)
            }],
        );
        assert!(
            !unanswered.contains(&recovery(INSTALL_CLASS)),
            "no install recovery while the check half's failure stands: {unanswered:?}"
        );
        let answered = reported(
            &mut reporter,
            [Pass {
                updated_at: checked_at,
                ..over("Up to date", "0.12.0", STARTED)
            }],
        );
        assert_eq!(
            answered,
            [recovery(CLASS), recovery(INSTALL_CLASS)],
            "a check answers the check half, and the install half's recovery follows"
        );
    }

    /// A WAIT THAT ENDS WITH NO START IS ANSWERED (the reviews of U21).
    #[test]
    fn a_wait_that_ends_with_no_start_is_answered() {
        let waiting = "aterm 0.11.0 is installed";
        let mut reporter = Reporter::new(START.into());
        let wait = reported(
            &mut reporter,
            std::iter::repeat_n(
                over(waiting, "0.11.0", WAITING),
                usize::try_from(START_WAIT_ANNOUNCED_AFTER).unwrap(),
            ),
        );
        assert_eq!(wait.len(), 1, "PRECONDITION: the wait was said: {wait:?}");
        let words = kept_body("0.11.0");
        let ended = reported(
            &mut reporter,
            [ending(
                over("aterm 0.11.0 stays", "0.11.0", NOTHING),
                from_ref(&words),
            )],
        );
        assert!(
            matches!(
                ended.first(),
                Some((title, _)) if crate::health_failing_class(title).is_none()
                    && title != crate::HEALTH_RECOVERED_TITLE
                    && title != crate::HEALTH_RESTATED_TITLE
                    && *title != wait[0].0
            ),
            "the window is told the wait is over, as no failure and no recovery: {ended:?}"
        );
        assert_eq!(
            ended.get(1..),
            Some(&[notice(crate::INSTALL_CRASH_LOOP_TITLE, &words)][..]),
            "and then that the build failed its trial and stays: {ended:?}"
        );
    }
}
