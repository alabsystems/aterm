// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct Fixture {
    _tmp: Tmp,
    env: Env,
    store: accounts::Store,
    path: PathBuf,
    auth: PathBuf,
    program: PathBuf,
}

impl Fixture {
    fn new(lone: bool, mode: &str, reads_named: bool) -> Self {
        let tmp = tmp_dir("default-identity-report");
        let env = env_at(&tmp);
        let home = env.home.as_ref().unwrap();
        let auth = home.join(".codex/auth.json");
        std::fs::create_dir_all(auth.parent().unwrap()).unwrap();
        std::fs::write(&auth, "{}").unwrap();
        let work = tmp.path().join("work-login");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(
            work.join("auth.json"),
            auth_document(Some("work@example.test")),
        )
        .unwrap();
        let mut rows = vec![accounts::Row {
            label: "main".into(),
            email: Some("alias@example.test".into()),
            identity_email: Some("owner@example.test".into()),
            ..accounts::Row::default()
        }];
        if !lone {
            rows.push(accounts::Row {
                label: "work".into(),
                email: Some("work@example.test".into()),
                dir: Some(work.to_string_lossy().into_owned()),
                ..accounts::Row::default()
            });
        }
        let path = env.accounts_roster_for(Cli::Codex).unwrap();
        std::fs::write(
            &path,
            accounts::append_rows_for(Cli::Codex, "", &rows).unwrap(),
        )
        .unwrap();
        std::fs::write(
            env.config.as_ref().unwrap(),
            format!("[harness]\naccounts = \"{mode}\"\n"),
        )
        .unwrap();
        let store = env.accounts_store_for(Cli::Codex).unwrap();
        store
            .transact(env.now, &mut None, |state| {
                state.active = Some("main".into());
                state.proven = true;
                Ok(Vec::new())
            })
            .unwrap();
        let program =
            crate::harness::codex_alab::lay_package(&tmp.path().join("program"), reads_named);
        Self {
            _tmp: tmp,
            env,
            store,
            path,
            auth,
            program,
        }
    }

    fn records(&self) -> accounts::AccountRecords {
        self.store
            .read_account_records(Some(&self.path), self.env.now)
            .unwrap()
    }

    fn reports(&self) -> (String, Value) {
        let records = self.records();
        let render = |json| {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            assert_eq!(
                run_accounts_report_snapshot(
                    (
                        Cli::Codex,
                        Launch {
                            program: Some(&self.program),
                            ..Launch::default()
                        },
                    ),
                    &self.env,
                    json,
                    &self.path,
                    Ok(records.clone()),
                    &mut out,
                    &mut err,
                ),
                ExitCode::SUCCESS,
                "{}",
                String::from_utf8_lossy(&err)
            );
            String::from_utf8(out).unwrap()
        };
        (render(false), aterm_json::from_str(&render(true)).unwrap())
    }

    fn use_takes(&self) -> UseTakes {
        let records = self.records();
        let rotation = rotation_beside(Cli::Codex, &self.env, &self.path).1;
        let enrolled = reported_enrolled(Cli::Codex, &records.roster, &self.env);
        use_recorded(
            (
                Cli::Codex,
                Launch {
                    program: Some(&self.program),
                    ..Launch::default()
                },
            ),
            &rotation,
            records
                .state
                .active
                .as_deref()
                .filter(|_| records.state.pinned),
            (&records.roster, &records.state, &enrolled),
            self.env.now,
        )
        .0
    }

    fn images(&self) -> [Option<Vec<u8>>; 7] {
        [
            self.store.state_file(),
            self.store.logins_file(),
            self.store.ledger_path(),
            self.store.wake_path(),
            self.store.state_witness_file(),
            self.path.clone(),
            self.auth.clone(),
        ]
        .map(|path| std::fs::read(path).ok())
    }
}

fn auth_document(email: Option<&str>) -> Vec<u8> {
    let token = crate::harness::codex_app_login::tests::make_jwt(&aterm_json::json!({
        "email": email,
        "https://api.openai.com/auth": {"chatgpt_user_id": "fixture-user"}
    }));
    aterm_json::to_vec(&aterm_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {"id_token": token, "access_token": token}
    }))
    .unwrap()
}

