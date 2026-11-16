// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a pass's stages leave System State's rows reading, and what a read of the rows shows
//! while its slowest batch, the repositories', still runs: no row is hidden behind that read,
//! and no row a stage of a retry did not touch is read again.

use super::*;
use aterm_cli::macos_doctor::{StageEvent, WorkstationReadback, repositories_readback};
use aterm_types::macos_setup::{Access, Standing};

const REPOSITORIES: Subject = Subject::Setting(Setting::Repositories);

/// The subjects queued for a read, without starting it: the test's own "running" worker holds
/// them.
fn queued() -> Vec<Subject> {
    OBSERVE_QUEUE
        .lock()
        .unwrap()
        .next
        .take()
        .map_or_else(Vec::new, |request| request.subjects)
}

/// One stage of an owned pass of run 1, its start and its end, carrying `readback` as it ends.
fn stage_of_a_pass(setting: Setting, subjects: &[Subject], readback: &WorkstationReadback) {
    let empty = WorkstationReadback::EMPTY;
    for running in [true, false] {
        setup_stage(StageEvent {
            revision: 1,
            settings: &[setting],
            subjects,
            running,
            readback: if running { &empty } else { readback },
        });
    }
}

/// The Repositories stage's own run reads the GitHub repositories row as it ends
/// ([`repositories_readback`]): that reading is the row's, dated from before the run read
/// anything, and neither the stage's end nor the pass's end reads the row, or asks GitHub,
/// again. NEGATIVE CONTROL: a stage that read nothing back has the row read again.
#[test]
fn a_repositories_stage_readback_needs_no_reread() {
    use aterm_machines::model::{Effect, ItemReport, Outcome};

    let _globals = globals();
    let _io = TestIo::install();
    let _queue = ObserverDispatchReset;
    // The ends' refreshes queue behind this "running" worker; the test takes them.
    OBSERVE_QUEUE.lock().unwrap().running = true;
    let acquired = ItemReport {
        id: "init.github.native@tester".into(),
        family: "github".into(),
        title: "GitHub repositories from synced development credentials".into(),
        outcome: Outcome::Ok,
        from: None,
        to: None,
        why: None,
        revert: None,
        effect: Effect::Now,
        detail: None,
        layer: None,
        class: None,
        explain: None,
        stood_aside: None,
        waits_on: None,
        essential: false,
        elapsed_ms: None,
    };
    let read_at = READ_AT - 600;
    let readback = repositories_readback(&[acquired], read_at);
    let mut plan = left_as_is();
    plan.selection.set(Setting::Repositories, true);
    {
        let mut rows = OBSERVED_ROWS.lock().unwrap();
        rows.plan = Some(plan.clone());
        rows.vault_url = Some(plan.vault_url.trim().to_owned());
        rows.rows = vec![Observation::unknown(REPOSITORIES)];
    }
    *SETUP_STOP.lock().unwrap() = None;
    set_setup_run(Some(1));
    let empty = WorkstationReadback::EMPTY;
    for read_back in [true, false] {
        OBSERVE_QUEUE.lock().unwrap().next = None;
        let ended = if read_back { &readback } else { &empty };
        with_setup_activity(|| {
            stage_of_a_pass(Setting::Repositories, &[REPOSITORIES], ended);
        });
        let queued = queued();
        let read: Vec<Observation> = OBSERVED_ROWS
            .lock()
            .unwrap()
            .rows
            .iter()
            .filter(|row| row.subject == REPOSITORIES)
            .cloned()
            .collect();
        if read_back {
            assert_eq!(read.len(), 1, "{read:?}");
            assert_eq!(
                (read[0].standing, read[0].read_at()),
                (Standing::InPlace, Some(read_at)),
                "the stage's reading is the row's, dated from before its run"
            );
            assert!(
                !queued.contains(&REPOSITORIES),
                "a row its stage read back is not read again: {queued:?}"
            );
        } else {
            assert!(
                queued.contains(&REPOSITORIES),
                "CONTROL: a stage that read nothing back has its row read again: {queued:?}"
            );
        }
    }
    set_setup_run(None);
}

