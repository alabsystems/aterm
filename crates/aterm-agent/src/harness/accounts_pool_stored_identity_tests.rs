// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

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
            class: accounts::Class::Shared,
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
        let mut session = session(
            Cli::Codex,
            Supervisor::Aterm,
            Mode::Early,
            Path::new("/opt/codex"),
        );
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
                    result.is_err_and(|why| why.contains(if named {
                        "expected account"
                    } else {
                        "different users"
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
