// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn coherent_codex_auth(email: Option<&str>) -> Vec<u8> {
    let token = crate::harness::codex_app_login::tests::make_jwt(&aterm_json::json!({
        "email": email,
        "https://api.openai.com/auth": {
            "chatgpt_user_id": "fixture-user",
            "chatgpt_account_id": "fixture-account"
        }
    }));
    aterm_json::to_vec(&aterm_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {"id_token": token, "access_token": token}
    }))
    .unwrap()
}

#[test]
fn a_passthrough_codex_launch_checks_the_known_default_identity() {
    for (lone, mode) in [(false, "off"), (true, "early")] {
        let world = World::new("passthrough-codex-identity");
        world.two(Cli::Codex);
        let mut roster = world.seat().roster(Cli::Codex).unwrap();
        roster.rows[0].email = Some("alias@example.test".into());
        roster.rows[0].identity_email = Some("owner@example.test".into());
        if lone {
            roster.rows.truncate(1);
        }
        std::fs::write(
            world.cfg.join(Cli::Codex.accounts().roster_file),
            accounts::append_rows_for(Cli::Codex, "", &roster.rows).unwrap(),
        )
        .unwrap();
        std::fs::write(
            world.cfg.join("aterm.toml"),
            format!("[harness]\naccounts = \"{mode}\"\n"),
        )
        .unwrap();
        let auth = world.home.join(".codex/auth.json");
        let no_poll = |_: Cli, _: &Path, _: Option<&str>, _: i64| -> Option<Usage> {
            panic!("a pass-through launch reads no provider usage")
        };
        let replaced = coherent_codex_auth(Some("other@example.test"));
        std::fs::write(&auth, &replaced).unwrap();
        assert!(super::super::launch_env(&world.seat(), Cli::Codex, &no_poll).is_err());
        assert!(super::super::default_row_now(&world.seat(), Cli::Codex).is_err());
        assert_eq!(std::fs::read(&auth).unwrap(), replaced);

        for email in [Some("owner@example.test"), None] {
            let accepted = coherent_codex_auth(email);
            std::fs::write(&auth, &accepted).unwrap();
            assert_eq!(
                super::super::launch_env(&world.seat(), Cli::Codex, &no_poll).unwrap(),
                None
            );
            assert!(matches!(
                super::super::default_row_now(&world.seat(), Cli::Codex).unwrap(),
                crate::harness::codex_alab::DefaultRow::Serves(label) if label == "main"
            ));
            assert_eq!(std::fs::read(&auth).unwrap(), accepted);
        }
    }
}

#[test]
fn a_default_identity_mismatch_does_not_refuse_an_independent_pinned_login() {
    let world = World::new("independent-codex-identity");
    let work = world.two(Cli::Codex);
    let mut roster = world.seat().roster(Cli::Codex).unwrap();
    for (row, email) in roster
        .rows
        .iter_mut()
        .zip(["owner@example.test", "work@example.test"])
    {
        row.email = Some(email.into());
    }
    std::fs::write(
        world.cfg.join(Cli::Codex.accounts().roster_file),
        accounts::append_rows_for(Cli::Codex, "", &roster.rows).unwrap(),
    )
    .unwrap();
    std::fs::write(
        world.cfg.join("aterm.toml"),
        "[harness]\naccounts = 'off'\n",
    )
    .unwrap();
    let default_auth = world.home.join(".codex/auth.json");
    let work_auth = work.join("auth.json");
    let replaced = coherent_codex_auth(Some("other@example.test"));
    let independent = coherent_codex_auth(Some("work@example.test"));
    std::fs::write(&default_auth, &replaced).unwrap();
    std::fs::write(&work_auth, &independent).unwrap();
    pin(&world, Cli::Codex, "work").unwrap();
    let no_poll = |_: Cli, _: &Path, _: Option<&str>, _: i64| -> Option<Usage> {
        panic!("rotation off reads no provider usage")
    };
    let launch = super::super::launch_env(&world.seat(), Cli::Codex, &no_poll)
        .unwrap()
        .unwrap();
    assert_eq!(launch.label, "work");
    assert_eq!(std::fs::read(default_auth).unwrap(), replaced);
    assert_eq!(std::fs::read(work_auth).unwrap(), independent);
}

