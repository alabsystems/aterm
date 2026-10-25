// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scratch plans at the common worker, park poll and retiring-watch boundaries.

use super::{
    AlabWatch, SetupWatch, WatchMaintenance, WatchReservation, WidenReview,
    maintain_setup_watch_with, read_setup_park_mark_with, run_reserved_alab_watch,
    wait_for_setup_park_change, widen_on_alab_signal, with_setup_watch,
};
use aterm_cli::macos_doctor::ProvisionalSetupWatchTest;
use std::cell::Cell;
use std::sync::atomic::Ordering;
use std::time::Duration;

struct Harness<'a> {
    root: &'a std::path::Path,
    setup: ProvisionalSetupWatchTest<'a>,
    watching: SetupWatch,
    failures: Cell<usize>,
    spawns: Cell<usize>,
    inline: Cell<usize>,
    runs: Cell<usize>,
    run_leases: Cell<usize>,
}

impl<'a> Harness<'a> {
    fn new(root: &'a std::path::Path) -> Self {
        let setup = ProvisionalSetupWatchTest::new(root).unwrap();
        setup.leave_permissions_pending().unwrap();
        Self {
            root,
            setup,
            watching: SetupWatch::new(),
            failures: Cell::new(0),
            spawns: Cell::new(0),
            inline: Cell::new(0),
            runs: Cell::new(0),
            run_leases: Cell::new(0),
        }
    }

    fn run_watch(&self, reservation: WatchReservation<'_>) {
        let mut watch = ScratchWatch { harness: self };
        run_reserved_alab_watch(reservation, || widen_on_alab_signal(&mut watch));
    }

    fn maintain(&self, how: WatchMaintenance) {
        maintain_setup_watch_with(
            &self.watching,
            how,
            || self.setup.widens(),
            |reservation| {
                self.spawns.set(self.spawns.get() + 1);
                if self.failures.get() > 0 {
                    self.failures.set(self.failures.get() - 1);
                    return Err(std::io::Error::other("injected watch spawn refusal"));
                }
                self.run_watch(reservation);
                Ok(())
            },
            |reservation| {
                assert_eq!(self.run_leases.get(), 0, "the finished run owns no leases");
                self.inline.set(self.inline.get() + 1);
                self.run_watch(reservation);
            },
        );
    }
}

struct ScratchWatch<'a, 'b> {
    harness: &'a Harness<'b>,
}

impl AlabWatch for ScratchWatch<'_, '_> {
    fn widens(&mut self) -> Result<bool, String> {
        self.harness.setup.widens()
    }
    fn wait(&mut self, _: Duration, _: Duration) {}
    fn review(&mut self) -> Result<WidenReview, String> {
        Ok(WidenReview::Applied)
    }
    fn look(&mut self) -> Result<Option<u64>, String> {
        // No write faults: the held old watch returns for Stop before any attempt.
        ProvisionalSetupWatchTest::at(self.harness.root).upgrade(None)
    }
    fn run(&mut self) {
        self.harness.setup.recover().unwrap();
        self.harness.runs.set(self.harness.runs.get() + 1);
    }
}

struct RunLeases<'a>(&'a Cell<usize>);
impl Drop for RunLeases<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

#[test]
fn watch_spawn_recovery_park_poll_retries_a_missing_watch_without_repeating_admission() {
    let root = aterm_tempfile::tempdir().unwrap();
    let h = Harness::new(root.path());
    let before = h.setup.snapshot().unwrap();
    h.failures.set(1);
    h.maintain(WatchMaintenance::Spawn);
    assert_eq!(h.spawns.get(), 1);
    assert_eq!(h.setup.snapshot().unwrap().observation, before.observation);
    let reads = Cell::new(0);
    let _ = wait_for_setup_park_change(
        || {
            read_setup_park_mark_with(
                || {
                    reads.set(reads.get() + 1);
                    h.setup.snapshot().unwrap().revision
                },
                || h.maintain(WatchMaintenance::Spawn),
            )
        },
        |_| h.runs.get() > 0,
        || reads.get() < 2,
        || {},
    );
    assert_eq!(
        h.spawns.get(),
        2,
        "the existing park poll must retry the watch"
    );
    assert_eq!(h.runs.get(), 1);
    assert_eq!(h.setup.snapshot().unwrap().revision, before.revision + 1);
}

