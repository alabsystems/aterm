// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    ALAB_LOOK_FIRST, ALAB_LOOK_LONGEST, AlabWait, AlabWatch, alab_look_after, widen_on_alab_signal,
};
use std::collections::VecDeque;
use std::time::Duration;

#[test]
fn membership_wait_arms_before_its_level_read_and_retains_a_healthy_listener() {
    use super::{AlabMembershipWait, SetupWakes};
    use std::cell::Cell;
    let wakes = SetupWakes::new();
    let mut waiting = AlabMembershipWait::new(wakes.marks());
    let armed = Cell::new(false);
    assert!(waiting.wait(
        &wakes,
        Duration::from_secs(60),
        || {
            assert!(armed.get(), "a commit before arming must not be lost");
            true
        },
        || {
            armed.set(true);
            Some(())
        },
        |()| true,
    ));
    assert!(!waiting.wait(
        &wakes,
        Duration::ZERO,
        || false,
        || panic!("a healthy membership listener must not be restarted"),
        |()| true,
    ));
}

#[test]
fn a_lost_membership_listener_does_not_spin_on_its_own_notification() {
    use super::{AlabMembershipWait, SetupWakes};
    use std::cell::Cell;
    let wakes = SetupWakes::new();
    let mut waiting = AlabMembershipWait::new(wakes.marks());
    let starts = Cell::new(0);
    for _ in 0..2 {
        assert!(!waiting.wait(
            &wakes,
            Duration::ZERO,
            || false,
            || {
                starts.set(starts.get() + 1);
                wakes.raise_facts();
                Some(false)
            },
            |whole| *whole,
        ));
    }
    assert_eq!(
        starts.get(),
        2,
        "a failed listener may recover on its retry"
    );
}

#[test]
fn a_committed_membership_widens_without_a_network_event_or_timer_expiring() {
    use super::{AlabMembershipWait, SetupWakes};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Watch<'a> {
        wakes: &'a SetupWakes,
        member: &'a AtomicBool,
        waiting: AlabMembershipWait<()>,
        read: std::sync::mpsc::Sender<()>,
        runs: u32,
    }
    impl AlabWatch for Watch<'_> {
        fn widens(&mut self) -> Result<bool, String> {
            Ok(self.runs == 0)
        }
        fn wait(&mut self, why: AlabWait, _limit: Duration) {
            assert_eq!(why, AlabWait::Signal);
            assert!(self.waiting.wait(
                self.wakes,
                Duration::from_secs(60), // A hang detector, not a latency target.
                || {
                    let member = self.member.load(Ordering::SeqCst);
                    assert!(!member);
                    self.read.send(()).unwrap();
                    member
                },
                || Some(()),
                |()| true,
            ));
        }
        fn heard(&mut self) -> Result<bool, String> {
            Ok(self.member.load(Ordering::SeqCst))
        }
        fn look(&mut self) -> Result<Option<u64>, String> {
            assert!(self.member.load(Ordering::SeqCst));
            Ok(Some(9))
        }
        fn run(&mut self) {
            self.runs += 1;
        }
    }

    let wakes = SetupWakes::new();
    let member = AtomicBool::new(false);
    let (read, reading) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let committed = &member;
        let events = &wakes;
        scope.spawn(move || {
            reading.recv_timeout(Duration::from_secs(60)).unwrap();
            committed.store(true, Ordering::SeqCst);
            events.raise_facts();
        });
        let mut watch = Watch {
            wakes: &wakes,
            member: &member,
            waiting: AlabMembershipWait::new(wakes.marks()),
            read,
            runs: 0,
        };
        widen_on_alab_signal(&mut watch);
        assert_eq!(watch.runs, 1);
        assert_eq!(wakes.marks().all, 0, "no focus, access or Check wake");
    });
}

/// A watch whose answers come from a script; the last `widens` answer sticks.
#[derive(Default)]
struct Scripted {
    widens: VecDeque<Option<bool>>,
    /// Whether each look hears the signal; once they run out, every look hears it.
    heard: VecDeque<bool>,
    looks: VecDeque<Result<Option<u64>, String>>,
    waits: Vec<(AlabWait, Duration)>,
    runs: u32,
}

impl AlabWatch for Scripted {
    fn widens(&mut self) -> Result<bool, String> {
        if self.widens.len() > 1 {
            self.widens.pop_front().unwrap()
        } else {
            self.widens.front().copied().unwrap_or(Some(false))
        }
        .ok_or_else(|| "a pass is saving the records".to_string())
    }
    fn wait(&mut self, why: AlabWait, limit: Duration) {
        self.waits.push((why, limit));
    }
    fn heard(&mut self) -> Result<bool, String> {
        Ok(self.heard.pop_front().unwrap_or(true))
    }
    fn look(&mut self) -> Result<Option<u64>, String> {
        self.looks.pop_front().expect("a look the script answers")
    }
    fn run(&mut self) {
        self.runs += 1;
    }
}

