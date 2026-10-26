// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;

const NOW: i64 = 1_790_000_000;

struct Fixture {
    _root: aterm_tempfile::TempDir,
    store: Store,
    path: PathBuf,
    row: Row,
    who: ProcessLogin,
    credential: PathBuf,
    edits: Cell<usize>,
}

fn apply(operation: i64, state: &mut State, logins: &mut Logins) -> Vec<Event> {
    match operation {
        1 => note_login_end(state, logins, "main", NOW),
        2 => note_signed_back(state, logins, "main", NOW),
        3 => {
            note_served_login(logins, "main", NOW);
            Vec::new()
        }
        4 => {
            note_login_notice(logins, "main", 1, NOW);
            Vec::new()
        }
        5 => {
            let mut events = note_signed_back(state, logins, "main", NOW);
            events.extend(prove(state, "main", NOW));
            events.extend(note_session(state, "session-one", "main", None, NOW));
            events
        }
        6 | 7 => {
            let mut events: Vec<_> = prove(state, "main", NOW).into_iter().collect();
            events.extend(note_session(state, "session-one", "main", None, NOW));
            events
        }
        _ => unreachable!(),
    }
}

impl Fixture {
    fn new(cli: Cli, operation: i64) -> Self {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let store = Store::at_for(root.path(), cli, &root.path().join("home"));
        let dir = root.path().join("login");
        let credential = dir.join(cli.connector().stores[0]);
        std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
        std::fs::write(&credential, b"private credential fixture").unwrap();
        let row = Row {
            label: "main".into(),
            dir: Some(dir.to_str().unwrap().into()),
            email: Some("owner@example.test".into()),
            vendor: Vendor {
                region: (cli == Cli::Kimi).then_some(Region::Global),
                ..Vendor::default()
            },
            ..Row::default()
        };
        let path = root.path().join("accounts.toml");
        std::fs::write(
            &path,
            append_rows_for(cli, "", std::slice::from_ref(&row)).unwrap(),
        )
        .unwrap();
        store
            .transact_logins(NOW - 1, &mut None, |state, logins| {
                let events = note_renewed(
                    state,
                    logins,
                    "main",
                    None,
                    row.dir.as_deref().unwrap(),
                    None,
                    NOW - 10 * DAY_S,
                );
                state.active = Some("main".into());
                state.proven = operation == 7;
                if matches!(operation, 2 | 5) {
                    note_login_end(state, logins, "main", NOW - 1);
                }
                Ok(events)
            })
            .unwrap();
        std::fs::rename(store.wake_path(), root.path().join("setup-wake")).unwrap();
        let raw = Roster::load_for(cli, &path).unwrap();
        let bound = store.bound(&raw, NOW).unwrap();
        let row = bound.row("main").unwrap().clone();
        Self {
            _root: root,
            store,
            path,
            who: ProcessLogin {
                login: row.dir.clone(),
                default_home: true,
                stored_login: true,
            },
            row,
            credential,
            edits: Cell::new(0),
        }
    }

    fn record(&self, operation: i64) -> Result<Vec<Event>, String> {
        self.store.observe_login_within(
            &[],
            NOW,
            LoginObservation {
                roster_path: &self.path,
                row: &self.row,
                who: &self.who,
            },
            |state, logins| {
                self.edits.set(self.edits.get() + 1);
                Ok(apply(operation, state, logins))
            },
        )
    }

    fn records(&self) -> [Option<Vec<u8>>; 5] {
        [
            self.store.state_file(),
            self.store.logins_file(),
            self.store.ledger_path(),
            self.path.clone(),
            self.store.wake_path(),
        ]
        .map(|path| std::fs::read(path).ok())
    }
}

struct Obstruction {
    path: PathBuf,
    retained: Option<PathBuf>,
}

impl Obstruction {
    fn new(path: PathBuf) -> Self {
        let retained = path.exists().then(|| {
            let retained = path.with_extension("retained");
            std::fs::rename(&path, &retained).unwrap();
            retained
        });
        std::fs::create_dir(&path).unwrap();
        Self { path, retained }
    }

