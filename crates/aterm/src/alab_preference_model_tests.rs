// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_db::alab::view::{Account, Machine, Preference};
use aterm_spec::interp::State;

// Keys are opaque, including letter case and tokens that resemble machine labels.
const FIRST: &str = "pk-m3";
const SECOND: &str = "PK_m4";
const ACCOUNT: &str = "ac-0000000000000001";

fn machine_key(value: i64) -> &'static str {
    match value {
        1 => FIRST,
        2 => SECOND,
        _ => panic!("the fixture has exactly two machine keys"),
    }
}

fn label(value: i64) -> Option<String> {
    match value {
        0 => None,
        1 => Some("m3".into()),
        2 => Some("m4".into()),
        _ => panic!("unknown fixture label"),
    }
}

/// The model begins at the adapter boundary: a validated LabView whose `me`
/// is either a live facts binding or absent. It models no signing or I/O.
fn fixture(state: &State) -> LabView {
    let mut view = LabView::default();
    for (key, name) in [(FIRST, "label_a"), (SECOND, "label_b")] {
        view.machines.insert(
            key.into(),
            Machine {
                pk: key.into(),
                label: label(state[name]),
                nickname: MachineNickname::Unset,
                aliases: Vec::new(),
                live: true,
            },
        );
    }
    view.me = (state["me"] > 0).then(|| machine_key(state["me"]).to_owned());
    view.accounts.insert(
        ACCOUNT.into(),
        Account {
            pk: ACCOUNT.into(),
            realm: Some("claude".into()),
            label: Some("claude-one".into()),
            legacy_label: None,
            login: Some("one@example.test".into()),
            identity: None,
            status: Some(
                if state["joinable"] == 1 {
                    "active"
                } else {
                    "paused"
                }
                .into(),
            ),
            region: None,
            flow: None,
            browser: None,
            live: true,
        },
    );
    if state["owner"] > 0 {
        view.preferences.push(Preference {
            machine: machine_key(state["owner"]).into(),
            account: ACCOUNT.into(),
            rank: match state["rank_in"] {
                0 => None,
                1 => Some(1),
                2 => Some(9),
                _ => panic!("unknown fixture rank"),
            },
        });
    }
    view
}

/// Environment actions select an input; Project drives the actual adapter,
/// typed preference lookup and local automatic-enrollment projection.
pub(crate) fn project(state: &State, action: &str) -> State {
    let mut next = state.clone();
    let input = match action {
        "Bind" => Some("me"),
        "Prefer" => Some("owner"),
        "LabelA" => Some("label_a"),
        "LabelB" => Some("label_b"),
        "Joinable" => Some("joinable"),
        "Rank" => Some("rank_in"),
        "Project" => None,
        _ => panic!("unknown projection action"),
    };
    if let Some(input) = input {
        next.insert(input, state[input] + 1);
        return next;
    }
    let view = fixture(state);
    let lab = harness_lab_with_placed(&view, &BTreeMap::new(), true, &BTreeMap::new());
    let account = &lab.accounts[0];
    let preference = lab.me.as_ref().and_then(|me| account.preferred_on(&me.key));
    assert_eq!(
        lab.class_of(account) == aterm_agent::harness::accounts::Class::Preferred,
        preference.is_some()
    );
    next.insert("matched", i64::from(preference.is_some()));
    next.insert(
        "rank_out",
        match preference {
            None => 0,
            Some(None) => 1,
            Some(Some(1)) => 2,
            Some(Some(9)) => 3,
            _ => panic!("the adapter introduced an unknown rank"),
        },
    );
    let fleet = aterm_agent::harness::alab_roster::fleet_for(&lab, Cli::Claude, None);
    let enrolled = fleet
        .zip(lab.me.as_ref())
        .is_some_and(|(fleet, me)| !fleet.accounts_for_machine(me.key.as_str()).is_empty());
    next.insert("enrolled", i64::from(enrolled));
    next.insert("phase", 1);
    next
}

#[test]
fn preference_projection_conforms_and_rejects_label_substitution() {
    let model = aterm_models_harness::lab_preference_identity_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    let mut projected = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = project(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&actual));
                if action.name == "Project" {
                    projected += 1;
                }
            }
        }
    }
    assert_eq!(projected, 486);

    let mut collision = model.init_state();
    for (name, value) in [
        ("me", 2),
        ("owner", 1),
        ("label_a", 1),
        ("label_b", 1),
        ("joinable", 1),
    ] {
        collision.insert(name, value);
    }
    let mut wrong = project(&collision, "Project");
    wrong.insert("matched", 1);
    wrong.insert("enrolled", 1);
    assert!(!model.successors("Project", &collision).contains(&wrong));
    assert!(!model.check_invariant("KeysDecide", &wrong));
}
