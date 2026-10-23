// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 trace conformance for `SelectionCustody` against [`aterm_spec::verify::validate_transition_tiered`].

#![cfg(test)]

use std::collections::BTreeMap;

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_models_misc::selection_custody_model;
use aterm_spec::xref::StepEvidence;

use crate::app_input::{PressKind, apply_press_custody};
use crate::{App, WindowId, term_lock};

/// The spec variables, in the model's declared order.
const VARS: [&str; 11] = [
    "alive",
    "sel_lo",
    "sel_hi",
    "band_lo",
    "band_hi",
    "floor",
    "truncated",
    "prev_alive",
    "prev_sel_lo",
    "prev_sel_hi",
    "last_event",
];

/// `last_event` tags, as documented on the model's `last_event` var.
const EV_GESTURE: i64 = 1;
const EV_TYPING: i64 = 2;
const EV_INERT: i64 = 3;
const EV_DAMAGE: i64 = 4;
const EV_SCROLL: i64 = 5;
const EV_EVICT: i64 = 6;
const EV_WHOLESALE: i64 = 7;

/// Projects `[alive, sel_lo, sel_hi, floor, truncated]` from `term` relative to `base`.
pub(crate) fn project_selection_custody(term: &Terminal, base: u64) -> [i64; 5] {
    let grid = term.grid();
    let base = i64::try_from(base).unwrap_or(i64::MAX);
    let live_top = i64::try_from(
        grid.absolute_row_counter()
            .saturating_sub(u64::from(grid.rows())),
    )
    .unwrap_or(i64::MAX);
    let floor = i64::try_from(grid.oldest_absolute_row()).unwrap_or(i64::MAX) - base;
    let sel = term.text_selection();
    let a = live_top + i64::from(sel.start().row) - base;
    let b = live_top + i64::from(sel.end().row) - base;
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    [
        i64::from(sel.has_selection()),
        lo,
        hi,
        floor,
        i64::from(sel.truncated()),
    ]
}

/// One fixture's running abstract state: the observed projection plus the
/// harness-carried variables (see the module doc).
struct Custody {
    /// Absolute row corresponding to model row 0.
    base: u64,
    /// Last damage band in model rows.
    band: (i64, i64),
    /// Last action tag.
    last_event: i64,
    /// Model pre-state shadows `(prev_alive, prev_sel_lo, prev_sel_hi)`.
    shadow: (i64, i64, i64),
    /// Last live selection interval.
    interval: (i64, i64),
}

impl Custody {
    fn new(base: u64) -> Self {
        Self {
            base,
            band: (0, 0),
            last_event: 0,
            shadow: (0, 0, 0),
            interval: (0, 0),
        }
    }

    /// The full 11-variable state. Refreshes the carried interval whenever the real
    /// selection is alive, so a later clearing step carries the true last-live span.
    fn state(&mut self, term: &Terminal) -> [i64; 11] {
        let [alive, lo, hi, floor, truncated] = project_selection_custody(term, self.base);
        if alive == 1 {
            self.interval = (lo, hi);
        }
        let (lo, hi) = if alive == 1 { (lo, hi) } else { self.interval };
        [
            alive,
            lo,
            hi,
            self.band.0,
            self.band.1,
            floor,
            truncated,
            self.shadow.0,
            self.shadow.1,
            self.shadow.2,
            self.last_event,
        ]
    }

    /// Record what the step just fired: the model writes the pre-state into the
    /// `prev_*` shadows, stamps `last_event`, and (for the damage actions only)
    /// assigns the band.
    fn fired(&mut self, prev: [i64; 11], tag: i64, band: Option<(i64, i64)>) {
        self.shadow = (prev[0], prev[1], prev[2]);
        self.last_event = tag;
        if let Some(b) = band {
            self.band = b;
        }
    }
}

fn as_state(s: [i64; 11]) -> BTreeMap<&'static str, i64> {
    VARS.iter().copied().zip(s).collect()
}