#[test]
fn an_observed_launch_preserves_the_last_identity_failure_when_alternatives_end() {
    let world = World::new("observed-codex-exhaustion");
    let work = world.two(Cli::Codex);
    let mut roster = world.seat().roster(Cli::Codex).unwrap();
    roster.rows[0].email = Some("owner@example.test".into());
    roster.rows[1].email = Some("work@example.test".into());
    std::fs::write(
        world.cfg.join(Cli::Codex.accounts().roster_file),
        accounts::append_rows_for(Cli::Codex, "", &roster.rows).unwrap(),
    )
    .unwrap();
    let token = |user| {
        crate::harness::codex_app_login::tests::make_jwt(&aterm_json::json!({
            "email": "owner@example.test",
            "https://api.openai.com/auth": {"chatgpt_user_id": user}
        }))
    };
    let conflicting = aterm_json::to_vec(&aterm_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {"id_token": token("one"), "access_token": token("another")}
    }))
    .unwrap();
    let default_auth = world.home.join(".codex/auth.json");
    let work_auth = work.join("auth.json");
    let displaced = coherent_codex_auth(Some("other@example.test"));
    std::fs::write(&default_auth, &conflicting).unwrap();
    std::fs::write(&work_auth, &displaced).unwrap();
    world.on_main(Cli::Codex, &[]);
    world
        .seat()
        .store(Cli::Codex)
        .transact(NOW, &mut None, |state| {
            state.active = Some("work".into());
            state.pinned = false;
            Ok(Vec::new())
        })
        .unwrap();
    let seat = world.seat();
    let state = world.load(Cli::Codex);
    let enrolled = accounts::enrolled_in(Cli::Codex, &roster, &world.home);
    let before = [
        seat.store(Cli::Codex).state_file(),
        seat.store(Cli::Codex).logins_file(),
    ]
    .map(|path| std::fs::read(path).ok());
    let refused = super::super::launch_observed(
        (Cli::Codex, false, Mode::Early),
        &roster,
        &state,
        &enrolled,
        Some(&world.exe),
        NOW,
        (&world.home, None),
    )
    .unwrap_err();
    assert!(refused.contains("different users"));
    assert_eq!(world.load(Cli::Codex), state);
    assert_eq!(
        [
            seat.store(Cli::Codex).state_file(),
            seat.store(Cli::Codex).logins_file()
        ]
        .map(|path| std::fs::read(path).ok()),
        before
    );
    assert_eq!(std::fs::read(default_auth).unwrap(), conflicting);
    assert_eq!(std::fs::read(work_auth).unwrap(), displaced);
}

#[test]
fn a_passthrough_codex_launch_does_not_judge_a_non_file_backends_stale_file() {
    for backend in ["keyring", "auto", "ephemeral", "unknown"] {
        let world = World::new("passthrough-codex-backend");
        world.two(Cli::Codex);
        let mut roster = world.seat().roster(Cli::Codex).unwrap();
        roster.rows[0].email = Some("owner@example.test".into());
        std::fs::write(
            world.cfg.join(Cli::Codex.accounts().roster_file),
            accounts::append_rows_for(Cli::Codex, "", &roster.rows).unwrap(),
        )
        .unwrap();
        std::fs::write(
            world.home.join(".codex/config.toml"),
            format!("cli_auth_credentials_store = \"{backend}\"\n"),
        )
        .unwrap();
        let path = world.home.join(".codex/auth.json");
        let stale = coherent_codex_auth(Some("other@example.test"));
        std::fs::write(&path, &stale).unwrap();
        assert!(world.seat().keyring_held(Cli::Codex));
        let no_poll = |_: Cli, _: &Path, _: Option<&str>, _: i64| -> Option<Usage> {
            panic!("a non-file backend reads no file-account usage")
        };
        assert_eq!(
            super::super::launch_env(&world.seat(), Cli::Codex, &no_poll).unwrap(),
            None
        );
        assert_eq!(std::fs::read(path).unwrap(), stale);
    }
}

