// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `FleetMachineAssignment`'s file area: the fleet file reader's machine list, on fixtures whose
//! every address is synthetic (`example.test`).

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
        class: Class::Preferred,
    };
    let admitted =
        |(fields, machines, class): (&'static str, &'static [&'static str], Class)| Fixture {
            fields,
            machines,
            class,
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
        // A stored class word, valid, unknown or mistyped, is no input: the row is admitted with
        // the order its machine list derives (design §0: no class).
        "StoredClass" => [
            (
                "machines=['m3']\nclass='sharedd'",
                &["m3"][..],
                Class::Preferred,
            ),
            ("machines=['m3']\nclass='Shared'", &["m3"], Class::Preferred),
            ("machines=['m3']\nclass=''", &["m3"], Class::Preferred),
            ("machines=['m3']\nclass=false", &["m3"], Class::Preferred),
            (
                "machines=['m3']\nclass=['shared']",
                &["m3"],
                Class::Preferred,
            ),
            (
                "machines=['m3']\nclass={name='shared'}",
                &["m3"],
                Class::Preferred,
            ),
            ("machines='m3'\nclass='shared'", &["m3"], Class::Preferred),
            (
                "machines=['M3', 'm3']\nclass='shared'",
                &["m3"],
                Class::Preferred,
            ),
            (
                "machines=['m8', 'm3']\nclass='dedicated'",
                &["m8", "m3"],
                Class::Pool,
            ),
            (
                "machines=['m8', 'm3']\nclass=17",
                &["m8", "m3"],
                Class::Pool,
            ),
        ]
        .into_iter()
        .map(admitted)
        .collect(),
        "Scalar" => [
            "machines='m3'",
            "machines=' M3.local '",
            "machines='workstation-m3.local'",
            "machines='m3.2'",
        ]
        .into_iter()
        .map(|fields| admitted((fields, &["m3"][..], Class::Preferred)))
        .collect(),
        "Distinct" => [
            "machines=['M8', 'M3.local', 'm100']",
            "machines=['M8', 'M3.local', 'm8', 'm100', 'm3']",
        ]
        .into_iter()
        .map(|fields| admitted((fields, &["m8", "m3", "m100"][..], Class::Pool)))
        .collect(),
        "Duplicate" => [
            "machines=['m3', 'm3']",
            "machines=['M3', 'M3.local', 'm3.2', 'workstation-m3']",
        ]
        .into_iter()
        .map(|fields| admitted((fields, &["m3"][..], Class::Preferred)))
        .collect(),
        _ => panic!("unknown fleet assignment scenario"),
    }
}

/// The file area's projection of `action` from `state`: every fixture of the scenario, read by
/// every pooled CLI's reader, must agree on whether the row is admitted and whether the admitted
/// row is the normalized list with its derived order, and survives a round trip.
pub(crate) fn assignment_transition(state: &State, action: &str) -> State {
    let case = match action {
        "Missing" => 1,
        "InvalidMachine" => 2,
        "WrongType" => 3,
        "StoredClass" => 4,
        "Scalar" => 5,
        "Distinct" => 6,
        "Duplicate" => 7,
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
                assert!(why.contains("`machines`"), "{cli:?} {action}: {why}");
            }
            let exact = result.as_ref().map_or(true, |fleet| {
                fleet.accounts.len() == 2
                    && fleet.accounts[0].label == "kept"
                    && fleet.accounts[0].email == "kept@example.test"
                    && fleet.accounts[0].machines == ["m9"]
                    && fleet.accounts[0].class == Class::Preferred
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

/// Every step the fleet reader takes is a `FleetMachineAssignment` successor from every state the
/// model reaches in its file area, and the model's mutants are caught.
/// NEGATIVE CONTROLS: each hand-built wrong step below (a malformed list admitted, a stored class
/// word refusing the row, a normalized list or derived order lost) breaks its law and is no
/// successor. Run against the code, `fleet_assignment` reading the stored word again (the
/// revision-1 `class` match) refuses `class='sharedd'` and gives `machines=['m8', 'm3']` with
/// `class='dedicated'` the preferred order, so `StoredClass` steps to `accepted = 0`, no successor.
#[test]
fn fleet_assignment_conforms_and_unsafe_coercions_are_detected() {
    let model = aterm_models_harness::fleet_machine_assignment_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let states: Vec<State> = aterm_spec::interp::reachable_states(&model)
        .into_iter()
        .filter(|state| state["area"] == 1)
        .collect();
    assert_eq!(
        states.len(),
        8,
        "the area's start and one state per scenario"
    );
    for state in &states {
        for action in &model.actions {
            if model.action_enabled(action.name, state) {
                let next = assignment_transition(state, action.name);
                assert!(
                    model.successors(action.name, state).contains(&next),
                    "{} {next:?}",
                    action.name
                );
            }
        }
    }
    let start = states
        .iter()
        .find(|state| state["case"] == 0)
        .expect("the area's start");
    for (action, field, value, invariant) in [
        ("WrongType", "accepted", 1, "ValidMachines"),
        ("StoredClass", "accepted", 0, "ValidAssignment"),
        ("StoredClass", "exact", 0, "ExactAssignment"),
        ("Scalar", "accepted", 0, "ValidAssignment"),
        ("Duplicate", "exact", 0, "ExactAssignment"),
    ] {
        let mut wrong = assignment_transition(start, action);
        wrong.insert(field, value);
        assert!(!model.check_invariant(invariant, &wrong));
        assert!(!model.successors(action, start).contains(&wrong));
    }
}
