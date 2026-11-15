// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a converge pass's end reads of the Secrets and GitHub repositories rows
//! ([`converge_pass_ended`], [`read_unsettled_vault_rows_again`]).

use super::*;
use aterm_types::macos_setup::Standing;

const SECRETS: Subject = Subject::Setting(Setting::Secrets);
const REPOSITORIES: Subject = Subject::Setting(Setting::Repositories);

fn row(subject: Subject, standing: Standing) -> Observation {
    Observation {
        standing,
        ..Observation::unknown(subject)
    }
}

/// The Secrets row as a reading under a held state lock leaves it.
fn out_of_sync() -> Observation {
    Observation {
        now: Some("Out of sync".into()),
        ..row(SECRETS, Standing::Waiting)
    }
}

/// The GitHub repositories row as a reading that failed leaves it: no standing, and why.
fn unread_repositories() -> Observation {
    Observation {
        detail: Some("the vault's state is busy".into()),
        ..row(REPOSITORIES, Standing::Unknown)
    }
}

/// The readings an earlier look left for `plan`.
fn readings_left(plan: &Plan, rows: Vec<Observation>) {
    let mut observed = OBSERVED_ROWS.lock().unwrap();
    observed.plan = Some(plan.clone());
    observed.vault_url = Some(plan.vault_url.trim().to_owned());
    observed.rows = rows;
}

/// Take a test's readings away, so no later test's converge end finds them.
fn forget_readings() {
    OBSERVED_ROWS.lock().unwrap().rows.clear();
}

fn standing_of(subject: Subject) -> Option<Standing> {
    OBSERVED_ROWS
        .lock()
        .unwrap()
        .rows
        .iter()
        .find(|row| row.subject == subject)
        .map(|row| row.standing)
}

/// A converge pass ends and everything it queued is read.
fn pass_ends() {
    converge_pass_ended();
    wait_until("the converge end's readings end", observe_idle);
}

/// A finished setup (no Repositories stage pending) whose Secrets row reads Waiting, whose
/// GitHub repositories row was not read, or both: the converge pass's end reads exactly
/// those rows again, as a refresh, and nothing once they read settled.
/// NEGATIVE CONTROL: with `observe_rows` returning on `Ok(None)` as before, no case reads.
#[test]
fn a_converge_pass_end_reads_a_finished_setups_unsettled_vault_rows_again() {
    let _globals = globals();
    let plan = Plan::default();
    for (name, left, owed) in [
        (
            "secrets-waiting",
            vec![out_of_sync(), row(REPOSITORIES, Standing::InPlace)],
            vec![SECRETS],
        ),
        (
            "repositories-unread",
            vec![row(SECRETS, Standing::InPlace), unread_repositories()],
            vec![REPOSITORIES],
        ),
        (
            "secrets-unread",
            vec![
                row(SECRETS, Standing::Unknown),
                row(REPOSITORIES, Standing::InPlace),
            ],
            vec![SECRETS],
        ),
        (
            "both",
            vec![out_of_sync(), unread_repositories()],
            vec![SECRETS, REPOSITORIES],
        ),
    ] {
        let _io = TestIo::install();
        readings_left(&plan, left);
        change(|script| {
            script.readings = [owed
                .iter()
                .map(|subject| row(*subject, Standing::InPlace))
                .collect()]
            .into();
        });
        pass_ends();
        assert_eq!(
            script().reads,
            [(plan.clone(), owed.clone(), true)],
            "exactly the unsettled rows: {name}"
        );
        assert_eq!(script().read_checks, [false], "a refresh: {name}");
        for subject in [SECRETS, REPOSITORIES] {
            assert_eq!(standing_of(subject), Some(Standing::InPlace), "{name}");
        }
        pass_ends();
        assert_eq!(script().reads.len(), 1, "settled rows end it: {name}");
        forget_readings();
    }
}