/// Validate one real transition against the derived `SelectionCustody` spec, `Buggy = 0`.
fn validate_transition(action: &str, prev: [i64; 11], next: [i64; 11]) -> (bool, String) {
    aterm_spec::verify::validate_transition_tiered(
        &selection_custody_model(),
        &[],
        &as_state(prev),
        &as_state(next),
        Some(action),
        "SelectionCustody Tier-1 conformance",
    )
}

/// `Terminal` fixture whose model row 0 is the oldest retained absolute row.
fn engine_fixture(rows: u16, history: usize) -> (Terminal, u64) {
    let mut term = Terminal::new(rows, 20);
    if history == 0 {
        term.set_scrollback_line_limit(Some(0));
        term.process(b"aaa\r\nbbb\r\nccc\r\nddd");
    } else {
        term.set_scrollback_line_limit(Some(history + 8));
        for i in 0..(usize::from(rows) + history + 4) {
            term.process(format!("line{i}\r\n").as_bytes());
        }
        term.set_scrollback_line_limit(Some(history));
        term.set_scrollback_line_limit(Some(history + 8));
        assert_eq!(
            term.grid().scrollback_lines(),
            history,
            "fixture must retain exactly {history} scrollback line(s)"
        );
    }
    let base = term.grid().oldest_absolute_row();
    (term, base)
}

