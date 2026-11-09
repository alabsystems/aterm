// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

pub(crate) fn completion_transition(state: &State, action: &str) -> State {
    complete(state, action, false)
}

fn complete(state: &State, action: &str, forget_end: bool) -> State {
    let mut next = state.clone();
    if action == "AnotherCase" {
        next.insert("case", state["case"] + 1);
        return next;
    }
    assert!(matches!(action, "Complete" | "Repeat"));
    let case = state["case"];
    let bed = Bed::new(match case {
        5 => Some("different"),
        6 => None,
        _ => Some("canonical"),
    });
    let old = Row {
        label: "legacy".into(),
        email: Some(email("alias")),
        ..Row::default()
    };
    bed.seed(std::slice::from_ref(&old));
    let ended = matches!(case, 3 | 4);
    let at = if matches!(case, 2 | 4) { NOW } else { NOW - 60 };
    if (1..=4).contains(&case) {
        bed.store
            .transact_logins(NOW, &mut None, |accounts, logins| {
                accounts.accounts.get_mut("legacy").unwrap().auth_at = Some(NOW - 120);
                if ended {
                    logins.labels.entry("legacy".into()).or_default().ended_at = Some(at);
                    Ok(Vec::new())
                } else {
                    Ok(accounts::note_auth(
                        accounts,
                        "legacy",
                        accounts::Auth::SignedOut,
                        at,
                    ))
                }
            })
            .unwrap();
    }
    let before = bed.store.load(NOW).unwrap().0;
    let before_logins = bed.store.load_logins(NOW).unwrap().0;
    assert_eq!(before.accounts["legacy"].ran_at, Some(NOW - 2));
    if ended {
        assert_eq!(
            before.accounts["legacy"].current_auth(&old),
            accounts::Auth::Ok
        );
        assert_eq!(
            accounts::login_ended_at(&old, &before_logins, &before),
            Some(at)
        );
    } else if matches!(case, 1 | 2) {
        assert_eq!(
            before.accounts["legacy"].current_auth(&old),
            accounts::Auth::SignedOut
        );
    }
    if forget_end {
        assert!(ended);
        bed.store
            .transact_logins(NOW, &mut None, |_, logins| {
                logins.labels.get_mut("legacy").unwrap().ended_at = None;
                Ok(Vec::new())
            })
            .unwrap();
    }
    let lab = lab_of(
        true,
        vec![LabAccount {
            legacy_label: Some("legacy".into()),
            identity: Some(email("canonical")),
            ..lab_account("alias", "claude-canonical", &[("m22", Some(1))])
        }],
    );
    bed.sync(&lab).unwrap();
    let first = bed.store.load(NOW).unwrap().0;
    let first_logins = bed.store.load_logins(NOW).unwrap().0;
    let first_roster = bed.roster();
    if action == "Repeat" {
        bed.sync(&lab).unwrap();
    }
    let after = bed.store.load(NOW).unwrap().0;
    let after_logins = bed.store.load_logins(NOW).unwrap().0;
    let after_roster = bed.roster();
    let row = after_roster.row("claude-canonical").unwrap();
    let account = &after.accounts["claude-canonical"];
    let prior_end_kept = !(1..=4).contains(&case)
        || (account.auth_at == before.accounts["legacy"].auth_at
            && (!ended || after_logins.get("claude-canonical").unwrap().ended_at == Some(at)));
    next.insert("phase", if action == "Repeat" { 2 } else { 1 });
    next.insert(
        "ready",
        i64::from(account.current_auth(row) == accounts::Auth::Ok),
    );
    next.insert(
        "usage_kept",
        i64::from(account.ran_at == before.accounts["legacy"].ran_at),
    );
    next.insert("end_kept", i64::from(prior_end_kept));
    next.insert(
        "stable",
        i64::from(
            after.accounts == first.accounts
                && after_logins == first_logins
                && after_roster == first_roster,
        ),
    );
    next
}

#[test]
fn native_alias_completion_conforms_and_preserves_ended_logins() {
    let model = aterm_models_harness::account_identity_completion_model();
    aterm_spec::interp::prove_and_catch(&model);
    let mut completed = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = completion_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{state:?}: {} -> {actual:?}",
                    action.name
                );
                completed += usize::from(action.name != "AnotherCase");
            }
        }
    }
    assert_eq!(completed, 14);
    let mut ended = model.init_state();
    ended.insert("case", 3);
    let forgotten = complete(&ended, "Complete", true);
    assert!(!model.successors("Complete", &ended).contains(&forgotten));
    assert!(!model.check_invariant("EndedLoginStaysUnusable", &forgotten));
}
