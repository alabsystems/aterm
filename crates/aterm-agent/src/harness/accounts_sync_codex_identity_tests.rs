// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Default-login ownership follows a coherent native file identity, not a mutable label.

use super::*;

#[path = "accounts_sync_rerun_tests.rs"]
mod rerun;

const NOW: i64 = 1_792_660_000;
const A: &str = "a@example.com";
const B: &str = "b@example.com";

struct World {
    _scratch: aterm_tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    config: PathBuf,
    roster: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        let scratch = aterm_tempfile::Builder::new()
            .prefix(tag)
            .tempdir()
            .unwrap();
        let home = scratch.path().join("home");
        let state = scratch.path().join("state");
        let config = scratch.path().join("config/aterm.toml");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::create_dir_all(home.join(".secrets")).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();
        aterm_uds::atomic_file::write(&config, b"[fleet]\nmachine='m16'\n", 0o600).unwrap();
        fs::write(
            home.join(".codex/config.toml"),
            "cli_auth_credentials_store='file'\n",
        )
        .unwrap();
        fs::write(home.join(".secrets").join(cli_fleet_file(Cli::Codex)), format!(
            "[[account]]\nlabel='a'\nemail='{A}'\nmachines=['m16']\n\n[[account]]\nlabel='b'\nemail='{B}'\nmachines=['m16']\n"
        )).unwrap();
        let roster = accounts::roster_beside_for(&config, Cli::Codex).unwrap();
        let world = Self {
            _scratch: scratch,
            home,
            state,
            config,
            roster,
        };
        world.save_rows(&Roster {
            rows: vec![
                Row {
                    label: "a".into(),
                    email: Some(A.into()),
                    ..Row::default()
                },
                Row {
                    label: "b".into(),
                    email: Some(B.into()),
                    dir: Some(world.store().login_dir("b").unwrap().display().to_string()),
                    ..Row::default()
                },
            ],
        });
        world.login(A);
        world
    }

    fn store(&self) -> accounts::Store {
        accounts::Store::at_for(&self.state, Cli::Codex, &self.home)
    }
    fn env(&self) -> Env {
        quiet_env(&self.home, &self.state, &self.config, NOW)
    }
    fn rows(&self) -> Roster {
        Roster::load_for(Cli::Codex, &self.roster).unwrap()
    }
    fn save_rows(&self, rows: &Roster) {
        fs::write(
            &self.roster,
            accounts::append_rows_for(Cli::Codex, "", &rows.rows).unwrap(),
        )
        .unwrap();
    }
    fn login(&self, email: &str) {
        write_login(&self.home.join(".codex"), email);
    }
    fn sync(&self) -> Result<CliSync, String> {
        sync_cli_with(
            Cli::Codex,
            (&self.home, &self.state, &self.config),
            (None, None),
            NOW,
            (&|_, _| panic!("local fixtures cannot fetch a source"), &[]),
        )
    }
    fn reassign(&self, before_publish: impl FnOnce()) -> Result<Option<(Roster, String)>, String> {
        let (target, _) = super::super::cli::roster_target(&self.roster)?;
        reassign_codex_default(
            &self.env(),
            &self.store(),
            &self.roster,
            &target,
            &[],
            before_publish,
        )
    }
    fn records(&self) -> Vec<Option<Vec<u8>>> {
        let store = self.store();
        [
            &self.roster,
            &store.state_file(),
            &store.logins_file(),
            &store.state_witness_file(),
            &store.logins_witness_file(),
            &store.ledger_path(),
            &store.wake_path(),
        ]
        .into_iter()
        .map(|path| fs::read(path).ok())
        .collect()
    }
}

