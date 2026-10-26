// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

struct Fixture {
    fields: &'static str,
    machines: &'static [&'static str],
    class: Class,
}

fn fixtures(action: &str) -> Vec<Fixture> {
    let refused = |fields| Fixture {
        fields,
        machines: &[],
        class: Class::Dedicated,
    };
    match action {
        "Missing" => ["", "machines=[]", "machines=''", "machines='   '"]
            .into_iter()
            .map(refused)
            .collect(),
        "InvalidMachine" => [
            "machines='unassigned'",
            "machines=['m3', 'unassigned']",
            "machines=['unassigned', 'm3']",
            "machines=['m3', '']",
            "machines=['   ']",
            "machines=['m3', '*m8']",
            "machines=['m3', 'm8?']",
        ]
        .into_iter()
        .map(refused)
        .collect(),
        "WrongType" => [
            "machines=17",
            "machines=false",
            "machines={name='m3'}",
            "machines=['m3', 17]",
            "machines=[false, 'm3']",
            "machines=['m3', ['m8']]",
            "machines=['m3', {name='m8'}]",
        ]
        .into_iter()
        .map(refused)
        .collect(),
        "InvalidClass" => [
            "machines=['m3']\nclass='sharedd'",
            "machines=['m3']\nclass='Shared'",
            "machines=['m3']\nclass=' shared '",
            "machines=['m3']\nclass=''",
            "machines=['m3']\nclass=false",
            "machines=['m3']\nclass=['shared']",
            "machines=['m3']\nclass={name='shared'}",
        ]
        .into_iter()
        .map(refused)
        .collect(),
        "Scalar" => [
            "machines='m3'",
            "machines=' M3.local '",
            "machines='workstation-m3.local'",
            "machines='m3.2'",
        ]
        .into_iter()
        .map(|fields| Fixture {
            fields,
            machines: &["m3"],
            class: Class::Dedicated,
        })
        .collect(),
        "Distinct" => [
            "machines=['M8', 'M3.local', 'm100']",
            "machines=['M8', 'M3.local', 'm8', 'm100', 'm3']",
        ]
        .into_iter()
        .map(|fields| Fixture {
            fields,
            machines: &["m8", "m3", "m100"],
            class: Class::Shared,
        })
        .collect(),
        "Duplicate" => [
            "machines=['m3', 'm3']",
            "machines=['M3', 'M3.local', 'm3.2', 'workstation-m3']",
        ]
        .into_iter()
        .map(|fields| Fixture {
            fields,
            machines: &["m3"],
            class: Class::Dedicated,
        })
        .collect(),
        "Override" => vec![
            Fixture {
                fields: "machines='m3'\nclass='shared'",
                machines: &["m3"],
                class: Class::Shared,
            },
            Fixture {
                fields: "machines=['m3']\nclass='shared'",
                machines: &["m3"],
                class: Class::Shared,
            },
            Fixture {
                fields: "machines=['m8', 'm3']\nclass='dedicated'",
                machines: &["m8", "m3"],
                class: Class::Dedicated,
            },
            Fixture {
                fields: "machines=['M3', 'm3']\nclass='shared'",
                machines: &["m3"],
                class: Class::Shared,
            },
        ],
        _ => panic!("unknown fleet assignment scenario"),
    }
}

pub(crate) fn assignment_transition(state: &State, action: &str) -> State {
    let case = match action {
        "Missing" => 1,
        "InvalidMachine" => 2,
        "WrongType" => 3,
        "InvalidClass" => 4,
        "Scalar" => 5,
        "Distinct" => 6,
        "Duplicate" => 7,
        "Override" => 8,
        _ => panic!("unknown fleet assignment scenario"),
    };
    let mut agreed = None;
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        let region = if cli == Cli::Kimi {
            "region='global'\n"
        } else {
            ""
        };
        for fixture in fixtures(action) {
            let source = format!(
                "[[account]]\nlabel='kept'\nemail='kept@example.test'\nmachines=['m9']\n{region}\n\
                 [[account]]\nlabel='m3'\nemail='first@example.test'\n{region}{}\n",
                fixture.fields
            );
            let result = FleetRoster::from_toml_for(cli, &source, "fixture.toml");
            if let Err(why) = &result {
                let field = if action == "InvalidClass" {
                    "`class`"
                } else {
                    "`machines`"
                };
                assert!(why.contains(field), "{cli:?} {action}: {why}");
            }
            let exact = result.as_ref().map_or(true, |fleet| {
                fleet.accounts.len() == 2
                    && fleet.accounts[0].label == "kept"
                    && fleet.accounts[0].email == "kept@example.test"
                    && fleet.accounts[0].machines == ["m9"]
                    && fleet.accounts[0].class == Class::Dedicated
                    && fleet.accounts[1].label == "m3"
                    && fleet.accounts[1].email == "first@example.test"
                    && fleet.accounts[1].machines == fixture.machines
                    && fleet.accounts[1].class == fixture.class
                    && FleetRoster::from_toml_for(cli, &fleet.to_toml(), "roundtrip.toml")
                        .is_ok_and(|restored| restored.accounts == fleet.accounts)
            });
            let mut next = state.clone();
            next.insert("case", case);
            next.insert("accepted", i64::from(result.is_ok()));
            next.insert("exact", i64::from(exact));
            if let Some(agreed) = &agreed {
                assert_eq!(agreed, &next, "{cli:?} {action}: {}", fixture.fields);
            } else {
                agreed = Some(next);
            }
        }
    }
    agreed.unwrap()
}

#[test]
fn fleet_assignment_conforms_and_unsafe_coercions_are_detected() {
    let model = aterm_models_harness::fleet_machine_assignment_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = assignment_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{} {next:?}",
                    action.name
                );
            }
        }
    }
    for (action, field, value, invariant) in [
        ("WrongType", "accepted", 1, "ValidMachines"),
        ("InvalidClass", "accepted", 1, "ValidClass"),
        ("Scalar", "accepted", 0, "ValidAssignment"),
        ("Duplicate", "exact", 0, "ExactAssignment"),
        ("Override", "exact", 0, "ExactAssignment"),
    ] {
        let mut wrong = assignment_transition(&model.init_state(), action);
        wrong.insert(field, value);
        assert!(!model.check_invariant(invariant, &wrong));
        assert!(
            !model
                .successors(action, &model.init_state())
                .contains(&wrong)
        );
    }
}
