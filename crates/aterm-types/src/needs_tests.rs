// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn every_word_round_trips_and_a_stranger_does_not_parse() {
    for need in NeedWord::ALL {
        assert_eq!(NeedWord::parse(need.word()), Some(need));
    }
    for wait in WaitWord::ALL {
        assert_eq!(WaitWord::parse(wait.word()), Some(wait));
    }
    for hold in HoldWord::ALL {
        assert_eq!(HoldWord::parse(hold.word()), Some(hold));
    }
    for kind in AttentionKind::ALL {
        assert_eq!(AttentionKind::parse(kind.word()), Some(kind));
    }
    for stranger in ["-", "", "Frozen", "limit", "ask "] {
        assert_eq!(NeedWord::parse(stranger), None, "{stranger:?}");
    }
    assert_eq!(AttentionKind::parse("control"), None);
}

#[test]
fn the_precedence_is_the_order_and_stuck_comes_first() {
    let mut sorted = NeedWord::ALL;
    sorted.sort();
    assert_eq!(sorted, NeedWord::ALL);
    let first_decide = NeedWord::ALL
        .iter()
        .position(|n| n.severity() == Severity::Decide)
        .unwrap();
    assert!(
        NeedWord::ALL[..first_decide]
            .iter()
            .all(|n| n.severity() == Severity::Stuck)
    );
    assert!(
        NeedWord::ALL[first_decide..]
            .iter()
            .all(|n| n.severity() == Severity::Decide)
    );
}