    fn repair(self) {
        std::fs::rename(&self.path, self.path.with_extension("obstruction")).unwrap();
        if let Some(retained) = self.retained {
            std::fs::rename(retained, self.path).unwrap();
        }
    }
}

#[test]
fn failed_lifecycle_history_keeps_the_observed_login_and_state_unchanged() {
    for operation in [1, 2, 5] {
        let fixture = Fixture::new(Cli::Claude, operation);
        let before = fixture.records();
        let obstruction = Obstruction::new(fixture.store.ledger_path());
        assert!(fixture.record(operation).is_err());
        obstruction.repair();
        assert_eq!(fixture.records(), before, "operation {operation}");
    }
}

fn exercise(cli: Cli, operation: i64, fault: i64) -> [bool; 6] {
    let fixture = Fixture::new(cli, operation);
    let before = fixture.records();
    let mut expected_state = fixture.store.load(NOW).unwrap().0;
    let mut expected_logins = fixture.store.load_logins(NOW).unwrap().0;
    let expected_events = apply(operation, &mut expected_state, &mut expected_logins);
    let wakes = expected_events.iter().any(Event::wakes);
    let obstruction = match fault {
        1 => None,
        2 => Some(fixture.store.ledger_path()),
        3 => Some(fixture.store.state_witness_file()),
        4 => Some(fixture.store.wake_path()),
        5 => Some(fixture.store.logins_witness_file()),
        _ => unreachable!(),
    }
    .map(Obstruction::new);
    let accepted = fixture.record(operation).is_ok();
    let pending = matches!(fault, 3 | 5) || (fault == 4 && wakes);
    let mut guarded = fixture.store.load(NOW).is_err() == pending
        && fixture.store.load_logins(NOW).is_err() == pending;
    if let Some(obstruction) = obstruction {
        obstruction.repair();
    }
    if fault == 2 {
        guarded &= fixture.records() == before;
        assert!(fixture.record(operation).is_ok());
    } else if pending {
        assert!(fixture.store.recover_records(&fixture.path).unwrap());
    }
    let actual_events: Vec<_> = fixture
        .store
        .tail(100)
        .unwrap()
        .into_iter()
        .filter(|event| event.t == NOW)
        .collect();
    let coherent = fixture.store.load(NOW).unwrap().0 == expected_state
        && fixture.store.load_logins(NOW).unwrap().0 == expected_logins
        && actual_events == expected_events;
    let after = fixture.records();
    let receipt = std::fs::read(fixture.store.dir.join("sync-commit.json")).unwrap();
    let once = !fixture.store.recover_records(&fixture.path).unwrap()
        && fixture.records() == after
        && std::fs::read(fixture.store.dir.join("sync-commit.json")).unwrap() == receipt
        && fixture.edits.get() == if fault == 2 { 2 } else { 1 };
    let preserved = after[3] == before[3]
        && std::fs::read(&fixture.credential).unwrap() == b"private credential fixture";
    let notification = fixture.store.wake_path().is_file() == wakes;
    [accepted, guarded, coherent, once, preserved, notification]
}

