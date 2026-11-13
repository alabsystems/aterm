// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::{Arc, Mutex};

use aterm_core::terminal::{RenderDebt, Terminal};

use super::seed_adopted_render_debt;
use crate::resize_epoch::EpochVerdict;
use crate::session_timeline::SessionTimeline;

fn restored() -> Arc<Mutex<Terminal>> {
    let mut t = Terminal::new(12, 20);
    t.process(b"\x1b[?1049h\x1b[2J");
    for r in 0..12 {
        t.process(format!("\x1b[{};1HL{r}", r + 1).as_bytes());
    }
    Arc::new(Mutex::new(t))
}

fn timeline() -> Arc<Mutex<SessionTimeline>> {
    Arc::new(Mutex::new(SessionTimeline::default()))
}

#[test]
fn a_seeded_debt_opens_an_epoch_that_is_judged_from_attach() {
    let term = restored();
    let tl = timeline();
    seed_adopted_render_debt(&term, &tl, RenderDebt::of_rows(-1, 0));
    assert_eq!(
        term.lock().unwrap().render_debt(),
        RenderDebt::of_rows(-1, 0)
    );
    let probe = {
        let t = term.lock().unwrap();
        crate::resize_ledger::EngineProbe::sample(&t)
    };
    let mut guard = tl.lock().unwrap();
    guard.set_agent_reader_for_test(true);
    let epoch = guard
        .resizes()
        .epochs()
        .current()
        .cloned()
        .expect("an epoch");
    assert_eq!(epoch.verdict, EpochVerdict::Open, "inside the risk wait");
    assert_eq!((epoch.shift, epoch.pushed), (-1, 0));
    assert!(guard.resize_nudge_view().is_none());
    let after = epoch.last_resize_ms + crate::resize_ledger::RISK_AFTER_MS;
    guard.evaluate_resizes(probe, after + 1);
    let view = guard.resize_nudge_view().expect("at risk, agent, untried");
    assert_eq!((view.shift, view.pushed), (-1, 0));
}

#[test]
fn a_seeded_cut_is_judged_from_attach_as_rows_are() {
    let term = restored();
    let tl = timeline();
    let cut = RenderDebt {
        cut: 2,
        ..RenderDebt::default()
    };
    seed_adopted_render_debt(&term, &tl, cut);
    assert_eq!(term.lock().unwrap().render_debt(), cut);
    let probe = {
        let t = term.lock().unwrap();
        crate::resize_ledger::EngineProbe::sample(&t)
    };
    let mut guard = tl.lock().unwrap();
    guard.set_agent_reader_for_test(true);
    let epoch = guard
        .resizes()
        .epochs()
        .current()
        .cloned()
        .expect("an epoch on a cut alone");
    let after = epoch.last_resize_ms + crate::resize_ledger::RISK_AFTER_MS;
    guard.evaluate_resizes(probe, after + 1);
    let view = guard.resize_nudge_view().expect("at risk, agent, untried");
    assert_eq!((view.shift, view.pushed, view.cut), (0, 0, 2));
}
