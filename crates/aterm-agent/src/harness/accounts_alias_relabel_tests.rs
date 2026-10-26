// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const ALIAS: &str = "alias@example.test";
const CANONICAL: &str = "owner@example.test";

fn row(label: &str, email: &str, identity: Option<&str>, dir: Option<&str>) -> Row {
    Row {
        label: label.into(),
        email: Some(email.into()),
        identity_email: identity.map(str::to_owned),
        dir: dir.map(str::to_owned),
        ..Row::default()
    }
}

fn fixture(case: i64) -> (Option<Roster>, FleetRoster, Option<&'static str>) {
    let mut old = Roster {
        rows: vec![row("m1", ALIAS, Some(CANONICAL), None)],
    };
    old.rows[0].browser = Some("owned-profile".into());
    let mut fleet = parse_accounting_csv(
        "email,computer,claude\nalias@example.test,m1,1\nother@example.test,m1,1\n",
        "fixture.csv",
    )
    .unwrap();
    let mut hint = Some(CANONICAL);
    match case {
        2 | 3 | 9 | 10 => {
            fleet.accounts[0].email = "other@example.test".into();
            fleet.accounts[1].email = if case == 9 {
                "ALIAS@EXAMPLE.TEST"
            } else {
                ALIAS
            }
            .into();
            if case == 3 {
                hint = None;
            }
            if case == 10 {
                old.rows.push(row(
                    "m1-2",
                    "other@example.test",
                    None,
                    Some("/tmp/other-login"),
                ));
            }
        }
        4 => {
            fleet.accounts[0].label = "m1-2".into();
            fleet.accounts[1].label = "m1-3".into();
        }
        5 => {
            fleet.accounts.remove(0);
            hint = None;
        }
        6 => {
            fleet.accounts.remove(0);
            fleet.accounts[0].label = "m1".into();
        }
        7 | 8 => {
            let email = if case == 7 {
                "new@example.test"
            } else {
                "alias+tag@example.test"
            };
            fleet.accounts[0].email = email.into();
            hint = Some(email);
        }
        11 => hint = Some("new@example.test"),
        12 => return (None, fleet, hint),
        _ => {}
    }
    (Some(old), fleet, hint)
}

fn default_unlisted(built: &Roster, observed: Option<&str>) -> bool {
    let desk = super::login_tests::Desk::new("m1");
    let config = desk.config.join("aterm.toml");
    fs::write(
        &config,
        "[fleet]\nmachine = \"m1\"\naccounts_source = \"https://example.test/fleet.csv\"\n",
    )
    .unwrap();
    fs::write(desk.roster(), format_roster_toml(built).unwrap()).unwrap();
    if let Some(email) = observed {
        desk.default_login(email);
    }
    let rediscovery = std::sync::Mutex::new(Rediscovery::default());
    let due = default_login_unlisted(
        &desk.home,
        &desk.state,
        (&desk.roster(), &config),
        super::login_tests::NOW,
        &rediscovery,
    );
    assert_eq!(rediscovery.lock().unwrap().synced_for.is_some(), due);
    if due {
        assert!(!default_login_unlisted(
            &desk.home,
            &desk.state,
            (&desk.roster(), &config),
            super::login_tests::NOW,
            &rediscovery,
        ));
    }
    due
}

