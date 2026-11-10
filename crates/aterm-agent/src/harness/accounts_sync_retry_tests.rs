// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;
use std::sync::Mutex;

#[test]
fn changed_source_during_a_real_sync_retries_without_a_long_cooldown() {
    use super::login_tests::{Desk, NOW, upkeep_effects};
    use crate::harness::alab_roster::{SourceSnapshot, TestSourceOverride};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let _source = TestSourceOverride::new(SourceSnapshot::Absent);
    let desk = Desk::new("m27");
    desk.default_login("m27@example.test");
    let source = desk.config.join("team.toml");
    fs::write(
        &source,
        "[[account]]\nlabel='m27'\nemail='m27@example.test'\nmachines=['m27']\n",
    )
    .unwrap();
    let config = desk.config.join("aterm.toml");
    let config_text = format!(
        "[fleet]\nmachine='m27'\naccounts_source={}\n",
        aterm_toml::Value::from(source.to_str().unwrap())
    );
    fs::write(&config, &config_text).unwrap();
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).unwrap();
    let before = fs::read(desk.roster()).unwrap();
    let attempts = AtomicUsize::new(0);
    let fetch = |env: &Env, source: &str| {
        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            fs::write(
                &config,
                format!("{config_text}\n# changed during capture\n"),
            )
            .unwrap();
        }
        fetch_source_text(env, source)
    };
    let rediscovery = Mutex::new(Rediscovery::default());
    let cooldown = Mutex::new(None);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let fx = upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);
    let first = resync_with(&desk.home, &desk.state, (&desk.roster(), NOW, false), &fx);
    assert!(first.synced && first.failed.is_some(), "{first:?}");
    assert_eq!(fs::read(desk.roster()).unwrap(), before);
    assert!(!rediscovery.lock().unwrap().failed.as_ref().unwrap().holds);
    let second = resync_with(&desk.home, &desk.state, (&desk.roster(), NOW, false), &fx);
    assert!(second.synced && second.failed.is_none(), "{second:?}");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

fn outcome(scenario: i64) -> Result<(), SyncError> {
    match scenario {
        0 => Ok(()),
        1 => Err("same diagnostic".into()),
        2 => Err(SyncError::retry(Retry::Busy, "same diagnostic")),
        3 => Err(SyncError::retry(Retry::Changed, "changed")),
        4 => Err(SyncError::retry(Retry::Unavailable, "unavailable")),
        _ => unreachable!(),
    }
}

pub(crate) fn retry_transition(state: &State, action: &str) -> State {
    let mut next = state.clone();
    match action {
        "Scenario" => {
            next.insert("scenario", state["scenario"] + 1);
        }
        "Supersede" => {
            next.insert("newer", 1);
            for field in ["read", "key", "login", "cooldown"] {
                next.insert(field, 2);
            }
        }
        "Finish" => {
            let own = Instant::now();
            let newer = own + Duration::from_secs(1);
            let selected = if state["newer"] == 1 { newer } else { own };
            let key = |at| LocalSourceAttempt {
                version: (PathBuf::from("source"), None),
                at,
            };
            let login = |at| ("m1@example.test".to_owned(), at);
            let rediscovery = Mutex::new(Rediscovery {
                read_at: Some(selected),
                synced_for: Some(login(selected)),
                key_attempt: Some(key(selected)),
                ..Rediscovery::default()
            });
            let held_cooldown = Mutex::new(Some(selected));
            let mut scheduling = SyncScheduleClaim::new(&rediscovery, None);
            scheduling.changes = vec![
                SyncScheduleChange::Read(None, Some(own)),
                SyncScheduleChange::Default(None, Some(login(own))),
                SyncScheduleChange::Key(None, Some(key(own))),
            ];
            let mut cooldown = SyncCooldownClaim {
                held: &held_cooldown,
                scope: None,
                before: None,
                own,
                retry: false,
            };
            // Diagnostic decoration must preserve the scheduling type.
            let result = outcome(state["scenario"])
                .map_err(|why| why.context(|why| format!("source sync: {why}")));
            scheduling.outcome(&result);
            cooldown.outcome(&result);
            let mut failed = None;
            note_sync_result(&mut failed, [1; 32], result.as_ref().map(|_| ()));
            let provider = result.map(|()| CliSync::NoSourceDefaultLogin(Box::default()));
            next.insert(
                "held",
                i64::from(failed.is_some_and(|failed| failed.holds_back(&[1; 32]))),
            );
            next.insert("default_held", i64::from(default_sync_settled(&provider)));
            drop(scheduling);
            drop(cooldown);
            let held = rediscovery.into_inner().unwrap();
            let rank = |value: Option<Instant>| match value {
                None => 0,
                Some(value) if value == own => 1,
                Some(value) if value == newer => 2,
                _ => unreachable!(),
            };
            next.insert("read", rank(held.read_at));
            next.insert("key", rank(held.key_attempt.map(|key| key.at)));
            next.insert("login", rank(held.synced_for.map(|(_, at)| at)));
            next.insert("cooldown", rank(held_cooldown.into_inner().unwrap()));
            next.insert("done", 1);
        }
        _ => panic!("unknown retry action {action}"),
    }
    next
}

#[test]
fn retry_failures_preserve_scheduling_ownership_and_do_not_hold_inputs() {
    let model = aterm_models_harness::account_sync_retry_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{faults:?}");
    let mut controls = BTreeMap::new();
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = retry_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} {} {next:?}",
                    action.name
                );
                if action.name == "Finish" {
                    controls.insert((state["scenario"], state["newer"]), (state.clone(), next));
                }
            }
        }
    }
    for (key, field, value, invariant) in [
        ((2, 0), "held", 1, "RetryNotHeld"),
        ((3, 0), "default_held", 1, "RetryNotHeld"),
        ((1, 0), "held", 0, "RejectedHeld"),
        ((4, 0), "key", 1, "OwnRetryRestored"),
        ((3, 1), "read", 0, "NewerClaimRetained"),
        ((3, 1), "cooldown", 0, "NewerClaimRetained"),
    ] {
        let (before, correct) = &controls[&key];
        let mut wrong = correct.clone();
        wrong.insert(field, value);
        assert!(!model.check_invariant(invariant, &wrong), "{invariant}");
        assert!(!model.successors("Finish", before).contains(&wrong));
    }
}
