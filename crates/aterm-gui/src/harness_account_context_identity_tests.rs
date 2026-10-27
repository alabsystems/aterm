// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn profile_context() -> aterm_agent::harness::profile::Context {
    aterm_agent::harness::profile::Context {
        plain: "/example/.claude.json".into(),
        records: "/example/.claude".into(),
        selection: aterm_agent::harness::profile::Selection::Standard,
        home: Some("/example".into()),
        config: None,
        login: None,
    }
}

fn change_field(snap: &mut Snapshot, field: i64) {
    match field {
        1 => {}
        2 => snap.pid += 1,
        3 => snap.start.push_str(" changed"),
        4 => snap.program = "/example/another-program".into(),
        5 => snap.argv.push("--different-profile".into()),
        6 => snap.login = Some("/example/another-login".into()),
        7 => snap.home = Some("/example/another-home".into()),
        8 => snap.claude_dir = Some("/example/another-records".into()),
        9 => snap.stored_login = !snap.stored_login,
        10 => {
            snap.profile = if snap.profile.is_some() {
                None
            } else {
                Some(profile_context())
            }
        }
        11..=16 => {
            let profile = snap.profile.get_or_insert_with(profile_context);
            match field {
                11 => profile.plain = "/example/another-profile.json".into(),
                12 => profile.records = "/example/another-profile-records".into(),
                13 => profile.selection = aterm_agent::harness::profile::Selection::CustomOauth,
                14 => profile.home = Some("/example/another-profile-home".into()),
                15 => profile.config = Some("/example/another-profile-config".into()),
                16 => profile.login = Some("/example/another-profile-login".into()),
                _ => unreachable!(),
            }
        }
        17 => {
            snap.run = aterm_agent::harness::relaunch::AgentRun::Codex(
                aterm_agent::harness::relaunch::CodexRun {
                    home: "/example/codex".into(),
                    home_env: None,
                    self_claimed: None,
                },
            )
        }
        18 => snap.tab.push_str("-another"),
        19 => snap.cwd = "/example/another-workspace".into(),
        20 => snap.session = Some(ACCT_TALK_B.into()),
        21 => snap.version = Some("newly-observed-version".into()),
        22 => snap.shell += 1,
        23 => snap.dialect = Some(aterm_agent::harness::upgrade::Dialect::Bash),
        _ => unreachable!(),
    }
}