/// Arm a completed selection over model rows `lo..=hi` of an engine fixture. Fixture
/// construction, not a claimed transition: the gesture seam is driven in
/// `gui_gesture_chain`.
fn arm(term: &mut Terminal, base: u64, lo: i64, hi: i64) {
    let grid = term.grid();
    let live_top = i64::try_from(
        grid.absolute_row_counter()
            .saturating_sub(u64::from(grid.rows())),
    )
    .expect("live top fits i64");
    let base = i64::try_from(base).expect("base fits i64");
    let rel = |r: i64| i32::try_from(base + r - live_top).expect("model row fits a selection row");
    let sel = term.text_selection_mut();
    sel.start_selection(rel(lo), 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(rel(hi), 5, SelectionSide::Right);
    sel.complete_selection();
    assert!(sel.has_selection(), "the fixture selection must be alive");
}

/// Model rows of the damage band the harness is about to aim at visible rows
/// `first..=last`, read back out of the real grid's own coordinate function.
fn band_of(term: &Terminal, base: u64, first: u16, last: u16) -> (i64, i64) {
    let grid = term.grid();
    let base = i64::try_from(base).expect("base fits i64");
    (
        i64::try_from(grid.visible_to_absolute(first)).expect("abs fits i64") - base,
        i64::try_from(grid.visible_to_absolute(last)).expect("abs fits i64") - base,
    )
}

/// Drive one damage case: arm `sel_lo..=sel_hi`, damage visible rows `first..=last`
/// with real EL batches, and validate the step against `action`.
fn damage_case(
    action: &'static str,
    sel: (i64, i64),
    rows: (u16, u16),
    expect_alive: bool,
    validated: &mut usize,
    ev: &mut StepEvidence,
) {
    let (mut term, base) = engine_fixture(4, 0);
    arm(&mut term, base, sel.0, sel.1);
    let mut c = Custody::new(base);
    let prev = c.state(&term);
    assert_eq!(
        [prev[0], prev[1], prev[2], prev[5]],
        [1, sel.0, sel.1, 0],
        "damage fixture must start alive over model rows {sel:?} with floor 0"
    );

    let band = band_of(&term, base, rows.0, rows.1);
    let mut batch = Vec::new();
    for row in rows.0..=rows.1 {
        batch.extend_from_slice(format!("\x1b[{};1H\x1b[K", row + 1).as_bytes());
    }
    ev.step(action, || term.process(&batch));

    c.fired(prev, EV_DAMAGE, Some(band));
    let next = c.state(&term);
    assert_eq!(
        next[0],
        i64::from(expect_alive),
        "real {action} over selection {sel:?} with band {band:?}: expected alive={expect_alive}, \
         got {next:?}"
    );
    let (ok, out) = validate_transition(action, prev, next);
    assert!(
        ok,
        "real {action} {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );
    *validated += 1;
}

/// The GUI half: one real `App`, one real terminal, six validated transitions driven
/// through the genuine gesture and press seams.
fn gui_gesture_chain(validated: &mut usize, ev: &mut StepEvidence) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.copy_on_select = false;
    let terminal = app
        .front_terminal(wid)
        .expect("headless_for_test seeds one window with one terminal")
        .term
        .clone();

    let base = {
        let t = term_lock(&terminal);
        let grid = t.grid();
        assert_eq!(
            grid.oldest_absolute_row(),
            grid.absolute_row_counter()
                .saturating_sub(u64::from(grid.rows())),
            "a fresh headless terminal has no history, so model row r IS visible row r"
        );
        assert!(grid.rows() >= 4, "the model's four-row window must fit");
        grid.oldest_absolute_row()
    };
    let mut c = Custody::new(base);

    let init = selection_custody_model().init_state();
    let model_init: [i64; 11] = VARS.map(|v| init[v]);
    assert_eq!(
        c.state(&term_lock(&terminal)),
        model_init,
        "the untouched headless terminal must project to SelectionCustody's Init"
    );
    assert_eq!(model_init, [0; 11], "sanity: Init is the all-zero state");

    let drag = |app: &mut App, from: (u16, u16), to: (u16, u16)| {
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.last_mouse_cell = from;
        }
        app.begin_selection(wid, SelectionType::Simple);
        app.drag_selection(wid, to.0, to.1);
        let _ = app.finish_selection(wid, true);
    };

    let prev = c.state(&term_lock(&terminal));
    ev.step("SelectLow", || drag(&mut app, (0, 0), (1, 5)));
    c.fired(prev, EV_GESTURE, None);
    let next = c.state(&term_lock(&terminal));
    let (ok, out) = validate_transition("SelectLow", prev, next);
    assert!(
        ok,
        "real drag-select {prev:?} -> {next:?} must conform to SelectLow\n--- ty ---\n{out}"
    );
    *validated += 1;

    let prev = next;
    let inert = ev.step("InertPress", || {
        apply_press_custody(&mut term_lock(&terminal), PressKind::Inert)
    });
    assert_eq!(
        inert,
        (false, false),
        "an inert press must neither snap the viewport nor clear the selection"
    );
    c.fired(prev, EV_INERT, None);
    let next = c.state(&term_lock(&terminal));
    let (ok, out) = validate_transition("InertPress", prev, next);
    assert!(
        ok,
        "real inert press {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );
    *validated += 1;

    let prev = next;
    let typing = ev.step("TypingPress", || {
        apply_press_custody(&mut term_lock(&terminal), PressKind::Typing)
    });
    assert!(typing.1, "a disturbing press must clear a live selection");
    c.fired(prev, EV_TYPING, None);
    let next = c.state(&term_lock(&terminal));
    let (ok, out) = validate_transition("TypingPress", prev, next);
    assert!(
        ok,
        "real typing press {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );
    *validated += 1;

    let prev = next;
    ev.step("SelectHigh", || drag(&mut app, (2, 0), (3, 5)));
    c.fired(prev, EV_GESTURE, None);
    let next = c.state(&term_lock(&terminal));
    let (ok, out) = validate_transition("SelectHigh", prev, next);
    assert!(
        ok,
        "real drag-select {prev:?} -> {next:?} must conform to SelectHigh\n--- ty ---\n{out}"
    );
    *validated += 1;

    let prev = next;
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.last_mouse_cell = (2, 0);
    }
    ev.step("UserClear", || {
        app.begin_selection(wid, SelectionType::Simple);
        let _ = app.finish_selection(wid, true);
    });
    c.fired(prev, EV_GESTURE, None);
    let next = c.state(&term_lock(&terminal));
    assert_eq!(next[0], 0, "a click without a drag deselects");
    let (ok, out) = validate_transition("UserClear", prev, next);
    assert!(
        ok,
        "real deselecting click {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );
    *validated += 1;

    let prev = next;
    ev.step("SelectOldest", || drag(&mut app, (0, 0), (0, 5)));
    c.fired(prev, EV_GESTURE, None);
    let next = c.state(&term_lock(&terminal));
    assert_eq!(
        [next[0], next[1], next[2]],
        [1, 0, 0],
        "a same-row drag is the model's one-row selection"
    );
    let (ok, out) = validate_transition("SelectOldest", prev, next);
    assert!(
        ok,
        "real one-row drag {prev:?} -> {next:?} must conform to SelectOldest\n--- ty ---\n{out}"
    );
    *validated += 1;
}

/// The both-endpoints-gone arm of `Evict`, asserted directly rather than ty-validated:
/// `Evict` re-floors `sel_lo` even when it destroys the selection, so the model's
/// post-state names a row the dead selection cannot be asked about.
fn evict_destroys_a_selection_whose_whole_interval_fell_off(ev: &mut StepEvidence) {
    let (mut term, base) = engine_fixture(4, 2);
    arm(&mut term, base, 0, 0);
    assert_eq!(
        project_selection_custody(&term, base),
        [1, 0, 0, 0, 0],
        "a one-row selection on the oldest retained row, floor 0"
    );
    let retained = term.grid().scrollback_lines();
    ev.step("Evict", || {
        term.set_scrollback_line_limit(Some(retained - 1))
    });
    let after = project_selection_custody(&term, base);
    assert_eq!(after[3], 1, "the floor rose by one");
    assert_eq!(
        after[0], 0,
        "a selection BOTH of whose endpoints were evicted is gone, not clamped: {after:?}"
    );
    assert_eq!(
        after[4], 0,
        "…and a destroyed selection reports no truncation"
    );
}

/// Runs the Tier-1 `SelectionCustody` conformance suite and returns step evidence.
pub(crate) fn run_conformance() -> StepEvidence {
    let mut validated = 0usize;
    let mut ev = StepEvidence::new("SelectionCustody");

    gui_gesture_chain(&mut validated, &mut ev);

    damage_case(
        "RegionDamageLow",
        (2, 3),
        (0, 1),
        true,
        &mut validated,
        &mut ev,
    );
    damage_case(
        "RegionDamageLow",
        (0, 1),
        (0, 1),
        false,
        &mut validated,
        &mut ev,
    );
    damage_case(
        "RegionDamageHigh",
        (2, 3),
        (3, 3),
        false,
        &mut validated,
        &mut ev,
    );
    damage_case(
        "RegionDamageHigh",
        (0, 1),
        (3, 3),
        true,
        &mut validated,
        &mut ev,
    );

    {
        let (mut term, base) = engine_fixture(4, 0);
        arm(&mut term, base, 0, 1);
        let mut c = Custody::new(base);
        let prev = c.state(&term);
        ev.step("WholesaleInvalidate", || term.process(b"\x1b[3J"));
        c.fired(prev, EV_WHOLESALE, None);
        let next = c.state(&term);
        assert_eq!(
            next[0], 0,
            "ED 3 erases the scrollback the selection names, so the selection must go"
        );
        let (ok, out) = validate_transition("WholesaleInvalidate", prev, next);
        assert!(
            ok,
            "real ED 3 batch {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
        );
        validated += 1;
    }

    {
        let (mut term, base) = engine_fixture(4, 0);
        arm(&mut term, base, 0, 1);
        let mut c = Custody::new(base);
        let prev = c.state(&term);
        ev.step("WholesaleInvalidate", || term.process(b"\x1bc"));
        c.fired(prev, EV_WHOLESALE, None);
        let next = c.state(&term);
        assert_eq!(
            next[0], 0,
            "RIS destroys the coordinate space the anchors name"
        );
        let (ok, out) = validate_transition("WholesaleInvalidate", prev, next);
        assert!(
            ok,
            "real RIS batch {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
        );
        validated += 1;
    }

    {
        let (mut term, base) = engine_fixture(4, 2);
        arm(&mut term, base, 0, 1);
        let mut c = Custody::new(base);

        let prev = c.state(&term);
        ev.step("UniformScroll", || term.process(b"more\r\n"));
        c.fired(prev, EV_SCROLL, None);
        let next = c.state(&term);
        assert_eq!(
            [next[0], next[1], next[2], next[5]],
            [1, 0, 1, 0],
            "a uniform scroll must leave the absolute interval and the floor alone"
        );
        let (ok, out) = validate_transition("UniformScroll", prev, next);
        assert!(
            ok,
            "real content scroll {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
        );
        validated += 1;

        let prev = next;
        let retained = term.grid().scrollback_lines();
        ev.step("Evict", || {
            term.set_scrollback_line_limit(Some(retained - 1))
        });
        c.fired(prev, EV_EVICT, None);
        let next = c.state(&term);
        assert_eq!(
            [next[0], next[1], next[5], next[6]],
            [1, 1, 1, 1],
            "partial eviction clamps the head to the new floor and RECORDS the loss"
        );
        let (ok, out) = validate_transition("Evict", prev, next);
        assert!(
            ok,
            "real partial eviction {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
        );
        validated += 1;
    }

    {
        let (mut term, base) = engine_fixture(4, 2);
        arm(&mut term, base, 2, 3);
        let mut c = Custody::new(base);
        let prev = c.state(&term);
        let retained = term.grid().scrollback_lines();
        ev.step("Evict", || {
            term.set_scrollback_line_limit(Some(retained - 1))
        });
        c.fired(prev, EV_EVICT, None);
        let next = c.state(&term);
        assert_eq!(
            [next[0], next[1], next[2], next[5], next[6]],
            [1, 2, 3, 1, 0],
            "an eviction that reached no endpoint leaves the span and the flag alone"
        );
        let (ok, out) = validate_transition("Evict", prev, next);
        assert!(
            ok,
            "real no-op eviction {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
        );
        validated += 1;
    }

    evict_destroys_a_selection_whose_whole_interval_fell_off(&mut ev);

    let live_low = [1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    let live_high = [1, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0];

    let rejected: &[(&str, [i64; 11], [i64; 11], &str)] = &[
        (
            "InertPress",
            live_low,
            [0, 0, 1, 0, 0, 0, 0, 1, 0, 1, EV_INERT],
            "a bare modifier expresses no intent and may destroy nothing",
        ),
        (
            "UniformScroll",
            live_low,
            [0, 0, 1, 0, 0, 0, 0, 1, 0, 1, EV_SCROLL],
            "ordinary output cannot take a selection",
        ),
        (
            "RegionDamageLow",
            live_high,
            [0, 2, 3, 0, 1, 0, 0, 1, 2, 3, EV_DAMAGE],
            "damage confined to rows 0-1 may not clear a selection on rows 2-3",
        ),
        (
            "RegionDamageHigh",
            live_high,
            [1, 2, 3, 3, 3, 0, 0, 1, 2, 3, EV_DAMAGE],
            "a highlight left over replaced text is worse than a lost one",
        ),
        (
            "Evict",
            live_low,
            [1, 0, 1, 0, 0, 1, 1, 1, 0, 1, EV_EVICT],
            "a truncation is only ever recorded against a CLAMPED head",
        ),
        (
            "Evict",
            live_low,
            [0, 1, 1, 0, 0, 1, 0, 1, 0, 1, EV_EVICT],
            "losing the oldest line of a selection is not losing the selection",
        ),
        (
            "SelectLow",
            [0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0],
            [1, 0, 1, 0, 0, 1, 0, 0, 0, 0, EV_GESTURE],
            "you cannot start a selection on a row that has already been evicted",
        ),
    ];
    for (action, prev, next, why) in rejected {
        let (ok, out) = validate_transition(action, *prev, *next);
        assert!(
            !ok,
            "NEGATIVE CONTROL ({action}) {prev:?} -> {next:?} MUST be rejected — {why}\
             \n--- ty ---\n{out}"
        );
    }

    crate::logging::stderr_line!(
        "SelectionCustody Tier-1 conformance: {validated} real transitions strictly validated \
         against the derived spec (6 through the real App gesture/press seams, 4 damage cases \
         through real VT batches, 1 wholesale, 1 uniform scroll, 2 evictions), the \
         both-endpoints-evicted arm asserted directly, and {} negative controls \
         (inert-press destroys, scroll destroys, disjoint damage clears, overlap damage \
         spares, truncation without a clamp, partial eviction destroys, select below the \
         floor) all rejected.",
        rejected.len()
    );

    ev
}
