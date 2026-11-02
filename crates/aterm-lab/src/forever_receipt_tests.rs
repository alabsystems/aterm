// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the landing receipt's want (§14.9, slice 9b) over a landing's journal lines.

use super::*;
use crate::forever::{Chain, Scenario, Seen, Verdict};

const T0: i64 = 1_792_867_220_000;
const HEAD: &str = "1c81bc6f0e2a3b4c5d6e7f8091a2b3c4d5e6f708";
const DONE: &str = "DONE seq=12 rule=land@v1 the worker defended that everything is landed on the \
                    remote main branch (autopilot land): nothing more is typed until a person or \
                    an orchestrator types";
const RECEIPT_12: &str = "RECEIPT seq=12 rule=land@v1 head=1c81bc6f0e2a on=origin/main leg=on-main \
                          check=land-check@v1 repo=/private/tmp/at.x/proj/land-a Nothing left: \
                          all of it is pushed.";

fn j(t: i64, line: &str) -> (i64, String) {
    (T0 + t, line.to_string())
}

/// The on-main leg's receipt want, the leg these tests' chain lands on.
fn landed(seen: &Seen, t0: i64) -> Verdict {
    landed_as(ReceiptLeg::OnMain, seen, t0)
}

fn landing(lines: &[&str]) -> Seen {
    let mut journal = vec![j(
        9_800,
        "TYPED seq=4 rule=land@v1 [aterm harness] Autopilot is Land",
    )];
    journal.extend(
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| j(40_000 + i64::try_from(i).unwrap(), l)),
    );
    Seen {
        journal,
        end_ms: T0 + 90_000,
        head: Some(HEAD.to_string()),
        ..Seen::default()
    }
}

/// A LANDING'S RECEIPT, JUDGED: the landing's `DONE` note followed by the `RECEIPT` of its seq,
/// naming the chain's `HEAD` on `origin/main`, the land check answered and the defense it audited,
/// passes, and so do a restated landing's second
/// pair and a landing after a person's `LEFT` note. NEGATIVE CONTROLS: main's journal (a `DONE`
/// note and no `RECEIPT`) fails, as do a receipt of another seq, another head, another leg, one
/// not right after its note, one under no note and one under a `LEFT` note, and main's receipt,
/// which names no land check, as does one quoting the check's word or nothing; a `HEAD` the lab
/// did not read is unobserved.
#[test]
fn a_landing_wants_its_receipt_right_after_its_done_note() {
    let pass = |s: &Seen| matches!(landed(s, T0), Verdict::Pass(_));
    let fail = |s: &Seen| matches!(landed(s, T0), Verdict::Fail(_));
    assert!(
        pass(&landing(&[DONE, RECEIPT_12])),
        "{:?}",
        landed(&landing(&[DONE, RECEIPT_12]), T0)
    );
    let again = DONE.replace("seq=12", "seq=19");
    let again_receipt = RECEIPT_12.replace("seq=12", "seq=19");
    assert!(pass(&landing(&[DONE, RECEIPT_12, &again, &again_receipt])));

    assert!(fail(&landing(&[DONE])), "main's journal");
    assert!(fail(&landing(&[
        DONE,
        &RECEIPT_12.replace("seq=12", "seq=11")
    ])));
    assert!(fail(&landing(&[
        DONE,
        &RECEIPT_12.replace("1c81bc6f0e2a", "b0b0b0b0b0b0")
    ])));
    assert!(fail(&landing(&[
        DONE,
        &RECEIPT_12.replace("on=origin/main leg=on-main", "on=- leg=pushed")
    ])));
    assert!(fail(&landing(&[DONE, "EVENT idle seq=12 x", RECEIPT_12])));
    assert!(
        fail(&landing(&[DONE, RECEIPT_12, &again])),
        "a restated note with none"
    );
    assert!(
        fail(&landing(&[RECEIPT_12, DONE, RECEIPT_12])),
        "a stray receipt"
    );
    let left = "LEFT seq=11 rule=left-to-person@v1 a person's own exchange";
    assert!(
        pass(&landing(&[left, DONE, RECEIPT_12])),
        "LEFT, then landed"
    );
    assert!(
        fail(&landing(&[
            left,
            &RECEIPT_12.replace("seq=12", "seq=11"),
            DONE,
            RECEIPT_12
        ])),
        "a receipt under a LEFT note"
    );
    let unread = Seen {
        head: None,
        ..landing(&[DONE, RECEIPT_12])
    };
    // THE LAND CHECK (slice 13a): main's receipt names no check, and a receipt that quotes the
    // check's word, or nothing, quotes no defense the check audited.
    assert!(
        fail(&landing(&[
            DONE,
            &RECEIPT_12.replace(" check=land-check@v1", "")
        ])),
        "main's receipt: no check answered"
    );
    assert!(fail(&landing(&[
        DONE,
        &RECEIPT_12.replace("check=land-check@v1", "check=-")
    ])));
    assert!(
        fail(&landing(&[
            DONE,
            &RECEIPT_12.replace("Nothing left: all of it is pushed.", "LANDED")
        ])),
        "the word, not the defense"
    );
    assert!(fail(&landing(&[
        DONE,
        &RECEIPT_12.replace(" Nothing left: all of it is pushed.", "")
    ])));
    assert!(matches!(landed(&unread, T0), Verdict::Unobserved(_)));
    assert!(matches!(landed(&landing(&[]), T0), Verdict::Unobserved(_)));
}

