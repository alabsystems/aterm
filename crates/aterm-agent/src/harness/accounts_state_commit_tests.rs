// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;
const HOUR: i64 = 3600;
const DAY: i64 = 86_400;

fn observe(store: &Store, usage: bool) -> Recorded {
    let who = ProcessLogin {
        login: None,
        default_home: true,
        stored_login: true,
    };
    if usage {
        record_usage(
            store,
            &Roster::own(),
            "session",
            &who,
            &UsageSaid {
                at: NOW,
                day: NOW.div_euclid(DAY),
                credits: Some(CreditsRead { until: None }),
                warning: None,
            },
            NOW,
        )
    } else {
        record_wall(
            store,
            &Roster::own(),
            "session",
            &who,
            &crate::harness::upgrade::StandingWall {
                at: NOW.unsigned_abs(),
                until: Some((NOW + HOUR).unsigned_abs()),
                window: Some("seven_day".into()),
                credits: None,
            },
            NOW - 1,
            NOW,
        )
    }
}

#[test]
fn unreadable_history_refuses_before_publishing_budget_state() {
    for usage in [false, true] {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let store = Store::at(root.path()).bounded_by(&[]);
        store.save(&State::default()).unwrap();
        let before = std::fs::read(store.state_file()).unwrap();
        std::fs::create_dir(store.ledger_path()).unwrap();
        assert!(matches!(observe(&store, usage), Recorded::Refused(_)));
        assert_eq!(std::fs::read(store.state_file()).unwrap(), before);
    }
}

#[test]
fn interrupted_state_publication_fences_the_budget_until_recovery() {
    for usage in [false, true] {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let store = Store::at(root.path()).bounded_by(&[]);
        store.save(&State::default()).unwrap();
        let witness = store.state_witness_file();
        let saved = root.path().join("saved-witness");
        std::fs::rename(&witness, &saved).unwrap();
        std::fs::create_dir(&witness).unwrap();
        assert!(matches!(observe(&store, usage), Recorded::Refused(_)));
        assert!(store.load(NOW).is_err());
        std::fs::rename(&witness, root.path().join("witness-obstruction")).unwrap();
        std::fs::rename(&saved, &witness).unwrap();
        assert_eq!(observe(&store, usage), Recorded::Unchanged);
        assert!(store.load(NOW).is_ok());
        assert_eq!(store.tail(10).unwrap().len(), 1);
        assert!(
            !store.wake_path().exists(),
            "budget observations do not wake holds"
        );
    }
}

#[test]
fn interrupted_notification_recovers_before_the_next_state_edit() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(root.path()).bounded_by(&[]);
    store.save(&State::default()).unwrap();
    std::fs::create_dir(store.wake_path()).unwrap();
    let edit = |state: &mut State| {
        if state.pinned {
            return Ok(Vec::new());
        }
        state.pinned = true;
        Ok(vec![Event::new(NOW, Kind::Use, "main")])
    };
    assert!(store.transact(NOW, &mut None, edit).is_err());
    assert!(store.load(NOW).is_err());
    std::fs::rename(store.wake_path(), root.path().join("wake-obstruction")).unwrap();
    assert!(store.transact(NOW, &mut None, edit).unwrap().is_empty());
    assert!(store.load(NOW).unwrap().0.pinned);
    assert_eq!(store.tail(10).unwrap().len(), 1);
    assert!(store.wake_path().is_file());
}

fn commit(store: &Store, operation: i64) -> Recorded {
    if operation < 3 {
        return observe(store, operation == 2);
    }
    match store.transact_within(&[], NOW, &mut None, |state| {
        if state.pinned {
            return Ok(Vec::new());
        }
        state.pinned = true;
        Ok(vec![Event::new(NOW, Kind::Use, "main")])
    }) {
        Err(why) => Recorded::Refused(why),
        Ok(events) if events.is_empty() => Recorded::Unchanged,
        Ok(events) => Recorded::Usage {
            account: "main".into(),
            events: events.len(),
        },
    }
}