pub(crate) fn alias_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action == "AnotherCase" {
        next.insert("case", state["case"] + 1);
        return next;
    }
    let (old, fleet, hint) = fixture(state["case"]);
    let store = accounts::Store::at(Path::new("/tmp/aterm-alias-model"));
    let built = build_machine_roster(&fleet, "m1", &store, old.as_ref(), hint);
    Roster::parse(&format_roster_toml(&built).unwrap()).unwrap();
    if action == "ChangedDefault" {
        next.insert("phase", 2);
        next.insert(
            "unlisted",
            i64::from(default_unlisted(&built, Some("different@example.test"))),
        );
        return next;
    }
    assert_eq!(action, "Build");
    let default = match built.row_for_login(None).unwrap().label.as_str() {
        "m1" => 1,
        "m1-2" => 2,
        "m1-3" => 3,
        _ => panic!("unexpected default"),
    };
    let identity = built
        .rows
        .iter()
        .find(|r| {
            r.email
                .as_deref()
                .is_some_and(|e| accounts::email_matches(e, ALIAS))
        })
        .and_then(|r| r.identity_email.as_deref())
        .is_some_and(|e| accounts::email_matches(e, CANONICAL));
    let browser_safe = built.rows.iter().all(|row| {
        let expected = old.as_ref().is_some()
            && row
                .email
                .as_deref()
                .is_some_and(|email| accounts::email_matches(email, ALIAS));
        row.browser.as_deref() == expected.then_some("owned-profile")
    });
    let decision = old
        .as_ref()
        .map_or(Ok(None), |old| default_relocation(old, &built));
    let mut saved = accounts::State::default();
    let mut logins = accounts::Logins::default();
    accounts::rename_tests::seed_reference(&mut saved, &mut logins, "m1", 0);
    let before = (saved.clone(), logins.clone());
    let (relocation, payload) = match decision {
        Ok(Some((from, to))) => {
            accounts::move_account(&mut saved, &mut logins, &from, &to).unwrap();
            (
                1,
                saved.to_json() == before.0.to_json().replace("\"m1\"", &format!("\"{to}\""))
                    && logins.to_json()
                        == before.1.to_json().replace("\"m1\"", &format!("\"{to}\"")),
            )
        }
        Ok(None) => (0, (saved, logins) == before),
        Err(_) => (2, (saved, logins) == before),
    };
    next.insert("phase", 1);
    next.insert("rows", i64::try_from(built.rows.len()).unwrap());
    next.insert("default", default);
    next.insert("identity", i64::from(identity));
    next.insert("relocation", relocation);
    next.insert("payload", i64::from(payload));
    next.insert("browser_safe", i64::from(browser_safe));
    next.insert("unlisted", i64::from(default_unlisted(&built, hint)));
    next
}

#[test]
fn alias_relabel_native_conforms_to_identity_and_move_model() {
    let model = aterm_models_harness::account_alias_relabel_model();
    assert_eq!(model.mutant_dials(), vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let (mut builds, mut changes) = (0, 0);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = alias_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
                builds += usize::from(action.name == "Build");
                changes += usize::from(action.name == "ChangedDefault");
            }
        }
    }
    assert_eq!((builds, changes), (12, 12));
    let accepted = alias_transition(&model.init_state(), "Build");
    for (field, bad_value, law) in [
        ("identity", 0, "IdentityBoundToLogin"),
        ("rows", 3, "NoDuplicateDefault"),
        ("default", 2, "DefaultFollowsLogin"),
        ("relocation", 1, "RelocationDecision"),
        ("payload", 0, "PayloadPreserved"),
        ("browser_safe", 0, "BrowserBoundToLogin"),
        ("unlisted", 1, "NoNeedlessRediscovery"),
    ] {
        let mut bad = accepted.clone();
        bad.insert(field, bad_value);
        assert!(!model.check_invariant(law, &bad));
    }
    let mut changed = alias_transition(&accepted, "ChangedDefault");
    changed.insert("unlisted", 0);
    assert!(!model.check_invariant("ChangedDefaultDetected", &changed));
}

#[test]
fn alias_assertion_requires_one_exact_login_address_and_moves_never_use_only_canonical() {
    let mut old = Roster {
        rows: vec![row("m1", ALIAS, Some(CANONICAL), None)],
    };
    assert!(retained_login_row(Some(&old), "alias+tag@example.test").is_none());
    old.rows.push(row(
        "m1-2",
        ALIAS,
        Some("other@example.test"),
        Some("/tmp/other-login"),
    ));
    assert!(retained_login_row(Some(&old), ALIAS).is_none());
    old.rows.pop();
    let next = Roster {
        rows: vec![row(
            "m1-2",
            "different-login@example.test",
            Some(CANONICAL),
            None,
        )],
    };
    assert_eq!(default_relocation(&old, &next).unwrap(), None);
    old.rows[0].email = Some(String::new());
    assert!(retained_login_row(Some(&old), "").is_none());
    assert!(default_identity_hint(Some(&old), None).is_none());
}