/// A row that still waits is read again at each pass's end; one aterm does not manage, or
/// that reads any settled or person's standing, never is.
/// NEGATIVE CONTROL: a rule that reads both rows at every pass's end reads here too.
#[test]
fn a_converge_pass_end_reads_no_row_that_is_not_left_waiting_or_unread() {
    let _globals = globals();
    for (name, plan, left) in [
        (
            "both-in-place",
            Plan::default(),
            vec![
                row(SECRETS, Standing::InPlace),
                row(REPOSITORIES, Standing::InPlace),
            ],
        ),
        (
            "a-person's",
            Plan::default(),
            vec![
                row(SECRETS, Standing::NeedsYou),
                row(REPOSITORIES, Standing::Failed),
            ],
        ),
        ("never-read", Plan::default(), Vec::new()),
        (
            "repositories-not-managed",
            left_as_is(),
            vec![row(SECRETS, Standing::InPlace), unread_repositories()],
        ),
    ] {
        let _io = TestIo::install();
        readings_left(&plan, left.clone());
        pass_ends();
        assert!(script().reads.is_empty(), "{name}");
        assert_eq!(OBSERVED_ROWS.lock().unwrap().rows, left, "{name}");
        forget_readings();
    }
    // Still waiting after its reading: owed another at the next pass's end.
    let _io = TestIo::install();
    let plan = Plan::default();
    readings_left(&plan, vec![out_of_sync()]);
    change(|script| script.readings = [vec![out_of_sync()], vec![out_of_sync()]].into());
    pass_ends();
    pass_ends();
    assert_eq!(
        script().reads,
        [
            (plan.clone(), vec![SECRETS], true),
            (plan, vec![SECRETS], true)
        ]
    );
    forget_readings();
}

/// What a pass's end can change of the kept readings: the rows, the last useful values, the
/// generation the page compares, the display history's epoch (each step of it is a write of
/// the setup ledger) and the unconfirmed marks.
fn kept() -> (Vec<Observation>, Vec<Observation>, u64, u64, Vec<Subject>) {
    let rows = OBSERVED_ROWS.lock().unwrap();
    (
        rows.rows.clone(),
        rows.last_known.clone(),
        rows.generation,
        rows.history_epoch,
        rows.unconfirmed.clone(),
    )
}

/// A row can wait or stay unread for days (a Mac no admin has approved, a vault file only a
/// person can mend) and is read at every pass's end meanwhile. A reading that says what the
/// row's kept one says changes nothing: the kept reading keeps its date, and neither the
/// generation nor the history moves. One that says anything else is taken, dated as read.
/// NEGATIVE CONTROL: without `says_what_is_kept`'s skip in `observe_rows_as` each pass dates
/// both rows anew and moves the generation and the history's epoch.
#[test]
fn a_converge_pass_end_reading_that_says_what_the_row_says_changes_nothing() {
    let _globals = globals();
    let _io = TestIo::install();
    let plan = Plan::default();
    let same = || vec![out_of_sync(), unread_repositories()];
    readings_left(&plan, same());
    change(|script| script.readings = [same(), same(), same(), same()].into());
    // The first pass dates the rows; the second keeps the waiting one as the row's last
    // useful value, once.
    pass_ends();
    change(|script| script.now_secs = READ_AT + 600);
    pass_ends();
    let settled = kept();
    assert_eq!(
        settled.0,
        [as_read(&out_of_sync()), as_read(&unread_repositories())],
        "dated by the first reading"
    );
    for pass in 2..4 {
        change(|script| script.now_secs = READ_AT + 600 * pass);
        pass_ends();
        assert_eq!(kept(), settled, "pass {pass}");
    }
    assert_eq!(script().reads.len(), 4, "read at every pass's end");
    // The vault came into sync: that reading is taken and dated, and the row left unread
    // beside it keeps its own.
    let in_sync = row(SECRETS, Standing::InPlace);
    change(|script| {
        script.now_secs = READ_AT + 3000;
        script.readings = [vec![in_sync.clone(), unread_repositories()]].into();
    });
    pass_ends();
    let now = kept();
    assert_eq!(
        now.0,
        [
            in_sync.stamped(READ_AT + 3000),
            as_read(&unread_repositories())
        ]
    );
    assert!(now.2 > settled.2, "a changed reading moves the generation");
    forget_readings();
}

/// A GitHub repositories row that waits is read again only once the vault's mark moved since
/// its last reading: a wait that is not the vault's is not asked of GitHub at every pass.
/// NEGATIVE CONTROL: without the mark's gate the first two passes read the row too.
#[test]
fn a_converge_pass_end_reads_waiting_repositories_only_once_the_vaults_mark_moved() {
    use aterm_secrets::client::MaterializationMark;
    let _globals = globals();
    let _io = TestIo::install();
    let mark = |byte: u8| MaterializationMark::from_cache_key(&format!("{byte:02x}").repeat(32));
    let plan = Plan::default();
    let waiting = row(REPOSITORIES, Standing::Waiting);
    readings_left(
        &plan,
        vec![row(SECRETS, Standing::InPlace), waiting.clone()],
    );
    // The row's last reading kept the vault's mark, as `observe_rows` does before it reads.
    script().vault_mark = mark(1);
    assert!(token_mark_moved(|| true));
    pass_ends();
    assert!(script().reads.is_empty(), "an unchanged mark");
    script().vault_mark = None;
    pass_ends();
    assert!(script().reads.is_empty(), "a mark that cannot be read");
    // The vault changed: what the row waited for may be there.
    script().vault_mark = mark(2);
    change(|script| script.readings = [vec![waiting]].into());
    pass_ends();
    assert_eq!(
        script().reads,
        [(plan, vec![REPOSITORIES], true)],
        "the moved mark reads the waiting row alone"
    );
    pass_ends();
    assert_eq!(script().reads.len(), 1, "once per move");
    forget_readings();
}