fn images(store: &Store) -> Vec<Option<Vec<u8>>> {
    [
        store.state_file(),
        store.ledger_path(),
        store.wake_path(),
        store.dir.join(RECEIPT),
    ]
    .map(|path| std::fs::read(path).ok())
    .into()
}

fn recorded_pair(store: &Store, operation: i64) -> bool {
    let Ok((state, _)) = store.load(NOW) else {
        return false;
    };
    let Ok(events) = store.tail(10) else {
        return false;
    };
    let [event] = events.as_slice() else {
        return false;
    };
    if event.account != "main" || event.t != NOW {
        return false;
    }
    match operation {
        1 => event.kind == Kind::Wall && event.sid.as_deref() == Some("session")
            && state.accounts.get("main").and_then(|a| a.walls.get("seven_day"))
                .is_some_and(|wall| wall.at == NOW && wall.until == Some(NOW + HOUR)),
        2 => event.kind == Kind::Credits && event.sid.as_deref() == Some("session")
            && state.accounts.get("main").is_some_and(|account|
                account.credits_day == Some(NOW.div_euclid(DAY))
                    && matches!(&account.credits, Some(Credits::InUse(used)) if used.since == NOW && used.seen == NOW)),
        3 => event.kind == Kind::Use && state.pinned,
        _ => unreachable!(),
    }
}

fn exercise(cli: Cli, operation: i64, fault: i64) -> ([bool; 3], [bool; 3]) {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at_for(&root.path().join("state"), cli, root.path()).bounded_by(&[]);
    store.save(&State::default()).unwrap();
    // State-only admission must neither parse nor replace unrelated account authority.
    std::fs::write(
        store.logins_file(),
        "owner's unrelated malformed login file",
    )
    .unwrap();
    let roster = root.path().join("accounts.toml");
    std::fs::write(&roster, "owner's unrelated malformed roster").unwrap();
    let credential = root.path().join("retained-credential");
    std::fs::write(&credential, "opaque credential").unwrap();
    let unrelated = || {
        [store.logins_file(), roster.clone(), credential.clone()]
            .map(|path| std::fs::read(path).unwrap())
    };
    let retained = unrelated();
    let before = images(&store);
    let witness = store.state_witness_file();
    let saved = root.path().join("saved-witness");
    if matches!(fault, 3..=5 | 7) {
        std::fs::rename(&witness, &saved).unwrap();
        std::fs::create_dir(&witness).unwrap();
    }
    if fault == 2 {
        std::fs::create_dir(store.ledger_path()).unwrap();
    }
    let held = (fault == 6).then(|| store.acquire().unwrap());
    let first = commit(&store, operation);
    let state_readable = store.load(NOW).is_ok();
    let history_readable = store.tail(10).is_ok();
    let atomic = if fault == 1 {
        state_readable
            && history_readable
            && recorded_pair(&store, operation)
            && store.wake_path().exists() == (operation == 3)
    } else if matches!(fault, 2 | 6) {
        std::fs::read(store.state_file()).ok() == before[0]
    } else {
        !state_readable && !history_readable
    };
    let written = [
        !matches!(first, Recorded::Refused(_)),
        atomic,
        unrelated() == retained,
    ];
    drop(held);
    if matches!(fault, 3..=5 | 7) {
        std::fs::rename(&witness, root.path().join("witness-obstruction")).unwrap();
        std::fs::rename(&saved, &witness).unwrap();
    }
    if fault == 2 {
        std::fs::rename(store.ledger_path(), root.path().join("ledger-obstruction")).unwrap();
    }
    if fault == 4 {
        let state = State {
            pinned: true,
            ..State::default()
        };
        std::fs::write(store.state_file(), state.to_json()).unwrap();
    }
    if fault == 5 {
        std::fs::rename(
            store.dir.join(RECEIPT),
            root.path().join("retained-receipt"),
        )
        .unwrap();
    }
    if fault == 7 {
        std::fs::write(store.dir.join(RECEIPT), "{").unwrap();
    }
    let pending = images(&store);
    let second = commit(&store, operation);
    let success = !matches!(second, Recorded::Refused(_));
    let coherent = if success {
        recorded_pair(&store, operation) && store.wake_path().exists() == (operation == 3)
    } else {
        images(&store) == pending && store.load(NOW).is_err() && store.tail(10).is_err()
    };
    let settled = images(&store);
    let repeated = commit(&store, operation);
    let stable = images(&store) == settled
        && if success {
            repeated == Recorded::Unchanged
        } else {
            matches!(repeated, Recorded::Refused(_))
        };
    (
        written,
        [success, coherent && stable, unrelated() == retained],
    )
}

