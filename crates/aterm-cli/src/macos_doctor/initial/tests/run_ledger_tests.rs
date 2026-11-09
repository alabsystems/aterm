// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The run's ledger: a crash resumes once with no press, a second crash parks the stage while
//! the others go on, and a stage that runs past the hang limit parks.

use super::*;

/// A profile root no quit latches ([`ledger::quitting`]): a begin there is not cut off.
const UNLATCHED: &str = "/nowhere/aterm-ledger-tests-unlatched";

/// One launch's pass, under profile.lock as aterm's pass runs (unless the test holds it
/// already), which a crash in a stage ends as the process ending there would: the lock goes
/// with it. A launch is a new process, which no quit of the last one latches
/// ([`ledger::quitting`]).
fn launch(host: &mut Fake, root: &Path) {
    ledger::relaunched(root);
    let _pass = HeldLock::try_take(root).ok().flatten();
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
        ..ledger::Live::default()
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

    // A software update resumes the same admitted run, not a newly authorized plan.
    let old = ledger::SoftwareEpoch {
        build: 100,
        commit: [0x11; 20],
    };
    let newer = ledger::SoftwareEpoch {
        build: 200,
        commit: [0x22; 20],
    };
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    let mut host = Fake {
        software_epoch: Some(old),
        crash: Some(Stage::Spotlight),
        ..Fake::default()
    };
    for _ in 0..3 {
        launch(&mut host, root.path());
    }
    assert_eq!(runs(&host.calls, Stage::Spotlight), 2);
    assert_eq!(row(root.path(), Stage::Spotlight), parked);
    let admitted = admission_at(root.path()).unwrap().unwrap();
    let saved = progress(root.path()).unwrap().unwrap();
    let records = observation_at(root.path()).unwrap().records;
    let held = std::fs::read(root.path().join(ledger::LEDGER)).unwrap();
    host.crash = None;
    for epoch in [
        Some(old),
        None,
        Some(ledger::SoftwareEpoch { build: 99, ..newer }),
        Some(ledger::SoftwareEpoch {
            build: 100,
            ..newer
        }),
        Some(ledger::SoftwareEpoch { build: 200, ..old }),
    ] {
        host.software_epoch = epoch;
        let before = host.calls.len();
        launch(&mut host, root.path());
        assert!(
            host.calls[before..].is_empty(),
            "{epoch:?} is not newer code"
        );
        assert_eq!(admission_at(root.path()).unwrap(), Some(admitted.clone()));
        assert_eq!(progress(root.path()).unwrap(), Some(saved));
        assert_eq!(observation_at(root.path()).unwrap().records, records);
        assert_eq!(
            std::fs::read(root.path().join(ledger::LEDGER)).unwrap(),
            held
        );
    }
    host.software_epoch = Some(newer);
    let before = host.calls.len();
    launch(&mut host, root.path());
    assert_eq!(
        host.calls[before..],
        [Stage::Spotlight],
        "positively newer code resumes only the unfinished stage without Check again"
    );
    let after = progress(root.path()).unwrap().unwrap();
    assert_eq!(after.revision, saved.revision);
    assert_eq!(after.done, saved.done | Stage::Spotlight.bit());
    assert!(after.admitted && after.completed);
    let after = observation_at(root.path()).unwrap().records;
    assert_eq!(after.plan, records.plan);
    assert_eq!(after.stop_request, records.stop_request);
    assert_eq!(after.person_access, records.person_access);
    assert!(finished_at(root.path()).unwrap());

    // A scoped Caps pass cannot consume the new code on behalf of another parked stage.
    let root = aterm_tempfile::tempdir().unwrap();
    let revision = authorize_at(root.path(), &Plan::default())
        .unwrap()
        .unwrap();
    let mut host = Fake {
        software_epoch: Some(old),
        pending: Some(Stage::Workstation),
        crash: Some(Stage::Spotlight),
        ..Fake::default()
    };
    for _ in 0..2 {
        launch(&mut host, root.path());
    }
    assert_eq!(runs(&host.calls, Stage::Spotlight), 2);
    assert_eq!(row(root.path(), Stage::Spotlight), parked);
    assert_eq!(
        input_setup_scope_at(root.path()).unwrap(),
        InputSetupScope::Pending(revision)
    );
    let before = ledger::load(root.path(), revision).ledger;
    let mut caps = Selection::none();
    caps.set(Setting::CapsLock, true);
    host.software_epoch = Some(newer);
    host.pending = None;
    host.crash = None;
    let start = host.calls.len();
    {
        let _profile = HeldLock::take(root.path()).unwrap();
        execute_initial_for_revision_target(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
            PassTarget::Inputs(caps),
        )
        .unwrap()
        .unwrap();
    }
    assert_eq!(host.calls[start..], [Stage::Workstation]);
    assert_eq!(host.only.last(), Some(&Some(caps)));
    assert_eq!(
        input_activation::revision_at(root.path()).unwrap(),
        Some(revision)
    );
    let after = ledger::load(root.path(), revision).ledger;
    assert_eq!(
        after.attempt(Stage::Spotlight),
        before.attempt(Stage::Spotlight)
    );
    assert_eq!(
        after.record()["software"][Stage::Spotlight as usize],
        before.record()["software"][Stage::Spotlight as usize],
        "the scoped pass leaves the sibling's software attribution untouched"
    );
    assert_eq!(
        after.crashed & Stage::Spotlight.bit(),
        before.crashed & Stage::Spotlight.bit()
    );
    assert_eq!(row(root.path(), Stage::Spotlight), parked);
    let start = host.calls.len();
    launch(&mut host, root.path());
    assert_eq!(runs(&host.calls[start..], Stage::Spotlight), 1);
    assert_eq!(progress(root.path()).unwrap().unwrap().revision, revision);
    assert!(finished_at(root.path()).unwrap());

    // An unattributed legacy ledger gets one adoption, not evidence of a fictitious old build.
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    let mut host = Fake {
        software_epoch: Some(old),
        crash: Some(Stage::Spotlight),
        ..Fake::default()
    };
    for _ in 0..3 {
        launch(&mut host, root.path());
    }
    let saved = progress(root.path()).unwrap().unwrap();
    let mut legacy: Value =
        aterm_json::from_slice(&std::fs::read(root.path().join(ledger::LEDGER)).unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("software");
    for stage in legacy
        .as_object_mut()
        .unwrap()
        .get_mut("stages")
        .unwrap()
        .as_array_mut()
        .unwrap()
    {
        stage.as_object_mut().unwrap().remove("software");
    }
    legacy
        .as_object_mut()
        .unwrap()
        .insert("format".into(), Value::from(1u64));
    write_json(&root.path().join(ledger::LEDGER), &legacy).unwrap();
    host.software_epoch = None;
    let start = host.calls.len();
    launch(&mut host, root.path());
    assert!(
        host.calls[start..].is_empty(),
        "unknown code cannot adopt a budget"
    );
    host.software_epoch = Some(newer);
    for _ in 0..3 {
        launch(&mut host, root.path());
    }
    assert_eq!(
        runs(&host.calls[start..], Stage::Spotlight),
        2,
        "legacy adoption is spent once"
    );
    assert_eq!(row(root.path(), Stage::Spotlight), parked);
    assert_eq!(progress(root.path()).unwrap(), Some(saved));
}

/// Control: a stage recorded running while the hold that began it has profile.lock, in this
/// boot, is running, not resumed; the same record with the lock free, from another boot, or
/// while another hold has the lock, is aterm stopped there.
#[test]
fn a_stage_runs_only_under_the_hold_that_began_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    let mut host = Fake {
        crash: Some(Stage::Spotlight),
        ..Fake::default()
    };
    launch(&mut host, root.path());

    let read = |active: bool, boot: Option<u128>| {
        // The lock file still names the hold the stopped pass took, as when it still runs.
        let rows = snapshot_at_when(root.path(), Plan::default(), |_| ledger::Live {
            active,
            boot,
            now: ledger::now_secs(),
            ..ledger::Live::default()
        })
        .unwrap()
        .rows;
        row_in(&rows, Stage::Spotlight)
    };
    let here = ledger::boot_session();
    assert_eq!(
        read(true, here).0,
        "running",
        "the live pass's stage, under its hold"
    );
    let resumed = ("pending".to_owned(), ledger::RESUMING.to_owned());
    assert_eq!(read(false, here), resumed, "the lock free");
    if let Some(here) = here {
        assert_eq!(read(true, Some(here ^ 1)), resumed, "another boot");
    }
    {
        let _other = HeldLock::take(root.path()).unwrap();
        assert_eq!(
            row(root.path(), Stage::Spotlight),
            resumed,
            "another hold has the lock"
        );
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
/// says it runs; one past the limit parks it, while its row says it still runs. aterm stopping in it then counts as one stop: the
/// next launch resumes it once with no press, and when it wedges again, the launch after leaves
/// it parked and runs the stages after it. Before the ledger it read running with no limit, and
/// the next launch ran it again and nothing after it.
#[test]
fn a_hung_stage_resumes_once_then_parks_while_the_others_run() {
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
    let still = (
        "running".to_owned(),
        ledger::overdue_words(Stage::Repositories),
    );
    assert_eq!(
        row_in(&late, Stage::Repositories),
        still,
        "past the limit, in the process it wedged"
    );

    let before = host.calls.len();
    launch(&mut host, root.path());
    let later = &host.calls[before..];
    assert_eq!(
        runs(later, Stage::Repositories),
        1,
        "resumed once, with no press"
    );
    let [_, late] = <[Vec<Row>; 2]>::try_from(std::mem::take(&mut host.wedged_rows)).unwrap();
    assert_eq!(row_in(&late, Stage::Repositories), still, "wedged again");

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

    let old = ledger::SoftwareEpoch {
        build: 100,
        commit: [0x11; 20],
    };
    let newer = ledger::SoftwareEpoch {
        build: 200,
        commit: [0x22; 20],
    };
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    let mut host = Fake {
        software_epoch: Some(old),
        wedge: Some((Stage::Repositories, root.path().into())),
        ..Fake::default()
    };
    for _ in 0..3 {
        launch(&mut host, root.path());
    }
    assert_eq!(runs(&host.calls, Stage::Repositories), 2);
    assert_eq!(row(root.path(), Stage::Repositories), parked);
    let saved = progress(root.path()).unwrap().unwrap();
    let admitted = admission_at(root.path()).unwrap().unwrap();
    let records = observation_at(root.path()).unwrap().records;
    host.wedge = None;
    for epoch in [
        Some(old),
        None,
        Some(ledger::SoftwareEpoch { build: 99, ..newer }),
        Some(ledger::SoftwareEpoch {
            build: 100,
            ..newer
        }),
        Some(ledger::SoftwareEpoch { build: 200, ..old }),
    ] {
        host.software_epoch = epoch;
        let before = host.calls.len();
        launch(&mut host, root.path());
        assert!(
            host.calls[before..].is_empty(),
            "{epoch:?} cannot clear a hang park"
        );
        assert_eq!(admission_at(root.path()).unwrap(), Some(admitted.clone()));
        assert_eq!(progress(root.path()).unwrap(), Some(saved));
        assert_eq!(observation_at(root.path()).unwrap().records, records);
    }
    host.software_epoch = Some(newer);
    let before = host.calls.len();
    launch(&mut host, root.path());
    assert_eq!(
        host.calls[before..],
        [Stage::Repositories],
        "newer code repairs only the hung unfinished stage in the existing admission"
    );
    let after = progress(root.path()).unwrap().unwrap();
    assert_eq!(after.revision, saved.revision);
    assert_eq!(after.done, saved.done | Stage::Repositories.bit());
    assert!(after.admitted && after.completed);
    let after = observation_at(root.path()).unwrap().records;
    assert_eq!(after.plan, records.plan);
    assert_eq!(after.stop_request, records.stop_request);
    assert_eq!(after.person_access, records.person_access);
    assert!(finished_at(root.path()).unwrap());
}

/// aterm stops once in Spotlight. More than an hour later, in the same boot, a reading of the run
/// is taken while another hold has profile.lock (`aterm doctor --apply` or `aterm init` in a
/// terminal, a choice's save, the Widgets follow-up): the stage is not alive under that hold, so
/// the reading parks nothing, and the next launch resumes it with no press. Before, any hold
/// kept the stage alive, the reading parked it as hung, and it waited for Check again.
#[test]
fn a_crashed_stage_read_under_anothers_lock_resumes_with_no_press() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    let mut host = Fake {
        crash: Some(Stage::Spotlight),
        ..Fake::default()
    };
    launch(&mut host, root.path());
    {
        let _other = HeldLock::take(root.path()).unwrap();
        let rows = rows_after(root.path(), 2 * ledger::HANG_LIMIT);
        assert_eq!(
            row_in(&rows, Stage::Spotlight),
            ("pending".into(), ledger::RESUMING.into()),
            "another hold says nothing of a stage a stopped aterm left"
        );
    }
    host.crash = None;
    let before = host.calls.len();
    launch(&mut host, root.path());
    assert_eq!(
        runs(&host.calls[before..], Stage::Spotlight),
        1,
        "one crash resumes once with no press; the row reads {:?}",
        row(root.path(), Stage::Spotlight)
    );
}

/// A clone runs when the lid closes; two hours of sleep later it goes on, and the page reads the
/// run: the hang limit counts awake time, so the stage is alive, not wedged. The person quits
/// (or an update hands off) before the clone ends, and the next launch resumes it with no press.
/// A stage a reading did park as hung (an hour awake) that a quit cuts off resumes once too, and
/// parks the second time.
#[test]
fn a_stage_alive_across_a_sleep_resumes_after_a_clean_quit() {
    let t0 = 1_800_000_000;
    let stamp = |at, awake| ledger::Stamp {
        boot: Some(7),
        at,
        awake: Some(awake),
        run: Some(9),
        software: None,
    };
    let mut ledger = ledger::Ledger::of(1);
    assert_eq!(
        ledger.begin(Stage::Repositories, 0, stamp(t0, 100), Path::new(UNLATCHED)),
        ledger::Admit::Run
    );
    let woke = ledger::Live {
        active: true,
        holder: Some(9),
        boot: Some(7),
        now: t0 + 2 * ledger::HANG_LIMIT,
        awake: Some(100 + 60),
    };
    assert_eq!(
        ledger.overdue(Stage::Repositories, woke),
        None,
        "two hours asleep and a minute awake"
    );
    assert_eq!(
        ledger.verdict(Stage::Repositories, 0, None, woke),
        ledger::Verdict::Running
    );
    assert!(ledger.interrupt());
    ledger.recover(0, None);
    assert_eq!(
        ledger.begin(
            Stage::Repositories,
            0,
            stamp(woke.now + 60, 200),
            Path::new(UNLATCHED)
        ),
        ledger::Admit::Run,
        "a stage that slept is not a wedge"
    );

    // An hour awake with no end, twice: each time a reading parks it and a quit cuts it off.
    let mut awake = 200;
    for resumes in [true, false] {
        let late = ledger::Live {
            awake: Some(awake + ledger::HANG_LIMIT),
            ..woke
        };
        let at = ledger
            .overdue(Stage::Repositories, late)
            .expect("an hour awake");
        assert!(ledger.mark_hung(Stage::Repositories, at));
        assert!(ledger.interrupt());
        ledger.recover(0, None);
        awake += ledger::HANG_LIMIT + 60;
        let next = ledger.begin(
            Stage::Repositories,
            0,
            stamp(t0, awake),
            Path::new(UNLATCHED),
        );
        if resumes {
            assert_eq!(
                next,
                ledger::Admit::Run,
                "a hung stage a quit cut off resumes once"
            );
        } else {
            assert_eq!(
                next,
                ledger::Admit::Parked(ledger::Park::Hung),
                "the second time it waits for Check again"
            );
        }
    }
}

/// At each launch an update's handoff notes the stage it would cut off and is then refused (a
/// tab still starting, a Commit not written), and then aterm crashes in that stage: the refusal
/// took the note back, so the crashes count and the stage parks after two. Before, the handoff
/// noted at its start and left the note when refused, and the crashes never parked the stage.
#[test]
fn refused_handoffs_keep_the_two_crash_park() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    let mut host = Fake {
        crash: Some(Stage::Spotlight),
        ..Fake::default()
    };
    for launch_number in 0..3 {
        launch(&mut host, root.path());
        // While the stage ran, before aterm stopped in it, a handoff noted it and was refused.
        let noted = ledger::interrupt_at(root.path()).unwrap();
        assert_eq!(
            noted,
            launch_number < 2,
            "the handoff notes the running stage"
        );
        assert_eq!(
            ledger::refused_at(root.path()).unwrap(),
            noted,
            "and takes it back"
        );
    }
    assert_eq!(
        runs(&host.calls, Stage::Spotlight),
        2,
        "stopped twice, then parked"
    );
    assert_eq!(
        row(root.path(), Stage::Spotlight),
        (
            WAITING.to_owned(),
            ledger::parked_words(Stage::Spotlight, ledger::Park::Crashed)
        )
    );

    let mut ledger = ledger::Ledger::of(1);
    let stamp = ledger::Stamp::default();
    for _ in 0..2 {
        assert_eq!(
            ledger.begin(Stage::Spotlight, 0, stamp, Path::new(UNLATCHED)),
            ledger::Admit::Run
        );
        assert!(ledger.interrupt());
        assert!(ledger.refused());
        ledger.recover(0, None);
    }
    assert_eq!(
        ledger.parked(Stage::Spotlight),
        Some(ledger::Park::Crashed),
        "{ledger:?}"
    );
}