#[test]
fn reports_refuse_a_known_default_identity_mismatch_without_changing_records() {
    for (lone, mode, reads_named) in [
        (false, "off", true),
        (true, "early", true),
        (false, "early", false),
    ] {
        let fixture = Fixture::new(lone, mode, reads_named);
        std::fs::write(&fixture.auth, auth_document(Some("other@example.test"))).unwrap();
        let before = fixture.images();
        let (text, json) = fixture.reports();
        assert_eq!(json["next"]["to"], Value::Null);
        assert!(
            json["next"]["none"]
                .as_str()
                .is_some_and(|why| why.contains("other@example.test")),
            "{json}"
        );
        assert!(text.contains("next: none"));
        assert!(text.contains("other@example.test"), "{text}");
        assert_eq!(json["active"].as_str(), Some("main"));
        assert_eq!(json["sessions"], Value::from(0));
        assert_eq!(fixture.use_takes(), UseTakes::Refused);
        assert_eq!(fixture.images(), before);

        for email in [Some("owner@example.test"), None] {
            std::fs::write(&fixture.auth, auth_document(email)).unwrap();
            let (_, json) = fixture.reports();
            if mode == "off" {
                assert_eq!(fixture.use_takes(), UseTakes::NoRow);
            } else {
                assert_eq!(json["next"]["to"].as_str(), Some("main"));
            }
            assert!(
                !json["next"]["none"]
                    .as_str()
                    .is_some_and(|why| why.contains("other@example.test")),
                "{json}"
            );
        }
    }
}

#[test]
fn reports_keep_an_independent_dedicated_login_when_the_default_has_drifted() {
    let fixture = Fixture::new(false, "off", true);
    std::fs::write(&fixture.auth, auth_document(Some("other@example.test"))).unwrap();
    fixture
        .store
        .transact(fixture.env.now, &mut None, |state| {
            state.active = Some("work".into());
            state.pinned = true;
            Ok(Vec::new())
        })
        .unwrap();
    let before = fixture.images();
    let (_, json) = fixture.reports();
    assert_eq!(json["next"]["to"].as_str(), Some("work"));
    assert_eq!(fixture.use_takes(), UseTakes::Row("work".into()));
    assert_eq!(fixture.images(), before);
}

#[test]
fn reports_refuse_a_selected_dedicated_login_that_names_another_account() {
    for mode in ["off", "early"] {
        let fixture = Fixture::new(false, mode, true);
        std::fs::write(&fixture.auth, auth_document(Some("owner@example.test"))).unwrap();
        fixture
            .store
            .transact(fixture.env.now, &mut None, |state| {
                state.active = Some("work".into());
                state.pinned = true;
                Ok(Vec::new())
            })
            .unwrap();
        let records = fixture.records();
        let work_auth = PathBuf::from(records.roster.row("work").unwrap().dir.as_ref().unwrap())
            .join("auth.json");
        let replaced = auth_document(Some("other@example.test"));
        std::fs::write(&work_auth, &replaced).unwrap();
        let before = fixture.images();
        let (text, json) = fixture.reports();
        assert_eq!(json["next"]["to"], Value::Null);
        assert!(
            json["next"]["none"]
                .as_str()
                .is_some_and(|why| { why.contains("expected account") })
        );
        assert!(text.contains("next: none"));
        assert!(text.contains("expected account"));
        assert_eq!(fixture.use_takes(), UseTakes::Refused);
        assert_eq!(fixture.images(), before);
        assert_eq!(std::fs::read(&work_auth).unwrap(), replaced);

        for email in [Some("work@example.test"), None] {
            let accepted = auth_document(email);
            std::fs::write(&work_auth, &accepted).unwrap();
            let (_, json) = fixture.reports();
            assert_eq!(json["next"]["to"].as_str(), Some("work"));
            assert_eq!(fixture.use_takes(), UseTakes::Row("work".into()));
            assert_eq!(fixture.images(), before);
            assert_eq!(std::fs::read(&work_auth).unwrap(), accepted);
        }
    }
}

#[test]
fn reports_and_review_choose_the_valid_alternative_a_managed_launch_uses() {
    let fixture = Fixture::new(false, "early", true);
    std::fs::write(&fixture.auth, auth_document(Some("owner@example.test"))).unwrap();
    fixture
        .store
        .transact(fixture.env.now, &mut None, |state| {
            state.active = Some("work".into());
            state.pinned = false;
            Ok(Vec::new())
        })
        .unwrap();
    let records = fixture.records();
    let work_auth =
        PathBuf::from(records.roster.row("work").unwrap().dir.as_ref().unwrap()).join("auth.json");
    let displaced = auth_document(Some("other@example.test"));
    std::fs::write(&work_auth, &displaced).unwrap();
    let before = fixture.images();
    let (_, json) = fixture.reports();
    assert_eq!(json["next"]["to"].as_str(), Some("main"));
    assert_eq!(fixture.use_takes(), UseTakes::Row("main".into()));
    let capture = crate::harness::accounts_sync::capture_current_review(
        &fixture.store,
        Cli::Codex,
        Some(&fixture.path),
        accounts::Mode::Early,
        Some(&fixture.program),
        fixture.env.now,
    )
    .unwrap();
    assert_eq!(capture.witness.row().unwrap().label, "main");
    assert_eq!(fixture.images(), before);
    assert_eq!(std::fs::read(&work_auth).unwrap(), displaced);

    let seat = crate::harness::accounts_pool::Seat {
        config_dir: fixture.path.parent().unwrap(),
        state: &fixture.env.state,
        home: fixture.env.home.as_deref().unwrap(),
        now: fixture.env.now,
    };
    let no_network = |_: Cli, _: &Path, _: Option<&str>, _: i64| None;
    let managed = crate::harness::accounts_pool::launch_env_for(
        &seat,
        Cli::Codex,
        &no_network,
        Some(&fixture.program),
    )
    .unwrap()
    .unwrap();
    assert_eq!(managed.label, "main");
    assert_eq!(std::fs::read(&work_auth).unwrap(), displaced);
    assert_eq!(
        std::fs::read(&fixture.auth).unwrap().as_slice(),
        before[6].as_ref().unwrap().as_slice()
    );
}

