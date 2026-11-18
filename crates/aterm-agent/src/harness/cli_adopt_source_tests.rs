// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::accounts_sync::{self, FleetRoster};

#[test]
fn default_adoption_is_kept_in_the_local_fleet_source() {
    let tmp = tmp_dir("adopt-source");
    let env = env_at(&tmp);
    crate::test_config::write(env.config.as_ref().unwrap(), "[fleet]\nmachine='m3'\n").unwrap();
    let fleet = tmp.path().join(accounts_sync::FLEET_ROSTER_FILE);
    std::fs::write(&fleet, "opt_in='named'\n[[account]]\nlabel='work'\nemail='work@example.test'\nclass='dedicated'\nmachines=['m3']\n").unwrap();
    let (code, out, err) = acc_quiet(
        AccountsCmd::AdoptDefault {
            account: None,
            email: Some("owner@example.test".into()),
            browser: None,
        },
        &env,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    let text = std::fs::read_to_string(&fleet).unwrap();
    let source = FleetRoster::from_toml(&text, "fixture").unwrap();
    let label =
        crate::harness::account_names::tests::added_label(Cli::Claude, "owner@example.test");
    assert!(
        source.accounts.iter().any(|row| row.label == label),
        "adoption would be dropped at the next sync: {text}"
    );
}

fn fleet_text(label: &str, email: &str, opt_in: Option<&str>, shared: bool) -> String {
    format!(
        "{}[[account]]\nlabel='{label}'\nemail='{email}'\nclass='dedicated'\nmachines={}\n",
        opt_in
            .map(|why| format!("opt_in='{why}'\n"))
            .unwrap_or_default(),
        if shared { "['m3','m4']" } else { "['m3']" }
    )
}

#[cfg(unix)]
fn source_case(case: i64) -> (i64, i64, i64, i64) {
    let tmp = tmp_dir("adopt-source-policy");
    let env = env_at(&tmp);
    let config = env.config.as_ref().unwrap();
    crate::test_config::write(config, "[fleet]\nmachine='m3'\n").unwrap();
    let beside = tmp.path().join(accounts_sync::FLEET_ROSTER_FILE);
    let home = env.home.as_ref().unwrap();
    // The vault's fleet roster, read in place from this Mac's store.
    let vault_text = || {
        crate::vault_entry::read_text(home, accounts_sync::FLEET_ROSTER_FILE, 4 << 20)
            .text()
            .map(str::to_owned)
    };
    // The default login's row is the account's, under its reserved slug; in cases 9 and 11 the
    // login signs in through an alias, so its provider must report the account's address.
    let label =
        crate::harness::account_names::tests::added_label(Cli::Claude, "owner@example.test");
    let label = label.as_str();
    let email = if matches!(case, 9 | 11) {
        "alias@example.test"
    } else {
        "owner@example.test"
    };
    let mut sources = Vec::new();
    match case {
        1 | 9 => {}
        4 => crate::test_config::write(
            config,
            "[fleet]\nmachine='m3'\naccounts_source='key:fixture'\n",
        )
        .unwrap(),
        5 => {
            crate::vault_entry::put(
                home,
                accounts_sync::FLEET_ROSTER_FILE,
                fleet_text("work", "work@example.test", Some("named"), true),
            );
            sources.push(None);
        }
        _ => {
            let (name, address) = match case {
                6 => (label, "another@example.test"),
                7 => ("another", email),
                10 => (label, email),
                _ => ("work", "work@example.test"),
            };
            let why = if case == 12 {
                None
            } else if case == 3 {
                Some("discovered")
            } else {
                Some("named")
            };
            std::fs::write(
                &beside,
                if case == 8 {
                    "[[broken".into()
                } else {
                    let text = fleet_text(name, address, why, false);
                    if matches!(case, 6 | 7) {
                        text.replace("['m3']", "['m4']")
                    } else {
                        text
                    }
                },
            )
            .unwrap();
            sources.push(Some(beside.clone()));
            if matches!(case, 3 | 13) {
                let (name, address) = if case == 13 {
                    (label, "another@example.test")
                } else {
                    ("work", "work@example.test")
                };
                // Matching accounts alone do not make a vault input a writable
                // legacy copy. Both cases remain externally owned.
                crate::vault_entry::put(
                    home,
                    accounts_sync::FLEET_ROSTER_FILE,
                    fleet_text(name, address, Some("discovered"), false),
                );
                sources.push(None);
            }
        }
    }
    let path = env.accounts_roster().unwrap();
    let store = env.accounts_store();
    let files = [
        beside,
        path.clone(),
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ];
    let before = files.each_ref().map(|path| std::fs::read(path).ok());
    let vault_before = vault_text();
    let status = |login: &str| {
        assert!(matches!(case, 9 | 11), "unexpected probe");
        assert!(login.is_empty());
        if case == 9 {
            crate::test_config::write(
                config,
                "[fleet]\nmachine='m3'\naccounts_source='key:changed-during-probe'\n",
            )
            .unwrap();
        }
        Ok(Captured {
            stdout: br#"{"loggedIn":true,"authMethod":"claude.ai","email":"owner@example.test"}"#
                .to_vec(),
            code: Some(0),
        })
    };
    let host = FakeHost::new(&status);
    let (code, out, err) = acc(
        AccountsCmd::AdoptDefault {
            account: matches!(case, 9 | 11).then(|| "claude owner@example.test".into()),
            email: Some(email.into()),
            browser: Some("Profile".into()),
        },
        &env,
        &host,
    );
    let success = code == exit(0);
    if !success {
        assert_eq!(
            files.each_ref().map(|path| std::fs::read(path).ok()),
            before,
            "refusal changed metadata: {out}{err}"
        );
        assert_eq!(
            vault_text(),
            vault_before,
            "refusal changed the vault's fleet"
        );
    }
    let mut retained = 0;
    for source in &sources {
        let text = match source {
            Some(path) => std::fs::read_to_string(path).unwrap(),
            None => vault_text().expect("the vault's fleet"),
        };
        let Ok(fleet) = FleetRoster::from_toml(&text, "fixture") else {
            continue;
        };
        let listed = fleet.accounts.iter().any(|row| {
            row.label == label && row.email == email && row.machines.contains(&"m3".into())
        });
        retained += i64::from(listed);
        if success {
            assert!(listed, "source lost adopted metadata: {text}");
            if case == 3 {
                assert!(text.contains("opt_in = \"discovered\""));
            }
            if case == 12 {
                assert!(!text.contains("opt_in ="));
            }
            let roster = Roster::load(&path).unwrap();
            for default_email in [None, Some("owner@example.test"), Some("work@example.test")] {
                let regenerated = accounts_sync::build_machine_roster(
                    &fleet,
                    "m3",
                    &store,
                    Some(&roster),
                    default_email,
                );
                let row = regenerated.row(label).expect("adopted row survives sync");
                assert_eq!(row.email.as_deref(), Some(email));
                assert_eq!(
                    row.identity_email.as_deref(),
                    (case == 11).then_some("owner@example.test")
                );
                assert_eq!(
                    regenerated
                        .rows
                        .iter()
                        .filter(|row| row.dir.is_none())
                        .count(),
                    1
                );
                if default_email != Some("work@example.test") {
                    assert_eq!(
                        Some(row),
                        roster.row(label),
                        "adopted metadata survives without a live profile"
                    );
                }
            }
        }
    }
    // The account store's state, logins and ledger: the wake marker a published adoption
    // replaces through its receipt tells a running host of the row and grants
    // nothing, while a refusal leaves it as it was with every other file (above).
    let granted = files[2..5]
        .iter()
        .enumerate()
        .any(|(i, path)| std::fs::read(path).ok() != before[i + 2]);
    let preserved = vault_text() == vault_before;
    (
        i64::from(success),
        retained,
        i64::from(granted),
        i64::from(preserved),
    )
}

#[cfg(unix)]
pub(crate) fn adopt_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Adopt" => {
            let (local, sources, granted, preserved) = source_case(state["case"]);
            next.insert("phase", 1);
            next.insert("local", local);
            next.insert("sources", sources);
            next.insert("granted", granted);
            next.insert("preserved", preserved);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
#[cfg(unix)]
fn default_adoption_conforms_to_source_authority() {
    let model = aterm_models_harness::account_adopt_source_model();
    assert_eq!(model.mutant_dials(), (1..=18).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut checked = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = adopt_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
                checked += usize::from(action.name == "Adopt");
            }
        }
    }
    assert_eq!(checked, 13);
    let mut remote = model.init_state();
    remote.insert("case", 4);
    let mut remote = adopt_transition(&remote, "Adopt");
    remote.insert("local", 1);
    assert!(!model.check_invariant("AuthorityBoundary", &remote));
}