/// A newer aterm left its ledger (a later format) of an older run; this build runs a later
/// revision. The newer ledger is stale like any other run's, so this run keeps its own, and a
/// crash loop in Spotlight still parks after two crashes. Before, the newer file froze the
/// ledger of every revision, and Spotlight ran at every launch.
#[test]
fn a_newer_ledger_of_an_old_run_still_breaks_crash_loops() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    let mut newer = ledger::Ledger::of(999).record();
    newer
        .as_object_mut()
        .unwrap()
        .insert("format".into(), Value::from(ledger::LEDGER_FORMAT + 1));
    write_json(&root.path().join(ledger::LEDGER), &newer).unwrap();
    let mut host = Fake {
        crash: Some(Stage::Spotlight),
        ..Fake::default()
    };
    for _ in 0..4 {
        launch(&mut host, root.path());
    }
    assert_eq!(
        runs(&host.calls, Stage::Spotlight),
        2,
        "stopped twice, then parked"
    );

    // One of this run's own revision stays frozen: nothing of it is used or written.
    let revision = progress(root.path()).unwrap().unwrap().revision;
    let mut own = ledger::Ledger::of(revision).record();
    own.as_object_mut()
        .unwrap()
        .insert("format".into(), Value::from(ledger::LEDGER_FORMAT + 1));
    write_json(&root.path().join(ledger::LEDGER), &own).unwrap();
    let before = std::fs::read(root.path().join(ledger::LEDGER)).unwrap();
    assert!(ledger::load(root.path(), revision).frozen);
    ledger::update(root.path(), revision, |ledger| {
        ledger.begin(
            Stage::Spotlight,
            0,
            ledger::Stamp::default(),
            Path::new(UNLATCHED),
        )
    })
    .unwrap();
    assert_eq!(
        std::fs::read(root.path().join(ledger::LEDGER)).unwrap(),
        before
    );
}