#[test]
fn named_claude_login_refuses_another_account_identity() {
    for default in [false, true] {
        for legacy in [false, true] {
            let world = World::new("named-claude-identity-replacement");
            let work = world.two(Cli::Claude);
            let directory = if default {
                world.home.join(".claude")
            } else {
                work
            };
            std::fs::create_dir_all(&directory).unwrap();
            let path = if default && !legacy {
                world.home.join(".claude.json")
            } else {
                directory.join(if legacy {
                    ".config.json"
                } else {
                    ".claude.json"
                })
            };
            let bytes = br#"{"oauthAccount":{"emailAddress":"other@example.test","accountUuid":"another-account"}}"#;
            std::fs::write(&path, bytes).unwrap();
            let mut roster = world.seat().roster(Cli::Claude).unwrap();
            let row = &mut roster.rows[usize::from(!default)];
            row.email = Some("expected@example.test".into());
            let result = super::super::login_env(Cli::Claude, row, &world.home);
            assert!(
                result.is_err(),
                "default={default}, legacy={legacy}: {result:?}"
            );
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
    }
}

#[test]
fn named_codex_login_refuses_another_consistent_account_identity() {
    for default in [false, true] {
        let world = World::new("named-codex-identity-replacement");
        let work = world.two(Cli::Codex);
        let directory = if default {
            world.home.join(".codex")
        } else {
            work
        };
        std::fs::create_dir_all(&directory).unwrap();
        let token = crate::harness::codex_app_login::tests::make_jwt(&aterm_json::json!({
            "email": "other@example.test",
            "https://api.openai.com/auth": {
                "chatgpt_user_id": "other-user",
                "chatgpt_account_id": "other-workspace"
            }
        }));
        let bytes = aterm_json::to_vec(&aterm_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {"id_token": token, "access_token": token}
        }))
        .unwrap();
        let path = directory.join("auth.json");
        std::fs::write(&path, &bytes).unwrap();
        let mut roster = world.seat().roster(Cli::Codex).unwrap();
        let row = &mut roster.rows[usize::from(!default)];
        row.email = Some("expected@example.test".into());
        let result = super::super::login_env(Cli::Codex, row, &world.home);
        assert!(result.is_err(), "default={default}: {result:?}");
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn polling_checks_identity_and_passes_the_canonical_expectation_to_its_reader() {
    for default in [false, true] {
        let world = World::new("named-codex-usage");
        let work = world.two(Cli::Codex);
        let home = world.home.join(".codex");
        let mut roster = world.seat().roster(Cli::Codex).unwrap();
        for row in &mut roster.rows {
            row.email = Some(format!("{}-alias@example.test", row.label));
            row.identity_email = Some(format!("{}-canonical@example.test", row.label));
        }
        std::fs::write(
            world.cfg.join(Cli::Codex.accounts().roster_file),
            accounts::append_rows_for(Cli::Codex, "", &roster.rows).unwrap(),
        )
        .unwrap();
        for (index, directory) in [&home, &work].into_iter().enumerate() {
            let email = if index == usize::from(!default) {
                "wrong@example.test".to_owned()
            } else {
                format!(
                    "{}-canonical@example.test",
                    if index == 0 { "main" } else { "work" }
                )
            };
            let token = crate::harness::codex_app_login::tests::make_jwt(
                &aterm_json::json!({"email":email}),
            );
            std::fs::create_dir_all(directory).unwrap();
            std::fs::write(
                directory.join("auth.json"),
                aterm_json::to_vec(&aterm_json::json!({
                    "auth_mode":"chatgpt", "tokens":{"id_token":token, "access_token":token}
                }))
                .unwrap(),
            )
            .unwrap();
        }
        let bad = if default { &home } else { &work };
        let evidence = std::fs::read(bad.join("auth.json")).unwrap();
        let seen = std::sync::Mutex::new(Vec::new());
        let expected_email = format!(
            "{}-canonical@example.test",
            if default { "work" } else { "main" }
        );
        let labels = record_usage(
            &world.seat(),
            Cli::Codex,
            &roster,
            &accounts::enrolled_for(Cli::Codex, &roster),
            &|_: Cli, directory: &Path, expected: Option<&str>, now: i64| {
                assert_ne!(directory, bad);
                assert_eq!(expected, Some(expected_email.as_str()));
                seen.lock().unwrap().push(directory.to_path_buf());
                Some(Usage {
                    sample: Sample {
                        at: now,
                        session_pct: Some(25),
                        ..Default::default()
                    },
                    walls: Vec::new(),
                })
            },
        )
        .unwrap();
        assert_eq!(labels, [if default { "work" } else { "main" }]);
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(!world.load(Cli::Codex).accounts.contains_key(if default {
            "main"
        } else {
            "work"
        }));
        assert_eq!(std::fs::read(bad.join("auth.json")).unwrap(), evidence);
    }
}

#[test]
fn running_codex_sessions_skip_a_conflicting_default_or_dedicated_login() {
    for default in [false, true] {
        let world = World::new("session-conflicting-codex-users");
        let work = world.two(Cli::Codex);
        let spare = world.root.join("spare");
        let path = world.cfg.join(Cli::Codex.accounts().roster_file);
        let mut roster = world.seat().roster(Cli::Codex).unwrap();
        roster.rows.push(Row {
            label: "spare".into(),
            class: accounts::Class::Pool,
            dir: Some(spare.to_string_lossy().into_owned()),
            ..Row::default()
        });
        std::fs::write(
            path,
            accounts::append_rows_for(Cli::Codex, "", &roster.rows).unwrap(),
        )
        .unwrap();
        let document = |access_user: &str| {
            let token = |user: &str| {
                crate::harness::codex_app_login::tests::make_jwt(&aterm_json::json!({
                    "https://api.openai.com/auth":{"chatgpt_user_id":user, "chatgpt_account_id":"shared"}
                }))
            };
            aterm_json::to_vec(&aterm_json::json!({"auth_mode":"chatgpt", "tokens": {
                "id_token":token("user-a"), "access_token":token(access_user)
            }}))
            .unwrap()
        };
        let home = world.home.join(".codex");
        for dir in [&home, &work, &spare] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("auth.json"), document("user-a")).unwrap();
        }
        let failed = if default { &home } else { &work }.join("auth.json");
        let conflicting = document("user-b");
        std::fs::write(&failed, &conflicting).unwrap();
        world.on_main(Cli::Codex, &[]);
        world
            .seat()
            .store(Cli::Codex)
            .transact(NOW, &mut None, |state| {
                state.active = Some(if default { "work" } else { "main" }.into());
                Ok(Vec::new())
            })
            .unwrap();
        let auth = work.join("auth.json").to_string_lossy().into_owned();
        let env = [("CODEX_AUTH_PATH", auth.as_str())];
        let mut session = session(Cli::Codex, Supervisor::Aterm, Mode::Early, &world.exe);
        if default {
            session.env = &env;
        }
        session.point = Point::Wall {
            window: "five_hour".into(),
            at: None,
        };
        let moved = super::session_move(&world.seat(), &session);
        assert!(moved.line.is_some(), "default={default}: {moved:?}");
        assert!(matches!(moved.advice, Advice::Move { to, .. } if to == "spare"));
        assert_eq!(world.load(Cli::Codex).active.as_deref(), Some("spare"));
        assert_eq!(std::fs::read(failed).unwrap(), conflicting);
    }
}