pub(crate) fn context_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherField" => {
            next.insert("field", state["field"] + 1);
        }
        "Choose" => {
            next.insert("phase", 1);
        }
        "Compare" => {
            let mut before = snap("s-context-model");
            before.profile = Some(profile_context());
            let mut after = before.clone();
            change_field(&mut after, state["field"]);
            next.insert("phase", 2);
            next.insert("same", i64::from(account_process_matches(&before, &after)));
            next.insert(
                "reverse",
                i64::from(account_process_matches(&after, &before)),
            );
            next.insert(
                "reflexive",
                i64::from(
                    account_process_matches(&before, &before)
                        && account_process_matches(&after, &after),
                ),
            );
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn account_process_context_conforms_for_every_snapshot_field() {
    let model = aterm_models_harness::account_process_context_model();
    assert_eq!(model.mutant_dials(), (1..=25).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 23);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = context_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (field, key, value, law) in [
        (5, "same", 1, "ChangedContextRejected"),
        (19, "same", 0, "MetadataAndIdentityKept"),
        (1, "reflexive", 0, "Reflexive"),
        (10, "reverse", 1, "Symmetric"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert("field", field);
        let mut bad = context_transition(&state, "Compare");
        bad.insert(key, value);
        assert!(!model.check_invariant(law, &bad));
    }
}

#[test]
fn a_changed_command_cannot_consume_the_previous_moves_response() {
    let (a, idle) = moved_at_wall("s-context-command");
    let before = idle.kept.lock().unwrap().account.moved.clone();
    let kept = Arc::clone(&idle.kept);
    a.at_seat_read(2, move |_| {
        kept.lock()
            .unwrap()
            .snapshot
            .as_mut()
            .unwrap()
            .argv
            .push("--different-profile".into());
    });
    idle.account_served();
    assert_eq!(idle.kept.lock().unwrap().account.moved, before);
}

#[test]
fn changed_authentication_facts_do_not_reattribute_the_session() {
    let sid = "s-context-auth";
    let (a, idle) = moved_at_wall(sid);
    let kept = Arc::clone(&idle.kept);
    a.at_seat_read(2, move |a| {
        kept.lock().unwrap().snapshot.as_mut().unwrap().stored_login = false;
        edited(a, |state| {
            accounts::note_session(state, sid, "pool-a", None, a.clock());
        });
    });
    idle.account_served();
    assert_eq!(a.state().sessions[sid].account, "pool-a");
}

#[test]
fn an_answer_from_another_login_cannot_complete_an_older_move() {
    for served in [false, true] {
        let (a, idle) = moved_at_wall("s-context-before-read");
        let before = idle.kept.lock().unwrap().account.moved.clone();
        let pid = {
            let mut k = idle.kept.lock().unwrap();
            let current = k.snapshot.as_mut().unwrap();
            current.login = Some(a.login_dir("pool-a"));
            current.pid
        };
        a.procs.lock().unwrap().insert(
            pid,
            vec![
                "PATH=/usr/bin".into(),
                format!("{}={}", accounts::LOGIN_ENV, a.login_dir("pool-a")),
            ],
        );
        if served {
            idle.account_served();
        } else {
            idle.account_answered(AccountBad::SignedOut);
        }
        assert_eq!(idle.kept.lock().unwrap().account.moved, before);
    }
}

fn follow_metadata(snap: &mut Snapshot) {
    let record = SessionFile {
        pid: snap.pid,
        proc_start: snap.start.clone(),
        session_id: ACCT_TALK_B.into(),
        cwd: "/example/new-workspace".into(),
        version: "newly-observed-version".into(),
        status: "running".into(),
        status_updated_at_ms: 0,
        kind: "interactive".into(),
        entrypoint: "cli".into(),
        claude_dir: None,
        profile: None,
    };
    assert!(relaunch::follow_record(snap, &record));
}

#[test]
fn real_record_enrichment_preserves_replies_and_fallback() {
    for reply in [MoveReply::Served, MoveReply::Failed] {
        let (a, idle) = moved_at_wall("s-context-enriched");
        let kept = Arc::clone(&idle.kept);
        a.at_seat_read(2, move |_| {
            follow_metadata(kept.lock().unwrap().snapshot.as_mut().unwrap());
        });
        match reply {
            MoveReply::Served => {
                idle.account_served();
                assert!(idle.kept.lock().unwrap().account.moved.is_none());
            }
            MoveReply::Failed => {
                idle.account_answered(AccountBad::SignedOut);
                assert!(
                    idle.kept
                        .lock()
                        .unwrap()
                        .account
                        .moved
                        .as_ref()
                        .unwrap()
                        .answered
                );
            }
        }
    }
    let (a, idle) = moved_at_wall("s-context-enriched-back");
    let mut snap = idle.kept.lock().unwrap().snapshot.clone().unwrap();
    follow_metadata(&mut snap);
    idle.kept.lock().unwrap().snapshot = Some(snap.clone());
    assert!(account_way_back(
        &acct_job(&idle.sid, &idle),
        &idle.hooks,
        &mut snap
    ));
    assert!(snap.login.is_none());
    assert!(idle.kept.lock().unwrap().account.moved.is_none());
    assert!(a.state().active.is_none());
}

#[test]
fn a_different_auth_context_neither_falls_back_nor_claims_an_old_move() {
    for field in [4, 5, 6, 7, 8, 9, 10, 17, 18] {
        let (a, idle) = moved_at_wall("s-context-no-back");
        let mut snap = idle.kept.lock().unwrap().snapshot.clone().unwrap();
        change_field(&mut snap, field);
        idle.kept.lock().unwrap().snapshot = Some(snap.clone());
        let before = snap.clone();
        let state = a.state();
        let ledger = a.ledger();
        assert!(account_way_back(
            &acct_job(&idle.sid, &idle),
            &idle.hooks,
            &mut snap
        ));
        assert_eq!(snap, before, "field {field}");
        assert_eq!(a.state(), state, "field {field}");
        assert_eq!(a.ledger(), ledger, "field {field}");
        assert!(idle.kept.lock().unwrap().account.moved.is_some());
    }
    let (_, idle) = moved_at_wall("s-context-no-claim");
    {
        let mut kept = idle.kept.lock().unwrap();
        kept.account.moved.as_mut().unwrap().untold = true;
        change_field(kept.snapshot.as_mut().unwrap(), 5);
    }
    let _ = AdviceTurn::begin(&idle.kept)
        .capture(&idle.kept)
        .unwrap()
        .publish(
            &idle.kept,
            Some(AccountAdvice::Move {
                to: "work-2".into(),
            }),
            usage_wall(),
        );
    assert_ne!(
        idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
        Some("adopted")
    );
    assert!(
        idle.kept
            .lock()
            .unwrap()
            .account
            .moved
            .as_ref()
            .unwrap()
            .untold
    );
}
