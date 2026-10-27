// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn bytes(store: &accounts::Store) -> Vec<Option<Vec<u8>>> {
    [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ]
    .iter()
    .map(|path| std::fs::read(path).ok())
    .collect()
}

#[test]
fn transcript_usage_rejects_answers_before_the_process_started() {
    for started in [
        None,
        Some(ACCT_NOW - 59),
        Some(ACCT_NOW - 60),
        Some(ACCT_NOW - 61),
    ] {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let store = accounts::Store::at(root.path());
        let captured = store.read_usage_roster(None, ACCT_NOW).unwrap();
        let before = bytes(&store);
        let env = vec!["HOME=/synthetic".to_string()];
        let settled = || true;
        let answered = |_| Some(ACCT_NOW - 60);
        let called = std::cell::Cell::new(false);
        let said = |at| {
            called.set(true);
            accounts::UsageSaid {
                at,
                day: 1,
                credits: None,
                warning: Some(accounts::WarningRead {
                    weekly: false,
                    pct: 89,
                    reset: Some(ACCT_NOW + 3600),
                }),
            }
        };
        let read = UsageRead {
            env: &env,
            started,
            settled: &settled,
            answered: &answered,
            said: &said,
        };
        let word = super::super::account_usage_of(&store, &captured, "transcript", &read, ACCT_NOW);
        let allowed = started.is_some_and(|start| start <= ACCT_NOW - 60);
        assert_eq!(word, if allowed { "usage" } else { "not-this-process" });
        assert_eq!(called.get(), allowed);
        if !allowed {
            assert_eq!(bytes(&store), before);
        }
    }
}

