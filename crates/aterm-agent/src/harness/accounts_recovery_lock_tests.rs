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

    fn staging(&self, label: &str) -> PathBuf {
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
        staging
    }

    fn checkpoint(&self, label: &str) -> PathBuf {
        let cli = self.store.cli();
        let staging = self.staging(label);
        accounts::write_renewal_checkpoint_for(
            cli,
            (cli == Cli::Kimi).then_some(Region::Global),
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
    if matches!(case, 15 | 18 | 20 | 21 | 23 | 24 | 27 | 28) {
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
    if matches!(case, 15 | 18 | 20 | 21 | 23 | 24 | 27 | 28) {
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
                .map(|path| fs::read(path).ok())
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
            if matches!(case, 23 | 24) && visits == 2 {
                let checkpoint = evidence[1].join(accounts::RENEWAL_CHECKPOINT_FILE);
                let text = if case == 23 {
                    "invalid checkpoint".into()
                } else {
                    fs::read_to_string(&checkpoint)
                        .unwrap()
                        .replace("second@example.test", "changed@example.test")
                };
                fs::write(checkpoint, text).unwrap();
                evidence_before = evidence_files();
                before = account_files(&bed.store);
            }
            if (matches!(case, 25 | 26) && visits == 1) || (matches!(case, 27 | 28) && visits == 2)
            {
                let candidate = usize::from(case >= 27);
                let login = evidence[candidate].join(cli.connector().stores[0]);
                fs::rename(&login, login.with_extension("retained")).unwrap();
                if matches!(case, 26 | 28) {
                    fs::create_dir(&login).unwrap();
                }
                evidence_before = evidence_files();
                before = account_files(&bed.store);
            }
        },
    );
    let mut preserved =
        i64::from((!matches!(case, 23..=28) && recovered) || before == account_files(&bed.store));
    for (dir, expected) in evidence.iter().zip(evidence_before) {
        let after = [
            dir.join(cli.connector().stores[0]),
            dir.join(accounts::RENEWAL_CHECKPOINT_FILE),
        ]
        .map(|path| fs::read(path).ok());
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
    assert_eq!(model.mutant_dials(), (1..=34).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 56);
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
    for (case, stale_publications) in [(23, 2), (24, 2), (25, 1), (26, 1), (27, 2), (28, 2)] {
        let mut before = model.init_state();
        before.insert("case", case);
        let mut forged = recovery_lock_transition(&before, "Recover");
        forged.insert("published", stale_publications);
        assert!(!model.check_invariant("PublicationPolicy", &forged));
    }
}

#[test]
fn recovery_lock_cases_cover_every_applicable_provider() {
    let model = aterm_models_harness::account_recovery_lock_model();
    for cli in CLIS {
        let last_case = if cli == Cli::Claude { 24 } else { 28 };
        for case in 1..=last_case {
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

#[test]
fn recovery_rechecks_each_checkpoint_after_the_previous_publication() {
    for cli in CLIS {
        for case in [23, 24] {
            let result = lock_case(cli, case);
            let field = |key| result.iter().find(|(name, _)| *name == key).unwrap().1;
            assert_eq!(field("published"), 1, "{cli:?}, case {case}");
            assert_eq!(field("overlap"), 0);
            assert_eq!(field("preserved"), 1);
        }
    }
}

#[test]
fn recovery_rechecks_vendor_login_files_before_publication() {
    // Claude may use the Keychain; its existing nonempty-home policy is separate.
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        for case in 25..=28 {
            let result = lock_case(cli, case);
            let field = |key| result.iter().find(|(name, _)| *name == key).unwrap().1;
            assert_eq!(
                field("published"),
                i64::from(case >= 27),
                "{cli:?}, case {case}"
            );
            assert_eq!(field("overlap"), 0);
            assert_eq!(field("preserved"), 1);
        }
    }
}

fn recovery_identity_bytes(cli: Cli, kind: i64) -> Vec<u8> {
    let email = if kind == 2 {
        "other@example.test"
    } else {
        "account@example.test"
    };
    if cli == Cli::Claude {
        return if kind == 0 {
            b"{}".to_vec()
        } else {
            aterm_json::to_vec(&aterm_json::json!({
                "oauthAccount": {"emailAddress": email}
            }))
            .unwrap()
        };
    }
    let token = |user: &str| {
        crate::harness::codex_app_login::tests::make_jwt(&aterm_json::json!({
            "email": email,
            "https://api.openai.com/auth": {"chatgpt_user_id": user}
        }))
    };
    aterm_json::to_vec(&aterm_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": if kind == 0 {"opaque-id".into()} else {token("user-a")},
            "access_token": if kind == 0 {"opaque-access".into()}
                else {token(if kind == 3 {"user-b"} else {"user-a"})}
        }
    }))
    .unwrap()
}

fn recovery_identity_case(
    cli: Cli,
    kind: i64,
    late: bool,
    variant: u8,
) -> [(&'static str, i64); 4] {
    let bed = Bed::new(cli);
    bed.store
        .transact(NOW - 10, &mut None, |state| {
            Ok(accounts::note_auth(
                state,
                "account",
                accounts::Auth::SignedOut,
                NOW - 10,
            ))
        })
        .unwrap();
    let staging = bed.checkpoint("account");
    if variant & 1 != 0 {
        let mut roster = Roster::load_for(cli, &bed.path).unwrap();
        roster.rows[0].email = Some("mailbox-alias@example.test".into());
        roster.rows[0].identity_email = Some("account@example.test".into());
        fs::write(&bed.path, format_roster_toml(&roster).unwrap()).unwrap();
    }
    let legacy = cli == Cli::Claude && variant & 2 != 0;
    let login = staging.join(if legacy {
        ".config.json"
    } else if cli == Cli::Claude {
        ".claude.json"
    } else {
        "auth.json"
    });
    let original = recovery_identity_bytes(cli, 1);
    let replacement = recovery_identity_bytes(cli, kind);
    if legacy {
        fs::write(
            staging.join(".claude.json"),
            recovery_identity_bytes(cli, if kind == 2 { 1 } else { 2 }),
        )
        .unwrap();
    }
    fs::write(&login, if late { &original } else { &replacement }).unwrap();
    let before = account_files(&bed.store);
    let checkpoint = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
    let checkpoint_bytes = fs::read(&checkpoint).unwrap();
    let mut replaced = !late;
    let published = recover_interrupted_staging_renewals_with(
        cli,
        &bed.store,
        &bed.path,
        &bed.guard(),
        NOW,
        &[],
        || {
            if !replaced {
                fs::write(&login, &replacement).unwrap();
                replaced = true;
            }
        },
    );
    assert!(replaced);
    let binding = bed.store.load_logins(NOW).unwrap().0;
    let bound = binding.get("account").is_some_and(|entry| {
        entry.dir.as_deref() == staging.to_str()
            && entry.proved_email.as_deref() == Some("account@example.test")
    });
    [
        ("published", i64::from(published)),
        ("bound", i64::from(bound)),
        ("metadata", i64::from(account_files(&bed.store) == before)),
        (
            "preserved",
            i64::from(
                fs::read(&login).unwrap() == replacement
                    && fs::read(checkpoint).unwrap() == checkpoint_bytes,
            ),
        ),
    ]
}

pub(crate) fn recovery_identity_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Provider" => {
            next.insert("provider", 1);
        }
        "Credential" => {
            next.insert("credential", state["credential"] + 1);
        }
        "Timing" => {
            next.insert("late", 1);
        }
        "Recover" => {
            let cli = if state["provider"] == 0 {
                Cli::Claude
            } else {
                Cli::Codex
            };
            let observed = recovery_identity_case(cli, state["credential"], state["late"] == 1, 0);
            for variant in 1..4 {
                assert_eq!(
                    recovery_identity_case(cli, state["credential"], state["late"] == 1, variant),
                    observed,
                    "{state:?}, variant={variant}"
                );
            }
            next.insert("phase", 1);
            for (field, value) in observed {
                next.insert(field, value);
            }
        }
        _ => panic!("unknown action {action}"),
    }
    next
}