fn write_login(home: &Path, email: &str) {
    fs::create_dir_all(home).unwrap();
    let token = super::super::codex_app_login::tests::make_jwt(&aterm_json::json!({
        "email": email,
        "https://api.openai.com/auth": {"chatgpt_user_id": email, "chatgpt_account_id": "fixture-workspace"}
    }));
    fs::write(
        home.join("auth.json"),
        aterm_json::to_vec(&aterm_json::json!({
            "auth_mode": "chatgpt", "tokens": {"id_token": token, "access_token": token}
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn an_explicit_codex_signin_without_current_file_context_preserves_account_custody() {
    for case in ["missing", "unreadable", "hidden"] {
        let world = World::new("codex-signin-context-");
        world.sync().unwrap();
        let store = world.store();
        store
            .transact(NOW, &mut None, |state| {
                state.active = Some("a".into());
                state.pinned = true;
                state.accounts.entry("a".into()).or_default().auth = accounts::Auth::Ok;
                Ok(Vec::new())
            })
            .unwrap();
        let witness = capture_default_login(&store, &world.roster, "a", NOW).unwrap();
        let home = world.home.join(".codex");
        match case {
            "missing" => fs::remove_file(home.join("auth.json")).unwrap(),
            "unreadable" => fs::write(home.join("auth.json"), "{").unwrap(),
            "hidden" => fs::write(
                home.join("config.toml"),
                "cli_auth_credentials_store='keyring'\n",
            )
            .unwrap(),
            _ => unreachable!(),
        }
        assert!(
            store
                .capture_auth_context(world.rows().row("a").unwrap())
                .is_none()
        );
        let before = world.records();
        assert!(
            record_default_sign_in(&store, Cli::Codex, &world.roster, &witness, &home, NOW + 1)
                .is_err(),
            "{case}"
        );
        assert_eq!(world.records(), before, "{case}");
        let state = store.load(NOW + 1).unwrap().0;
        assert_eq!(state.active.as_deref(), Some("a"));
        assert!(state.pinned);
    }
}

#[test]
fn a_replaced_default_login_keeps_old_auth_history_without_reusing_its_verdict() {
    let world = World::new("codex-default-auth-context-");
    let store = world.store();
    let old_b = store.login_dir("b").unwrap();
    write_login(&old_b, B);
    assert!(matches!(world.sync(), Ok(CliSync::Synced(_))));
    let mut bindings = store.load_logins(NOW).unwrap().0;
    bindings.labels.insert(
        "b".into(),
        accounts::Binding {
            dir: Some(old_b.display().to_string()),
            ..accounts::Binding::default()
        },
    );
    store.save_logins(&bindings).unwrap();
    let witness = capture_login(&store, &world.roster, "b", NOW - 20).unwrap();
    let events = record_review_login(
        &store,
        Cli::Codex,
        &world.roster,
        &witness,
        &world.home,
        crate::ai::store::Outcome::Ended(crate::ai::store::Ended::ReviewAuth),
        NOW - 20,
    )
    .unwrap();
    assert!(!events.is_empty());
    let state = store.load(NOW).unwrap().0;
    let old_auth = fs::read(old_b.join("auth.json")).unwrap();
    let history = state.accounts.get("b").unwrap().clone();
    let ledger = fs::read(store.ledger_path()).ok();

    // An unchanged login must still expose its recorded failure.
    assert!(matches!(world.sync(), Ok(CliSync::Synced(_))));
    let unchanged_records = store
        .read_account_records(Some(&world.roster), NOW)
        .unwrap();
    let unchanged = &unchanged_records.state;
    let unchanged_logins = &unchanged_records.logins;
    let row = unchanged_records.roster.row("b").unwrap();
    assert_eq!(unchanged.accounts.get("b"), Some(&history));
    assert_eq!(fs::read(old_b.join("auth.json")).unwrap(), old_auth);
    assert_eq!(
        accounts::condition(row, unchanged, true, NOW),
        accounts::Condition::SignedOut
    );
    assert_eq!(
        accounts::login_state(row, unchanged_logins, unchanged, NOW),
        accounts::LoginState::Expired
    );

    world.login(B);
    let new_path = world.home.join(".codex/auth.json");
    let token = super::super::codex_app_login::tests::make_jwt(&aterm_json::json!({
        "email": B, "iat": NOW,
        "https://api.openai.com/auth": {"chatgpt_user_id": B, "chatgpt_account_id": "fixture-workspace"}
    }));
    let new_auth = aterm_json::json!({
        "auth_mode": "chatgpt", "tokens": {"id_token": token, "access_token": token}
    });
    fs::write(&new_path, aterm_json::to_vec(&new_auth).unwrap()).unwrap();
    let new_auth = fs::read(&new_path).unwrap();
    assert_ne!(new_auth, old_auth);
    assert!(world.reassign(|| {}).unwrap().is_some());
    let after = store.load(NOW).unwrap().0;
    let after_records = store
        .read_account_records(Some(&world.roster), NOW)
        .unwrap();
    let row = after_records.roster.row("b").unwrap();
    assert_eq!(after_records.roster.row_for_login(None).unwrap().label, "b");
    assert_eq!(row.expected_identity_email(), Some(B));
    let retained = after.accounts.get("b").unwrap();
    assert_eq!(retained.auth, history.auth);
    assert_eq!(retained.auth_at, history.auth_at);
    assert_eq!(fs::read(store.ledger_path()).ok(), ledger);
    assert_eq!(fs::read(old_b.join("auth.json")).unwrap(), old_auth);
    assert_eq!(fs::read(&new_path).unwrap(), new_auth);
    assert_eq!(
        accounts::condition(row, &after_records.state, true, NOW),
        accounts::Condition::Unseen
    );
    assert_eq!(
        accounts::login_state(row, &after_records.logins, &after_records.state, NOW),
        accounts::LoginState::Idle
    );
    let machine = crate::ai::facts::Machine {
        now: NOW,
        home: Some(world.home.clone()),
        var: &|_| None,
        pkg: None,
        exclude: Vec::new(),
        records: Default::default(),
        harness_state: Some(world.state.clone()),
        roster: Some(world.roster.clone()),
        accounts_mode: Some(accounts::Mode::Off),
        config: Some(world.config.clone()),
    };
    let facts = crate::ai::facts::pool_for(&machine, Cli::Codex).unwrap();
    assert_eq!(facts.serving_label.as_deref(), Some("b"));
    assert_eq!(facts.signed_out_at, None);
}

#[test]
fn sign_in_status_results_remain_bound_to_the_credentials_seen_before_the_status_process() {
    for signed_out in [false, true] {
        for replaced in [false, true] {
            let world = World::new("codex-status-context-");
            world.sync().unwrap();
            let store = world.store();
            store
                .transact(NOW, &mut None, |state| {
                    state.active = Some("a".into());
                    state.pinned = true;
                    Ok(Vec::new())
                })
                .unwrap();
            let witness = capture_default_login(&store, &world.roster, "a", NOW).unwrap();
            let status = capture_status_context(&store, &witness);
            let images = world.records();
            if replaced {
                let token = super::super::codex_app_login::tests::make_jwt(&aterm_json::json!({
                    "email": A, "iat": NOW,
                    "https://api.openai.com/auth": {"chatgpt_user_id": A, "chatgpt_account_id": "fixture-workspace"}
                }));
                fs::write(
                    world.home.join(".codex/auth.json"),
                    aterm_json::to_vec(&aterm_json::json!({
                        "tokens": {"id_token": token, "access_token": token}
                    }))
                    .unwrap(),
                )
                .unwrap();
            }
            let result = if signed_out {
                record_default_sign_out(
                    &store,
                    Cli::Codex,
                    &world.roster,
                    &status,
                    &world.home.join(".codex"),
                    NOW,
                )
                .map(|_| ())
            } else {
                record_default_sign_in(
                    &store,
                    Cli::Codex,
                    &world.roster,
                    &status,
                    &world.home.join(".codex"),
                    NOW,
                )
                .map(|result| assert_eq!(result, DefaultSignIn::Accepted))
            };
            if replaced {
                assert!(result.is_err());
                assert_eq!(world.records(), images);
            } else {
                result.unwrap();
                let records = store
                    .read_account_records(Some(&world.roster), NOW)
                    .unwrap();
                assert_eq!(
                    records
                        .state
                        .account("a")
                        .current_auth(records.roster.row("a").unwrap()),
                    if signed_out {
                        accounts::Auth::SignedOut
                    } else {
                        accounts::Auth::Ok
                    }
                );
                assert_eq!(records.state.pinned, !signed_out);
            }
        }
    }
}

#[test]
fn codex_sync_reassigns_the_default_without_moving_account_history_or_sessions() {
    for (independent, alias) in [(false, false), (true, false), (false, true)] {
        let world = World::new("codex-default-reassignment-");
        if alias {
            let mut rows = world.rows();
            rows.rows[1].email = Some("b-alias@example.com".into());
            rows.rows[1].identity_email = Some(B.into());
            world.save_rows(&rows);
        }
        let store = world.store();
        let old_b = store.login_dir("b").unwrap();
        write_login(&old_b, B);
        let own_a = world.home.join("independent-a");
        if independent {
            write_login(&own_a, A);
        }
        let mut state = accounts::State {
            machine: store.machine_id("0123456789abcdef").unwrap(),
            active: Some("a".into()),
            pinned: true,
            ..accounts::State::default()
        };
        accounts::note_wall(
            &mut state,
            "a",
            accounts::WEEK_LIMIT,
            Some(NOW + 3600),
            NOW - 100,
            NOW,
        );
        accounts::note_auth(&mut state, "a", accounts::Auth::Ok, NOW - 100);
        accounts::note_auth(&mut state, "b", accounts::Auth::SignedOut, NOW - 20);
        // note_wall ends an earlier pin; this is a live pin made after that observation.
        state.pinned = true;
        assert!(state.pinned);
        state.sessions.insert(
            "still-a".into(),
            accounts::SessionRow {
                account: "a".into(),
                at: NOW,
                held: None,
            },
        );
        store.save(&state).unwrap();
        let mut bindings = accounts::Logins {
            generation_clock: 7,
            sign_in_clock: 3,
            ..accounts::Logins::default()
        };
        bindings.labels.insert(
            "a".into(),
            accounts::Binding {
                dir: independent.then(|| own_a.display().to_string()),
                renewal_generation: 6,
                last_success_generation: 6,
                served_at: Some(NOW - 50),
                ..accounts::Binding::default()
            },
        );
        bindings.labels.insert(
            "b".into(),
            accounts::Binding {
                dir: Some(old_b.display().to_string()),
                renewal_generation: 7,
                last_success_generation: 7,
                served_at: Some(NOW - 40),
                ..accounts::Binding::default()
            },
        );
        bindings
            .sign_in_receipts
            .extend([("a".into(), 2), ("b".into(), 3)]);
        store.save_logins(&bindings).unwrap();
        assert!(matches!(world.sync(), Ok(CliSync::Synced(_))));
        let state_before = store.load(NOW).unwrap().0;
        let state_image_before = fs::read(store.state_file()).unwrap();
        let ledger_before = fs::read(store.ledger_path()).ok();
        let parents = ["a", "b"].map(|label| {
            let parent = accounts::RenewalParent::capture(&bindings, label);
            parent
                .require_current(&store.load_logins(NOW).unwrap().0, label)
                .unwrap();
            parent
        });
        let old_auth = fs::read(old_b.join("auth.json")).unwrap();
        let own_auth = independent.then(|| fs::read(own_a.join("auth.json")).unwrap());
        world.login(B);
        let auth = fs::read(world.home.join(".codex/auth.json")).unwrap();
        let sync = world.sync().unwrap();
        let CliSync::Synced(sync) = sync else {
            panic!("mutating sync must complete");
        };
        assert!(sync.changed);
        assert_eq!(sync.own_row.as_deref(), Some("b"));
        let roster = world.rows();
        assert_eq!(roster.row_for_login(None).unwrap().label, "b");
        assert_eq!(roster.row("a").unwrap().expected_identity_email(), Some(A));
        assert_eq!(roster.row("b").unwrap().expected_identity_email(), Some(B));
        assert_eq!(
            roster.row("b").unwrap().email.as_deref(),
            Some(if alias { "b-alias@example.com" } else { B })
        );
        assert_eq!(store.load(NOW).unwrap().0, state_before);
        assert_eq!(fs::read(store.state_file()).unwrap(), state_image_before);
        assert_eq!(fs::read(store.ledger_path()).ok(), ledger_before);
        let now = store.load_logins(NOW).unwrap().0;
        for (label, parent) in ["a", "b"].into_iter().zip(parents) {
            assert!(now.get(label).unwrap().renewal_generation > bindings.generation_clock);
            assert!(parent.require_current(&now, label).is_err());
            assert!(!now.sign_in_receipts.contains_key(label));
            assert_eq!(now.get(label).unwrap().last_success_generation, 0);
        }
        assert_eq!(now.get("b").unwrap().dir, None);
        assert!(
            now.get("b")
                .unwrap()
                .was
                .contains(&Some(old_b.display().to_string()))
        );
        assert!(now.get("a").unwrap().was.contains(&None));
        if independent {
            assert_eq!(now.get("a").unwrap().dir.as_deref(), own_a.to_str());
            assert_eq!(now.get("a").unwrap().served_at, Some(NOW - 50));
            assert_eq!(
                fs::read(own_a.join("auth.json")).unwrap(),
                own_auth.unwrap()
            );
        } else {
            assert!(
                !Path::new(roster.row("a").unwrap().dir.as_deref().unwrap())
                    .join("auth.json")
                    .exists()
            );
        }
        assert_eq!(fs::read(old_b.join("auth.json")).unwrap(), old_auth);
        assert_eq!(fs::read(world.home.join(".codex/auth.json")).unwrap(), auth);
        let before = world.records();
        let CliSync::Synced(again) = world.sync().unwrap() else {
            panic!("idempotent sync");
        };
        assert!(!again.changed);
        assert_eq!(world.records(), before);
    }
}

#[test]
fn the_second_lab_sync_follows_the_native_default_after_the_actual_first_import() {
    use super::super::alab_roster::{self, Lab, LabAccount};
    let world = World::new("codex-default-lab-import-");
    fs::remove_file(&world.roster).unwrap();
    let lab = Lab {
        me: Some("m16".into()),
        settled: true,
        accounts: [("a", A), ("b", B)]
            .into_iter()
            .enumerate()
            .map(|(index, (label, email))| LabAccount {
                pk: format!("ac-{:016x}", index + 1),
                cli: Cli::Codex,
                vendor: accounts::Vendor::default(),
                label: label.into(),
                legacy_label: None,
                email: email.into(),
                identity: None,
                browser: None,
                joinable: true,
                preferred: vec![("m16".into(), Some(index as u64 + 1))],
            })
            .collect(),
        aliases: BTreeMap::new(),
        mailboxes: BTreeMap::new(),
        places: Default::default(),
    };
    alab_roster::with_lab(lab, || {
        assert!(matches!(world.sync(), Ok(CliSync::Synced(_))));
        assert_eq!(world.rows().row_for_login(None).unwrap().label, "a");
        let before = world.store().load(NOW).unwrap().0;
        world.login(B);
        assert!(matches!(world.sync(), Ok(CliSync::Synced(_))));
        let roster = world.rows();
        assert_eq!(roster.row_for_login(None).unwrap().label, "b");
        assert_eq!(roster.row("a").unwrap().expected_identity_email(), Some(A));
        assert_eq!(world.store().load(NOW).unwrap().0.accounts, before.accounts);
        let records = world.records();
        assert!(matches!(world.sync(), Ok(CliSync::Synced(_))));
        assert_eq!(world.records(), records);
    });
}

#[test]
fn codex_reassignment_needs_unambiguous_native_identity_and_current_local_policy() {
    for case in [
        "missing",
        "unknown",
        "api-key",
        "keyring",
        "unlisted",
        "ambiguous",
        "held",
        "unnamed",
    ] {
        let world = World::new("codex-default-evidence-");
        world.login(B);
        let auth = world.home.join(".codex/auth.json");
        match case {
            "missing" => fs::remove_file(&auth).unwrap(),
            "unknown" => fs::write(&auth, b"{}").unwrap(),
            "api-key" => fs::write(
                &auth,
                br#"{"auth_mode":"apikey","OPENAI_API_KEY":"synthetic-only"}"#,
            )
            .unwrap(),
            "keyring" => fs::write(
                world.home.join(".codex/config.toml"),
                "cli_auth_credentials_store='keyring'\n",
            )
            .unwrap(),
            "unlisted" => world.login("unlisted@example.com"),
            "ambiguous" => {
                let mut text = fs::read_to_string(&world.roster).unwrap();
                text.push_str("\n[[account]]\nlabel='c'\nemail='alias@example.com'\nidentity_email='b@example.com'\ndir='/synthetic/c'\n");
                fs::write(&world.roster, text).unwrap();
            }
            "held" => {
                let mut rows = world.rows();
                rows.rows[1].held_by = Some("m22".into());
                world.save_rows(&rows);
            }
            "unnamed" => {
                let mut rows = world.rows();
                rows.rows[0].email = None;
                world.save_rows(&rows);
            }
            _ => unreachable!(),
        }
        let before = world.records();
        let result = world.reassign(|| {});
        if ["ambiguous", "held"].contains(&case) {
            assert!(result.is_err(), "{case}");
        } else {
            assert!(matches!(result, Ok(None)), "{case}: {result:?}");
        }
        assert_eq!(world.records(), before, "{case}");
    }
}

#[test]
fn codex_reassignment_rechecks_policy_auth_and_record_custody_before_publication() {
    for case in [
        "auth", "backend", "config", "source", "roster", "binding", "occupied",
    ] {
        let world = World::new("codex-default-race-");
        world.login(B);
        let before = world.records();
        let result = world.reassign(|| match case {
            "auth" => world.login(A),
            "backend" => fs::write(
                world.home.join(".codex/config.toml"),
                "cli_auth_credentials_store='keyring'\n",
            )
            .unwrap(),
            "config" => fs::write(&world.config, "[fleet]\nmachine='m22'\n").unwrap(),
            "source" => fs::write(
                world.home.join(".secrets").join(cli_fleet_file(Cli::Codex)),
                "",
            )
            .unwrap(),
            "roster" => {
                let mut rows = world.rows();
                rows.rows[1].identity_email = Some("other@example.com".into());
                world.save_rows(&rows);
            }
            "binding" => {
                let logins = accounts::Logins {
                    generation_clock: 99,
                    ..accounts::Logins::default()
                };
                world.store().save_logins(&logins).unwrap();
            }
            "occupied" => {
                let directory = world.store().login_dir("a").unwrap();
                fs::create_dir_all(&directory).unwrap();
                fs::write(directory.join("auth.json"), "unrelated").unwrap();
            }
            _ => unreachable!(),
        });
        assert!(result.is_err(), "{case}: {result:?}");
        let after = world.records();
        for (index, (old, new)) in before.iter().zip(&after).enumerate() {
            if (case == "roster" && index == 0) || (case == "binding" && [2, 4].contains(&index)) {
                continue;
            }
            assert_eq!(
                old, new,
                "{case}, file {index}: no publication after a failed recheck"
            );
        }
    }
}

#[test]
fn codex_reassignment_cannot_take_another_owner_or_an_occupied_login_directory() {
    for case in ["occupied", "owner", "default-owner"] {
        let world = World::new("codex-default-directory-");
        world.login(B);
        let directory = world.store().login_dir("a").unwrap();
        if case == "occupied" {
            write_login(&directory, "unrelated@example.com");
        } else {
            let mut rows = world.rows();
            rows.rows.push(Row {
                label: "other".into(),
                email: Some("other@example.com".into()),
                dir: Some(
                    if case == "owner" {
                        directory
                    } else {
                        world.home.join(".codex")
                    }
                    .display()
                    .to_string(),
                ),
                ..Row::default()
            });
            world.save_rows(&rows);
        }
        let before = world.records();
        assert!(world.reassign(|| {}).is_err(), "{case}");
        assert_eq!(world.records(), before, "{case}");
    }
}

#[test]
fn codex_reassignment_waits_for_existing_signin_and_auth_custody() {
    let world = World::new("codex-default-locks-");
    world.login(B);
    let store = world.store();
    {
        let _held = store.signin_now().unwrap().unwrap();
        let before = world.records();
        assert!(world.reassign(|| {}).is_err());
        assert_eq!(world.records(), before);
    }
    {
        let _held =
            super::super::codex_app_login::AuthLock::try_take(&world.home.join(".codex/auth.json"))
                .unwrap()
                .unwrap();
        let before = world.records();
        assert!(world.reassign(|| {}).is_err());
        assert_eq!(world.records(), before);
    }
    assert!(world.reassign(|| {}).unwrap().is_some());
}

#[test]
fn a_review_witness_checks_its_selected_codex_login_not_an_unrelated_default() {
    let world = World::new("codex-default-review-");
    let program =
        super::super::codex_alab::lay_package(&world._scratch.path().join("program"), true);
    world.login(B);
    let store = world.store();
    let before = world.records();
    assert!(
        capture_current_review(
            &store,
            Cli::Codex,
            Some(&world.roster),
            accounts::Mode::Off,
            None,
            NOW
        )
        .is_err()
    );
    assert_eq!(world.records(), before);
    let directory = store.login_dir("b").unwrap();
    write_login(&directory, B);
    store
        .transact(NOW, &mut None, |state| {
            state.active = Some("b".into());
            state.pinned = true;
            Ok(Vec::new())
        })
        .unwrap();
    let capture = capture_current_review(
        &store,
        Cli::Codex,
        Some(&world.roster),
        accounts::Mode::Off,
        Some(&program),
        NOW,
    )
    .unwrap();
    assert_eq!(capture.witness.row().unwrap().label, "b");
    let mut rows = world.rows();
    rows.rows.retain(|row| row.label == "a");
    world.save_rows(&rows);
    store
        .transact(NOW, &mut None, |state| {
            state.pinned = false;
            Ok(Vec::new())
        })
        .unwrap();
    assert!(
        capture_current_review(
            &store,
            Cli::Codex,
            Some(&world.roster),
            accounts::Mode::Early,
            None,
            NOW
        )
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn codex_reassignment_declines_a_configured_roster_link_that_changes_target() {
    let world = World::new("codex-default-link-");
    world.login(B);
    let first = world.roster.with_file_name("first.toml");
    let second = world.roster.with_file_name("second.toml");
    fs::rename(&world.roster, &first).unwrap();
    fs::write(&second, fs::read(&first).unwrap()).unwrap();
    std::os::unix::fs::symlink(&first, &world.roster).unwrap();
    let before = world.records();
    let result = world.reassign(|| {
        fs::remove_file(&world.roster).unwrap();
        std::os::unix::fs::symlink(&second, &world.roster).unwrap();
    });
    assert!(result.is_err());
    assert_eq!(world.records(), before);
    assert_eq!(fs::read(first).unwrap(), fs::read(second).unwrap());
}

/// A ROW TAKES ITS OWN LOGIN BACK: moved A, B, A and B again, every sync follows the default login,
/// each row's managed directory keeps the login it held, and no directory is listed both as where a
/// row is and where it was. CONTROL: below, a directory it left with another account's login in it,
/// or its own account's in one it never left, stays where it is.
#[test]
fn a_default_login_moved_back_takes_the_rows_own_directory_back() {
    let world = World::new("codex-default-back-");
    let store = world.store();
    let (dir_a, dir_b) = (store.login_dir("a").unwrap(), store.login_dir("b").unwrap());
    write_login(&dir_b, B);
    let own_b = fs::read(dir_b.join("auth.json")).unwrap();
    let sync_to = |email: &str, label: &str| {
        world.login(email);
        let synced = world.sync().unwrap();
        let CliSync::Synced(synced) = synced else {
            panic!("a synced fleet: {synced:?}");
        };
        assert_eq!(synced.default_login.refused, None, "{email}");
        assert_eq!(synced.own_row.as_deref(), Some(label), "{email}");
        assert_eq!(
            world.rows().row_for_login(None).unwrap().label,
            label,
            "{email}"
        );
    };
    sync_to(B, "b");
    assert_eq!(
        world.rows().row("a").unwrap().dir.as_deref(),
        dir_a.to_str(),
        "the first move"
    );
    sync_to(A, "a");
    assert_eq!(
        world.rows().row("b").unwrap().dir.as_deref(),
        dir_b.to_str(),
        "`b` is on its own login again"
    );
    assert_eq!(fs::read(dir_b.join("auth.json")).unwrap(), own_b);
    let logins = store.load_logins(NOW).unwrap().0;
    assert_eq!(logins.get("b").unwrap().dir.as_deref(), dir_b.to_str());
    assert_eq!(logins.get("b").unwrap().was, vec![None]);
    assert_eq!(
        logins.get("a").unwrap().was,
        vec![dir_a.to_str().map(str::to_owned)],
        "`a` left its own directory and the default login holds it"
    );
    // `a`'s own directory holds its login by now, and the third move takes it back too.
    write_login(&dir_a, A);
    sync_to(B, "b");
    sync_to(A, "a");
    assert_eq!(
        world.rows().row_for_login(None).unwrap().label,
        "a",
        "settled"
    );
    let settled = world.records();
    let CliSync::Synced(synced) = world.sync().unwrap() else {
        panic!("a synced fleet");
    };
    assert_eq!(synced.own_row, None, "settled");
    assert_eq!(world.records(), settled, "settled");

    for (case, occupant, lists) in [
        (
            "another account's login in a directory it left",
            "z@example.com",
            true,
        ),
        ("its own account's in one it never left", B, false),
    ] {
        let world = World::new("codex-default-back-refused-");
        let store = world.store();
        let dir_b = store.login_dir("b").unwrap();
        write_login(&dir_b, B);
        world.login(B);
        assert!(matches!(world.sync(), Ok(CliSync::Synced(_))), "{case}");
        write_login(&dir_b, occupant);
        if !lists {
            let mut logins = store.load_logins(NOW).unwrap().0;
            logins.labels.get_mut("b").unwrap().was.clear();
            store.save_logins(&logins).unwrap();
        }
        world.login(A);
        let before = world.records();
        let CliSync::Synced(synced) = world.sync().unwrap() else {
            panic!("{case}: a synced fleet");
        };
        let refused = synced.default_login.refused.expect(case);
        assert!(
            refused.contains("`b`'s managed directory") && refused.ends_with("no row moved"),
            "{case}: {refused}"
        );
        assert_eq!(
            world.rows().row_for_login(None).unwrap().label,
            "b",
            "{case}"
        );
        assert_eq!(world.records()[1..], before[1..], "{case}: no record moved");
    }
}

/// `accounts sync --cli codex` with no source named: its exit, stdout and stderr.
fn sync_words(world: &World) -> (ExitCode, String, String) {
    let fetch = |_: &Env, _: &str| -> Result<String, String> { panic!("no source is fetched") };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_cli_accounts_sync_with(
        Cli::Codex,
        (&world.env(), None),
        (None, None, false),
        (&mut out, &mut err),
        &fetch,
    );
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

/// FOLLOWING THE DEFAULT LOGIN READS NO SOURCE: on a Mac with nothing to read a sync still moves the
/// login to the row that holds its address, which is what it came to do, and says what stands where
/// it cannot, then fails as such a sync does. The step it names works: a row of that address takes
/// the login. CONTROL: a login on its row's address is a plain nothing to read.
#[test]
fn a_sync_with_no_source_still_follows_the_default_login() {
    let world = World::new("codex-default-no-source-");
    fs::remove_file(world.home.join(".secrets").join(cli_fleet_file(Cli::Codex))).unwrap();
    assert_eq!(world.sync(), Ok(CliSync::NoSource), "CONTROL");
    let (code, _, err) = sync_words(&world);
    assert_eq!(code, ExitCode::FAILURE, "CONTROL");
    assert!(err.contains("nothing to read"), "CONTROL: {err}");

    world.login("far@example.com");
    let before = world.rows();
    let (code, out, err) = sync_words(&world);
    assert_eq!(code, ExitCode::FAILURE, "{out}{err}");
    assert!(err.contains("nothing to read"), "{err}");
    assert!(
        out.contains("default login: it names far@example.com"),
        "{out}"
    );
    assert!(
        out.contains("`aterm harness accounts login a --cli codex`"),
        "{out}"
    );
    assert_eq!(world.rows(), before, "nothing holds that address");
    let step = "aterm harness accounts add <label> --cli codex --email far@example.com";
    assert!(out.contains(step), "{out}");

    let mut rows = world.rows();
    rows.rows.push(Row {
        label: "far".into(),
        email: Some("far@example.com".into()),
        dir: Some(
            world
                .store()
                .login_dir("far")
                .unwrap()
                .display()
                .to_string(),
        ),
        ..Row::default()
    });
    world.save_rows(&rows);
    let (code, out, err) = sync_words(&world);
    assert_eq!(code, ExitCode::SUCCESS, "{out}{err}");
    assert!(out.contains("on this machine's own login: far"), "{out}");
    assert_eq!(world.rows().row_for_login(None).unwrap().label, "far");
    assert_eq!(
        world.rows().row("a").unwrap().dir.as_deref(),
        world.store().login_dir("a").unwrap().to_str()
    );
}