/// An unfinished run whose Repositories stage is pending keeps its own reading: the stage's
/// row alone, under its admission, and the Secrets row is not read beside it however it reads.
/// NEGATIVE CONTROL: a re-read that ignores the pending admission reads the Secrets row here.
#[test]
fn a_converge_pass_end_keeps_a_pending_repositories_stages_own_reading() {
    use aterm_cli::macos_doctor::InitialSetupAdmission;
    let _globals = globals();
    let _io = TestIo::install();
    let plan = Plan::default();
    let admitted = InitialSetupAdmission {
        plan: plan.clone(),
        revision: 9,
        completed: false,
    };
    readings_left(&plan, vec![out_of_sync(), unread_repositories()]);
    change(|script| {
        script.pending_repositories = [Ok(Some(admitted.clone())), Ok(Some(admitted))].into();
        script.readings = [vec![row(REPOSITORIES, Standing::Waiting)]].into();
    });
    let seen = SETUP_WAKES.marks();
    pass_ends();
    assert_eq!(
        script().reads,
        [(plan, vec![REPOSITORIES], true)],
        "the pending stage's row alone"
    );
    assert_eq!(standing_of(SECRETS), Some(Standing::Waiting));
    assert_eq!(standing_of(REPOSITORIES), Some(Standing::Waiting));
    assert_eq!(
        SETUP_WAKES.marks(),
        seen,
        "a row still waiting wakes no run"
    );
    forget_readings();
}

/// A pass with no native reading owed still saves the private fences. Dropping that save
/// (the old path) leaves no write here. Publishing a stale snapshot cannot replace a newer
/// reading or a newer fence: both are negative controls for the in-memory update.
#[test]
fn a_settled_converge_end_keeps_the_private_fences_current() {
    let _globals = globals();
    let _io = TestIo::install();
    let dock = Subject::Setting(Setting::Dock);
    let seen = row(dock, Standing::InPlace).stamped(READ_AT);
    let old = aterm_cli::macos_doctor::Fence::at(1, 10);
    let new = aterm_cli::macos_doctor::Fence::at(2, 20);
    // Give this synthetic reading the same stage history a real completed read has.
    // The process-wide history can include Dock changes from earlier test cases.
    let epoch = {
        let mut activity = SETUP_ACTIVITY.lock().unwrap();
        activity.mark(&[Setting::Dock], &[dock], true);
        activity.mark(&[Setting::Dock], &[dock], false);
        activity.epoch
    };
    {
        let mut rows = OBSERVED_ROWS.lock().unwrap();
        rows.remembered_looked = true;
        rows.rows = vec![seen.clone()];
        rows.fences = vec![(dock, old)];
        rows.read_epochs = vec![(dock, epoch - 1)];
    }
    pass_ends();
    assert_eq!(
        script().remembered_writes,
        [Vec::new()],
        "CONTROL: a reading from before the stage is not kept"
    );
    change(|script| script.remembered_writes.clear());
    OBSERVED_ROWS.lock().unwrap().read_epochs = vec![(dock, epoch)];
    pass_ends();
    assert_eq!(script().remembered_writes, [vec![(seen.clone(), old)]]);
    change(|script| {
        script.remembered_writes.clear();
        script.pending_repositories = [Err("no pending-stage reading".into())].into();
    });
    pass_ends();
    assert_eq!(
        script().remembered_writes,
        [vec![(seen.clone(), old)]],
        "a pending-stage lookup error does not prevent the fence check"
    );
    let before = [(seen.clone(), old)];
    let after = [(seen.clone(), new)];
    let mut rows = OBSERVED_ROWS.lock().unwrap();
    rows.advance_fences(&before, &after);
    assert_eq!(rows.fences, [(dock, new)]);
    rows.advance_fences(&before, &[(seen.clone(), old)]);
    assert_eq!(
        rows.fences,
        [(dock, new)],
        "a changed fence belongs to its newer reading"
    );
    rows.fences = vec![(dock, old)];
    rows.rows[0] = row(dock, Standing::Differs).stamped(READ_AT + 1);
    rows.advance_fences(&before, &after);
    assert_eq!(
        rows.fences,
        [(dock, old)],
        "a changed reading keeps its own fence"
    );
}