#[test]
fn watch_spawn_recovery_manual_resume_rearms_a_watch_and_inline_fallback_has_no_leases() {
    for fail in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let h = Harness::new(root.path());
        h.setup.stop().unwrap();
        assert!(!h.setup.widens().unwrap());
        let resumed = h.setup.resume().unwrap();
        assert!(
            h.setup.widens().unwrap(),
            "Set up now preserves provisional origin"
        );
        h.failures.set(usize::from(fail));
        with_setup_watch(
            true,
            || {
                h.run_leases.set(h.run_leases.get() + 1);
                let _leases = RunLeases(&h.run_leases);
            },
            |how| h.maintain(how),
        );
        assert_eq!(
            h.runs.get(),
            1,
            "an accepted manual resume needs its ALab watch"
        );
        assert_eq!(h.spawns.get(), 1);
        assert_eq!(h.inline.get(), usize::from(fail));
        assert_eq!(h.run_leases.get(), 0);
        assert_eq!(h.setup.snapshot().unwrap().revision, resumed + 1);
    }
}

#[test]
fn watch_spawn_recovery_a_finished_resume_is_handed_back_to_the_retiring_old_watch() {
    let root = aterm_tempfile::tempdir().unwrap();
    let h = Harness::new(root.path());
    h.setup.stop().unwrap();
    let reservation = h.watching.reserve().unwrap();
    let resumed = Cell::new(false);
    let revision = Cell::new(0);
    run_reserved_alab_watch(reservation, || {
        let mut old = ScratchWatch { harness: &h };
        widen_on_alab_signal(&mut old);
        if !resumed.replace(true) {
            // Old watch read Stop; a new run enters and finishes before it releases.
            revision.set(h.setup.resume().unwrap());
            with_setup_watch(true, || {}, |how| h.maintain(how));
            assert_eq!(h.runs.get(), 0);
        }
    });
    assert_eq!(
        h.runs.get(),
        1,
        "the retiring watch owes the completed resume a recheck"
    );
    assert_eq!(
        h.spawns.get(),
        0,
        "the old watch can fulfill the handoff itself"
    );
    assert_eq!(h.setup.snapshot().unwrap().revision, revision.get() + 1);
    assert!(!h.watching.running.load(Ordering::Acquire));
}

#[test]
fn watch_spawn_recovery_stop_person_and_full_plans_create_no_extra_watch() {
    for terminal in ["Stop", "person", "full"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let h = Harness::new(root.path());
        match terminal {
            "Stop" => h.setup.stop().unwrap(),
            "person" => h.setup.choose().unwrap(),
            _ => {
                ProvisionalSetupWatchTest::at(root.path())
                    .upgrade(None)
                    .unwrap()
                    .unwrap();
            }
        }
        let before = h.setup.snapshot().unwrap().observation;
        with_setup_watch(true, || {}, |how| h.maintain(how));
        h.maintain(WatchMaintenance::Spawn);
        assert_eq!(h.spawns.get(), 0, "{terminal}");
        assert_eq!(h.inline.get(), 0, "{terminal}");
        assert_eq!(h.runs.get(), 0, "{terminal}");
        assert_eq!(
            h.setup.snapshot().unwrap().observation,
            before,
            "{terminal}"
        );
    }
}

#[test]
fn watch_spawn_recovery_an_ineligible_preflight_rechecks_a_newly_finished_resume() {
    let root = aterm_tempfile::tempdir().unwrap();
    let h = Harness::new(root.path());
    h.setup.stop().unwrap();
    let first = Cell::new(true);
    let resumed = Cell::new(0);
    maintain_setup_watch_with(
        &h.watching,
        WatchMaintenance::Spawn,
        || {
            let eligible = h.setup.widens();
            if first.replace(false) {
                assert!(!eligible.as_ref().unwrap());
                resumed.set(h.setup.resume().unwrap());
                with_setup_watch(true, || {}, |how| h.maintain(how));
            }
            eligible
        },
        |reservation| {
            h.spawns.set(h.spawns.get() + 1);
            h.run_watch(reservation);
            Ok(())
        },
        |_| panic!("the preflight was a spawned watch"),
    );
    assert_eq!(h.spawns.get(), 1);
    assert_eq!(h.runs.get(), 1);
    assert_eq!(h.setup.snapshot().unwrap().revision, resumed.get() + 1);
}

#[test]
fn watch_spawn_recovery_reserved_and_unwinding_workers_leave_one_reusable_slot() {
    let root = aterm_tempfile::tempdir().unwrap();
    let h = Harness::new(root.path());
    let reservation = h.watching.reserve().unwrap();
    maintain_setup_watch_with(
        &h.watching,
        WatchMaintenance::Spawn,
        || panic!("an existing watch needs no eligibility I/O"),
        |_| panic!("an existing watch needs no second worker"),
        |_| panic!("an existing watch needs no inline worker"),
    );
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_reserved_alab_watch(reservation, || panic!("injected watch unwind"));
    }));
    assert!(unwound.is_err());
    assert!(!h.watching.running.load(Ordering::Acquire));
    h.maintain(WatchMaintenance::Spawn);
    assert_eq!(h.spawns.get(), 1);
    assert_eq!(h.runs.get(), 1);
    assert!(!h.watching.running.load(Ordering::Acquire));
}