#[test]
fn staged_recovery_identity_conforms_and_preserves_refused_evidence() {
    let model = aterm_models_harness::account_recovery_identity_model();
    assert_eq!(model.mutant_dials(), (1..=6).collect::<Vec<_>>());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = recovery_identity_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut refusal = model.init_state();
    refusal.insert("credential", 2);
    let after = model.successors("Recover", &refusal).pop().unwrap();
    for (field, value, law) in [
        ("published", 1, "Publication"),
        ("bound", 1, "Association"),
        ("metadata", 0, "Refusal"),
        ("preserved", 0, "Evidence"),
    ] {
        let mut corrupted = after.clone();
        corrupted.insert(field, value);
        assert!(!model.check_invariant(law, &corrupted), "{law}");
    }
}

#[test]
fn a_refused_recovery_identity_can_be_corrected_and_recovered() {
    for cli in [Cli::Claude, Cli::Codex] {
        let bed = Bed::new(cli);
        let staging = bed.checkpoint("account");
        let login = staging.join(if cli == Cli::Claude {
            ".claude.json"
        } else {
            "auth.json"
        });
        fs::write(&login, recovery_identity_bytes(cli, 2)).unwrap();
        let recover = || {
            recover_interrupted_staging_renewals_with(
                cli,
                &bed.store,
                &bed.path,
                &bed.guard(),
                NOW,
                &[],
                || {},
            )
        };
        assert!(!recover());
        fs::write(&login, recovery_identity_bytes(cli, 1)).unwrap();
        assert!(recover());
        assert!(!recover());
        assert_eq!(
            bed.store
                .load_logins(NOW)
                .unwrap()
                .0
                .get("account")
                .and_then(|b| b.dir.as_deref()),
            staging.to_str()
        );
    }
}

