// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

fn renew(store: &Store) -> Result<Vec<Event>, String> {
    store.transact_logins_within(&[], NOW, &mut None, |state, logins| {
        if logins
            .get("main")
            .and_then(|binding| binding.dir.as_deref())
            == Some("/retained/new")
        {
            return Ok(Vec::new());
        }
        Ok(note_renewed(
            state,
            logins,
            "main",
            None,
            "/retained/new",
            None,
            NOW,
        ))
    })
}

#[test]
fn unreadable_history_refuses_before_changing_login_bindings() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(root.path()).bounded_by(&[]);
    store.save(&State::default()).unwrap();
    store.save_logins(&Logins::default()).unwrap();
    let state = std::fs::read(store.state_file()).unwrap();
    let logins = std::fs::read(store.logins_file()).unwrap();
    std::fs::create_dir(store.ledger_path()).unwrap();
    assert!(renew(&store).is_err());
    assert_eq!(std::fs::read(store.state_file()).unwrap(), state);
    assert_eq!(std::fs::read(store.logins_file()).unwrap(), logins);
}

#[test]
fn interrupted_state_publication_does_not_expose_a_new_login_binding() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(root.path()).bounded_by(&[]);
    store.save(&State::default()).unwrap();
    store.save_logins(&Logins::default()).unwrap();
    let witness = store.state_witness_file();
    std::fs::rename(&witness, root.path().join("retained-witness")).unwrap();
    std::fs::create_dir(&witness).unwrap();
    assert!(renew(&store).is_err());
    assert!(store.load_logins(NOW).is_err());
    assert!(store.load(NOW).is_err());
}

struct Fixture {
    root: aterm_tempfile::TempDir,
    store: Store,
    old: String,
    new: String,
    credentials: [PathBuf; 2],
}

impl Fixture {
    fn new(cli: Cli) -> Self {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let store = Store::at_for(&root.path().join("state"), cli, root.path()).bounded_by(&[]);
        let old = root.path().join("old").to_str().unwrap().to_owned();
        let new = root.path().join("new").to_str().unwrap().to_owned();
        let credentials =
            [&old, &new].map(|dir| PathBuf::from(dir).join(cli.connector().stores[0]));
        for path in &credentials {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "retained opaque credential").unwrap();
        }
        store
            .transact_logins(NOW - 1, &mut None, |state, logins| {
                Ok(note_renewed(
                    state,
                    logins,
                    "main",
                    None,
                    &old,
                    None,
                    NOW - 1,
                ))
            })
            .unwrap();
        std::fs::write(root.path().join("roster.toml"), "unrelated owner edit").unwrap();
        Self {
            root,
            store,
            old,
            new,
            credentials,
        }
    }

    fn write(&self, operation: i64) -> Result<Vec<Event>, String> {
        self.store
            .transact_logins_within(&[], NOW, &mut None, |state, logins| {
                if operation == 1 {
                    if logins
                        .get("main")
                        .and_then(|binding| binding.dir.as_deref())
                        == Some(self.new.as_str())
                    {
                        return Ok(Vec::new());
                    }
                    Ok(note_renewed(
                        state,
                        logins,
                        "main",
                        Some(&self.old),
                        &self.new,
                        None,
                        NOW,
                    ))
                } else {
                    note_served_login(logins, "main", NOW);
                    Ok(Vec::new())
                }
            })
    }

    fn images(&self) -> [Option<Vec<u8>>; 5] {
        [
            self.store.state_file(),
            self.store.logins_file(),
            self.store.ledger_path(),
            self.store.wake_path(),
            self.store.dir.join(RECEIPT),
        ]
        .map(|path| std::fs::read(path).ok())
    }

    fn preserved(&self) -> bool {
        self.credentials
            .iter()
            .all(|path| std::fs::read(path).unwrap() == b"retained opaque credential")
            && std::fs::read(self.root.path().join("roster.toml")).unwrap()
                == b"unrelated owner edit"
    }
}

struct Obstruction {
    path: PathBuf,
    saved: PathBuf,
}
impl Obstruction {
    fn new(path: PathBuf) -> Self {
        let saved = path.with_extension("retained");
        std::fs::rename(&path, &saved).unwrap();
        std::fs::create_dir(&path).unwrap();
        Self { path, saved }
    }
    fn repair(self) {
        std::fs::rename(&self.path, self.path.with_extension("obstruction")).unwrap();
        std::fs::rename(&self.saved, &self.path).unwrap();
    }
}

