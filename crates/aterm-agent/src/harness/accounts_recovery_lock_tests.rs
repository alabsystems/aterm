// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;
const CLIS: [Cli; 4] = [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi];

#[test]
fn checkpoint_recovery_finishes_its_interrupted_commit_without_a_remaining_checkpoint() {
    for cli in CLIS {
        let bed = Bed::new(cli);
        bed.store
            .transact(NOW - 2, &mut None, |state| {
                Ok(accounts::note_auth(
                    state,
                    "account",
                    accounts::Auth::SignedOut,
                    NOW - 2,
                ))
            })
            .unwrap();
        let staging = bed.checkpoint("account");
        let blocked = bed.store.state_witness_file();
        std::fs::rename(&blocked, blocked.with_extension("retained")).unwrap();
        std::fs::create_dir(&blocked).unwrap();
        assert!(!recover_interrupted_staging_renewals_with(
            cli,
            &bed.store,
            &bed.path,
            &bed.guard(),
            NOW,
            &[],
            || {},
        ));
        assert!(bed.store.load(NOW).is_err());
        assert!(bed.store.load_logins(NOW).is_err());
        std::fs::rename(&blocked, blocked.with_extension("blocked")).unwrap();
        std::fs::rename(blocked.with_extension("retained"), &blocked).unwrap();
        let checkpoint = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
        std::fs::rename(&checkpoint, checkpoint.with_extension("retained")).unwrap();
        assert!(recover_interrupted_staging_renewals_with(
            cli,
            &bed.store,
            &bed.path,
            &bed.guard(),
            NOW,
            &[],
            || {},
        ));
        assert_eq!(
            bed.store
                .load_logins(NOW)
                .unwrap()
                .0
                .get("account")
                .unwrap()
                .dir
                .as_deref(),
            staging.to_str()
        );
        assert!(!recover_interrupted_staging_renewals_with(
            cli,
            &bed.store,
            &bed.path,
            &bed.guard(),
            NOW,
            &[],
            || {},
        ));
        assert_eq!(
            bed.store
                .tail(100)
                .unwrap()
                .iter()
                .filter(|event| event.kind == accounts::Kind::Renewed)
                .count(),
            1
        );
    }
}

struct Bed {
    _root: aterm_tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    path: PathBuf,
    store: accounts::Store,
}

impl Bed {
    fn new(cli: Cli) -> Self {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        fs::create_dir_all(&home).unwrap();
        let path = root.path().join("roster.toml");
        let store = accounts::Store::at_for(&state, cli, &home);
        Self {
            _root: root,
            home,
            state,
            path,
            store,
        }
    }

    fn checkpoint(&self, label: &str) -> PathBuf {
        let cli = self.store.cli();
        let row = Row {
            label: label.into(),
            email: Some(format!("{label}@example.test")),
            dir: Some(
                self.store
                    .login_dir(label)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .into(),
            ),
            vendor: accounts::Vendor {
                region: (cli == Cli::Kimi).then_some(Region::Global),
                ..accounts::Vendor::default()
            },
            ..Row::default()
        };
        let text = fs::read_to_string(&self.path).unwrap_or_default();
        fs::write(
            &self.path,
            accounts::append_rows_for(cli, &text, &[row]).unwrap(),
        )
        .unwrap();
        let staging = self.store.claim_staging_dir(label).unwrap();
        let login_file = staging.join(cli.connector().stores[0]);
        fs::create_dir_all(login_file.parent().unwrap()).unwrap();
        fs::write(login_file, b"private checkpoint fixture").unwrap();
        accounts::write_renewal_checkpoint_for(
            cli,
            label,
            &format!("{label}@example.test"),
            staging.to_str().unwrap(),
            None,
            Some(accounts::Proof::Status),
            NOW - 1,
            &self.store.load_logins(NOW).unwrap().0,
        )
        .unwrap();
        staging
    }

    fn guard(&self) -> std::sync::Arc<std::sync::Mutex<AutoRenewGuard>> {
        std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()))
    }
}

