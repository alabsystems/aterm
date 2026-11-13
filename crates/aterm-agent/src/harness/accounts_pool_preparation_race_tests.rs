// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn fallback_uses_the_post_poll_state_as_its_preparation_snapshot() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for aba in [false, true] {
        let world = World::new("post-poll-fallback");
        world.two(Cli::Codex);
        let spare = world.root.join("spare");
        std::fs::create_dir_all(&spare).unwrap();
        std::fs::write(spare.join("auth.json"), "{}").unwrap();
        let mut roster = world.seat().roster(Cli::Codex).unwrap();
        roster.rows.push(Row {
            label: "spare".into(),
            class: accounts::Class::Pool,
            dir: Some(spare.to_string_lossy().into_owned()),
            ..Row::default()
        });
        std::fs::write(
            world.cfg.join(Cli::Codex.accounts().roster_file),
            accounts::append_rows_for(Cli::Codex, "", &roster.rows).unwrap(),
        )
        .unwrap();
        world.on_main(Cli::Codex, &[]);
        wall(&world, Cli::Codex, "main", NOW + HOUR);
        let store = world.seat().store(Cli::Codex);
        store
            .transact(NOW, &mut None, |state| {
                state.active = Some(if aba { "work" } else { "main" }.into());
                Ok(Vec::new())
            })
            .unwrap();
        let polled = AtomicBool::new(false);
        let read = |_: Cli, _: &Path, _: Option<&str>, _: i64| {
            if !polled.swap(true, Ordering::SeqCst) {
                store
                    .transact(NOW, &mut None, |state| {
                        state.active = Some(if aba { "main" } else { "work" }.into());
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            None
        };
        let mut attempted = Vec::new();
        let result =
            super::super::launch_env_using(&world.seat(), Cli::Codex, &read, None, |row| {
                attempted.push(row.label.clone());
                if row.label == "work" {
                    return Err(io::Error::other("failed preparation"));
                }
                if aba {
                    store
                        .transact(NOW, &mut None, |state| {
                            state.active = Some("work".into());
                            Ok(Vec::new())
                        })
                        .unwrap();
                }
                login_env(Cli::Codex, row, &world.home)
            });
        assert!(polled.load(Ordering::SeqCst));
        assert_eq!(attempted, ["work", "spare"]);
        if aba {
            assert!(result.is_err_and(|why| why.contains("active account changed")));
        } else {
            assert_eq!(result.unwrap().unwrap().label, "spare");
        }
    }
}

#[test]
fn a_lost_login_is_recorded_before_any_move_and_never_retried_in_place() {
    for default in [false, true] {
        for mode in 0..3 {
            let world = World::new("lost-login-fallback");
            let work = world.two(Cli::Codex);
            world.on_main(Cli::Codex, &[]);
            let source = if default { "main" } else { "work" };
            let other = if default { "work" } else { "main" };
            let store = world.seat().store(Cli::Codex);
            store
                .transact_logins(NOW, &mut None, |state, logins| {
                    state.active = Some(source.into());
                    state.proven = mode != 2;
                    state.pinned = mode == 1;
                    Ok(stage_current_auth(
                        &world,
                        Cli::Codex,
                        state,
                        logins,
                        source,
                        accounts::Auth::Ok,
                        NOW,
                    ))
                })
                .unwrap();
            let auth = work.join("auth.json").to_string_lossy().into_owned();
            let env = [("CODEX_AUTH_PATH", auth.as_str())];
            let mut session = session(Cli::Codex, Supervisor::Aterm, Mode::Early, &world.exe);
            if !default {
                session.env = &env;
            }
            session.sample = None;
            session.point = Point::LostLogin { seen: NOW - MIN };
            let mut attempted = Vec::new();
            let moved = super::session_move_using(&world.seat(), &session, |row| {
                attempted.push(row.label.clone());
                if row.label == source {
                    return None;
                }
                let env = login_env(Cli::Codex, row, &world.home).ok()?;
                relaunch::pool_line(&session.relaunch, Cli::Codex, &env).ok()
            });
            // The lost login is the row's fact before any move is weighed: the row reads
            // signed out under its credential context, so no relaunch is tried on it again
            // (a renewal gives it a new context, which this word does not cover).
            assert!(
                !attempted.iter().any(|label| label == source),
                "{attempted:?}"
            );
            if let Advice::Move { to, .. } = &moved.advice {
                assert_eq!(to, other);
                assert!(moved.line.is_some());
            }
            if mode == 0 {
                assert!(matches!(moved.advice, Advice::Move { to, .. } if to == other));
                assert_eq!(world.load(Cli::Codex).rotations.len(), 1);
            }
            let mut state = world.load(Cli::Codex);
            let roster = world.seat().bound_roster(Cli::Codex).unwrap();
            store.hydrate_auth_contexts(&mut state, &roster);
            assert_eq!(
                state
                    .account(source)
                    .current_auth(roster.row(source).unwrap()),
                accounts::Auth::SignedOut,
                "mode {mode}, default {default}"
            );
        }
    }
}

#[test]
fn launch_fallback_does_not_bypass_an_existing_live_canary() {
    let world = World::new("launch-canary-fallback");
    world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    world
        .seat()
        .store(Cli::Codex)
        .transact(NOW, &mut None, |state| {
            state.proven = false;
            state.canary = Some(accounts::Canary {
                sid: "held-canary".into(),
                at: NOW,
                took: false,
                native_receipt: None,
            });
            Ok(Vec::new())
        })
        .unwrap();
    let result =
        super::super::launch_env_using(&world.seat(), Cli::Codex, &no_usage, None, |row| {
            if row.label == "main" {
                return Err(io::Error::other("failed preparation"));
            }
            login_env(Cli::Codex, row, &world.home)
        });
    assert!(result.is_err_and(|why| why.contains("active account changed")));
    let state = world.load(Cli::Codex);
    assert!(!state.proven);
    assert_eq!(state.canary.unwrap().sid, "held-canary");
    assert!(state.rotations.is_empty());
}