#[test]
fn a_contradictory_newest_checkpoint_does_not_hide_an_older_valid_candidate() {
    for cli in [Cli::Claude, Cli::Codex] {
        let bed = Bed::new(cli);
        let newest = bed.checkpoint("account");
        let file = if cli == Cli::Claude {
            ".claude.json"
        } else {
            "auth.json"
        };
        let wrong = recovery_identity_bytes(cli, 2);
        fs::write(newest.join(file), &wrong).unwrap();
        let older = bed.store.claim_staging_dir("account").unwrap();
        fs::write(older.join(file), recovery_identity_bytes(cli, 1)).unwrap();
        accounts::write_renewal_checkpoint_for(
            cli,
            None,
            "account",
            "account@example.test",
            older.to_str().unwrap(),
            None,
            Some(accounts::Proof::Status),
            NOW - 2,
            &bed.store.load_logins(NOW).unwrap().0,
        )
        .unwrap();
        let saved = fs::read(newest.join(accounts::RENEWAL_CHECKPOINT_FILE)).unwrap();
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
                .and_then(|b| b.dir.as_deref()),
            older.to_str()
        );
        assert_eq!(fs::read(newest.join(file)).unwrap(), wrong);
        assert_eq!(
            fs::read(newest.join(accounts::RENEWAL_CHECKPOINT_FILE)).unwrap(),
            saved
        );
    }
}

#[test]
fn shared_recovery_uses_observed_identity_for_first_and_pinned_checkpoints() {
    use accounts::shared::{Intent, Outcome, Source};
    let first = super::super::profile::principal_id("first").unwrap();
    let second = super::super::profile::principal_id("second").unwrap();
    let source = Source {
        network: [1; 32],
        account: [2; 32],
    };
    for pinned in [false, true] {
        for legacy in [false, true] {
            for (text, id, allowed) in [
                (
                    r#"{"oauthAccount":{"emailAddress":"account@example.test","accountUuid":"first"}}"#,
                    Some(first),
                    true,
                ),
                (
                    r#"{"oauthAccount":{"emailAddress":"other@example.test","accountUuid":"second"}}"#,
                    Some(second),
                    false,
                ),
                (
                    r#"{"oauthAccount":{"accountUuid":"first"}}"#,
                    Some(first),
                    true,
                ),
                (
                    r#"{"oauthAccount":{"emailAddress":"account@example.test"}}"#,
                    None,
                    false,
                ),
                (
                    r#"{"oauthAccount":{"emailAddress":"account@example.test","accountUuid":"second"}}"#,
                    Some(second),
                    !pinned,
                ),
                (
                    r#"{"oauthAccount":{"emailAddress":"other@example.test","accountUuid":"first","accountUuid":"second"}}"#,
                    None,
                    false,
                ),
                (
                    r#"{"oauthAccount":{"emailAddress":"other@example.test","emailAddress":"account@example.test","accountUuid":"first"}}"#,
                    Some(first),
                    true,
                ),
            ] {
                let bed = Bed::new(Cli::Claude);
                let staging = bed.staging("account");
                let login = staging.join(if legacy {
                    ".config.json"
                } else {
                    ".claude.json"
                });
                fs::write(&login, text).unwrap();
                bed.store
                    .transact_logins(NOW - 1, &mut None, |_, logins| {
                        logins.shared.begin(
                            Intent {
                                source,
                                decision: [3; 32],
                                generation: 1,
                                configuration: [4; 32],
                                request: [5; 16],
                                label: "account".into(),
                                identity: "account@example.test".into(),
                                staging: staging.to_str().unwrap().into(),
                                before_generation: None,
                                begun_at: NOW - 1,
                            },
                            pinned.then_some(first),
                        )?;
                        Ok(Vec::new())
                    })
                    .unwrap();
                // Shared admission advances the generation fence before the
                // provider check can produce a recoverable checkpoint.
                accounts::write_renewal_checkpoint_for(
                    Cli::Claude,
                    None,
                    "account",
                    "account@example.test",
                    staging.to_str().unwrap(),
                    None,
                    Some(accounts::Proof::Status),
                    NOW - 1,
                    &bed.store.load_logins(NOW).unwrap().0,
                )
                .unwrap();
                let before = account_files(&bed.store);
                let checkpoint = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
                let evidence = fs::read(&checkpoint).unwrap();
                assert_eq!(
                    recover_interrupted_staging_renewals_with(
                        Cli::Claude,
                        &bed.store,
                        &bed.path,
                        &bed.guard(),
                        NOW,
                        &[],
                        || {}
                    ),
                    allowed,
                    "pinned={pinned}, legacy={legacy}, {text}"
                );
                let logins = bed.store.load_logins(NOW).unwrap().0;
                assert_eq!(
                    matches!(
                        logins.shared.latest(source).unwrap().outcome,
                        Outcome::Published { .. }
                    ),
                    allowed
                );
                if allowed {
                    assert_eq!(logins.shared.principal_source(id.unwrap()), Some(source));
                    assert_eq!(
                        logins.get("account").unwrap().dir.as_deref(),
                        staging.to_str()
                    );
                } else {
                    assert_eq!(account_files(&bed.store), before);
                    assert_eq!(
                        logins.shared.latest(source).unwrap().outcome,
                        Outcome::Pending
                    );
                }
                assert_eq!(fs::read(&login).unwrap(), text.as_bytes());
                assert_eq!(fs::read(checkpoint).unwrap(), evidence);
            }
        }
    }
}