/// A HOLD HAS NO RECEIPT: a holding chain's journal with no `RECEIPT` passes. NEGATIVE CONTROL:
/// one `RECEIPT` line fails it.
#[test]
fn a_hold_wants_no_receipt() {
    let hold = landing(&["TYPED seq=9 rule=land-nudge@v1 [aterm harness] Still landing"]);
    assert!(matches!(held(&hold, T0), Verdict::Pass(_)));
    assert!(matches!(
        held(&landing(&[RECEIPT_12]), T0),
        Verdict::Fail(_)
    ));
}

/// The want sits on one scenario of each landing chain and on every holding chain's, and
/// composes with the scenario's own verdict: the worse word, both reasons.
#[test]
fn the_receipt_want_sits_beside_the_landing_and_hold_scenarios() {
    assert_eq!(judged(Chain::LandA, Scenario::L1), Some(true));
    assert_eq!(judged(Chain::LandA, Scenario::L3), None);
    assert_eq!(judged(Chain::LandB, Scenario::L2), Some(false));
    assert_eq!(judged(Chain::SameLand, Scenario::HL), Some(false));
    assert_eq!(judged(Chain::Worker, Scenario::L15), Some(true));
    assert_eq!(judged(Chain::Wall, Scenario::W1), Some(true));
    assert_eq!(
        judged(Chain::LandP, Scenario::L13),
        Some(true),
        "LEFT, then landed"
    );
    assert_eq!(judged(Chain::LandD, Scenario::L6), Some(false));
    assert_eq!(judged(Chain::Cruise, Scenario::C1), None);
    assert!(reads_head(Chain::LandA) && reads_head(Chain::LandJ));
    assert!(reads_head(Chain::InstantLand) && reads_head(Chain::Worker));
    assert!(!reads_head(Chain::LandB) && !reads_head(Chain::Off) && !reads_head(Chain::LandD));
    let s = landing(&[DONE]);
    let v = with_receipt(
        Chain::LandA,
        Scenario::L1,
        &s,
        T0,
        Verdict::Pass("typed".into()),
    );
    assert!(
        matches!(&v, Verdict::Fail(w) if w.starts_with("typed; receipt: FAIL")),
        "{v:?}"
    );
    let v = with_receipt(
        Chain::LandA,
        Scenario::L3,
        &s,
        T0,
        Verdict::Pass("typed".into()),
    );
    assert_eq!(v, Verdict::Pass("typed".into()));
}