#[test]
fn a_wall_reset_does_not_hide_a_terminal_identity_refusal() {
    for (lone, reads_named) in [(true, true), (false, true), (false, false)] {
        let fixture = Fixture::new(lone, "early", reads_named);
        let wrong_default = lone || !reads_named;
        std::fs::write(
            &fixture.auth,
            auth_document(Some(if wrong_default {
                "other@example.test"
            } else {
                "owner@example.test"
            })),
        )
        .unwrap();
        if !wrong_default {
            let records = fixture.records();
            let work_auth =
                PathBuf::from(records.roster.row("work").unwrap().dir.as_ref().unwrap())
                    .join("auth.json");
            std::fs::write(work_auth, auth_document(Some("other@example.test"))).unwrap();
        }
        fixture
            .store
            .transact(fixture.env.now, &mut None, |state| {
                state.active = Some(if wrong_default { "main" } else { "work" }.into());
                state.pinned = !wrong_default;
                Ok(accounts::note_wall(
                    state,
                    "main",
                    "five_hour",
                    Some(fixture.env.now + 3600),
                    fixture.env.now,
                    fixture.env.now,
                ))
            })
            .unwrap();
        let before = fixture.images();
        let (text, json) = fixture.reports();
        assert_eq!(json["next"]["to"], Value::Null);
        assert_eq!(json["next"]["wait_until"], Value::Null);
        // The default login's row says what that login names; a pinned row of its own says only
        // that its login is not that row's.
        let refusal = if wrong_default {
            "other@example.test"
        } else {
            "expected account"
        };
        assert!(
            json["next"]["none"]
                .as_str()
                .is_some_and(|why| why.contains(refusal)),
            "{json}"
        );
        assert!(text.contains("next: none"));
        assert!(text.contains(refusal), "{text}");
        assert!(!text.contains("next: wait until"));
        assert_eq!(fixture.use_takes(), UseTakes::Refused);
        assert_eq!(fixture.images(), before);

        if !reads_named {
            std::fs::write(&fixture.auth, auth_document(Some("owner@example.test"))).unwrap();
            let before = fixture.images();
            let until = fixture
                .records()
                .state
                .account("main")
                .limited_until(fixture.env.now)
                .unwrap();
            let (_, matching) = fixture.reports();
            assert_eq!(matching["next"]["to"], Value::Null);
            assert_eq!(matching["next"]["wait_until"], Value::from(until));
            assert_eq!(matching["next"]["none"], Value::Null);
            assert_eq!(fixture.use_takes(), UseTakes::Refused);
            assert_eq!(fixture.images(), before);
        }
    }
}

#[test]
fn non_file_backend_reports_ignore_a_contradictory_stale_default_auth_file() {
    for backend in ["keyring", "auto", "ephemeral", "unknown"] {
        let fixture = Fixture::new(false, "off", true);
        std::fs::write(
            fixture.auth.parent().unwrap().join("config.toml"),
            format!("cli_auth_credentials_store = \"{backend}\"\n"),
        )
        .unwrap();
        let (_, baseline) = fixture.reports();
        std::fs::write(&fixture.auth, auth_document(Some("other@example.test"))).unwrap();
        let records = fixture.records();
        let row = records.roster.row("work").unwrap();
        let work_auth = PathBuf::from(row.dir.as_ref().unwrap()).join("auth.json");
        let stale_work = auth_document(Some("another@example.test"));
        std::fs::write(&work_auth, &stale_work).unwrap();
        let before = fixture.images();
        let (_, changed) = fixture.reports();
        assert_eq!(changed["next"], baseline["next"]);
        assert_eq!(fixture.use_takes(), UseTakes::NoRow);
        assert!(
            crate::harness::accounts_pool::check_launch_login_identity(
                Cli::Codex,
                &records.roster,
                Some(row),
                fixture.env.home.as_deref().unwrap(),
                None,
                None,
            )
            .is_ok()
        );
        assert_eq!(fixture.images(), before);
        assert_eq!(std::fs::read(work_auth).unwrap(), stale_work);
    }
}
