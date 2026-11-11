// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;
use crate::error::HttpRef;

/// A 502 whose why carries `token`, as classify builds one from GitHub's `message`.
fn bad_gateway(token: &str) -> Error {
    Error::new(
        Kind::OutcomeUnknown,
        format!("GitHub answered 502: {token}"),
    )
    .with_side_effect(SideEffect::Unknown)
    .with_http(HttpRef {
        status: 502,
        method: "POST",
        path: "/repos/fixture-dev/app/releases".to_string(),
        request_id: None,
    })
}

#[test]
fn an_unknown_settle_never_carries_a_token_shaped_why() {
    for token in [agh_fake::token("7"), agh_fake::fine_grained_token("7")] {
        let settled = settle_table(&Err(bad_gateway(&token)), None);
        let Settled::Unknown { why } = settled else {
            panic!("a 502 settles Unknown: {settled:?}");
        };
        assert!(!why.contains(&token), "the journal's why holds the token");
        assert_eq!(why, crate::redact::Redactor::shapes(&why));
    }
}

#[test]
fn only_an_error_that_proves_nothing_was_sent_settles_never_sent() {
    let not_sent = Error::could_not_run("connect refused");
    assert!(matches!(
        settle_table(&Err(not_sent), None),
        Settled::NeverSent
    ));
    let lost = Error::new(Kind::OutcomeUnknown, "no answer was read");
    assert!(matches!(
        settle_table(&Err(lost), None),
        Settled::Unknown { .. }
    ));
    let refused_after_the_exchange = Error::could_not_run("a tape cannot hold this answer")
        .with_side_effect(SideEffect::Unknown);
    assert!(matches!(
        settle_table(&Err(refused_after_the_exchange), None),
        Settled::Unknown { .. }
    ));
}

#[test]
fn an_unknown_outcome_without_a_repo_names_a_command_that_runs() {
    let intent = IntentId::parse("3-0123456789ab").expect("an id");
    let error = Error::new(Kind::OutcomeUnknown, "GitHub answered 502");
    let unknown = Outcome::<()>::Unknown {
        intent: intent.clone(),
        error: error.clone(),
    };
    let without = unknown.into_result().expect_err("unknown is an error");
    assert_eq!(without.next, "agh intent list --open");

    let with = Outcome::<()>::Unknown {
        intent,
        error: error.with_repo("fixture-dev/app"),
    }
    .into_result()
    .expect_err("unknown is an error");
    assert_eq!(with.next, "agh intent show fixture-dev/app 3-0123456789ab");
}