#[test]
fn a_watch_looks_again_more_slowly_and_runs_the_widened_plan_once() {
    let mut watch = Scripted {
        widens: [Some(true)].into(),
        looks: [
            Ok(None),
            Err("a profile pass is running".into()),
            Ok(None),
            Ok(Some(9)),
        ]
        .into(),
        ..Scripted::default()
    };
    widen_on_alab_signal(&mut watch);
    let minute = Duration::from_secs(60);
    assert_eq!(
        watch.waits,
        [
            (AlabWait::Retry, ALAB_LOOK_FIRST),
            (AlabWait::Retry, minute),
            (AlabWait::Retry, 2 * minute),
        ],
        "the first look goes at once, then each waits longer"
    );
    assert_eq!(watch.runs, 1);
    assert!(watch.looks.is_empty());
}

/// The plan widens to the groups the ALab signal adds the moment a look hears it, the first
/// look at once as the watch starts, with nothing asked (design §6.1); a look that hears
/// nothing yet waits for the next.
#[test]
fn a_watch_widens_at_once_when_a_look_hears_the_signal() {
    let mut later = Scripted {
        widens: [Some(true)].into(),
        heard: [false].into(),
        looks: [Ok(Some(3))].into(),
        ..Scripted::default()
    };
    widen_on_alab_signal(&mut later);
    assert_eq!(later.waits, [(AlabWait::Signal, ALAB_LOOK_FIRST)]);
    assert_eq!(later.runs, 1);
    let mut first = Scripted {
        widens: [Some(true)].into(),
        looks: [Ok(Some(3))].into(),
        ..Scripted::default()
    };
    widen_on_alab_signal(&mut first);
    assert!(first.waits.is_empty(), "the first look went at once");
    assert_eq!(first.runs, 1, "widened and run with nothing asked");
}

#[test]
fn a_watch_ends_once_the_plan_is_no_longer_aterms_to_widen() {
    let mut watch = Scripted {
        widens: [Some(true), Some(true), Some(false)].into(),
        looks: [Ok(None), Ok(None)].into(),
        ..Scripted::default()
    };
    widen_on_alab_signal(&mut watch);
    assert_eq!(watch.waits.len(), 1);
    assert_eq!(watch.runs, 0);
    let mut unread = Scripted {
        widens: [None, Some(true), None, Some(false)].into(),
        looks: [Ok(None)].into(),
        ..Scripted::default()
    };
    widen_on_alab_signal(&mut unread);
    assert_eq!(
        unread.waits.len(),
        2,
        "records that cannot be read keep the watch"
    );
    assert!(
        unread.looks.is_empty(),
        "and take no look while they cannot"
    );
    let mut found_later = Scripted {
        widens: [None, None, Some(true)].into(),
        looks: [Ok(Some(4))].into(),
        ..Scripted::default()
    };
    widen_on_alab_signal(&mut found_later);
    assert_eq!(found_later.runs, 1);
    assert!(found_later.looks.is_empty());
    let mut idle = Scripted::default();
    widen_on_alab_signal(&mut idle);
    assert!(idle.waits.is_empty(), "nothing to widen, nothing to watch");
}

#[test]
fn a_watch_waits_at_most_half_an_hour_between_looks() {
    assert_eq!(alab_look_after(0), ALAB_LOOK_FIRST);
    assert_eq!(alab_look_after(5), Duration::from_secs(16 * 60));
    assert_eq!(alab_look_after(6), ALAB_LOOK_LONGEST);
    assert_eq!(alab_look_after(u32::MAX), ALAB_LOOK_LONGEST);
}

#[cfg(target_os = "macos")]
mod recovery {
    use super::*;
    use aterm_cli::macos_doctor::ProvisionalSetupWatchTest;
    use aterm_types::macos_setup::Selection;

    #[derive(Clone, Copy)]
    enum Replacement {
        Stop,
        OldStop(u64),
        Person,
    }

