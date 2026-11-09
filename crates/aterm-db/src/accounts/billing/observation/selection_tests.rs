// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::accounts::broker::{Broker, Selection, command};

pub(crate) fn selection_transition(
    template: &super::broker_tests::Template,
    before: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    transition_with(template, before, action, false)
}

fn transition_with(
    template: &super::broker_tests::Template,
    before: &aterm_spec::interp::State,
    action: &str,
    pick_first: bool,
) -> aterm_spec::interp::State {
    let (f, broker, _, grant) = super::broker_tests::fork(template);
    let mut grants = vec![grant.clone()];
    let mut selection = Selection::Account {
        account: grant.binding.account,
        adapter: grant.binding.adapter.clone(),
    };
    match action {
        "Absent" => grants.clear(),
        "Ambiguous" => {
            let mut extra = grant.clone();
            extra.id = [71; 16];
            extra.producer = [72; 32];
            // A stale matching row must still be ambiguous, not silently ignored.
            extra.binding.revision = [73; 32];
            grants.push(extra);
            if pick_first {
                selection = Selection::Grant(grant.id);
            }
        }
        "ForeignActor" => grants[0].actor = [74; 48],
        "WrongAdapter" => grants[0].binding.adapter = "other-v1".into(),
        "Stale" => grants[0].binding.revision = [75; 32],
        "Unique" => {
            for variant in 0..3 {
                let mut other = grant.clone();
                other.id = [81 + variant; 16];
                other.producer = [81 + variant; 32];
                match variant {
                    0 => other.actor = [85; 48],
                    1 => other.binding.account = [86; 32],
                    _ => other.binding.adapter = "other-v1".into(),
                }
                grants.push(other);
            }
        }
        "ChangedDuring" | "Retry" => {}
        _ => panic!("unknown selection scenario"),
    }
    super::broker_tests::publish(&f, grants);
    let result = broker.test_issue_selected_after_billing(&selection, deadline(), || {
        if action == "ChangedDuring" {
            super::broker_tests::publish(&f, vec![]);
        }
    });
    let issued = i64::from(result.is_ok());
    let entered = Cell::new(0);
    if let Ok(token) = result {
        let dispatch = broker
            .dispatch_until(
                "example-v1",
                token.bytes(),
                [76; 16],
                b"private request",
                deadline(),
                |_, credential| {
                    assert_eq!(credential, b"synthetic-private-credential");
                    entered.set(entered.get() + 1);
                    None
                },
            )
            .unwrap();
        assert!(dispatch.entered);
        if action == "Retry" {
            let next = broker
                .issue_selected_until(&selection, Duration::from_secs(60), deadline())
                .unwrap();
            let retry = broker
                .dispatch_until(
                    "example-v1",
                    next.bytes(),
                    [76; 16],
                    b"private request",
                    deadline(),
                    |_, _| panic!("same producer/request entered twice"),
                )
                .unwrap();
            assert!(!retry.entered);
            assert_eq!(retry.accounting.claim.key, dispatch.accounting.claim.key);
            assert_eq!(retry.accounting.claim.key.producer(), grant.producer);
        }
    }
    let used = f
        .source(31, 0)
        .prepare_until(deadline())
        .unwrap()
        .resources()
        .snapshot()
        .unwrap()
        .used
        .values()
        .sum::<u64>();
    let mut next = before.clone();
    let scenario = [
        "Unique",
        "Absent",
        "Ambiguous",
        "ForeignActor",
        "WrongAdapter",
        "Stale",
        "ChangedDuring",
        "Retry",
    ]
    .iter()
    .position(|name| *name == action)
    .unwrap()
        + 1;
    next.insert("scenario", scenario as i64);
    next.insert("issued", issued);
    next.insert("entered", entered.get());
    next.insert("used", used as i64);
    next
}

#[test]
fn account_selection_conforms_and_choosing_first_breaks_ambiguity() {
    let model = aterm_models_db::broker_selection_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let initial = model.init_state();
    let template = super::broker_tests::template();
    aterm_spec::fs_lane::each(&model.actions, |action| {
        let next = super::selection_transition(&template, &initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&next),
            "{}: {next:?}",
            action.name
        );
        next
    });
    let unsafe_next = transition_with(&template, &initial, "Ambiguous", true);
    assert!(!model.check_invariant("ExactSelection", &unsafe_next));
}

#[test]
fn proposal_derives_exact_binding_without_grant_or_ledger_and_preserves_ids() {
    let f = Fixture::new(1);
    f.issue(
        31,
        0,
        f.policy("example-v1").reference([41; 32]).unwrap(),
        vec![],
    );
    let proposal = command::propose_at(
        f.lab.paths[0].clone(),
        [31; 32],
        [81; 16],
        [82; 32],
        Duration::from_secs(60),
        deadline(),
    )
    .unwrap();
    assert_eq!(
        proposal.actor,
        f.lab.bootstrap(0).request().unwrap().binding().actor()
    );
    let prepared = f.source(31, 0).prepare_until(deadline()).unwrap();
    assert_eq!(&proposal.binding, prepared.binding());
    assert_eq!(proposal.producer, [82; 32]);
    assert!(
        prepared.resources().snapshot().is_err(),
        "proposal initialized a ledger"
    );
    let broker = Broker::at(
        f.lab.paths[0].clone(),
        client::config(&f.lab.paths[0]).unwrap().unwrap(),
    )
    .unwrap();
    assert!(
        broker
            .issue_until(proposal.id, Duration::from_secs(60), deadline())
            .is_err(),
        "proposal authorized itself"
    );
    super::broker_tests::publish(&f, vec![proposal.clone()]);
    assert!(
        broker
            .issue_until(proposal.id, Duration::from_secs(60), deadline())
            .is_ok()
    );
    assert!(
        prepared.resources().snapshot().is_err(),
        "issuance initialized a ledger"
    );
}
