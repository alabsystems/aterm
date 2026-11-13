// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::supervise::policy::question::LAND_TEXT;
use crate::supervise::policy::turn_end::is_land_rule;

/// A SESSION WORD NEVER YIELDS LAND'S PRIORITY: each word a person sets answers by itself, a
/// priority other than Land's whose text is not Land's and whose prose rule is no land rule,
/// and `land`, in any case, is a word this supervisor does not know, so the question is the
/// person's with an unknown word's reason: Land's priority is the session's Autopilot's alone
/// (`dialog_priority`). NEGATIVE CONTROL: Land's own priority writes Land's text; CONTROL: `ask`
/// hands the question over with no such reason. MUTANT: `From<PriorityWord>` taking
/// `short-term` to Land, a `land` word added to `PriorityWord`, or `land` read as `ask`.
#[test]
fn a_session_word_never_yields_lands_priority() {
    let land = QuestionPriority::Land;
    assert_eq!(land.text(), Some(LAND_TEXT), "NEGATIVE CONTROL");
    for p in PriorityWord::ALL {
        let policy = session_word(p.word());
        assert_eq!(
            (policy.answer, policy.by_session, policy.session_reason()),
            (Some(p), true, None),
            "{p:?}"
        );
        let priority = QuestionPriority::from(p);
        assert_ne!(priority, land, "{p:?}");
        assert_eq!(priority.word(), p.word(), "{p:?}");
        assert_ne!(p.text(), land.text(), "{p:?}");
        assert!(!is_land_rule(p.prose_rule_id()), "{p:?}");
    }
    let shape = |policy: &QuestionPolicy| {
        (
            policy.answer,
            policy.by_session,
            policy.why.is_some(),
            policy.session_reason().is_some(),
        )
    };
    assert_eq!(
        shape(&session_word("ask")),
        (None, true, false, true),
        "CONTROL: `ask` hands the question over without an unknown word's reason"
    );
    for word in ["land", "Land", " LAND "] {
        let policy = session_word(word);
        assert_eq!(
            shape(&policy),
            (None, true, true, true),
            "{word:?}: {policy:?}"
        );
    }
}