pub(crate) fn state_commit_transition(
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
        "Write" | "Retry" => {
            let (written, retried) = exercise(
                Cli::ALL[usize::try_from(state["provider"] - 1).unwrap()],
                state["operation"],
                state["fault"],
            );
            let flags = if action == "Write" { written } else { retried };
            next.insert("phase", if action == "Write" { 1 } else { 2 });
            for (name, flag) in ["success", "coherent", "isolated"].into_iter().zip(flags) {
                next.insert(name, i64::from(flag));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn state_commits_conform_to_durable_retry_and_refusal() {
    let model = aterm_models_harness::account_state_commit_model();
    assert_eq!(model.mutant_dials(), (1..=5).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = state_commit_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} {} {next:?}",
                    action.name
                );
            }
        }
    }
    let written = state_commit_transition(&model.init_state(), "Write");
    for (field, law) in [
        ("success", "WriteAvailability"),
        ("coherent", "StateAndHistoryTogether"),
        ("isolated", "UnrelatedAuthorityPreserved"),
    ] {
        let mut bad = written.clone();
        bad.insert(field, 0);
        assert!(!model.check_invariant(law, &bad));
    }
    let mut retried = state_commit_transition(&written, "Retry");
    retried.insert("success", 0);
    assert!(!model.check_invariant("RetryAvailability", &retried));
    retried.insert("fault", 4);
    retried.insert("success", 1);
    assert!(!model.check_invariant("ConflictRefused", &retried));
}

#[test]
fn every_state_publication_prefix_replays_exactly_once() {
    for cli in Cli::ALL {
        for operation in 1..=3 {
            for cut in [0, 1, 3, 5] {
                let root = aterm_tempfile::TempDir::new().unwrap();
                let store =
                    Store::at_for(&root.path().join("state"), cli, root.path()).bounded_by(&[]);
                store.save(&State::default()).unwrap();
                let saved = root.path().join("saved-witness");
                std::fs::rename(store.state_witness_file(), &saved).unwrap();
                std::fs::create_dir(store.state_witness_file()).unwrap();
                assert!(matches!(commit(&store, operation), Recorded::Refused(_)));
                let mut receipt = store.receipt().unwrap().unwrap();
                let plan = receipt.plan.as_mut().unwrap();
                // Model a first publication with the production-serialized after image.
                // Its retained witness must not turn recovery into a lost-state refusal.
                plan.state.before = None;
                std::fs::rename(store.state_file(), root.path().join("retained-state")).unwrap();
                let plan = plan.clone();
                store.save_receipt(&receipt).unwrap();
                std::fs::rename(store.state_witness_file(), root.path().join("obstruction"))
                    .unwrap();
                std::fs::rename(&saved, store.state_witness_file()).unwrap();
                let held = store.acquire().unwrap();
                assert_eq!(
                    store.recover_sync_at(&held, None, &mut |step| {
                        if step == cut {
                            Err("injected interruption".into())
                        } else {
                            Ok(())
                        }
                    }),
                    Err("injected interruption".into())
                );
                assert!(store.load(NOW).is_err());
                assert!(store.tail(10).is_err());
                assert!(store.load_logins(NOW).is_err());
                drop(held);
                // The no-roster usage-tap path itself can finish state-only recovery.
                assert_eq!(tap_roster(None, &store, NOW), Some(Roster::own()));
                assert_eq!(
                    read(&store.state_file(), MAX_STATE_BYTES).unwrap(),
                    plan.state.after
                );
                assert_eq!(
                    read(&store.ledger_path(), MAX_LEDGER).unwrap(),
                    plan.ledger.after
                );
                assert!(!store.logins_file().exists());
                assert!(store.load_logins(NOW).unwrap().0.labels.is_empty());
                assert_eq!(commit(&store, operation), Recorded::Unchanged);
                assert_eq!(store.tail(10).unwrap().len(), 1);
                assert!(!store.recover_state_records().unwrap());
            }
        }
    }
}

