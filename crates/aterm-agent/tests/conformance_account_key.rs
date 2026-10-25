// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance test for `HarnessAccountKey` against `accounts::note_reading`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_agent::harness::accounts::{
    self, Account, KEY_ADOPT_S, SESSION_LIMIT, SOURCE_TAP, State, TapRead, WindowFig, WindowRead,
};
use aterm_agent::harness::profile::AccountKey;
use aterm_models_harness::harness_account_key_model;
use aterm_spec::derive::Model;

type S = BTreeMap<&'static str, i64>;

/// The host's clock at a read.
const NOW: i64 = 1_792_050_000;
/// The five-hour window's reset both accounts' figures name.
const RESET: i64 = NOW + 3 * 3600;
/// Each account's figure, in tenths: A's, B's.
const FIGURE: [u32; 2] = [100, 600];

/// What the real code decides.
const DECIDED: [&str; 3] = ["key", "held", "gate"];

/// Every state `m` reaches, invariants unchecked.
fn reachable(m: &Model) -> Vec<S> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([m.init_state()]);
    let mut out = Vec::new();
    while let Some(s) = queue.pop_front() {
        if !seen.insert(crate::common::key(m, &s)) {
            continue;
        }
        for a in &m.actions {
            let mut next = s.clone();
            if m.fire(a.name, &mut next) {
                queue.push_back(next);
            }
        }
        out.push(s);
    }
    out
}

fn key_of(account: i64) -> Option<AccountKey> {
    AccountKey::claude(if account == 0 {
        "11111111-1111-4111-8111-111111111111"
    } else {
        "22222222-2222-4222-8222-222222222222"
    })
}

fn figure_of(account: i64) -> u32 {
    FIGURE[usize::try_from(account).expect("an account")]
}

/// The instant the record's key changed, for `gate`.
fn keyed_at(s: &S) -> Option<i64> {
    match s["gate"] {
        0 => None,
        1 => Some(NOW - 20),
        _ => Some(NOW - 3600),
    }
}

/// When the file's figure was served, for `early` under `gate`.
fn served_at(s: &S) -> i64 {
    match (s["early"], s["gate"]) {
        (1, 1) => NOW - 10,
        (1, 2) => NOW - 3600 + 30,
        _ => NOW - 5,
    }
}

/// The real code's step from `s` at the host's read, with the record's key
/// instant handed by `keyed` and the reading's stamp by `stamp`.
fn read_by(s: &S, keyed: &dyn Fn(&S) -> Option<i64>, stamp: &dyn Fn(&S) -> i64) -> S {
    let mut state = State::default();
    let mut record = Account {
        key: key_of(s["key"]),
        keyed_at: keyed(s),
        ..Account::default()
    };
    if s["held"] < 2 {
        record.windows.insert(
            SESSION_LIMIT.to_owned(),
            WindowFig {
                pct_x10: figure_of(s["held"]),
                reset: RESET,
                at: NOW - 3600 - 600,
                by: String::new(),
                source: SOURCE_TAP.to_owned(),
                epoch_at: None,
            },
        );
    }
    state.accounts.insert("main".to_owned(), record);
    let read = TapRead {
        five_hour: Some(WindowRead {
            pct_x10: figure_of(s["file"] - 1),
            reset: RESET,
            refilled: None,
        }),
        seven_day: None,
        at: stamp(s),
        seen_before: true,
        born: None,
        key: key_of(s["home"]),
    };
    let _ = accounts::note_reading(&mut state, "main", &read, NOW);
    let after = &state.accounts["main"];
    let mut out = s.clone();
    out.insert("key", i64::from(after.key != key_of(0)));
    out.insert(
        "held",
        after
            .windows
            .get(SESSION_LIMIT)
            .map_or(2, |f| i64::from(f.pct_x10 == FIGURE[1])),
    );
    out.insert(
        "gate",
        match after.keyed_at {
            None => 0,
            Some(at) if NOW < at + KEY_ADOPT_S => 1,
            Some(_) => 2,
        },
    );
    out
}

/// The disagreements of the real code with model `m` at every reachable
/// `Read`: `(state, real, model)`.
fn disagreements(
    m: &Model,
    keyed: &dyn Fn(&S) -> Option<i64>,
    stamp: &dyn Fn(&S) -> i64,
) -> Vec<(S, S, S)> {
    let mut out = Vec::new();
    for s in reachable(m) {
        let mut model = s.clone();
        if !m.fire("Read", &mut model) {
            continue;
        }
        let real = read_by(&s, keyed, stamp);
        if DECIDED.iter().any(|v| real[v] != model[v]) {
            out.push((s, real, model));
        }
    }
    out
}

/// THE REAL RECORD TAKES THE MODEL'S STEP at every reachable read: another
/// key drops it and starts the gate, nothing served before the gate's end is
/// taken, anything after is. NEGATIVE CONTROLS: the gate blind to the key's
/// instant, and a reading stamped by its late write, each take the old
/// login's figure under the new key where the model takes none.
#[test]
fn the_real_record_takes_the_models_step_at_every_read() {
    let m = harness_account_key_model();
    let states = reachable(&m);
    assert!(states.len() > 10, "a walk worth the name: {}", states.len());
    let wrong = disagreements(&m, &keyed_at, &served_at);
    assert!(wrong.is_empty(), "{:?}", wrong.first());
    let reads = |pick: &dyn Fn(&S, &S) -> bool| {
        states.iter().any(|s| {
            let mut n = s.clone();
            m.fire("Read", &mut n) && pick(s, &n)
        })
    };
    assert!(
        reads(&|s, n| n["key"] != s["key"] && n["held"] == 2),
        "a key change"
    );
    assert!(
        reads(&|s, n| s["gate"] > 0 && n["held"] == s["held"] && s["file"] - 1 != s["held"]),
        "a figure inside the bound left untaken"
    );
    assert!(
        reads(&|s, n| s["gate"] == 2 && n["held"] == 1 && s["held"] == 2),
        "the new account's figure after the bound"
    );
    let blind = |_: &S| -> Option<i64> { None };
    let caught = disagreements(&m, &blind, &served_at);
    assert!(
        caught
            .iter()
            .any(|(_, real, _)| real["held"] < 2 && real["held"] != real["key"]),
        "a record blind to its key's instant must take the old login's figure: {:?}",
        caught.first()
    );
    let late = |_: &S| NOW - 5;
    let caught = disagreements(&m, &keyed_at, &late);
    assert!(
        caught
            .iter()
            .any(|(_, real, _)| real["held"] < 2 && real["held"] != real["key"]),
        "a reading stamped by its late write must take the old login's figure: {:?}",
        caught.first()
    );
}
