// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Responses for the focused native cybersecurity-policy continuation proof.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::codex_rig::{Answered, ResponseGate, message_item, request_view};
use crate::pyjson::J;

pub use aterm_agent::harness::acts::RULE_POLICY_CONTINUE as RULE;
pub const ADVANCE: &str = "[aterm harness] Respect the cybersecurity policy refusal and abandon the refused action. Do not repeat or rephrase the rejected request, seek a workaround, change model or account, or enroll in or enable Daybreak. Preserve existing work and continue only clearly allowed, independently authorized work. If no such work remains, explain the limitation, put POLICY-WORK-EXHAUSTED on a line by itself and stop.";
pub const BOUNDARY: &str =
    "A cybersecurity policy refused an earlier action. Leave that action excluded:";
pub const DRAFT: &str = "Keep this unsent draft intact";

static DRAFT_GATE: OnceLock<Arc<ResponseGate>> = OnceLock::new();

pub fn release_draft_error() {
    DRAFT_GATE.get_or_init(Default::default).release();
}

fn refused() -> Answered {
    Answered::now((
        400,
        "application/json",
        J::obj([(
            "error",
            J::obj([
                ("code", J::s("cyber_policy")),
                ("type", J::s("invalid_request")),
                (
                    "message",
                    J::s("The lab refused this synthetic request under cybersecurity policy."),
                ),
            ]),
        )])
        .py(),
    ))
}

/// Each first request is refused. Exactly the policy-aware advance succeeds,
/// except for the repeated-refusal control. Later automatic work must retain
/// its boundary. Unexpected prompts remain visible as their own request tag.
pub fn answer(body: &J, leg: &str) -> Option<Answered> {
    if !matches!(
        leg,
        "policy" | "policyagain" | "policydraft" | "policyempty"
    ) {
        return None;
    }
    let format = body.get("text").and_then(|t| t.get("format"));
    if format.and_then(|f| f.get("type")).and_then(J::as_str) == Some("json_schema") {
        return None;
    }
    let (last, _) = request_view(body);
    let (mut answer, step) = if last.starts_with(ADVANCE) {
        if leg == "policyagain" {
            (refused(), "advance-refused")
        } else if leg == "policyempty" {
            (
                Answered::items(vec![message_item(
                    "No permitted independent work remains.\nPOLICY-WORK-EXHAUSTED",
                )]),
                "advance-exhausted",
            )
        } else {
            (
                Answered::items(vec![message_item(
                    "Recorded the completed build results in the local documentation.",
                )]),
                "advance",
            )
        }
    } else if last.contains(BOUNDARY) {
        let mut held = Answered::items(vec![message_item(
            "Continuing only the allowed documentation work.",
        )]);
        held.delay = Duration::from_secs(3600);
        (held, "after-boundary")
    } else if last.starts_with(&format!("LEG-{leg} ")) {
        let mut error = refused();
        if leg == "policydraft" {
            error.gate = Some(Arc::clone(DRAFT_GATE.get_or_init(Default::default)));
        }
        (error, "refused")
    } else {
        let mut held = Answered::items(Vec::new());
        held.delay = Duration::from_secs(3600);
        (held, "unexpected")
    };
    answer.tag = format!("{leg}:{step}");
    Some(answer)
}

#[cfg(test)]
mod tests {
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
}