#[test]
fn a_pathless_writer_does_not_recover_roster_bound_authority() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(root.path()).bounded_by(&[]);
    store.save(&State::default()).unwrap();
    let path = root.path().join("accounts.toml");
    std::fs::write(&path, "").unwrap();
    store
        .save_receipt(&Receipt {
            schema: 1,
            id: "a".repeat(32),
            plan: Some(Plan {
                local_logins: false,
                roster_path: Some(path.to_str().unwrap().into()),
                roster_mode: None,
                state: Image {
                    before: read(&store.state_file(), MAX_STATE_BYTES).unwrap(),
                    after: Some(State::default().to_json()),
                },
                logins: Image::default(),
                ledger: Image::default(),
                roster: Image {
                    before: Some(String::new()),
                    after: Some(String::new()),
                },
                wake: None,
            }),
        })
        .unwrap();
    let before = images(&store);
    let mut edited = false;
    assert!(
        store
            .transact(NOW, &mut None, |_| {
                edited = true;
                Ok(Vec::new())
            })
            .is_err()
    );
    assert!(!edited);
    assert!(store.recover_state_records().is_err());
    assert_eq!(images(&store), before);
}

#[test]
fn old_roster_receipts_keep_the_same_digest_preimage() {
    #[derive(Serialize)]
    struct LegacyPlan<'a> {
        roster_path: &'a str,
        roster_mode: Option<u32>,
        state: Image,
        logins: Image,
        ledger: Image,
        roster: Image,
    }
    #[derive(Serialize)]
    struct LegacyReceipt<'a> {
        schema: u8,
        id: &'a str,
        plan: LegacyPlan<'a>,
    }
    let empty = Image::default();
    let old = LegacyReceipt {
        schema: 1,
        id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        plan: LegacyPlan {
            roster_path: "/retained/accounts.toml",
            roster_mode: None,
            state: empty.clone(),
            logins: empty.clone(),
            ledger: empty.clone(),
            roster: empty,
        },
    };
    let legacy_bytes = aterm_json::to_string(&old).unwrap();
    let parsed: Receipt = aterm_json::from_str(&legacy_bytes).unwrap();
    assert_eq!(aterm_json::to_string(&parsed).unwrap(), legacy_bytes);
    assert_eq!(
        receipt_digest(&parsed).unwrap(),
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(legacy_bytes.as_bytes()))
    );
}

