// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_types::macos_setup::Plan;

fn answered(
    chosen: bool,
    vault_typed: bool,
) -> LiveSetupStartup<impl FnMut(Plan, StartupHandoff) -> aterm_gui::SetupAnswer> {
    let plan = Plan::default();
    let mut host = LiveSetupStartup {
        offer: move |plan, _| aterm_gui::SetupAnswer {
            chosen,
            vault_typed,
            ..aterm_gui::WorkstationSetupDecision::Begin(plan).into()
        },
        receipt: None,
        authorization: None,
        last_failure: None,
        observation: None,
        offered: Some((plan.clone(), true)),
        chosen: false,
        vault_typed: false,
        sent: StartupSent::default(),
        retry_seen: SETUP_WAKES.count(),
        recovered: false,
    };
    assert_eq!(
        host.offer(plan.clone()),
        aterm_gui::WorkstationSetupDecision::Begin(plan)
    );
    host
}

#[test]
fn a_same_plan_startup_choice_is_not_a_provisional_automatic_answer() {
    assert_eq!(
        answered(false, false).answer_kind(&Plan::default()),
        StartupAnswerKind::Automatic
    );
    assert_eq!(
        answered(true, false).answer_kind(&Plan::default()),
        StartupAnswerKind::Person
    );
}

/// The page's answer with no press, of a plan aterm did not choose by itself (the plan a
/// set-aside or a Restore's bring-forward left for the offer), chooses nothing: it is the
/// offered admission, never a person's, so a set-aside's note stays.
#[test]
fn an_unpressed_answer_of_a_persons_plan_is_the_offered_admission() {
    let mut host = answered(false, false);
    host.offered = Some((Plan::default(), false));
    assert_eq!(
        host.answer_kind(&Plan::default()),
        StartupAnswerKind::Offered
    );
}

#[test]
fn a_startup_join_uses_the_explicit_admission_path() {
    assert_eq!(
        answered(true, true).answer_kind(&Plan::default()),
        StartupAnswerKind::Join
    );
}
