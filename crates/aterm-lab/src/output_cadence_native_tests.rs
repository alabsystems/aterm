// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn empty() -> Cadence {
    Cadence {
        now_ns: 100,
        sessions: SESSIONS,
        ordered: 0,
        indexed: 0,
        session: 7,
        pooled: true,
        enrolled: false,
        current: false,
        held: false,
        dirty: false,
        due_ns: 0,
    }
}

fn held(due_ns: u64, dirty: bool) -> Cadence {
    Cadence {
        ordered: 1,
        indexed: 1,
        enrolled: true,
        current: true,
        held: true,
        dirty,
        due_ns,
        ..empty()
    }
}

#[test]
fn an_idle_verdict_needs_empty_indexes_and_no_selected_hold() {
    assert!(empty().consistent(7).unwrap().idle());
    for bad in [
        Cadence {
            sessions: SESSIONS - 1,
            ..empty()
        },
        Cadence {
            indexed: 1,
            ..empty()
        },
        Cadence {
            held: true,
            ..empty()
        },
        Cadence {
            current: false,
            ..held(200, true)
        },
        Cadence {
            pooled: false,
            ..held(200, true)
        },
    ] {
        assert!(bad.consistent(7).is_err(), "{bad:?}");
    }
    assert!(
        held(200, true).consistent(8).is_err(),
        "wrong selected identity"
    );
    let cleared = Cadence {
        held: false,
        dirty: false,
        ..held(200, true)
    };
    assert!(
        !cleared.consistent(7).unwrap().idle(),
        "an indexed reader clear is still owed UI retirement"
    );
}

#[test]
fn a_native_hold_witness_requires_dirty_then_a_later_rearm() {
    let mut witness = Witness::default();
    for due in [100, 200, 300] {
        witness.observe(held(due, false), (1, 2));
    }
    assert!(
        !witness.complete(),
        "clean holds alone prove no dirty-tail service"
    );
    witness.observe(held(400, true), (3, 4));
    for due in [400, 399] {
        witness.observe(held(due, true), (5, 6));
    }
    assert!(
        !witness.complete(),
        "one dirty deadline, even reread, proves no rearm"
    );
    witness.observe(
        Cadence {
            held: false,
            dirty: false,
            ..held(500, true)
        },
        (7, 8),
    );
    assert!(!witness.complete(), "a reader clear is not a later hold");
    witness.observe(held(500, false), (9, 10));
    assert!(witness.complete());
    assert_eq!(witness.positive_intervals, [(3, 4), (9, 10)]);
}

#[test]
fn malformed_or_unversioned_native_snapshots_cannot_satisfy_the_oracle() {
    let text = "OK cadence_schema=1 now_ns=100 sessions=202 ordered=0 indexed=0 session=7 pooled=1 enrolled=0 current=0 held=0 dirty=0 due_ns=0\n";
    assert_eq!(Cadence::parse(text).unwrap(), empty());
    for bad in [
        text.replace("cadence_schema=1", "cadence_schema=2"),
        text.replace(" pooled=1", " pooled=2"),
        text.replace(" indexed=0", ""),
        text.replace("session=7", "session=none"),
        text.replace("OK ", "OKAY "),
        format!("{text} indexed=0"),
    ] {
        assert!(Cadence::parse(&bad).is_err(), "{bad}");
    }
}

#[test]
fn functional_budget_refuses_before_reading_an_app_or_starting_a_window() {
    for budget in ["0", "-1", "18446744073709551615", "bad"] {
        let args = ["--functional-memory-mib", budget, "--out", "/unused"].map(str::to_owned);
        let failure = Options::parse(&args).err().expect("invalid budget");
        assert!(failure.msg.as_deref().unwrap().contains("positive MiB"));
    }
}