fn account_files(store: &accounts::Store) -> [Option<Vec<u8>>; 4] {
    [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ]
    .map(|path| fs::read(path).ok())
}

fn lock_case(cli: Cli, case: i64) -> [(&'static str, i64); 8] {
    let bed = Bed::new(cli);
    let peer_cli = if cli == Cli::Claude {
        Cli::Codex
    } else {
        Cli::Claude
    };
    let peer = accounts::Store::at_for(&bed.state, peer_cli, &bed.home);
    assert_eq!(peer.shared_dir(), bed.store.shared_dir());
    let guard = bed.guard();
    if matches!(case, 15 | 18 | 20 | 21) {
        bed.store
            .transact_logins(NOW - 10, &mut None, |_, logins| {
                for label in ["account", "second"] {
                    logins
                        .labels
                        .insert(label.into(), accounts::Binding::default());
                }
                Ok(Vec::new())
            })
            .unwrap();
    }
    let mut evidence = Vec::new();
    if case != 7 {
        let staging = bed.checkpoint("account");
        if case == 8 {
            fs::write(
                staging.join(accounts::RENEWAL_CHECKPOINT_FILE),
                "invalid checkpoint",
            )
            .unwrap();
        }
        evidence.push(staging);
    }
    if matches!(case, 15 | 18 | 20 | 21) {
        evidence.push(bed.checkpoint("second"));
        if case == 18 {
            let text = fs::read_to_string(&bed.path).unwrap();
            fs::write(
                &bed.path,
                text.replace("second@example.test", "changed@example.test"),
            )
            .unwrap();
        }
    }
    let lock_path = bed.store.shared_dir().join(accounts::SIGNIN_LOCK_FILE);
    if case == 2 {
        drop(bed.store.signin_now().unwrap().unwrap());
    }
    let held_signin = match case {
        3 => bed.store.signin_now().unwrap(),
        4 => peer.signin_now().unwrap(),
        _ => None,
    };
    if case == 5 {
        fs::create_dir_all(&lock_path).unwrap();
    }
    if case == 6 {
        guard.lock().unwrap().in_flight = true;
    }
    let mut held_writer = match case {
        9 | 20 => bed.store.try_acquire().unwrap(),
        17 => peer.try_acquire().unwrap(),
        _ => None,
    };
    if matches!(case, 10 | 11) {
        bed.store.ensure().unwrap();
    }
    match case {
        10 => fs::write(bed.store.state_file(), "invalid state").unwrap(),
        11 => fs::write(bed.store.logins_file(), "invalid logins").unwrap(),
        12 => fs::rename(&bed.path, bed._root.path().join("kept-roster")).unwrap(),
        16 => {
            let login = bed
                .store
                .login_dir("account")
                .unwrap()
                .join(cli.connector().stores[0]);
            fs::create_dir_all(login.parent().unwrap()).unwrap();
            fs::write(login, b"existing account credentials").unwrap();
        }
        _ => {}
    }
    let evidence_files = || {
        evidence
            .iter()
            .map(|dir| {
                [
                    dir.join(cli.connector().stores[0]),
                    dir.join(accounts::RENEWAL_CHECKPOINT_FILE),
                ]
                .map(|path| fs::read(path).unwrap())
            })
            .collect::<Vec<_>>()
    };
    let mut evidence_before = evidence_files();
    let mut before = account_files(&bed.store);
    let mut visits = 0;
    let mut overlap = 0;
    let mut lock_order = 1;
    let recovered = recover_interrupted_staging_renewals_with(
        cli,
        &bed.store,
        &bed.path,
        &guard,
        NOW,
        &[],
        || {
            visits += 1;
            for contender in [&bed.store, &peer] {
                if contender.signin_now().unwrap().is_some() {
                    overlap = 1;
                }
            }
            // The observer runs before the first writer acquisition and after each
            // transaction releases it. Some cases deliberately hold this writer.
            let writer_held = matches!(case, 9 | 20) || (case == 21 && visits > 2);
            if bed.store.try_acquire().unwrap().is_some() == writer_held {
                lock_order = 0;
            }
            if visits == 1 {
                if case == 13 {
                    let text = fs::read_to_string(&bed.path).unwrap();
                    fs::write(
                        &bed.path,
                        text.replace("account@example.test", "changed@example.test"),
                    )
                    .unwrap();
                }
                if case == 14 {
                    bed.store
                        .transact_logins(NOW - 2, &mut None, |state, logins| {
                            Ok(accounts::note_renewed(
                                state,
                                logins,
                                "account",
                                None,
                                bed.store.login_dir("account").unwrap().to_str().unwrap(),
                                None,
                                NOW - 2,
                            ))
                        })
                        .unwrap();
                }
                if matches!(case, 19 | 22) {
                    let proof = evidence[0].join(accounts::RENEWAL_CHECKPOINT_FILE);
                    let text = if case == 19 {
                        "invalid checkpoint".into()
                    } else {
                        fs::read_to_string(&proof)
                            .unwrap()
                            .replace("account@example.test", "changed@example.test")
                    };
                    fs::write(proof, text).unwrap();
                    evidence_before = evidence_files();
                }
                before = account_files(&bed.store);
            }
            if case == 21 && visits == 2 {
                held_writer = bed.store.try_acquire().unwrap();
                assert!(held_writer.is_some());
            }
        },
    );
    let mut preserved = i64::from(recovered || before == account_files(&bed.store));
    for (dir, expected) in evidence.iter().zip(evidence_before) {
        let after = [
            dir.join(cli.connector().stores[0]),
            dir.join(accounts::RENEWAL_CHECKPOINT_FILE),
        ]
        .map(|path| fs::read(path).unwrap());
        preserved &= i64::from(expected == after);
    }
    let published = if recovered {
        let logins = bed.store.load_logins(NOW).unwrap().0;
        let count = evidence
            .iter()
            .filter(|dir| {
                logins
                    .labels
                    .values()
                    .any(|binding| binding.dir.as_deref() == dir.to_str())
            })
            .count();
        assert!(count > 0, "success must name a recovered checkpoint");
        i64::try_from(count).unwrap()
    } else {
        0
    };
    let noop_created_lock = i64::from(matches!(case, 7 | 8) && lock_path.exists());
    drop(held_writer);
    drop(held_signin);
    if case == 5 {
        fs::rename(&lock_path, bed._root.path().join("kept-invalid-lock")).unwrap();
    }
    let released = i64::from(peer.signin_now().unwrap().is_some());
    [
        ("phase", 1),
        ("published", published),
        ("overlap", overlap),
        ("lock_order", lock_order),
        ("released", released),
        ("preserved", preserved),
        ("visits", visits),
        ("noop_created_lock", noop_created_lock),
    ]
}

pub(crate) fn recovery_lock_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Recover" => {
            for (key, value) in lock_case(Cli::Codex, state["case"]) {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn recovery_lock_conforms_and_catches_overlap_and_lost_evidence() {
    let model = aterm_models_harness::account_recovery_lock_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = recovery_lock_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let after = recovery_lock_transition(&model.init_state(), "Recover");
    for (field, value, invariant) in [
        ("overlap", 1, "SigninExcluded"),
        ("lock_order", 0, "WriterTakenAfterSignin"),
        ("released", 0, "GuardReleased"),
        ("preserved", 0, "EvidencePreserved"),
    ] {
        let mut forged = after.clone();
        forged.insert(field, value);
        assert!(!model.check_invariant(invariant, &forged));
    }
}

#[test]
fn recovery_lock_cases_cover_every_provider() {
    let model = aterm_models_harness::account_recovery_lock_model();
    for cli in CLIS {
        for case in 1..=22 {
            let mut state = model.init_state();
            state.insert("case", case);
            let mut next = state.clone();
            for (key, value) in lock_case(cli, case) {
                next.insert(key, value);
            }
            assert!(
                model.successors("Recover", &state).contains(&next),
                "{cli:?}: {state:?} -> {next:?}"
            );
        }
    }
}
