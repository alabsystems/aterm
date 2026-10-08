// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! What the Linux background check tells the window about update health
//! ([`crate::HealthNotify`]): a check lane that keeps failing, its later reasons, and
//! its recovery — never an ordinary result. The window reads an unknown title as a
//! failing check, so a healthy pass must send nothing.
//!
//! The loop speaks the macOS lane's words on the same threshold
//! ([`crate::PERSISTENT_AFTER`] consecutive failed checks): the failing title
//! ([`crate::health_failing_title`]) once, a quiet restatement
//! ([`crate::HEALTH_RESTATED_TITLE`]) when the body changes while it still fails, and
//! the recovery ([`crate::HEALTH_RECOVERED_TITLE`]) once the checks work again. A
//! streak whose last failed check came before this process started is history the
//! status line already shows, not news.
//!
//! A failure the record cannot hold (update folder full or read-only, a record or
//! settings that will not read) never raises `failing_checks`, so the loop counts its
//! own unrecorded failures ([`Record::Lost`]) and judges the larger count. Such a body
//! names no count: neither count is the streak's, and a moving one would restate
//! every pass. A pass that found another aterm holding the copy's update lock
//! ([`Record::Busy`]) is no failure: it neither adds to the count nor ends it. A lock
//! held past [`BUSY_ANNOUNCED_AFTER`] passes is said once, and only a pass that took
//! the lock ([`Pass::took_lock`]) answers it.
//!
//! A recovery is owed only once a check has been RECORDED since the failure was said
//! (the record's `updated_at` moved): an install zeroes the count with no check run.
//! A pass that only waited on a pending trial is no recorded check
//! (`linux::Checked::Nothing`). The lock's notice, said over an announced failure,
//! keeps that failure under it: the lock coming free answers the notice, never the
//! failure.
//!
//! Compiled into every test build, like `linux_trial`, so the macOS gate runs it.

/// The class a Linux check failure is announced under: the check half.
const CLASS: &str = "manifest";

/// How many passes in a row may find another aterm holding the copy's update lock
/// before it is announced ([`Record::Busy`]): two hours at the loop's one-minute
/// pass, longer than any bounded download of a release holds it.
const BUSY_ANNOUNCED_AFTER: u32 = 120;

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
    /// Whether this pass TOOK the copy's update lock, which shows that no other
    /// aterm held it then. A pass that failed before it reached the lock (its
    /// settings or the record would not read) or found it held ([`Record::Busy`])
    /// did not, and says nothing of whether the lock is free.
    pub(crate) took_lock: bool,
}

/// Whether the record took a pass's result ([`Pass::record`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Record {
    /// The record holds this pass's result: a success, a recorded failure, or a
    /// copy whose updates are off (the remedy's sentence).
    Kept,
    /// This pass failed and the record did not take the failure: `failing_checks`
    /// is the count from before it.
    Lost,
    /// Another aterm held the copy's update lock (or changed its enrollment under
    /// this pass): nothing was checked, and nothing is judged.
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
    ///
    /// A lock busy for [`BUSY_ANNOUNCED_AFTER`] passes in a row (e.g. `aterm update`
    /// stopped with Ctrl-Z mid-download) is announced once; the first pass that TAKES
    /// it answers that notice. A failure kept under the notice is answered only by a
    /// check recorded since it was said, and otherwise said again.
    pub(crate) fn tick(&mut self, pass: Pass<'_>) -> Option<(String, String)> {
        if pass.record == Record::Busy {
            self.busy = self.busy.saturating_add(1);
            if self.busy < BUSY_ANNOUNCED_AFTER || self.busy_shown {
                return None;
            }
            self.busy_shown = true;
            // It happened in this process, whatever time the record last kept.
            return self.show(BUSY_BODY.to_string(), true);
        }
        // Only a pass that took the lock shows it free: one that failed before
        // reaching it learned nothing of it.
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
            // The lock came free over a failure no check has answered: it stands.
            Some(failure) if lock_freed => {
                let body = failure.body.clone();
                self.show(body, true)
            }
            _ => None,
        }
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

