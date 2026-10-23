// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance tests for process-wide scrollback budget sharing (`shared_budget_model`).

use std::collections::BTreeMap;

use aterm_core::scrollback::{Scrollback, ScrollbackStorage};
use aterm_core::terminal::Terminal;
use aterm_core::terminal::scrollback_shared_budget::{
    ScrollbackBudgetShare, set_global_scrollback_budget,
};
use aterm_models_misc::shared_budget_model;
use aterm_spec::derive::Model;
use aterm_spec::verify;

const UNIT: usize = 4_096;
const GLOBAL: i64 = 6;
const CFG: i64 = 6;
const EXHAUSTIVE_STEPS: usize = 3;

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct RealPane {
    share: ScrollbackBudgetShare,
    term: Terminal,
}

fn real_pane() -> RealPane {
    let sb = Scrollback::new(4, 16, UNIT * CFG as usize);
    RealPane {
        share: ScrollbackBudgetShare::register(UNIT * CFG as usize),
        term: Terminal::with_scrollback(3, 20, 2, sb),
    }
}

fn applied_units(pane: &RealPane) -> i64 {
    let bytes = pane
        .term
        .scrollback()
        .map_or(0, ScrollbackStorage::memory_budget);
    (bytes / UNIT) as i64
}

fn touch(pane: &mut RealPane) {
    if let Some(bytes) = pane.share.pending_effective() {
        pane.term
            .set_memory_budget(bytes)
            .expect("budget enforcement on an attached store");
    }
}

fn validate(
    m: &Model,
    prev: &BTreeMap<&'static str, i64>,
    next: &BTreeMap<&'static str, i64>,
    action: &str,
) -> (bool, String) {
    verify::validate_transition_tiered(
        m,
        &[
            ("Global", GLOBAL),
            ("HalfShare", GLOBAL / 2),
            ("Cfg", CFG),
            ("Buggy", 0),
        ],
        prev,
        next,
        Some(action),
        "shared scrollback budget Tier-1 binding",
    )
}

const ACTIONS: [&str; 4] = ["Join", "Leave", "Apply1", "Apply2"];

type Step = (BTreeMap<&'static str, i64>, BTreeMap<&'static str, i64>);

fn run_lockstep(m: &Model, seq: &[&str]) -> Option<Step> {
    set_global_scrollback_budget(UNIT * GLOBAL as usize);
    let mut pane1 = real_pane();
    let mut pane2: Option<RealPane> = None;
    let mut st = m.init_state();
    let mut last = None;

    for &action in seq {
        let prev = st.clone();
        if !m.fire(action, &mut st) {
            set_global_scrollback_budget(0);
            return last;
        }
        match action {
            "Join" => pane2 = Some(real_pane()),
            "Leave" => pane2 = None,
            "Apply1" => touch(&mut pane1),
            "Apply2" => touch(pane2.as_mut().expect("guard: pane 2 live")),
            _ => unreachable!("bounded action alphabet"),
        }
        let mut next = st.clone();
        next.insert("a1", applied_units(&pane1));
        next.insert("a2", pane2.as_ref().map_or(0, applied_units));
        st = next.clone();

        let (ok, why) = validate(m, &prev, &next, action);
        assert!(
            ok,
            "spec REJECTED real {action} transition of {seq:?}\nprev={prev:?}\nnext={next:?}\n{why}"
        );
        last = Some((prev, next));
    }
    set_global_scrollback_budget(0);
    last
}

#[test]
fn real_share_registry_conforms_to_spec() {
    let _guard = SERIAL.lock().expect("serial");
    let m = shared_budget_model();
    let mut sequences: Vec<Vec<&str>> = vec![vec![]];
    for _ in 0..EXHAUSTIVE_STEPS {
        let mut next_round = Vec::new();
        for seq in &sequences {
            for action in ACTIONS {
                let mut s = seq.clone();
                s.push(action);
                next_round.push(s);
            }
        }
        for seq in &next_round {
            run_lockstep(&m, seq);
        }
        sequences = next_round;
    }
    for seq in [
        ["Apply1", "Join", "Apply2", "Leave", "Apply1"],
        ["Join", "Apply1", "Leave", "Join", "Apply2"],
        ["Join", "Apply2", "Apply1", "Leave", "Apply1"],
        ["Join", "Leave", "Join", "Apply1", "Apply2"],
    ] {
        run_lockstep(&m, &seq);
    }
}

#[test]
fn forged_over_cap_share_is_rejected() {
    let _guard = SERIAL.lock().expect("serial");
    let m = shared_budget_model();
    let (prev, next) =
        run_lockstep(&m, &["Join", "Apply1", "Apply2"]).expect("feasible canonical sequence");
    let mut forged = next.clone();
    forged.insert("a1", CFG); // real applied share was Half — forge the full cfg
    let (ok, _) = validate(&m, &prev, &forged, "Apply2");
    assert!(
        !ok,
        "spec must reject an over-cap applied share (binding is vacuous otherwise)"
    );
}

#[test]
fn real_stores_evict_to_the_shared_global() {
    let _guard = SERIAL.lock().expect("serial");
    set_global_scrollback_budget(UNIT * GLOBAL as usize);
    let mut a = real_pane();
    let mut b = real_pane();
    for pane in [&mut a, &mut b] {
        let mut buf = Vec::new();
        for i in 0..800 {
            buf.extend_from_slice(format!("budget-load line {i} padding padding\r\n").as_bytes());
        }
        pane.term.process(&buf);
        pane.term.sync_scrollback_buffers();
    }
    touch(&mut a);
    touch(&mut b);
    let budget = |p: &RealPane| {
        p.term
            .scrollback()
            .map_or(0, ScrollbackStorage::memory_budget)
    };
    let used = |p: &RealPane| {
        p.term
            .scrollback()
            .map_or(0, ScrollbackStorage::budgeted_memory_used)
    };
    assert!(
        budget(&a) + budget(&b) <= UNIT * GLOBAL as usize,
        "applied budgets sum within the global cap"
    );
    assert!(
        used(&a) <= budget(&a) && used(&b) <= budget(&b),
        "each store really evicted to its applied share ({} <= {}, {} <= {})",
        used(&a),
        budget(&a),
        used(&b),
        budget(&b)
    );
    set_global_scrollback_budget(0);
}