fn exercise(cli: Cli, operation: i64, fault: i64) -> [bool; 6] {
    let f = Fixture::new(cli);
    let before = f.images();
    let obstruction = match fault {
        2 => Some(f.store.ledger_path()),
        3 | 5 | 6 => Some(f.store.logins_witness_file()),
        4 => Some(f.store.state_witness_file()),
        8 => Some(f.store.wake_path()),
        _ => None,
    }
    .map(Obstruction::new);
    let held = (fault == 7).then(|| f.store.acquire().unwrap());
    let initial = f.write(operation);
    let pending = matches!(fault, 3..=6) || fault == 8 && operation == 1;
    assert_eq!(initial.is_ok(), fault == 1 || fault == 8 && operation == 2);
    let mut fenced = if pending {
        f.store.load(NOW).is_err() && f.store.load_logins(NOW).is_err() && f.store.tail(10).is_err()
    } else if matches!(fault, 2 | 7) {
        f.images()[0..2] == before[0..2]
    } else {
        true
    };
    drop(held);
    if let Some(obstruction) = obstruction {
        obstruction.repair();
    }
    if fault == 5 {
        std::fs::write(f.store.logins_file(), "{\"schema\":99}").unwrap();
    } else if fault == 6 {
        std::fs::rename(
            f.store.dir.join(RECEIPT),
            f.root.path().join("retained-receipt"),
        )
        .unwrap();
    }
    let interrupted = f.images();
    let recovered = f.write(operation).is_ok();
    let (coherent, once) = if recovered {
        let state = f.store.load(NOW).unwrap().0;
        let logins = f.store.load_logins(NOW).unwrap().0;
        let binding = logins.get("main").unwrap();
        let expected_dir = if operation == 1 { &f.new } else { &f.old };
        (
            binding.dir.as_ref() == Some(expected_dir)
                && if operation == 1 {
                    state.accounts["main"].auth == Auth::Ok
                } else {
                    binding.served_at == Some(NOW)
                },
            logins.generation_clock == u64::try_from(if operation == 1 { 2 } else { 1 }).unwrap()
                && f.store
                    .tail(10)
                    .unwrap()
                    .iter()
                    .filter(|event| event.kind == Kind::Renewed)
                    .count()
                    == if operation == 1 { 2 } else { 1 },
        )
    } else {
        fenced &= f.store.load(NOW).is_err() && f.store.load_logins(NOW).is_err();
        (f.images() == interrupted, true)
    };
    let settled = f.images();
    let repeated = f.write(operation);
    let stable = f.images() == settled
        && if recovered {
            repeated.unwrap().is_empty()
        } else {
            repeated.is_err()
        };
    [recovered, fenced, coherent, once, stable, f.preserved()]
}

pub(crate) fn local_transition(
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
        "Commit" => {
            let flags = exercise(
                Cli::ALL[usize::try_from(state["provider"] - 1).unwrap()],
                state["operation"],
                state["fault"],
            );
            next.insert("phase", 1);
            for (name, flag) in [
                "recovered",
                "fenced",
                "coherent",
                "once",
                "stable",
                "preserved",
            ]
            .into_iter()
            .zip(flags)
            {
                next.insert(name, i64::from(flag));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn local_transactions_conform_to_recovery_and_generation_rules() {
    let model = aterm_models_harness::account_local_commit_model();
    assert_eq!(model.mutant_dials(), (1..=7).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = local_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} {} {next:?}",
                    action.name
                );
            }
        }
    }
    let complete = local_transition(&model.init_state(), "Commit");
    for (field, law) in [
        ("recovered", "RecoverableCompletes"),
        ("fenced", "PendingFencesReaders"),
        ("coherent", "RecordsStayTogether"),
        ("once", "GenerationAndHistoryOnce"),
        ("stable", "RetryIsStable"),
        ("preserved", "UnrelatedFilesPreserved"),
    ] {
        let mut bad = complete.clone();
        bad.insert(field, 0);
        assert!(!model.check_invariant(law, &bad));
    }
    let mut bad = complete;
    bad.insert("fault", 5);
    assert!(!model.check_invariant("ConflictsRefuse", &bad));
}

#[test]
fn every_local_publication_prefix_recovers_exact_images_once() {
    for cli in Cli::ALL {
        for operation in 1..=2 {
            for cut in [0, 1, 2, 3, 5] {
                let f = Fixture::new(cli);
                let obstruction = Obstruction::new(f.store.logins_witness_file());
                assert!(f.write(operation).is_err());
                obstruction.repair();
                let plan = f.store.receipt().unwrap().unwrap().plan.unwrap();
                let held = f.store.acquire().unwrap();
                assert_eq!(
                    f.store.recover_sync_at(&held, None, &mut |step| {
                        if step == cut {
                            Err("injected interruption".into())
                        } else {
                            Ok(())
                        }
                    }),
                    Err("injected interruption".into())
                );
                assert!(f.store.load(NOW).is_err());
                assert!(f.store.load_logins(NOW).is_err());
                drop(held);
                assert!(
                    f.store
                        .recover_records(&f.root.path().join("absent/roster.toml"))
                        .unwrap()
                );
                assert_eq!(
                    read(&f.store.state_file(), MAX_STATE_BYTES).unwrap(),
                    plan.state.after
                );
                assert_eq!(
                    read(&f.store.logins_file(), MAX_LOGINS_BYTES).unwrap(),
                    plan.logins.after
                );
                assert_eq!(
                    read(&f.store.ledger_path(), MAX_LEDGER).unwrap(),
                    plan.ledger.after
                );
                assert!(f.write(operation).unwrap().is_empty());
                assert_eq!(
                    f.store.load_logins(NOW).unwrap().0.generation_clock,
                    if operation == 1 { 2 } else { 1 }
                );
                assert!(f.preserved());
            }
        }
    }
}