/// A retry pass that ran its Repositories stage alone (every other stage done or parked) reads
/// again, as it ends, only that stage's row: the Access rows, the Mac settings and every other
/// row keep their readings rather than go back to being read behind each retry. CONTROL: a pass
/// whose Permissions stage ran too reads the Access rows again.
#[test]
fn a_retry_pass_end_reads_only_what_its_stages_touched() {
    let _globals = globals();
    let _io = TestIo::install();
    let _queue = ObserverDispatchReset;
    OBSERVE_QUEUE.lock().unwrap().running = true;
    let access: Vec<Subject> = Access::ALL.map(Subject::Access).to_vec();
    let mut plan = left_as_is();
    plan.selection.set(Setting::Repositories, true);
    {
        let mut rows = OBSERVED_ROWS.lock().unwrap();
        rows.plan = Some(plan.clone());
        rows.vault_url = Some(plan.vault_url.trim().to_owned());
    }
    *SETUP_STOP.lock().unwrap() = None;
    set_setup_run(Some(1));
    for permissions in [false, true] {
        OBSERVE_QUEUE.lock().unwrap().next = None;
        with_setup_activity(|| {
            stage_of_a_pass(
                Setting::Repositories,
                &[REPOSITORIES],
                &WorkstationReadback::EMPTY,
            );
            if permissions {
                stage_of_a_pass(Setting::Permissions, &access, &WorkstationReadback::EMPTY);
            }
        });
        let queued = queued();
        for subject in Subject::all() {
            let ran = subject == REPOSITORIES || (permissions && access.contains(&subject));
            assert_eq!(
                queued.contains(&subject),
                ran,
                "{subject:?} (the Permissions stage ran: {permissions}): {queued:?}"
            );
        }
    }
    set_setup_run(None);
}

/// A read of System State's rows hands each batch over as it completes, and each is kept and
/// published at once ([`ObservedBatches::keep`]): the Access rows, read in this process, show
/// while the repositories' read, the slow one that asks GitHub, still runs. CONTROL: the
/// repositories' row is not read before that read ends, and it reads once it does.
#[test]
fn access_rows_publish_while_the_repositories_read_runs() {
    struct Reader(Option<std::thread::JoinHandle<()>>);
    impl Drop for Reader {
        fn drop(&mut self) {
            release_readings();
            if let Some(reader) = self.0.take() {
                let _ = reader.join();
            }
        }
    }

    let _globals = globals();
    let _io = TestIo::install();
    let _queue = ObserverDispatchReset;
    let fda = Subject::Access(Access::FullDiskAccess);
    let mut plan = left_as_is();
    plan.selection.set(Setting::Repositories, true);
    let (choices, generation) = {
        let mut rows = OBSERVED_ROWS.lock().unwrap();
        rows.plan = Some(plan.clone());
        rows.vault_url = Some(plan.vault_url.trim().to_owned());
        rows.rows.clear();
        (rows.choices, rows.generation)
    };
    let in_place = |subject| Observation {
        standing: Standing::InPlace,
        ..Observation::unknown(subject)
    };
    change(|script| {
        script.reads.clear();
        script.hold = true;
        script.ahead = vec![in_place(fda)];
        script.readings = [vec![in_place(REPOSITORIES)]].into();
    });
    let read_plan = plan.clone();
    let mut reader = Reader(Some(std::thread::spawn(move || {
        observe_rows(ObserveRequest {
            plan: Some(read_plan),
            choices,
            subjects: vec![fda, REPOSITORIES],
            forced: false,
            pending_repositories: false,
        });
    })));
    wait_until("the Access row's batch is kept", || {
        setup_readings()
            .observed
            .iter()
            .any(|seen| seen.subject == fda && seen.standing == Standing::InPlace)
    });
    let shown = setup_readings();
    {
        let script = script();
        assert_eq!(script.reads.len(), 1, "one read hands both batches over");
        assert!(
            script.reads[0].2,
            "the read asks GitHub of the repositories"
        );
        assert!(script.hold, "the repositories' read still runs");
    }
    assert!(
        shown.observed_generation > generation,
        "the Access row's batch is published as it is kept"
    );
    assert!(
        !shown
            .observed
            .iter()
            .any(|seen| seen.subject == REPOSITORIES),
        "CONTROL: the repositories' row is not read before its read ends"
    );
    release_readings();
    reader.0.take().unwrap().join().unwrap();
    let shown = setup_readings();
    for subject in [fda, REPOSITORIES] {
        assert!(
            shown
                .observed
                .iter()
                .any(|seen| seen.subject == subject && seen.standing == Standing::InPlace),
            "{subject:?}: {:?}",
            shown.observed
        );
    }
}