/// ⌘Q lands as the pass is about to begin a stage: the quit notes the stages running (none),
/// and shutdown goes on with the setup worker in it. That stage does not begin, so the quit
/// cuts off nothing it did not note: two quick quits (a Quit & Reopen, ⌘Q just after launch)
/// park nothing, and the next launch runs every stage once. Before, the stage began after the
/// note, the quit cut it off unnoted, the next launch counted that as a crash, and the second
/// parked it with no crash at all.
#[test]
fn a_stage_begun_while_aterm_quits_is_no_crash() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    let mut host = Fake {
        // A stage that ran would end with the process: the quit goes.
        crash: Some(Stage::Workstation),
        quit_at_begin: Some(root.path().into()),
        ..Fake::default()
    };
    for quit in 0..2 {
        launch(&mut host, root.path());
        assert!(
            ledger::quitting(root.path()),
            "quit {quit}: the quit latched the run"
        );
    }
    assert_eq!(host.calls, [], "no stage began while aterm quit");
    assert_ne!(
        row(root.path(), Stage::Workstation),
        (
            WAITING.to_owned(),
            ledger::parked_words(Stage::Workstation, ledger::Park::Crashed)
        ),
        "two quits and no crash park nothing"
    );

    host.crash = None;
    host.quit_at_begin = None;
    launch(&mut host, root.path());
    for stage in [
        Stage::Workstation,
        Stage::Spotlight,
        Stage::Permissions,
        Stage::Repositories,
        Stage::Accounts,
    ] {
        assert_eq!(runs(&host.calls, stage), 1, "{} ran once", stage.name());
    }
    assert!(finished_at(root.path()).unwrap());
}

