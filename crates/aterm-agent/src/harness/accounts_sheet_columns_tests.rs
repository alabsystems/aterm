// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

fn source(cli: Cli, action: &str, variant: bool) -> String {
    let member = csv_columns(cli)[0];
    let alias = csv_columns(cli).last().unwrap();
    let value = if cli == Cli::Kimi { "global" } else { "1" };
    let second = if variant { value } else { "0" };
    match action {
        "Unique" if variant && cli != Cli::Claude => format!(
            "claude, CLAUDE ,email,computer,,{member}\n0,1,first@example.test,m3,m8,{value}\n"
        ),
        "Unique" => {
            let unused = if cli == Cli::Claude { "kimi" } else { "claude" };
            format!(
                "email,computer,{member},{unused},{unused}\nfirst@example.test,m3,{value},0,1\n"
            )
        }
        "DuplicateEmail" => {
            let other = if variant {
                "first@example.test"
            } else {
                "other@example.test"
            };
            format!("email, EMAIL ,computer,{member}\nfirst@example.test,{other},m3,{value}\n")
        }
        "DuplicateMember" => {
            format!("email,computer,{member},{alias}\nfirst@example.test,m3,{value},{second}\n")
        }
        "MissingEmail" => format!("computer,{member}\nm3,{value}\n"),
        "MissingMember" => "email,computer\nfirst@example.test,m3\n".into(),
        "DuplicateAnchor" if variant && cli != Cli::Claude => format!(
            "claude,email,computer,, CLAUDE ,{member}\n1,first@example.test,m3,m8,1,{value}\n"
        ),
        "DuplicateAnchor" => format!(
            "email,computer,,claude, CLAUDE ,{member}\nfirst@example.test,m3,m8,1,1,{value}\n"
        ),
        "MultiMachine" if !variant => {
            format!("email,computer,computer,{member}\nfirst@example.test,m3,m8,{value}\n")
        }
        "MultiMachine" if cli == Cli::Claude => {
            "email,computer,,claude\nfirst@example.test,m3,m8,1\n".into()
        }
        "MultiMachine" => {
            format!("email,computer,,claude,{member}\nfirst@example.test,m3,m8,1,{value}\n")
        }
        _ => panic!("unknown sheet scenario"),
    }
}

fn project(state: &State, cli: Cli, action: &str, source: &str) -> State {
    let case = match action {
        "Unique" => 1,
        "DuplicateEmail" => 2,
        "DuplicateMember" => 3,
        "MissingEmail" => 4,
        "MissingMember" => 5,
        "DuplicateAnchor" => 6,
        "MultiMachine" => 7,
        _ => panic!("unknown sheet scenario"),
    };
    let present = csv_names_column(cli, source);
    let result = parse_accounting_csv_for(cli, source, "fixture.csv");
    let exact = result.as_ref().map_or(true, |fleet| {
        let machines = if action == "MultiMachine" {
            vec!["m3".to_owned(), "m8".to_owned()]
        } else {
            vec!["m3".to_owned()]
        };
        let label = if action == "MultiMachine" {
            "m3-m8"
        } else {
            "m3"
        };
        let label = if cli == Cli::Claude {
            label.to_owned()
        } else {
            format!("{}-{label}", cli.id())
        };
        fleet.cli == cli
            && fleet.held.is_empty()
            && fleet.roster.accounts.len() == 1
            && fleet.roster.accounts[0].email == "first@example.test"
            && fleet.roster.accounts[0].machines == machines
            && fleet.roster.accounts[0].label == label
            && fleet.roster.accounts[0].class
                == if action == "MultiMachine" {
                    Class::Shared
                } else {
                    Class::Dedicated
                }
    });
    let mut next = state.clone();
    next.insert("case", case);
    next.insert("accepted", i64::from(result.is_ok()));
    next.insert("present", i64::from(present));
    next.insert("exact", i64::from(exact));
    next
}

pub(crate) fn sheet_transition(state: &State, action: &str) -> State {
    let mut agreed = None;
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        for variant in [false, true] {
            let source = source(cli, action, variant);
            let (first, rest) = source.split_once(',').unwrap();
            let encoded = format!("\u{feff}\"{first}\",{rest}");
            for source in [&source, &encoded] {
                let next = project(state, cli, action, source);
                if let Some(agreed) = &agreed {
                    assert_eq!(agreed, &next, "{cli:?} {action} {variant} {source}");
                } else {
                    agreed = Some(next);
                }
            }
        }
    }
    agreed.unwrap()
}

#[test]
fn sheet_columns_conform_and_ambiguity_mutants_are_detected() {
    let model = aterm_models_harness::account_sheet_columns_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = sheet_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{} {next:?}",
                    action.name
                );
            }
        }
    }
    for (action, field, value, invariant) in [
        ("DuplicateEmail", "accepted", 1, "NoAmbiguousSource"),
        ("DuplicateMember", "present", 0, "HeaderPresence"),
        ("MultiMachine", "exact", 0, "ExactMapping"),
    ] {
        let mut wrong = sheet_transition(&model.init_state(), action);
        wrong.insert(field, value);
        assert!(!model.check_invariant(invariant, &wrong));
        assert!(
            !model
                .successors(action, &model.init_state())
                .contains(&wrong)
        );
    }
}