#[test]
fn state_and_roster_writers_can_follow_pending_local_login_publication() {
    for following in 0..=2 {
        let f = Fixture::new(Cli::Claude);
        let obstruction = Obstruction::new(f.store.logins_witness_file());
        assert!(f.write(1).is_err());
        obstruction.repair();
        if following == 0 {
            f.store
                .transact(NOW, &mut None, |state| {
                    assert_eq!(state.accounts["main"].auth, Auth::Ok);
                    state.pinned = true;
                    Ok(Vec::new())
                })
                .unwrap();
        } else if following == 1 {
            let roster = f.root.path().join("valid-roster.toml");
            std::fs::write(&roster, "").unwrap();
            f.store
                .transact_pool_logins_within(&[], &roster, NOW, &mut None, |_, state, logins| {
                    assert_eq!(logins.get("main").unwrap().dir.as_ref(), Some(&f.new));
                    state.pinned = true;
                    Ok(Vec::new())
                })
                .unwrap();
        } else {
            let held = f.store.acquire().unwrap();
            f.store
                .transact_logins_held(&held, NOW, &mut None, |_, logins| {
                    assert_eq!(logins.get("main").unwrap().dir.as_ref(), Some(&f.new));
                    note_served_login(logins, "main", NOW + 1);
                    Ok(Vec::new())
                })
                .unwrap();
        }
        assert_eq!(f.store.load_logins(NOW).unwrap().0.generation_clock, 2);
        assert_eq!(f.store.tail(10).unwrap().len(), 3);
        assert!(f.preserved());
    }
}

#[test]
fn a_login_conflict_also_blocks_the_state_only_writer() {
    let f = Fixture::new(Cli::Claude);
    let obstruction = Obstruction::new(f.store.logins_witness_file());
    assert!(f.write(1).is_err());
    obstruction.repair();
    std::fs::write(f.store.logins_file(), "conflicting owner bytes").unwrap();
    let before = f.images();
    let mut edited = false;
    assert!(
        f.store
            .transact(NOW, &mut None, |_| {
                edited = true;
                Ok(Vec::new())
            })
            .is_err()
    );
    assert!(!edited);
    assert_eq!(f.images(), before);
}

#[test]
fn local_scope_preserves_old_receipts_and_refuses_mixed_authority() {
    let f = Fixture::new(Cli::Claude);
    let obstruction = Obstruction::new(f.store.logins_witness_file());
    assert!(f.write(1).is_err());
    obstruction.repair();
    let mut receipt = f.store.receipt().unwrap().unwrap();
    assert!(receipt.plan.as_ref().unwrap().local_logins);
    let text = aterm_json::to_string(&receipt).unwrap();
    // A pre-extension Plan does not admit the new field, even though its images are known.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct OldPlan {
        roster_path: Option<String>,
        roster_mode: Option<u32>,
        state: Image,
        logins: Image,
        ledger: Image,
        roster: Image,
        #[serde(default)]
        wake: Option<bool>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct OldReceipt {
        schema: u8,
        id: String,
        plan: Option<OldPlan>,
    }
    assert!(aterm_json::from_str::<OldReceipt>(&text).is_err());
    let plan = receipt.plan.as_mut().unwrap();
    plan.roster_path = Some(f.root.path().join("roster.toml").to_str().unwrap().into());
    plan.roster = Image {
        before: Some(String::new()),
        after: Some(String::new()),
    };
    f.store.save_receipt(&receipt).unwrap();
    let before = f.images();
    assert!(
        f.store
            .recover_records(&f.root.path().join("roster.toml"))
            .is_err()
    );
    assert_eq!(f.images(), before);
    receipt.plan = None;
    let complete = aterm_json::to_string(&receipt).unwrap();
    assert!(aterm_json::from_str::<OldReceipt>(&complete).is_ok());
}

#[test]
fn no_op_and_login_only_first_publication_preserve_absence() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(root.path()).bounded_by(&[]);
    assert!(
        store
            .transact_logins(NOW, &mut None, |_, _| Ok(Vec::new()))
            .unwrap()
            .is_empty()
    );
    for path in [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.dir.join(RECEIPT),
    ] {
        assert!(!path.exists());
    }
    store
        .transact_logins(NOW, &mut None, |_, logins| {
            logins.labels.insert(
                "main".into(),
                Binding {
                    dir: Some("/retained/login".into()),
                    ..Binding::default()
                },
            );
            Ok(Vec::new())
        })
        .unwrap();
    assert!(!store.state_file().exists());
    assert!(!store.ledger_path().exists());
    assert_eq!(store.load_logins(NOW).unwrap().0.generation_clock, 1);
    assert!(store.load(NOW).is_ok());
}