#[test]
fn eventless_selection_and_identical_event_images_recover() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(root.path()).bounded_by(&[]);
    let selected = store.request_pin("main", NOW).unwrap();
    assert_eq!(store.load(NOW).unwrap().0.pending_pin, Some(selected));
    assert!(!store.ledger_path().exists());
    assert!(!store.wake_path().exists());
    assert_eq!(store.load_logins(NOW).unwrap().0, Logins::default());

    let mut event = Event::new(NOW, Kind::Use, "main")
        .with_sid(&"\\".repeat(WORD_MAX))
        .with_to(&"\\".repeat(LABEL_MAX));
    event.why = "\\".repeat(LEDGER_WHY_MAX);
    let line = event.to_json_line() + "\n";
    let ledger = line.repeat(LEDGER_KEEP_ROWS);
    assert!(ledger.len() as u64 > LEDGER_MAX_BYTES);
    std::fs::write(store.ledger_path(), &ledger).unwrap();
    let state_before = std::fs::read(store.state_file()).unwrap();
    let receipt_before = store.sync_read_receipt().unwrap();
    std::fs::create_dir(store.wake_path()).unwrap();
    assert!(store.transact(NOW, &mut None, |_| Ok(vec![event])).is_err());
    assert!(store.load(NOW).is_err());
    std::fs::rename(store.wake_path(), root.path().join("wake-obstruction")).unwrap();
    // A supplied roster path is immaterial to a state-only plan, even if nonexistent.
    assert!(
        store
            .recover_records(&root.path().join("absent/roster.toml"))
            .unwrap()
    );
    assert_eq!(std::fs::read(store.state_file()).unwrap(), state_before);
    assert_eq!(
        std::fs::read_to_string(store.ledger_path()).unwrap(),
        ledger
    );
    assert_ne!(store.sync_read_receipt().unwrap(), receipt_before);
    assert!(store.wake_path().is_file());
    assert!(!store.recover_state_records().unwrap());
}

#[test]
fn legacy_login_writers_refuse_pending_state_publication_before_edit() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(root.path()).bounded_by(&[]);
    store.save(&State::default()).unwrap();
    let witness = store.state_witness_file();
    std::fs::rename(&witness, root.path().join("saved-witness")).unwrap();
    std::fs::create_dir(&witness).unwrap();
    assert!(matches!(commit(&store, 1), Recorded::Refused(_)));
    let before = images(&store);
    let mut edited = false;
    assert!(
        store
            .transact_logins(NOW, &mut None, |_, _| {
                edited = true;
                Ok(Vec::new())
            })
            .is_err()
    );
    assert!(!edited);
    assert_eq!(images(&store), before);
    assert!(!store.logins_file().exists());
}

#[test]
fn state_only_recovery_rejects_out_of_scope_images() {
    for field in 0..5 {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let store = Store::at(root.path()).bounded_by(&[]);
        store.save(&State::default()).unwrap();
        std::fs::rename(
            store.state_witness_file(),
            root.path().join("saved-witness"),
        )
        .unwrap();
        std::fs::create_dir(store.state_witness_file()).unwrap();
        assert!(matches!(commit(&store, 1), Recorded::Refused(_)));
        let mut receipt = store.receipt().unwrap().unwrap();
        let plan = receipt.plan.as_mut().unwrap();
        match field {
            0 => plan.roster_mode = Some(0o600),
            1 => plan.roster.before = Some(String::new()),
            2 => plan.roster.after = Some(String::new()),
            3 => plan.logins.before = Some(Logins::default().to_json()),
            4 => plan.logins.after = Some(Logins::default().to_json()),
            _ => unreachable!(),
        }
        store.save_receipt(&receipt).unwrap();
        let before = images(&store);
        assert!(store.recover_state_records().is_err());
        assert_eq!(images(&store), before);
        assert!(!store.logins_file().exists());
    }
}

