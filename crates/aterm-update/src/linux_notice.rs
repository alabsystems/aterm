// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! What the Linux background check tells the window about update health
//! ([`crate::HealthNotify`]): a check lane that keeps failing, its later reasons, and
//! its recovery — never an ordinary result.
//!
//! Until round six the loop handed the window EVERY change of its recorded outcome
//! under one title, from an empty start, so the first pass always sent one. The
//! window reads a title it does not know as a failing check ("a title it does not
//! know is the check half"), so every window launch of a healthy copy put up a
//! warning row — "Up to date", "aterm X is installed" — and latched the check kind
//! for the launch; the Linux loop never sent the recovery that clears that latch, so
//! the first REAL failure afterwards (a refused signature, the minimum-build floor,
//! the network down) was swallowed as "already said this launch" and reached only
//! the log.
//!
//! Now the loop speaks the macOS lane's words, on the same threshold
//! ([`crate::PERSISTENT_AFTER`] consecutive failed checks): the failing title
//! ([`crate::health_failing_title`]) once, a quiet restatement
//! ([`crate::HEALTH_RESTATED_TITLE`]) when the reason changes while it still fails,
//! and the recovery ([`crate::HEALTH_RECOVERED_TITLE`], naming the class it proves)
//! once the checks work again. A streak whose last failed check came before this
//! process started is history the status line already shows, not news.
//!
//! AND A FAILURE THE RECORD CANNOT HOLD IS COUNTED HERE. The record's
//! `failing_checks` rises only when the save after a check lands, so a failure that
//! comes before it — the update folder's filesystem full or read-only, a record that
//! will not read or was written by a newer aterm, settings that will not read — left
//! the count at most 1 at every pass, and the threshold hid it from the window for
//! good. The loop now counts its own consecutive unrecorded failures
//! ([`Record::Lost`]) and judges the larger of the two counts; a streak of its own
//! that reaches the threshold is news, since it happened in this process, whatever
//! time the record last kept. A single unrecorded failure is not: with a streak on
//! record from before this launch, that blip announced the old failure under the
//! blip's reason. And a pass that found another aterm updating the copy
//! ([`Record::Busy`]: its lock held past the wait, a download minutes long) is no
//! failure of the checks at all: it neither adds to the loop's count nor ends it, so
//! another process's long download announces nothing in every other window.
//!
//! Compiled into every test build, like `linux_trial`, so the macOS gate runs it.

/// The class a Linux check failure is announced under: the check half.
const CLASS: &str = "manifest";

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
    /// The reason last announced while failing; `None` = nothing announced.
    announced: Option<String>,
    /// Consecutive passes whose failure the record did not take.
    unrecorded: u32,
}

impl Announcer {
    pub(crate) fn new(started: String) -> Self {
        Self {
            started,
            announced: None,
            unrecorded: 0,
        }
    }

    /// The `(title, body)` owed to the window for `pass`, or `None`.
    pub(crate) fn tick(&mut self, pass: Pass<'_>) -> Option<(String, String)> {
        self.unrecorded = match pass.record {
            Record::Busy => return None,
            Record::Lost => self.unrecorded.saturating_add(1),
            Record::Kept => 0,
        };
        let failing_checks = pass.failing_checks.max(self.unrecorded);
        if failing_checks >= crate::PERSISTENT_AFTER {
            let news = self.unrecorded >= crate::PERSISTENT_AFTER
                || pass.updated_at >= self.started.as_str();
            return match &self.announced {
                None if news => {
                    self.announced = Some(pass.outcome.to_string());
                    Some((
                        crate::health_failing_title(CLASS).to_string(),
                        pass.outcome.to_string(),
                    ))
                }
                Some(said) if said != pass.outcome => {
                    self.announced = Some(pass.outcome.to_string());
                    Some((
                        crate::HEALTH_RESTATED_TITLE.to_string(),
                        pass.outcome.to_string(),
                    ))
                }
                _ => None,
            };
        }
        if failing_checks == 0 && self.announced.take().is_some() {
            return Some((crate::HEALTH_RECOVERED_TITLE.to_string(), CLASS.to_string()));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: &str = "2026-09-28T10:00:00Z";

    fn pass<'a>(outcome: &'a str, failing_checks: u32, updated_at: &'a str) -> Pass<'a> {
        Pass {
            outcome,
            failing_checks,
            updated_at,
            record: Record::Kept,
        }
    }

    /// A failed pass the record did not take: its count is the one from before.
    fn unrecorded<'a>(outcome: &'a str, failing_checks: u32, updated_at: &'a str) -> Pass<'a> {
        Pass {
            record: Record::Lost,
            ..pass(outcome, failing_checks, updated_at)
        }
    }

    /// A pass that found another aterm updating the copy: nothing was checked.
    fn busy<'a>(outcome: &'a str, failing_checks: u32, updated_at: &'a str) -> Pass<'a> {
        Pass {
            record: Record::Busy,
            ..pass(outcome, failing_checks, updated_at)
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
        assert!(
            notice.tick(unrecorded(blip, 5, before)).is_some(),
            "three of its own in a row are"
        );
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
        assert_eq!(body, full);
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
        let (t1, t2, t3, t4) = (t(1), t(2), t(3), t(4));
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
        assert_eq!(body, "machine roster signature refused");
        assert_eq!(
            notice.tick(pass("machine roster signature refused", 4, &t4)),
            None,
            "the same reason is not said twice"
        );
        let (title, _) = notice
            .tick(pass("below the signed minimum-build floor", 5, &t4))
            .expect("a changed reason is restated");
        assert_eq!(title, crate::HEALTH_RESTATED_TITLE);
        assert_eq!(
            notice.tick(pass("Up to date", 0, &t4)),
            Some((crate::HEALTH_RECOVERED_TITLE.to_string(), CLASS.to_string()))
        );
        assert_eq!(notice.tick(pass("Up to date", 0, &t4)), None);
        // A new episode speaks the failing title again.
        let (title, _) = notice
            .tick(pass("network down", 3, &t4))
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
