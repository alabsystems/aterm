// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn weekly_session(world: &World) -> Session<'_> {
    let mut s = session(Cli::Codex, Supervisor::Own, Mode::Early, &world.exe);
    s.point = Point::Wall {
        window: "seven_day".into(),
        at: Some(NOW),
    };
    s.sample.as_mut().unwrap().week_pct = Some(100);
    s
}

#[test]
fn native_weekly_plan_uses_pool_policy_without_giving_the_window_ownership() {
    let world = World::new("native-weekly-plan");
    world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let s = weekly_session(&world);
    let ordinary = super::super::session_plan(&world.seat(), &s, |note| note.record());
    assert!(matches!(
        ordinary.advice(),
        Advice::Stay(StayWhy::OtherSupervisor)
    ));
    let plan = super::super::native_quota_plan(&world.seat(), &s, |note| note.record());
    let (source, target) = plan.selected_rows().expect("captured account rows");
    assert_eq!((&*source.label, &*target.label), ("main", "work"));
    let before = world.load(Cli::Codex);
    assert_eq!(before.active.as_deref(), Some("main"));
    assert!(before.rotations.is_empty());
    assert!(before.canary.is_none());
    let moved = plan.commit(NOW);
    assert!(moved.line.is_some());
    let after = world.load(Cli::Codex);
    assert_eq!(after.active.as_deref(), Some("work"));
    assert_eq!(after.rotations, [NOW]);
}

#[test]
fn native_plan_refuses_nonweekly_unstamped_and_foreign_session_requests() {
    let world = World::new("native-weekly-refusals");
    world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    for case in 0..5 {
        let mut s = weekly_session(&world);
        match case {
            0 => s.point = Point::TurnEnd,
            1 => {
                s.point = Point::Wall {
                    window: "five_hour".into(),
                    at: Some(NOW),
                }
            }
            2 => {
                s.point = Point::Wall {
                    window: "seven_day".into(),
                    at: None,
                }
            }
            3 => s.cli = Cli::Grok,
            _ => s.supervisor = Supervisor::Aterm,
        }
        let plan = super::super::native_quota_plan(&world.seat(), &s, |note| note.record());
        assert!(plan.selected_rows().is_none());
        assert!(plan.commit(NOW).line.is_none());
    }
    assert!(world.load(Cli::Codex).rotations.is_empty());
}

#[test]
fn native_commit_rechecks_policy_and_target_generation() {
    for change_generation in [false, true] {
        let world = World::new("native-weekly-changed");
        world.two(Cli::Codex);
        world.on_main(Cli::Codex, &[]);
        let s = weekly_session(&world);
        let plan = super::super::native_quota_plan(&world.seat(), &s, |note| note.record());
        assert!(plan.selected_rows().is_some());
        if change_generation {
            world
                .seat()
                .store(Cli::Codex)
                .transact_logins(NOW, &mut None, |_, logins| {
                    logins
                        .labels
                        .entry("work".into())
                        .or_default()
                        .renewal_generation += 1;
                    Ok(Vec::new())
                })
                .unwrap();
        } else {
            aterm_uds::atomic_file::write(
                &(world.cfg.join("aterm.toml")),
                ("[harness]\naccounts = 'off'\n").as_ref(),
                0o600,
            )
            .unwrap();
        }
        assert!(plan.commit(NOW).line.is_none());
        assert!(world.load(Cli::Codex).rotations.is_empty());
        assert_eq!(world.load(Cli::Codex).active.as_deref(), Some("main"));
    }
}

#[cfg(unix)]
fn exec_command(world: &World, row: &Row) -> super::super::super::codex_exec_handoff::ExecCommand {
    let program = std::env::current_exe().unwrap();
    super::super::super::codex_exec_handoff::ExecCommand {
        argv0: "codex".into(),
        image: super::super::super::native_act::Image::at(&program).unwrap(),
        program: program.to_str().unwrap().into(),
        argv: vec!["exec".into(), "--json".into(), "original\ninput".into()],
        auth_path: row
            .dir
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| world.home.join(Cli::Codex.accounts().default_home))
            .join("auth.json")
            .to_str()
            .unwrap()
            .into(),
        cwd: None,
    }
}