#[test]
fn recovery_chooses_the_scope_current_after_acquiring_the_lock() {
    for initial_bound in [false, true] {
        for final_scope in 0..=2 {
            for caller_path in [false, true] {
                let root = aterm_tempfile::TempDir::new().unwrap();
                let store = Store::at(root.path()).bounded_by(&[]);
                store.save(&State::default()).unwrap();
                let path = root.path().join("roster.toml");
                std::fs::write(&path, "").unwrap();
                let make_plan = |bound: bool| Plan {
                    local_logins: false,
                    roster_path: bound.then(|| path.to_str().unwrap().into()),
                    roster_mode: None,
                    state: Image {
                        before: read(&store.state_file(), MAX_STATE_BYTES).unwrap(),
                        after: read(&store.state_file(), MAX_STATE_BYTES).unwrap(),
                    },
                    logins: Image::default(),
                    ledger: Image::default(),
                    roster: if bound {
                        Image {
                            before: Some(String::new()),
                            after: Some(String::new()),
                        }
                    } else {
                        Image::default()
                    },
                    wake: Some(false),
                };
                store
                    .save_receipt(&Receipt {
                        schema: 1,
                        id: "a".repeat(32),
                        plan: Some(make_plan(initial_bound)),
                    })
                    .unwrap();
                let mut interleaved = false;
                let result =
                    store.recover_within(&[], caller_path.then_some(path.as_path()), || {
                        interleaved = true;
                        // The previous writer completes, and may admit another scope before
                        // this caller takes the writer lock. No sleep or scheduling dependency.
                        let _held = store.acquire().unwrap();
                        store
                            .save_receipt(&Receipt {
                                schema: 1,
                                id: "b".repeat(32),
                                plan: (final_scope > 0).then(|| make_plan(final_scope == 2)),
                            })
                            .unwrap();
                    });
                assert!(interleaved);
                if final_scope == 2 && !caller_path {
                    assert!(result.unwrap_err().contains("roster path"));
                } else {
                    assert_eq!(result.unwrap(), final_scope > 0);
                    assert!(!store.sync_needs_recovery());
                }
            }
        }
    }
}

#[test]
fn a_roster_writer_recovers_state_then_composes_with_legacy_login_edits() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(root.path()).bounded_by(&[]);
    store.save(&State::default()).unwrap();
    let path = root.path().join("roster.toml");
    std::fs::write(&path, "").unwrap();
    let witness = store.state_witness_file();
    let saved = root.path().join("saved-witness");
    std::fs::rename(&witness, &saved).unwrap();
    std::fs::create_dir(&witness).unwrap();
    assert!(matches!(commit(&store, 3), Recorded::Refused(_)));
    std::fs::rename(&witness, root.path().join("obstruction")).unwrap();
    std::fs::rename(&saved, &witness).unwrap();
    store
        .transact_pool_logins_within(&[], &path, NOW, &mut None, |_, state, _| {
            assert!(
                state.pinned,
                "the pending state edit recovered before this edit"
            );
            Ok(vec![Event::new(NOW + 1, Kind::Use, "main")])
        })
        .unwrap();
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            state.pinned = false;
            logins.labels.insert(
                "main".into(),
                Binding {
                    dir: Some("/retained/login".into()),
                    ..Binding::default()
                },
            );
            Ok(vec![Event::new(NOW + 2, Kind::Use, "main")])
        })
        .unwrap();
    store
        .transact_pool_logins_within(&[], &path, NOW, &mut None, |_, state, logins| {
            assert!(!state.pinned);
            assert_eq!(logins.generation_clock, 1);
            assert_eq!(
                logins.get("main").unwrap().dir.as_deref(),
                Some("/retained/login")
            );
            Ok(vec![Event::new(NOW + 3, Kind::Use, "main")])
        })
        .unwrap();
    assert_eq!(
        store
            .tail(10)
            .unwrap()
            .iter()
            .map(|event| event.t)
            .collect::<Vec<_>>(),
        [NOW, NOW + 1, NOW + 2, NOW + 3]
    );
}

#[test]
fn a_history_read_refuses_a_publication_between_its_receipt_checks() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(root.path()).bounded_by(&[]);
    assert!(!matches!(commit(&store, 3), Recorded::Refused(_)));
    assert!(
        store
            .tail_with(10, || {
                store
                    .transact(NOW, &mut None, |_| {
                        Ok(vec![Event::new(NOW + 1, Kind::Use, "main")])
                    })
                    .unwrap();
            })
            .is_err()
    );
    assert_eq!(store.tail(10).unwrap().len(), 2);
}