/// The loop's memory of the outcome it last reported: a changed outcome is logged and
/// cues the window to re-read the copy's state (`crate::CheckedNotify`), since the
/// health notice above stays silent on a healthy pass.
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

/// What one pass of the Linux check loop tells the window (`crate::linux`'s loop
/// hands every pass to [`Reporter::report`]): a changed outcome cues a re-read
/// ([`OutcomeWatch`], `on_checked`), and a notice owed by [`Announcer`] goes through
/// `notify`. One step, so the macOS gate tests the Linux-only loop's cue.
#[derive(Debug)]
pub(crate) struct Reporter {
    outcome: OutcomeWatch,
    announcer: Announcer,
}

impl Reporter {
    /// The loop's reporter; `started` is the loop's start ([`Announcer::new`]).
    pub(crate) fn new(started: String) -> Self {
        Self {
            outcome: OutcomeWatch::default(),
            announcer: Announcer::new(started),
        }
    }

    /// Tell the window what `pass` changed: `on_checked` when its outcome
    /// changed (the first pass's always is), then the notice it owes, if any.
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
        if let (Some(notify), Some((title, body))) = (notify, self.announcer.tick(pass)) {
            notify(title, body);
        }
    }
}

/// The body of a failing or restated announcement: the count and the reason, in the
/// macOS notice's shape. `failing_checks` is `None` while the record has not taken
/// this pass's failure: it then holds no true count.
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

    /// A HEALTHY PASS THAT CHANGES THE OUTCOME STILL CUES THE WINDOW. "Up to date"
    /// then "aterm 0.99.0 is installed" owe no notice, but each is a change the window
    /// is cued for, through the loop's own step ([`Reporter::report`]). NEGATIVE
    /// CONTROL: the same outcome again is no change.
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
        // The notice still goes through the same step: a streak this process
        // recorded announces once, and is cued as a change too.
        let failing = "No space left on device (os error 28)";
        reporter.report(
            pass(failing, crate::PERSISTENT_AFTER, "2026-09-28T10:04:00Z"),
            Some(&on_checked),
            Some(&notify),
        );
        assert_eq!(notices.load(Ordering::SeqCst), 1);
        assert_eq!(cues.load(Ordering::SeqCst), 3);
    }

    /// The Linux loop hands every pass to [`Reporter::report`] with the window's
    /// two hooks, and reports nothing by any other road. A scrape, in this
    /// crate's idiom for the Linux-only lane the macOS gate cannot run: deleting
    /// the call or passing no cue fails here.
    #[test]
    fn the_linux_loop_reports_every_pass_through_the_reporter() {
        let src: String = include_str!("linux.rs").split_whitespace().collect();
        let start = src
            .find("pub(crate)fnspawn_background_check_with_settings(")
            .expect("the background loop");
        let body = &src[start..];
        let body = &body[..body.find("#[cfg(test)]").unwrap_or(body.len())];
        assert!(
            body.contains("letmutreporter=crate::linux_notice::Reporter::new(started);"),
            "the loop's reporter"
        );
        let report = body
            .find("reporter.report(")
            .expect("every pass is reported");
        let check = body
            .find("let(status,record,took_lock)=check_recording(current_build,&provider,false)")
            .expect("the loop's check");
        assert!(check < report, "after the check it reports");
        assert!(
            body[report..].starts_with(
                "reporter.report(crate::linux_notice::Pass{outcome:&status.outcome,\
                 failing_checks:status.failing_checks,updated_at:&status.updated_at,record,\
                 took_lock,},on_checked.as_ref(),notify.as_ref(),);"
            ),
            "the pass and both of the window's hooks: {}",
            &body[report..]
        );
        for inline in ["on_checked()", "notify(title", ".tick("] {
            assert!(
                !body.contains(inline),
                "reported only by the reporter: {inline}"
            );
        }
    }

    const START: &str = "2026-09-28T10:00:00Z";

    fn pass<'a>(outcome: &'a str, failing_checks: u32, updated_at: &'a str) -> Pass<'a> {
        Pass {
            outcome,
            failing_checks,
            updated_at,
            record: Record::Kept,
            took_lock: true,
        }
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

    /// A PASS THAT FAILED BEFORE THE LOCK SAYS NOTHING OF IT. A stopped
    /// `aterm update check` holds the copy's lock and the notice was said. A pass that
    /// fails before reaching the lock (settings timed out, record unreadable) must
    /// neither answer the notice nor restart its count; the first pass that TAKES the
    /// lock answers it.
    ///
    /// FAILS WITHOUT THE FIX: the failed pass answered the notice with the recovery
    /// title, and 119 busy passes after a failure before the lock said nothing.
    ///
    /// NEGATIVE CONTROL: a pass that took the lock and lost its failure (the save the
    /// full folder refused) does show the lock free.
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

        // A failure before the lock does not start the count again.
        let mut notice = Announcer::new(START.into());
        for n in 1..BUSY_ANNOUNCED_AFTER {
            assert_eq!(notice.tick(busy(held, 0, before)), None, "busy pass {n}");
        }
        assert_eq!(notice.tick(before_the_lock(stalled, 1, before)), None);
        let (_, body) = notice
            .tick(busy(held, 0, before))
            .expect("the bound's last busy pass, a failure before the lock between");
        assert!(body.contains("Ctrl-Z"), "{body}");

        // NEGATIVE CONTROL: a pass that took the lock shows it free, failed or not.
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

    /// ANOTHER ATERM'S LONG UPDATE IS NO FAILURE IN EVERY OTHER WINDOW. One window
    /// downloads for minutes under the copy's lock; every other window's pass gives
    /// up on the lock after half a second, before any save, so the record never
    /// takes it. Those passes are no failed checks: however many come in a row
    /// nothing is announced, and so no recovery either. They also neither add to a
    /// streak of the loop's own nor end one.
    ///
    /// FAILS WITHOUT THE FIX: each busy pass counted as an unrecorded failure, so
    /// the third announced "Another aterm is updating this copy" as a persistent
    /// failure, and the next pass announced a recovery.
    #[test]
    fn another_aterms_long_update_announces_nothing() {
        let mut notice = Announcer::new(START.into());
        let held = "Another aterm is updating this copy right now; try again when it finishes";
        let before = "2026-09-28T09:00:00Z";
        for n in 0..10 {
            assert_eq!(notice.tick(busy(held, 1, before)), None, "busy pass {n}");
        }
        assert_eq!(notice.tick(pass("Up to date", 0, before)), None);
        // Busy passes neither extend nor end the loop's own streak.
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

    /// A LOCK HELD PAST ITS BOUND IS SAID.
    /// A person presses Ctrl-Z on `aterm update check` during its download: the stopped
    /// job keeps the copy's update lock for as long as it stays stopped, days in a tmux
    /// pane. Every pass in every window then found the lock busy, and a busy pass is no
    /// failed check, so nothing was ever said, while no check ran, no install finished
    /// and no start of an installed update was counted or confirmed. A lock busy for
    /// longer than any bounded download holds it ([`BUSY_ANNOUNCED_AFTER`] passes) is now
    /// announced once, under the check half's failing title, and the lock coming free
    /// is its recovery.
    ///
    /// FAILS WITHOUT THE FIX: a day of busy passes announced nothing.
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
        let (title, body) = &said[0];
        assert_eq!(crate::health_failing_class(title), Some(CLASS));
        assert!(body.contains("Ctrl-Z"), "{body}");
        assert_eq!(
            notice.tick(pass("Up to date", 0, before)),
            Some((crate::HEALTH_RECOVERED_TITLE.to_string(), CLASS.to_string())),
            "the lock came free: no check need have run"
        );
        // NEGATIVE CONTROLS: a lock held for less than the bound says nothing, and
        // a free pass between busy ones starts the count again.
        let mut notice = Announcer::new(START.into());
        for _ in 0..2 {
            for n in 1..BUSY_ANNOUNCED_AFTER {
                assert_eq!(notice.tick(busy(held, 0, before)), None, "busy pass {n}");
            }
            assert_eq!(notice.tick(pass("Up to date", 0, before)), None);
        }
        // A streak of failed checks after the lock came free is said as such.
        let mut notice = Announcer::new(START.into());
        for _ in 0..BUSY_ANNOUNCED_AFTER {
            notice.tick(busy(held, 0, before));
        }
        let (title, body) = notice
            .tick(pass("network down", 3, "2026-09-28T12:00:00Z"))
            .expect("the failing checks replace the lock's notice");
        assert_eq!(title, crate::HEALTH_RESTATED_TITLE);
        assert_eq!(body, "3 failed checks in a row: network down.");
    }

    /// ONLY A CHECK SAYS THE CHECKS WORK AGAIN.
    /// Checks fail persistently with a stage held, and the window announced it. The
    /// person runs `aterm update apply` (or the installer's reinstall), and the install
    /// zeroes the record's failure count; the next pass checked nothing (the failure
    /// backoff), read a count of 0 and announced that updates work again. The recovery
    /// now waits for a check recorded after the announcement, by any aterm.
    ///
    /// FAILS WITHOUT THE FIX: the pass after the install announced the recovery.
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
        // Checks that fail again keep the announcement standing.
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

    /// THE LOCK COMING FREE DOES NOT ANSWER A FAILURE UNDER ITS NOTICE. Checks fail
    /// and the window announced it. The person runs `aterm update apply`, presses Ctrl-Z during its
    /// probe and resumes it more than two hours later: the lock's notice is said over
    /// the failure, then the apply commits, which zeroes the record's count with no
    /// check run (its `updated_at` stays), and frees the lock. The failure is said
    /// again, and only a check recorded since it was said answers it.
    ///
    /// FAILS WITHOUT THE FIX: the lock's notice replaced the failure's announcement,
    /// and the first pass that found the lock free announced the recovery.
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
        assert!(said[0].1.contains("Ctrl-Z"), "{}", said[0].1);
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
        // NEGATIVE CONTROL: a check recorded while the lock was held (its holder was
        // the check) answers the failure the moment the lock comes free.
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

    /// ONE UNRECORDED BLIP IS NOT NEWS OF AN OLD STREAK. The record holds five
    /// failed checks, all before this launch: history the status line shows. A
    /// single pass whose failure the record could not take is no streak of this
    /// process's own, and must not announce the old failure under its reason.
    ///
    /// FAILS WITHOUT THE FIX: any unrecorded pass was news, so the blip announced a
    /// persistent failure at the first pass.
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

    /// A STREAK THE RECORD STOPPED TAKING NAMES NO STALE COUNT. Five failed checks
    /// are recorded this launch, then the update folder turns read-only: every later
    /// pass reads the record's 5, which is no longer the streak.
    ///
    /// FAILS WITHOUT THE FIX: the body kept saying "5 failed checks in a row"
    /// through the tenth failure and said "6" at the eleventh.
    #[test]
    fn a_streak_the_record_stopped_taking_names_no_stale_count() {
        let mut notice = Announcer::new(START.into());
        let later = "2026-09-28T10:30:00Z";
        let (_, body) = notice
            .tick(pass("network down", 5, later))
            .expect("a recorded streak of this launch is announced");
        assert!(body.starts_with("5 failed checks in a row: "), "{body}");
        let read_only = "Read-only file system (os error 30)";
        let (title, body) = notice
            .tick(unrecorded(read_only, 5, later))
            .expect("the reason changed");
        assert_eq!(title, crate::HEALTH_RESTATED_TITLE);
        assert_eq!(body, "Read-only file system (os error 30).");
        for n in 0..8 {
            assert_eq!(
                notice.tick(unrecorded(read_only, 5, later)),
                None,
                "unrecorded pass {n}"
            );
        }
    }

    /// A FAILURE THE RECORD CANNOT HOLD STILL REACHES THE WINDOW. The update folder's
    /// filesystem is full, so the save before the check fails at every pass and the
    /// record's count never rises past what it was (here 0, the status reporting
    /// its floor of 1), with its time from before this launch. The third such pass
    /// in a row is announced; a pass whose failure is recorded, or a success, ends
    /// the loop's own count, and the recovery follows.
    ///
    /// FAILS WITHOUT THE FIX: every pass read `failing_checks` 1 from a record last
    /// written before the launch, and nothing was ever announced.
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
        // No count: the record's is stale (1 here, through every pass) and the
        // loop's counts passes, so neither is the streak's.
        assert_eq!(body, "No space left on device (os error 28).");
        // Said once, not restated every pass.
        assert_eq!(notice.tick(unrecorded(full, 1, before)), None, "said once");
        assert_eq!(notice.tick(unrecorded(full, 1, before)), None, "said once");
        assert_eq!(
            notice.tick(pass("Up to date", 0, "2026-09-28T10:30:00Z")),
            Some((crate::HEALTH_RECOVERED_TITLE.to_string(), CLASS.to_string()))
        );
        // The loop's count starts again after any pass the record took.
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

    /// A HEALTHY COPY'S RESULTS ARE NEVER A WARNING, AND A REAL FAILURE STILL IS.
    /// The window's first pass of an up-to-date copy sends nothing, nor does an
    /// install or a single failed check; three failed checks in a row are announced
    /// under the check half's failing title, which the window reads back to its
    /// class; a changed reason restates quietly; and the recovery names the class
    /// it proves, which is what clears the window's row and latch.
    ///
    /// FAILS WITHOUT THE FIX: the loop sent every changed outcome — "Up to date"
    /// first — under a title the window takes for a failing check, latched for the
    /// launch, so the failure below was never shown.
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
        assert_eq!(
            body,
            "3 failed checks in a row: machine roster signature refused."
        );
        assert_eq!(
            notice.tick(pass("machine roster signature refused", 3, &t4)),
            None,
            "a pass that checked nothing (not due) says nothing twice"
        );
        let (title, body) = notice
            .tick(pass("machine roster signature refused", 4, &t4))
            .expect("a moved count is restated, as the macOS lane restates it");
        assert_eq!(title, crate::HEALTH_RESTATED_TITLE);
        assert!(body.starts_with("4 failed checks in a row: "), "{body}");
        let (title, body) = notice
            .tick(pass("below the signed minimum-build floor", 5, &t5))
            .expect("a changed reason is restated");
        assert_eq!(title, crate::HEALTH_RESTATED_TITLE);
        assert!(
            body.contains("below the signed minimum-build floor"),
            "{body}"
        );
        // The check that succeeds is recorded after the last failure was said.
        assert_eq!(
            notice.tick(pass("Up to date", 0, &t6)),
            Some((crate::HEALTH_RECOVERED_TITLE.to_string(), CLASS.to_string()))
        );
        assert_eq!(notice.tick(pass("Up to date", 0, &t6)), None);
        // A new episode speaks the failing title again.
        let (title, _) = notice
            .tick(pass("network down", 3, &t6))
            .expect("a new episode");
        assert_eq!(crate::health_failing_class(&title), Some(CLASS));
    }

    /// A streak recorded before this window started is on the status line already:
    /// a window launch does not re-announce it, but the streak's next failed check
    /// does.
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
        // And nothing announced means no recovery owed.
        let mut quiet = Announcer::new(START.into());
        assert_eq!(
            quiet.tick(pass("Up to date", 0, "2026-09-28T10:30:00Z")),
            None
        );
    }
}
