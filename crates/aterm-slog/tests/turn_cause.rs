// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A turn names what caused it (design §2.6, phase 3A): input that said
//! which mail or record it acted on (`send cause=`, `turn cause=`) gives
//! the turn it opens `cause=` and `cause_k=`; later input from anyone, or an
//! input past the attribution window, takes the turn without it. The cause
//! crosses a seamless update with the input it rode, and a 2A carry line
//! (no cause) still reads.

use aterm_slog::{Actor, Kind, Log, Op, Rec, ShortId, Tier, TurnTracker, Verdict, encode};

use crate::support::{agent_session, at};

fn turn_open(log: &Log) -> Rec {
    log.window()
        .find(|r| r.op == Op::Open && r.k == Kind::Turn)
        .cloned()
        .expect("a turn opened")
}

fn id(s: &str) -> Option<ShortId> {
    ShortId::new(s)
}

#[test]
fn a_caused_input_names_the_turn_it_opens() {
    // Mail by its offset.
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let mut tr = TurnTracker::new(run);
    tr.note_input_caused(Actor::Harness, id("@88231"), at(1_000));
    tr.observe(&mut log, Verdict::Busy, None, at(4_000));
    let line = encode(&turn_open(&log), Tier::Host).expect("fits");
    assert!(
        line.contains(" by=harness ")
            && line.contains("cause=@88231")
            && line.contains("cause_k=mail"),
        "{line}"
    );
    // A record by its seq, through a `ctl turn` exchange.
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let mut tr = TurnTracker::new(run);
    tr.ctl_turn_caused(&mut log, Actor::Ctl, 9, id("412"), at(100));
    tr.observe(&mut log, Verdict::Busy, None, at(200));
    let line = encode(&turn_open(&log), Tier::Host).expect("fits");
    assert!(
        line.contains("ledger=9") && line.contains("cause=412") && line.contains("cause_k=rec"),
        "{line}"
    );
    // The bus never learns which mail: `cause=` is a host field.
    let wire = encode(&turn_open(&log), Tier::Wire).expect("fits");
    assert!(
        !wire.contains("cause=412") && wire.contains("cause_k=rec"),
        "{wire}"
    );
}

/// NEGATIVE: an input six seconds old attributes nothing, and another
/// actor's later input takes the turn without the cause.
#[test]
fn a_stale_or_superseded_cause_names_no_turn() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let mut tr = TurnTracker::new(run);
    tr.note_input_caused(Actor::Harness, id("@7"), at(1_000));
    tr.observe(&mut log, Verdict::Busy, None, at(7_000));
    let line = encode(&turn_open(&log), Tier::Host).expect("fits");
    assert!(!line.contains(" by=") && !line.contains("cause="), "{line}");

    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let mut tr = TurnTracker::new(run);
    tr.note_input_caused(Actor::Harness, id("@7"), at(1_000));
    tr.note_input(Actor::Human, at(1_500));
    tr.observe(&mut log, Verdict::Busy, None, at(2_000));
    let line = encode(&turn_open(&log), Tier::Host).expect("fits");
    assert!(
        line.contains(" by=human ") && !line.contains("cause="),
        "{line}"
    );
}

/// The cause crosses a seamless update with its input and its ledger, and a
/// 2A carry line (none) reads with none.
#[test]
fn a_cause_crosses_the_carry_and_a_2a_line_reads() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let mut tr = TurnTracker::new(run);
    tr.ctl_turn_caused(&mut log, Actor::Harness, 3, id("@55"), at(1_000));
    let line = tr.carry_line(at(1_100));
    assert!(
        line.contains("input_cause=@55") && line.contains("ledger_cause=@55"),
        "{line}"
    );
    let mut back = TurnTracker::from_carry_line(&line, at(1_200)).expect("reads");
    back.observe(&mut log, Verdict::Busy, None, at(1_300));
    let open = encode(&turn_open(&log), Tier::Host).expect("fits");
    assert!(
        open.contains("cause=@55") && open.contains("ledger=3"),
        "{open}"
    );

    let two_a = line
        .split(' ')
        .filter(|t| !t.starts_with("input_cause=") && !t.starts_with("ledger_cause="))
        .collect::<Vec<_>>()
        .join(" ");
    let mut old = TurnTracker::from_carry_line(&two_a, at(1_200)).expect("a 2A line reads");
    let mut log = Log::new(1);
    let _ = agent_session(&mut log, 0);
    old.observe(&mut log, Verdict::Busy, None, at(1_300));
    let open = encode(&turn_open(&log), Tier::Host).expect("fits");
    assert!(
        open.contains(" by=harness ") && !open.contains("cause="),
        "{open}"
    );
    assert!(
        TurnTracker::from_carry_line(&format!("{two_a} input_cause=%%"), at(1_200)).is_none(),
        "a cause that is no id refuses the line"
    );
}