/// A refused handoff's take-back lifts the quit's latch with its notes: aterm goes on, and its
/// pass begins stages again.
#[test]
fn a_refused_handoff_lets_stages_begin_again() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    ledger::interrupt_at(root.path()).unwrap();
    assert!(ledger::quitting(root.path()));
    ledger::refused_at(root.path()).unwrap();
    assert!(!ledger::quitting(root.path()));
    let mut host = Fake::default();
    let _pass = HeldLock::try_take(root.path())
        .unwrap()
        .expect("the pass's hold");
    execute_initial(&mut host, root.path(), &mut None).unwrap();
    assert!(finished_at(root.path()).unwrap());
}

/// A stage wedged in this process's own pass runs past the hang limit: a reading parks it as
/// hung, so a quit then counts as one stop, but its row says it still runs and ends by itself
/// or when aterm quits, with no Check again, since the wedged pass holds profile.lock and the
/// setup slot and no press reaches it. Check again pressed meanwhile (for another stage)
/// changes nothing of it. Before, the row read "Took over an hour …; Check again tries again",
/// and Check again lifted a park the next reading set again from the same begin.
#[test]
fn a_stage_wedged_in_this_process_says_it_still_runs_with_no_check_again() {
    let root = aterm_tempfile::tempdir().unwrap();
    let revision = authorize_at(root.path(), &Plan::default())
        .unwrap()
        .expect("a revision");
    // The live pass, wedged in Repositories under its own hold.
    let _pass = HeldLock::try_take(root.path())
        .unwrap()
        .expect("the pass's hold");
    let stamp = ledger::Stamp {
        boot: ledger::boot_session(),
        at: ledger::now_secs(),
        awake: None,
        run: HeldLock::profile_holder(root.path()),
        software: None,
    };
    assert_eq!(
        ledger::update(root.path(), revision, |ledger| {
            ledger.begin(Stage::Repositories, 0, stamp, Path::new(UNLATCHED))
        })
        .unwrap(),
        ledger::Admit::Run
    );
    let still = (
        "running".to_owned(),
        ledger::overdue_words(Stage::Repositories),
    );
    let late = 2 * ledger::HANG_LIMIT;
    assert_eq!(
        row_in(&rows_after(root.path(), late), Stage::Repositories),
        still,
        "two hours on, it still runs"
    );
    assert!(
        still
            .1
            .ends_with(aterm_types::macos_setup::StageRow::STILL_RUNNING_TAIL)
            && !still
                .1
                .ends_with(aterm_types::macos_setup::StageRow::PARKED_TAIL),
        "it ends by itself or when aterm quits, and names no Check again: {}",
        still.1
    );
    assert!(
        ledger::load(root.path(), revision)
            .ledger
            .parked(Stage::Repositories)
            == Some(ledger::Park::Hung),
        "the reading parked it, so a quit now counts as one stop"
    );
    ledger::retry_parked_at(root.path()).unwrap();
    assert_eq!(
        row_in(&rows_after(root.path(), late + 1), Stage::Repositories),
        still,
        "Check again changes nothing of it"
    );
}