    struct RealWatch<'a> {
        setup: ProvisionalSetupWatchTest<'a>,
        faults: VecDeque<Option<(usize, bool)>>,
        replacement: Option<Replacement>,
        failed_read: bool,
        reads: usize,
        waits: usize,
        looks: usize,
        recovered: Vec<u64>,
    }

    impl<'a> RealWatch<'a> {
        fn new(root: &'a std::path::Path) -> Self {
            Self {
                setup: ProvisionalSetupWatchTest::new(root).unwrap(),
                faults: VecDeque::new(),
                replacement: None,
                failed_read: false,
                reads: 0,
                waits: 0,
                looks: 0,
                recovered: Vec::new(),
            }
        }
    }

    impl AlabWatch for RealWatch<'_> {
        fn widens(&mut self) -> Result<bool, String> {
            self.reads += 1;
            if std::mem::take(&mut self.failed_read) {
                assert_eq!(self.looks, 0, "a failed read authorizes no probe or write");
                let before = self.setup.snapshot().unwrap();
                let eligible = self.setup.widens_after_failed_read();
                let repaired = self.setup.snapshot().unwrap();
                assert_eq!(repaired.observation, before.observation);
                assert_eq!(repaired.permissions_allowed, before.permissions_allowed);
                eligible
            } else {
                self.setup.widens()
            }
        }

        fn wait(&mut self, _why: AlabWait, limit: Duration) {
            assert!(!limit.is_zero());
            self.waits += 1;
        }

        fn heard(&mut self) -> Result<bool, String> {
            Ok(true)
        }

        fn look(&mut self) -> Result<Option<u64>, String> {
            self.looks += 1;
            assert!(
                self.looks <= 3,
                "the watch must converge without a retry spin"
            );
            let upgraded = self.setup.upgrade(self.faults.pop_front().flatten());
            if let Some(replacement) = self.replacement.take() {
                assert!(upgraded.is_err());
                assert_eq!(
                    self.setup.snapshot().unwrap().plan.selection,
                    Selection::all()
                );
                match replacement {
                    Replacement::Stop => self.setup.stop().unwrap(),
                    Replacement::OldStop(revision) => self.setup.stop_revision(revision).unwrap(),
                    Replacement::Person => self.setup.choose().unwrap(),
                }
            }
            upgraded
        }

        fn run(&mut self) {
            self.recovered.push(self.setup.recover().unwrap());
            let resumed = self.setup.snapshot().unwrap();
            assert_eq!(resumed.plan.selection, Selection::all());
            assert!(!resumed.finished, "the widened stages are still owed");
            assert!(resumed.run_allowed, "recovery confirms real admission");
            assert_eq!(resumed.revision, *self.recovered.last().unwrap());
        }
    }

    #[test]
    fn watch_recovery_starts_after_a_real_upgrade_publication_reports_failure() {
        for fault in [(1, false), (3, true)] {
            let root = aterm_tempfile::tempdir().unwrap();
            let mut watch = RealWatch::new(root.path());
            let original = watch.setup.snapshot().unwrap().revision;
            watch.faults.push_back(Some(fault));
            widen_on_alab_signal(&mut watch);
            assert_eq!(
                watch.recovered.len(),
                1,
                "save {fault:?}: the published full run needs startup recovery in this process"
            );
            assert_eq!(
                watch.looks, 2,
                "retry only the owned admission after its failed publication"
            );
            assert_eq!(watch.waits, 1, "the first look goes at once");
            assert_eq!(watch.recovered[0], original + 1);
        }
    }

    #[test]
    fn watch_recovery_ignores_a_delayed_stop_of_its_completed_predecessor() {
        for fault in [(1, false), (3, false), (3, true)] {
            let root = aterm_tempfile::tempdir().unwrap();
            let mut watch = RealWatch::new(root.path());
            let original = watch.setup.snapshot().unwrap().revision;
            watch.faults.push_back(Some(fault));
            watch.replacement = Some(Replacement::OldStop(original));
            widen_on_alab_signal(&mut watch);
            assert_eq!(watch.recovered, vec![original + 1], "save {fault:?}");
            assert_eq!(watch.looks, 2, "the same owned admission must recover");
            assert_eq!(watch.waits, 1, "the first look goes at once");
        }
    }

    #[test]
    fn watch_recovery_retries_a_repaired_eligibility_read_without_granting_authority() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut watch = RealWatch::new(root.path());
        watch.failed_read = true;
        widen_on_alab_signal(&mut watch);
        assert_eq!(
            watch.recovered.len(),
            1,
            "a transient record read must not end the only in-process ALab watch"
        );
        assert!(watch.reads >= 2);
        assert_eq!(watch.looks, 1, "probe only after eligibility is readable");
        assert!(watch.waits >= 1);
    }

    #[test]
    fn watch_recovery_keeps_prepublication_retries_and_authoritative_replacements() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut watch = RealWatch::new(root.path());
        watch.faults.push_back(Some((0, false)));
        widen_on_alab_signal(&mut watch);
        assert_eq!(watch.recovered.len(), 1);
        assert_eq!(
            watch.looks, 2,
            "an unpublished failure may try the upgrade again"
        );

        for replacement in [Replacement::Stop, Replacement::Person] {
            let root = aterm_tempfile::tempdir().unwrap();
            let mut watch = RealWatch::new(root.path());
            watch.faults.push_back(Some((1, false)));
            watch.replacement = Some(replacement);
            widen_on_alab_signal(&mut watch);
            match replacement {
                Replacement::Stop => {
                    assert!(watch.recovered.is_empty(), "an explicit Stop holds setup");
                    assert_eq!(
                        watch.looks, 2,
                        "the retained ticket confirms its retirement"
                    );
                }
                Replacement::Person => {
                    assert!(watch.recovered.is_empty(), "the person's plan stays chosen");
                    assert_eq!(
                        watch.looks, 2,
                        "the retained ticket retires without upgrading the person's plan"
                    );
                    let chosen = watch.setup.snapshot().unwrap();
                    assert!(!chosen.provisional);
                    assert_ne!(chosen.plan.selection, Selection::all());
                }
                Replacement::OldStop(_) => unreachable!(),
            }
            assert!(!watch.setup.widens().unwrap());
        }
    }
}