#[test]
#[cfg(unix)]
fn native_exec_quota_plan_keeps_typed_dispatch_and_commits_shared_selection() {
    let world = World::new("native-exec-selection");
    world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let mut request = weekly_session(&world);
    let flags = ["exec".into(), "--json".into()];
    request.relaunch.flags = &flags;
    request.relaunch.resume = None;
    let plan = super::super::native_exec_quota_plan(
        &world.seat(),
        &request,
        &world.exe,
        |note| note.record(),
        |row| Some(exec_command(&world, row)),
    );
    let (source, target) = plan.selected_rows().unwrap();
    assert_eq!((&*source.label, &*target.label), ("main", "work"));
    let expected = exec_command(&world, target);
    assert_eq!(plan.prepared_command(), Some(&expected));
    assert!(world.load(Cli::Codex).rotations.is_empty());
    let moved = plan.commit_native(NOW, &"a".repeat(32), &"b".repeat(64));
    assert_eq!(moved.line, Some(expected));
    let after = world.load(Cli::Codex);
    assert_eq!(after.active.as_deref(), Some("work"));
    assert_eq!(after.rotations, [NOW]);
}

/// A NATIVE MOVE IS REFUSED ON A BUILD THAT KEEPS NO ROW'S LOGIN ITS OWN, as a relaunch's is: the
/// exec handoff names its own image, and the weekly handoff the session's program. CONTROL: on a
/// build that keeps it (each test above), each moves to `work`.
#[test]
#[cfg(unix)]
fn a_native_move_has_no_line_on_a_build_that_keeps_no_rows_login() {
    let world = World::new("native-old-build");
    world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let old = crate::harness::codex_alab::lay_package(&world.root.join("store/old"), false);
    let session = weekly_session(&world);
    let exec = super::super::native_exec_quota_plan(
        &world.seat(),
        &session,
        &old,
        |note| note.record(),
        |row| Some(exec_command(&world, row)),
    );
    assert!(
        matches!(exec.advice(), Advice::Stay(StayWhy::NoLine)),
        "exec: {:?}",
        exec.advice()
    );
    let mut relaunch = weekly_session(&world);
    relaunch.relaunch.exe = &old;
    let weekly = super::super::native_quota_plan(&world.seat(), &relaunch, |note| note.record());
    assert!(
        matches!(weekly.advice(), Advice::Stay(StayWhy::NoLine)),
        "weekly: {:?}",
        weekly.advice()
    );
}

#[test]
#[cfg(unix)]
fn native_exec_quota_plan_rejects_foreign_auth_and_stale_generation() {
    for stale in [false, true] {
        let world = World::new("native-exec-binding");
        world.two(Cli::Codex);
        world.on_main(Cli::Codex, &[]);
        let request = weekly_session(&world);
        let plan = super::super::native_exec_quota_plan(
            &world.seat(),
            &request,
            &world.exe,
            |note| note.record(),
            |row| {
                let mut command = exec_command(&world, row);
                if !stale {
                    command.auth_path = world
                        .home
                        .join("foreign-auth.json")
                        .to_str()
                        .unwrap()
                        .into();
                }
                Some(command)
            },
        );
        if stale {
            assert!(plan.selected_rows().is_some());
            world
                .seat()
                .store(Cli::Codex)
                .transact_logins(NOW, &mut None, |_, logins| {
                    logins
                        .labels
                        .entry("work".into())
                        .or_default()
                        .renewal_generation += 1;
                    Ok(Vec::new())
                })
                .unwrap();
        } else {
            assert!(plan.selected_rows().is_none());
        }
        assert!(
            plan.commit_native(NOW, &"a".repeat(32), &"b".repeat(64))
                .line
                .is_none()
        );
        let after = world.load(Cli::Codex);
        assert_eq!(after.active.as_deref(), Some("main"));
        assert!(after.rotations.is_empty());
    }
}