/// A reader that writes nothing (`aterm status`) sees a stage the live pass has run past the
/// hang limit as running past it, and leaves the ledger as it was: the park stays the first
/// marking reading's. NEGATIVE CONTROL: the marking reading of the same run parks it.
#[test]
fn a_read_only_reading_parks_no_hung_stage() {
    let root = aterm_tempfile::tempdir().unwrap();
    let revision = authorize_at(root.path(), &Plan::default())
        .unwrap()
        .expect("a revision");
    let _pass = HeldLock::try_take(root.path())
        .unwrap()
        .expect("the pass's hold");
    let stamp = ledger::Stamp {
        boot: ledger::boot_session(),
        at: ledger::now_secs(),
        awake: None,
        run: HeldLock::profile_holder(root.path()),
        software: None,
    };
    ledger::update(root.path(), revision, |ledger| {
        ledger.begin(Stage::Repositories, 0, stamp, Path::new(UNLATCHED))
    })
    .unwrap();
    let late = |active| ledger::Live {
        active,
        boot: ledger::boot_session(),
        now: ledger::now_secs() + 2 * ledger::HANG_LIMIT,
        ..ledger::Live::default()
    };
    let parked = || {
        ledger::load(root.path(), revision)
            .ledger
            .parked(Stage::Repositories)
    };
    let rows = snapshot_at_reading(root.path(), Plan::default(), late, Hung::Leave)
        .unwrap()
        .rows;
    assert_eq!(
        row_in(&rows, Stage::Repositories),
        (
            "running".to_owned(),
            ledger::overdue_words(Stage::Repositories)
        ),
        "it reads as running past the limit"
    );
    assert_eq!(parked(), None, "the read-only reading wrote no park");
    snapshot_at_reading(root.path(), Plan::default(), late, Hung::Mark).unwrap();
    assert_eq!(
        parked(),
        Some(ledger::Park::Hung),
        "the marking reading parks it"
    );
}