#[test]
fn transcript_usage_and_walls_admit_the_captured_source_at_the_writer() {
    for implicit in [false, true] {
        for change in ["same", "renew", "policy", "missing", "store"] {
            if implicit && change == "missing" {
                continue;
            }
            let root = aterm_tempfile::TempDir::new().unwrap();
            let store = accounts::Store::at(&root.path().join("state"));
            let other = accounts::Store::at(&root.path().join("other"));
            let path = root.path().join("accounts.toml");
            let row = accounts::Row {
                label: "main".into(),
                email: Some("owner@example.test".into()),
                ..accounts::Row::default()
            };
            if !implicit {
                std::fs::write(&path, accounts::row_text(&row).unwrap()).unwrap();
            }
            let captured = store.read_usage_roster(Some(&path), ACCT_NOW).unwrap();
            let mutate = || match change {
                "renew" => {
                    store
                        .transact_logins(ACCT_NOW, &mut None, |state, logins| {
                            Ok(accounts::note_renewed(
                                state,
                                logins,
                                "main",
                                None,
                                "/login/new",
                                None,
                                ACCT_NOW,
                            ))
                        })
                        .unwrap();
                }
                "policy" => {
                    std::fs::write(
                        &path,
                        accounts::row_text(&accounts::Row {
                            email: Some("changed@example.test".into()),
                            ..row.clone()
                        })
                        .unwrap(),
                    )
                    .unwrap();
                }
                "missing" => {
                    std::fs::rename(&path, root.path().join("retained.toml")).unwrap();
                }
                _ => {}
            };
            let env = vec!["HOME=/Users//example".to_string()];
            let settled = || true;
            let answered = |_| Some(ACCT_NOW - 60);
            let said = |at| {
                mutate();
                accounts::UsageSaid {
                    at,
                    day: 1,
                    credits: None,
                    warning: Some(accounts::WarningRead {
                        weekly: false,
                        pct: 89,
                        reset: Some(ACCT_NOW + 3600),
                    }),
                }
            };
            let read = UsageRead {
                env: &env,
                started: Some(ACCT_NOW - 120),
                settled: &settled,
                answered: &answered,
                said: &said,
            };
            let writer = if change == "store" { &other } else { &store };
            let result =
                super::super::account_usage_of(writer, &captured, "transcript", &read, ACCT_NOW);
            let accepted = matches!(change, "same" | "renew");
            assert_eq!(
                result == "usage",
                accepted,
                "implicit={implicit} change={change}: {result}"
            );
            let before = bytes(writer);
            let wall = WallRead {
                env: &env,
                settled: true,
                started: Some(ACCT_NOW - 120),
                wall: Some(aterm_agent::harness::upgrade::StandingWall {
                    at: (ACCT_NOW - 30).unsigned_abs(),
                    until: Some((ACCT_NOW + 3600).unsigned_abs()),
                    window: Some("five_hour".into()),
                    credits: None,
                }),
            };
            let result =
                super::super::account_wall_of(writer, &captured, "transcript", &wall, ACCT_NOW);
            assert_eq!(
                result.0 == "wall",
                accepted,
                "implicit={implicit} change={change}: {result:?}"
            );
            if !accepted {
                assert_eq!(result.1, None);
                assert_eq!(before, bytes(writer));
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn taps_recheck_their_source_and_retry_refused_readings() {
    for implicit in [false, true] {
        for change in [
            "same",
            "renew",
            "policy",
            "missing",
            "context",
            "process-login",
        ] {
            if implicit && change == "missing" {
                continue;
            }
            let w = TapWorld::new();
            w.listed.store(!implicit, Ordering::SeqCst);
            let mut idle = w.worker(&tap_on(), tapped_argv(), true);
            let original = std::fs::read(w.roster_path()).ok();
            let original_seat = Arc::clone(&idle.hooks.acts.account_seat);
            let seats = Arc::clone(&original_seat);
            let world = Arc::clone(&w);
            let count = AtomicUsize::new(0);
            idle.hooks.acts.account_seat = Arc::new(move || {
                let mut seat = seats()?;
                if count.fetch_add(1, Ordering::SeqCst) == 1 {
                    match change {
                        "renew" => {
                            accounts::Store::at(&world.state())
                                .transact_logins(world.clock(), &mut None, |state, logins| {
                                    Ok(accounts::note_renewed(
                                        state,
                                        logins,
                                        "main",
                                        None,
                                        "/login/new",
                                        None,
                                        world.clock(),
                                    ))
                                })
                                .unwrap();
                        }
                        "policy" => {
                            std::fs::write(world.roster_path(), "[[account]]\nlabel='main'\nclass='dedicated'\nemail='changed@example.test'\n").unwrap();
                        }
                        "missing" => {
                            std::fs::rename(
                                world.roster_path(),
                                world.tmp.path().join("retained.toml"),
                            )
                            .unwrap();
                        }
                        "context" => {
                            seat.state = world.tmp.path().join("other-state");
                        }
                        _ => {}
                    }
                }
                Some(seat)
            });
            if change == "process-login" {
                idle.hooks.acts.account_env =
                    Arc::new(|_| Some(vec![format!("{}=/logins/work-2", accounts::LOGIN_ENV)]));
            }
            w.tap(10, 42, None);
            let result = idle.tap_step(TapAt::Point);
            let accepted = matches!(change, "same" | "renew");
            assert_eq!(
                w.sample().is_some(),
                accepted,
                "implicit={implicit} change={change}: {result:?}"
            );
            if !accepted && change != "process-login" {
                idle.hooks.acts.account_seat = original_seat;
                match &original {
                    Some(bytes) => std::fs::write(w.roster_path(), bytes).unwrap(),
                    None if w.roster_path().exists() => {
                        std::fs::rename(w.roster_path(), w.tmp.path().join("retained-added.toml"))
                            .unwrap()
                    }
                    None => {}
                }
                assert_eq!(
                    idle.tap_step(TapAt::Point),
                    Some("reading recorded"),
                    "same reading retried: {change}"
                );
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn equal_quota_windows_on_a_new_account_are_not_suppressed_by_the_old_heartbeat() {
    let w = TapWorld::new();
    w.listed.store(true, Ordering::SeqCst);
    let idle = w.worker(&tap_on(), tapped_argv(), true);
    w.tap(10, 42, None);
    assert_eq!(idle.tap_step(TapAt::Point), Some("reading recorded"));
    assert!(w.sample_of("main").is_some());
    assert!(w.sample_of("work-2").is_none());
    *w.env.lock().unwrap() = vec![format!("{}=/logins/work-2", accounts::LOGIN_ENV)];
    idle.kept.lock().unwrap().snapshot.as_mut().unwrap().login = Some("/logins/work-2".into());
    w.later(1);
    w.tap(20, 42, Some("/logins/work-2"));
    assert_eq!(idle.tap_step(TapAt::Point), Some("reading recorded"));
    assert_eq!(w.sample_of("work-2").unwrap().session_pct, Some(42));
}
