// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn request(text: &str) -> J {
    J::obj([(
        "input",
        J::Arr(vec![J::obj([
            ("role", J::s("user")),
            ("content", J::s(text)),
        ])]),
    )])
}

#[test]
fn policy_fixtures_distinguish_safe_advance_exclusion_and_repeated_refusal() {
    let first = answer(
        &request("LEG-policy Record permitted build results."),
        "policy",
    )
    .unwrap();
    assert_eq!(first.status, 400);
    assert!(first.body.contains("cyber_policy"));
    let allowed = answer(&request(ADVANCE), "policy").unwrap();
    assert_eq!(allowed.tag, "policy:advance");
    assert_eq!(allowed.status, 200);
    let refused = answer(&request(ADVANCE), "policyagain").unwrap();
    assert_eq!(refused.tag, "policyagain:advance-refused");
    assert_eq!(refused.status, 400);
    let later = answer(
        &request(&format!("Keep going. {BOUNDARY} respect the refusal.")),
        "policy",
    )
    .unwrap();
    assert_eq!(later.tag, "policy:after-boundary");
    let unsafe_retry = answer(&request("Retry the previous request."), "policy").unwrap();
    assert_eq!(unsafe_retry.tag, "policy:unexpected");
    let draft = answer(
        &request("LEG-policydraft Record permitted build results."),
        "policydraft",
    )
    .unwrap();
    assert!(draft.gate.is_some());
    assert!(answer(&request("other"), "usage").is_none());
}