/// Before aterm.app's first launch writes a record the setup has not begun, though its rows show
/// the default plan's stages pending; a saved plan begins it.
#[test]
fn a_setup_begins_with_its_first_record() {
    let root = aterm_tempfile::tempdir().unwrap();
    let fresh = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(!fresh.begun, "no records: not begun");
    assert!(
        fresh.rows.iter().all(|row| row.status == "pending"),
        "{:?}",
        fresh.rows
    );
    authorize_at(root.path(), &Plan::default()).unwrap();
    assert!(snapshot_at(root.path(), Plan::default()).unwrap().begun);

    let installed = aterm_stamp::Stamp {
        build_number: "100",
        git_commit_full: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ..aterm_stamp::Stamp::UNSTAMPED
    };
    let epoch = ledger::SoftwareEpoch {
        build: 100,
        commit: [0xaa; 20],
    };
    assert_eq!(ledger::SoftwareEpoch::from_stamp(&installed), Some(epoch));
    for invalid in [
        aterm_stamp::Stamp {
            git_dirty: "true",
            ..installed
        },
        aterm_stamp::Stamp {
            git_dirty: "unknown",
            ..installed
        },
        aterm_stamp::Stamp {
            git_commit_full: "unknown",
            ..installed
        },
        aterm_stamp::Stamp {
            git_commit_full: "0000000000000000000000000000000000000000",
            ..installed
        },
        aterm_stamp::Stamp {
            build_number: "0",
            ..installed
        },
        aterm_stamp::Stamp {
            build_number: "unknown",
            ..installed
        },
    ] {
        assert_eq!(ledger::SoftwareEpoch::from_stamp(&invalid), None);
    }

    // Corrupt attribution never erases an actual parked budget or turns into legacy adoption.
    let mut host = Fake {
        crash: Some(Stage::Spotlight),
        software_epoch: Some(epoch),
        ..Fake::default()
    };
    launch(&mut host, root.path());
    launch(&mut host, root.path());
    launch(&mut host, root.path());
    let revision = progress(root.path()).unwrap().unwrap().revision;
    let parked = ledger::load(root.path(), revision).ledger;
    assert_eq!(parked.parked(Stage::Spotlight), Some(ledger::Park::Crashed));
    let next = ledger::SoftwareEpoch {
        build: 200,
        commit: [0xbb; 20],
    };
    let unread_slot = json!({ "build": 200, "commit": "unknown" });
    let mut unread_slots = vec![Value::from("unattributed"); Stage::ALL.len()];
    unread_slots[Stage::Spotlight as usize] = unread_slot;
    for software in [
        None,
        Some(Value::Null),
        Some(json!([])),
        Some(json!(unread_slots)),
    ] {
        let mut record = parked.record();
        let object = record.as_object_mut().unwrap();
        match software {
            Some(value) => {
                object.insert("software".into(), value);
            }
            None => {
                object.remove("software");
            }
        }
        write_json(&root.path().join(ledger::LEDGER), &record).unwrap();
        let loaded = ledger::load(root.path(), revision);
        assert!(!loaded.frozen);
        let unread = loaded.ledger;
        assert_eq!(unread.revision, parked.revision);
        assert_eq!(unread.attempts, parked.attempts);
        assert_eq!(
            (unread.running, unread.crashed, unread.hung, unread.quit),
            (parked.running, parked.crashed, parked.hung, parked.quit)
        );
        assert_eq!(
            unread.software[Stage::Spotlight as usize],
            ledger::Software::Unread
        );
        assert_eq!(
            ledger::Ledger::parse(&unread.record()).unwrap(),
            unread,
            "a canonical rewrite preserves unread attribution"
        );
        assert!(
            !ledger::update(root.path(), revision, |ledger| ledger.software_for(
                Stage::Spotlight,
                0,
                Some(next),
                Path::new(UNLATCHED)
            ))
            .unwrap()
        );
        assert_eq!(ledger::load(root.path(), revision).ledger, unread);
        assert!(ledger::retry_parked_at(root.path()).unwrap());
        assert_eq!(
            ledger::load(root.path(), revision).ledger.software[Stage::Spotlight as usize],
            ledger::Software::Unattributed
        );
        assert_eq!(
            ledger::update(root.path(), revision, |ledger| ledger.begin(
                Stage::Spotlight,
                0,
                ledger::Stamp {
                    software: Some(next),
                    ..ledger::Stamp::default()
                },
                Path::new(UNLATCHED)
            ))
            .unwrap(),
            ledger::Admit::Run
        );
        assert_eq!(
            ledger::load(root.path(), revision).ledger.software[Stage::Spotlight as usize],
            ledger::Software::Known(next)
        );
    }
}