#[test]
fn cached_usage_cannot_admit_conflicting_codex_users() {
    cached_usage_identity_case(false);
}

#[test]
fn cached_usage_cannot_admit_another_named_codex_account() {
    cached_usage_identity_case(true);
}

fn cached_usage_identity_case(named: bool) {
    for default in [false, true] {
        for (pinned, off, blocked) in [false, true].into_iter().flat_map(|blocked| {
            [
                (false, false, blocked),
                (true, false, blocked),
                (true, true, blocked),
            ]
        }) {
            let world = World::new("conflicting-codex-users");
            let work = world.two(Cli::Codex);
            let target = if default { "main" } else { "work" };
            if named {
                let mut roster = world.seat().roster(Cli::Codex).unwrap();
                for row in &mut roster.rows {
                    row.email = Some(
                        if row.label == target {
                            "owner@example.test"
                        } else {
                            "backup@example.test"
                        }
                        .into(),
                    );
                }
                std::fs::write(
                    world.cfg.join(Cli::Codex.accounts().roster_file),
                    accounts::append_rows_for(Cli::Codex, "", &roster.rows).unwrap(),
                )
                .unwrap();
            }
            world.on_main(Cli::Codex, &[]);
            let other = if default { "work" } else { "main" };
            let directory = if default {
                world.home.join(".codex")
            } else {
                work.clone()
            };
            std::fs::create_dir_all(&directory).unwrap();
            let token = |user, email| {
                crate::harness::codex_app_login::tests::make_jwt(&aterm_json::json!({
                    "email":email, "https://api.openai.com/auth":{
                        "chatgpt_user_id":user, "chatgpt_account_id":"shared-workspace"
                    }
                }))
            };
            let document = |access_user, email| {
                aterm_json::to_vec(&aterm_json::json!({
                    "auth_mode":"chatgpt", "tokens":{
                        "id_token":token("user-a", email), "access_token":token(if named { "user-a" } else { access_user }, email)
                    }
                }))
                .unwrap()
            };
            let path = directory.join("auth.json");
            let valid = document("user-a", "owner@example.test");
            for candidate in [world.home.join(".codex"), work] {
                let bytes = if candidate == directory {
                    valid.clone()
                } else {
                    document("user-a", "backup@example.test")
                };
                std::fs::create_dir_all(&candidate).unwrap();
                std::fs::write(candidate.join("auth.json"), bytes).unwrap();
            }
            if off {
                std::fs::write(world.cfg.join("aterm.toml"), "[harness]\naccounts='off'\n")
                    .unwrap();
            }
            let seat = world.seat();
            let roster = seat.roster(Cli::Codex).unwrap();
            record_usage(
                &seat,
                Cli::Codex,
                &roster,
                &accounts::enrolled_for(Cli::Codex, &roster),
                &|_: Cli, _: &Path, _: Option<&str>, _: i64| {
                    Some(Usage {
                        sample: Sample {
                            session_pct: Some(10),
                            at: NOW,
                            ..Sample::default()
                        },
                        walls: Vec::new(),
                    })
                },
            )
            .unwrap();
            seat.store(Cli::Codex)
                .transact_within(&accounts::LOCK_LADDER, NOW, &mut None, |state| {
                    state.active = Some(target.into());
                    state.pinned = pinned;
                    Ok(Vec::new())
                })
                .unwrap();
            if blocked {
                wall(&world, Cli::Codex, other, NOW + HOUR);
            }
            let conflicting = document(
                "user-b",
                if named {
                    "other@example.test"
                } else {
                    "owner@example.test"
                },
            );
            std::fs::write(&path, &conflicting).unwrap();
            let no_poll = |_: Cli, _: &Path, _: Option<&str>, _: i64| -> Option<Usage> {
                panic!("a fresh cached row was polled")
            };
            let result = super::super::launch_env(&seat, Cli::Codex, &no_poll);
            if pinned || blocked {
                assert!(
                    result.is_err_and(|why| why.contains(match (named, default) {
                        // A refusal of the default login names what it holds.
                        (true, true) => "other@example.test",
                        (true, false) => "expected account",
                        (false, _) => "different users",
                    })),
                    "default={default}, pinned={pinned}, off={off}, blocked={blocked}"
                );
            } else {
                assert_eq!(result.unwrap().unwrap().label, other);
            }
            assert_eq!(std::fs::read(&path).unwrap(), conflicting);
            std::fs::write(&path, &valid).unwrap();
            let admitted = super::super::launch_env(&seat, Cli::Codex, &no_poll)
                .unwrap()
                .unwrap();
            assert_eq!(admitted.label, target);
            assert_eq!(std::fs::read(&path).unwrap(), valid);
        }
    }
}
