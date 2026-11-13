// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Equal timestamps do not erase append order or make repeated sequence evidence unique.
#[test]
fn first_land_requires_unique_acts_and_preceding_evidence_in_each_stream() {
    let point = (20, "EVENT idle seq=7 Implemented the parser".to_string());
    let act = (
        30,
        "CONTINUED seq=7 rule=land@v1 [aterm harness] Autopilot is Land:".to_string(),
    );
    let recorded = || Seen {
        journal: vec![point.clone(), act.clone()],
        ledger: vec![
            Row {
                ts: 20,
                box_seq: Some(7),
                rule: RULE_POINT.into(),
                decision: "waiting".into(),
                ..Row::default()
            },
            Row {
                ts: 30,
                box_seq: Some(7),
                rule: turn_end::RULE_LAND.into(),
                decision: "typed".into(),
                ..Row::default()
            },
        ],
        ..Seen::default()
    };
    assert!(first_land_at_point(&recorded(), 10, &point, &act));
    for seq in [6, 7] {
        let mut prior_journal = recorded();
        prior_journal.journal.insert(
            0,
            (
                act.0,
                format!("CONTINUED seq={seq} rule=land@v1 earlier instruction"),
            ),
        );
        assert!(!first_land_at_point(&prior_journal, 10, &point, &act));
        let mut prior_ledger = recorded();
        let mut prior = prior_ledger.ledger[1].clone();
        prior.box_seq = Some(seq);
        prior_ledger.ledger.insert(0, prior);
        assert!(!first_land_at_point(&prior_ledger, 10, &point, &act));
    }
    let mut duplicate_journal = recorded();
    duplicate_journal.journal.push(act.clone());
    assert!(!first_land_at_point(&duplicate_journal, 10, &point, &act));
    let mut duplicate_ledger = recorded();
    duplicate_ledger
        .ledger
        .push(duplicate_ledger.ledger[1].clone());
    assert!(!first_land_at_point(&duplicate_ledger, 10, &point, &act));
    let mut late = recorded();
    late.ledger.swap(0, 1);
    assert!(!first_land_at_point(&late, 10, &point, &act));
    let mut absent = recorded();
    absent.journal.remove(0);
    assert!(!first_land_at_point(&absent, 10, &point, &act));
    let mut absent = recorded();
    absent.journal.pop();
    assert!(!first_land_at_point(&absent, 10, &point, &act));
}

#[test]
fn a_prior_instruction_or_a_point_after_the_act_cannot_qualify_first_land() {
    let point = (20, "EVENT idle seq=7 Implemented the parser".to_string());
    let act = (
        30,
        "CONTINUED seq=7 rule=land@v1 [aterm harness] Autopilot is Land:".to_string(),
    );
    let seen = Seen {
        journal: vec![point.clone(), act.clone()],
        ledger: vec![
            Row {
                ts: 20,
                box_seq: Some(7),
                rule: RULE_POINT.into(),
                decision: "waiting".into(),
                ..Row::default()
            },
            Row {
                ts: 30,
                box_seq: Some(7),
                rule: turn_end::RULE_LAND.into(),
                decision: "typed".into(),
                ..Row::default()
            },
        ],
        ..Seen::default()
    };
    assert!(first_land_at_point(&seen, 10, &point, &act));
    let mut journal_only = seen.clone();
    journal_only.journal.insert(
        0,
        (
            15,
            "CONTINUED seq=6 rule=land@v1 [aterm harness] Autopilot is Land:".into(),
        ),
    );
    assert!(!first_land_at_point(&journal_only, 10, &point, &act));
    let mut ledger_only = seen.clone();
    ledger_only.ledger.insert(
        0,
        Row {
            ts: 15,
            box_seq: Some(6),
            rule: turn_end::RULE_LAND.into(),
            decision: "typed".into(),
            ..Row::default()
        },
    );
    assert!(!first_land_at_point(&ledger_only, 10, &point, &act));
    let mut late_point = seen;
    late_point.ledger[0].ts = act.0 + 1;
    assert!(!first_land_at_point(&late_point, 10, &point, &act));
}