pub(crate) fn lifecycle_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherProvider" => {
            next.insert("provider", state["provider"] + 1);
        }
        "AnotherOperation" => {
            next.insert("operation", state["operation"] + 1);
        }
        "AnotherFault" => {
            next.insert("fault", state["fault"] + 1);
        }
        "Record" => {
            let flags = exercise(
                Cli::ALL[usize::try_from(state["provider"] - 1).unwrap()],
                state["operation"],
                state["fault"],
            );
            next.insert("phase", 1);
            for (key, value) in [
                "accepted",
                "guarded",
                "coherent",
                "once",
                "preserved",
                "notification",
            ]
            .into_iter()
            .zip(flags)
            {
                next.insert(key, i64::from(value));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn lifecycle_observations_conform_to_recoverable_publication() {
    let model = aterm_models_harness::account_lifecycle_commit_model();
    assert_eq!(model.mutant_dials(), (1..=7).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = lifecycle_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut forged = model.init_state();
    forged.insert("phase", 1);
    forged.insert("coherent", 0);
    assert!(!model.check_invariant("CoherentLifecycleAndHistory", &forged));
}

#[test]
fn repeated_lifecycle_observations_do_not_republish_or_allocate_generations() {
    for cli in Cli::ALL {
        for operation in 1..=7 {
            let fixture = Fixture::new(cli, operation);
            fixture.record(operation).unwrap();
            let before = fixture.records();
            let receipt = std::fs::read(fixture.store.dir.join("sync-commit.json")).unwrap();
            assert!(fixture.record(operation).unwrap().is_empty());
            assert_eq!(fixture.records(), before, "{cli:?}, operation {operation}");
            assert_eq!(
                std::fs::read(fixture.store.dir.join("sync-commit.json")).unwrap(),
                receipt
            );
        }
    }
}

#[test]
fn bound_roster_read_recovers_new_roster_bytes_before_binding() {
    for cli in Cli::ALL {
        let fixture = Fixture::new(cli, 1);
        let before = std::fs::read_to_string(&fixture.path).unwrap();
        let raw = Roster::load_for(cli, &fixture.path).unwrap();
        let mut changed = raw.clone();
        changed.rows[0].email = Some("changed@example.test".into());
        let after = append_rows_for(cli, "", &changed.rows).unwrap();
        let obstruction = Obstruction::new(fixture.store.state_witness_file());
        let held = fixture.store.acquire().unwrap();
        assert!(
            fixture
                .store
                .transact_sync_held(
                    &held,
                    NOW,
                    &mut None,
                    SyncTarget {
                        path: &fixture.path,
                        before: Some(&before),
                        after: &after,
                        mode: None
                    },
                    |_, _| Ok(Vec::new()),
                    || Ok(()),
                )
                .is_err()
        );
        drop(held);
        assert_eq!(std::fs::read_to_string(&fixture.path).unwrap(), before);
        obstruction.repair();
        let current = fixture.store.read_bound_roster(&fixture.path, NOW).unwrap();
        assert_eq!(
            current.rows[0].email.as_deref(),
            Some("changed@example.test")
        );
        assert_eq!(std::fs::read_to_string(&fixture.path).unwrap(), after);
        assert_eq!(
            std::fs::read(&fixture.credential).unwrap(),
            b"private credential fixture"
        );
        assert!(!fixture.store.recover_records(&fixture.path).unwrap());
    }
}

#[cfg(unix)]
#[test]
fn pending_publication_with_an_unresolvable_roster_is_a_record_error() {
    for pending in [false, true] {
        let fixture = Fixture::new(Cli::Claude, 1);
        if pending {
            let obstruction = Obstruction::new(fixture.store.state_witness_file());
            assert!(fixture.record(1).is_err());
            obstruction.repair();
        }
        std::fs::rename(&fixture.path, fixture.path.with_extension("retained")).unwrap();
        std::os::unix::fs::symlink(fixture.path.with_extension("missing"), &fixture.path).unwrap();
        let before = fixture.records();
        let result = fixture.store.read_bound_roster(&fixture.path, NOW);
        if pending {
            assert!(matches!(result, Err(RosterReadError::Records(_))));
        } else {
            assert!(matches!(result, Err(RosterReadError::Roster(_))));
        }
        assert_eq!(fixture.records(), before);
    }
}

#[test]
fn ordinary_bound_roster_reads_create_no_storage_and_tolerate_a_held_writer() {
    for contents in [
        None,
        Some(""),
        Some("[[account]]\nlabel = 'main'\nclass = 'dedicated'\n"),
    ] {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let path = root.path().join("accounts.toml");
        if let Some(contents) = contents {
            std::fs::write(&path, contents).unwrap();
        }
        let store = Store::at(&root.path().join("state")).bounded_by(&[]);
        let result = store.read_bound_roster(&path, NOW);
        assert!(result.is_ok(), "{contents:?}: {result:?}");
        assert!(
            !store.dir.exists(),
            "an ordinary read must not initialize account storage"
        );
    }
    let fixture = Fixture::new(Cli::Claude, 1);
    let before = fixture.records();
    let _held = fixture.store.acquire().unwrap();
    assert!(fixture.store.read_bound_roster(&fixture.path, NOW).is_ok());
    assert_eq!(fixture.records(), before);
}

#[test]
fn publication_between_raw_and_binding_reads_is_refused_including_an_empty_roster() {
    for case in 0..3 {
        let fixture = Fixture::new(Cli::Claude, 1);
        let mut next = Roster::load(&fixture.path).unwrap();
        next.rows[0].email = Some("changed@example.test".into());
        let next = append_rows_for(Cli::Claude, "", &next.rows).unwrap();
        if case == 2 {
            std::fs::write(&fixture.path, "").unwrap();
        }
        let before = std::fs::read_to_string(&fixture.path).unwrap();
        let mut obstruction = None;
        let mut called = false;
        let result = fixture
            .store
            .read_bound_roster_with(&fixture.path, NOW, || {
                called = true;
                let held = fixture
                    .store
                    .acquire_within(&[])
                    .unwrap()
                    .expect("ordinary reader holds no writer");
                if case == 1 {
                    obstruction = Some(Obstruction::new(fixture.store.state_witness_file()));
                }
                let written = fixture.store.transact_sync_held(
                    &held,
                    NOW,
                    &mut None,
                    SyncTarget {
                        path: &fixture.path,
                        before: Some(&before),
                        after: &next,
                        mode: None,
                    },
                    |_, _| Ok(Vec::new()),
                    || Ok(()),
                );
                assert_eq!(written.is_err(), case == 1);
            });
        assert!(called);
        assert!(
            matches!(result, Err(RosterReadError::Records(_))),
            "case {case}"
        );
        if let Some(obstruction) = obstruction {
            obstruction.repair();
        }
        let current = fixture.store.read_bound_roster(&fixture.path, NOW).unwrap();
        assert_eq!(
            current.rows[0].email.as_deref(),
            Some("changed@example.test")
        );
    }
}

#[test]
fn retained_identical_history_still_delivers_an_event_wake() {
    for blocked in [false, true] {
        let fixture = Fixture::new(Cli::Claude, 3);
        let mut event = Event::new(NOW, Kind::Proven, "main")
            .with_sid(&"\\".repeat(WORD_MAX))
            .with_to(&"\\".repeat(LABEL_MAX));
        event.why = "\\".repeat(LEDGER_WHY_MAX);
        let retained = (event.to_json_line() + "\n").repeat(LEDGER_KEEP_ROWS);
        assert!(retained.len() as u64 > LEDGER_MAX_BYTES);
        std::fs::write(fixture.store.ledger_path(), &retained).unwrap();
        let obstruction = blocked.then(|| Obstruction::new(fixture.store.wake_path()));
        let result = fixture.store.transact_pool_logins_within(
            &[],
            &fixture.path,
            NOW,
            &mut None,
            |_, _, _| Ok(vec![event]),
        );
        assert_eq!(result.is_err(), blocked);
        if let Some(obstruction) = obstruction {
            assert!(fixture.store.load(NOW).is_err());
            obstruction.repair();
            assert!(fixture.store.recover_records(&fixture.path).unwrap());
        }
        assert_eq!(
            std::fs::read_to_string(fixture.store.ledger_path()).unwrap(),
            retained
        );
        assert!(
            fixture.store.wake_path().is_file(),
            "accepted event must deliver its wake even when bounded history has identical bytes"
        );
        assert!(!fixture.store.recover_records(&fixture.path).unwrap());
    }
}
