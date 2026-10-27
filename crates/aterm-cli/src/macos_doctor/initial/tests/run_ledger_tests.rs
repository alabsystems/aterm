// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The run's ledger: a crash resumes once with no press, a second crash parks the stage while
//! the others go on, and a stage that runs past the hang limit parks.

use super::*;

/// One launch's pass, which a crash in a stage ends as the process ending there would.
fn launch(host: &mut Fake, root: &Path) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        execute_initial(host, root, &mut None)
    }));
}

/// `stage`'s row as a reading of the run says it now.
fn row(root: &Path, stage: Stage) -> (String, String) {
    row_in(&status_at(root, Plan::default()).unwrap().2, stage)
}

fn row_in(rows: &[Row], stage: Stage) -> (String, String) {
    rows.iter()
        .find(|row| row.name == stage.name())
        .map(|row| (row.status.clone(), row.detail.clone()))
        .unwrap_or_else(|| panic!("{} has a row: {rows:?}", stage.name()))
}

/// How often `stage` ran in `calls`.
fn runs(calls: &[Stage], stage: Stage) -> usize {
    calls.iter().filter(|each| **each == stage).count()
}

/// The run's rows read `after` seconds from now, as a reading by a live launch takes them.
pub(super) fn rows_after(root: &Path, after: u64) -> Vec<Row> {
    snapshot_at_when(root, Plan::default(), |active| ledger::Live {
        active,
        boot: ledger::boot_session(),
        now: ledger::now_secs() + after,
    })
    .unwrap()
    .rows
}

/// The rows a wedged stage's run reads: now, and two hours on.
pub(super) fn wedged_rows(root: &Path) -> Vec<Vec<Row>> {
    vec![
        rows_after(root, 0),
        rows_after(root, 2 * ledger::HANG_LIMIT),
    ]
}

/// aterm ends in Spotlight at every launch. The first launch's end resumes at the next with no
/// press; the second parks Spotlight, and every other stage runs once and completes. Check again
/// then runs it. Before the ledger, every launch ran Spotlight again and no later stage ran.
#[test]
fn a_crash_resumes_once_and_a_second_parks_while_the_others_complete() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    let mut host = Fake {
        crash: Some(Stage::Spotlight),
        ..Fake::default()
    };

    launch(&mut host, root.path());
    assert_eq!(runs(&host.calls, Stage::Spotlight), 1);
    assert_eq!(
        row(root.path(), Stage::Spotlight),
        ("pending".into(), ledger::RESUMING.into()),
        "with the lock free, the stage aterm stopped in resumes"
    );

    launch(&mut host, root.path());
    assert_eq!(
        runs(&host.calls, Stage::Spotlight),
        2,
        "resumed once, with no press"
    );
    let parked = (
        WAITING.to_owned(),
        ledger::parked_words(Stage::Spotlight, ledger::Park::Crashed),
    );
    assert_eq!(row(root.path(), Stage::Spotlight), parked);

    let before = host.calls.len();
    launch(&mut host, root.path());
    let later = &host.calls[before..];
    assert_eq!(runs(later, Stage::Spotlight), 0, "a parked stage waits");
    let done = progress(root.path()).unwrap().unwrap().done;
    for stage in [
        Stage::Workstation,
        Stage::Permissions,
        Stage::Repositories,
        Stage::Accounts,
    ] {
        assert_eq!(runs(&host.calls, stage), 1, "{} ran once", stage.name());
        assert_ne!(done & stage.bit(), 0, "{} completed", stage.name());
    }
    assert_eq!(done & Stage::Spotlight.bit(), 0);
    assert_eq!(row(root.path(), Stage::Spotlight), parked);
    assert!(!finished_at(root.path()).unwrap());

    // Check again lifts the park: the next launch runs the stage, and the run finishes.
    assert!(ledger::retry_parked_at(root.path()).unwrap());
    host.crash = None;
    let before = host.calls.len();
    launch(&mut host, root.path());
    assert_eq!(host.calls[before..], [Stage::Spotlight]);
    assert!(finished_at(root.path()).unwrap());
}

/// Control: a stage recorded running while profile.lock is held, in this boot, is running, not
/// resumed; the same record with the lock free, or from another boot, is aterm stopped there.
#[test]
fn a_stage_running_under_the_held_lock_is_not_resumed() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    let mut host = Fake {
        crash: Some(Stage::Spotlight),
        ..Fake::default()
    };
    launch(&mut host, root.path());

    let read = |active: bool, boot: Option<u128>| {
        let rows = snapshot_at_when(root.path(), Plan::default(), |_| ledger::Live {
            active,
            boot,
            now: ledger::now_secs(),
        })
        .unwrap()
        .rows;
        row_in(&rows, Stage::Spotlight)
    };
    let here = ledger::boot_session();
    assert_eq!(read(true, here).0, "running", "the live pass's stage");
    let resumed = ("pending".to_owned(), ledger::RESUMING.to_owned());
    assert_eq!(read(false, here), resumed, "the lock free");
    if let Some(here) = here {
        assert_eq!(read(true, Some(here ^ 1)), resumed, "another boot");
    }
    {
        let _live = HeldLock::take(root.path()).unwrap();
        assert_eq!(row(root.path(), Stage::Spotlight).0, "running");
    }
    assert_eq!(row(root.path(), Stage::Spotlight), resumed);
}

/// A quit or an update's handoff that cuts a stage off is no crash: the stage resumes at every
/// next launch, never parks.
#[test]
fn a_quit_is_not_a_crash() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    let mut host = Fake {
        crash: Some(Stage::Spotlight),
        ..Fake::default()
    };
    for _ in 0..3 {
        launch(&mut host, root.path());
        assert!(ledger::interrupt_at(root.path()).unwrap());
        assert_eq!(
            row(root.path(), Stage::Spotlight),
            ("pending".into(), ledger::RESUMING.into())
        );
    }
    assert_eq!(
        runs(&host.calls, Stage::Spotlight),
        3,
        "each launch resumed it"
    );
}

/// The process wedges in Repositories with profile.lock held. A reading before the hang limit
/// says it runs; one past the limit parks it, and the next launch leaves it parked and runs the
/// stages after it. Before the ledger it read running with no limit, and the next launch ran it
/// again and nothing after it.
#[test]
fn a_hung_stage_parks_and_the_next_launch_runs_the_others() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    let mut host = Fake {
        wedge: Some((Stage::Repositories, root.path().into())),
        ..Fake::default()
    };
    {
        let _profile = HeldLock::take(root.path()).unwrap();
        launch(&mut host, root.path());
    }
    let [now, late] = <[Vec<Row>; 2]>::try_from(std::mem::take(&mut host.wedged_rows)).unwrap();
    assert_eq!(
        row_in(&now, Stage::Repositories).0,
        "running",
        "within the limit"
    );
    let parked = (
        WAITING.to_owned(),
        ledger::parked_words(Stage::Repositories, ledger::Park::Hung),
    );
    assert_eq!(row_in(&late, Stage::Repositories), parked, "past the limit");

    let before = host.calls.len();
    launch(&mut host, root.path());
    let later = &host.calls[before..];
    assert_eq!(runs(later, Stage::Repositories), 0, "a parked stage waits");
    assert_eq!(runs(later, Stage::Accounts), 1, "the stage after it runs");
    assert_eq!(row(root.path(), Stage::Repositories), parked);

    assert!(ledger::retry_parked_at(root.path()).unwrap());
    host.wedge = None;
    let before = host.calls.len();
    launch(&mut host, root.path());
    assert_eq!(host.calls[before..], [Stage::Repositories]);
    assert!(finished_at(root.path()).unwrap());
}
