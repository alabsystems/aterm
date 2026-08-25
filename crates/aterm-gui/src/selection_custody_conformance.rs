// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 trace conformance for `SelectionCustody` — the alt-screen-agnostic
//! LIFECYCLE of a text selection, bound to the code that actually runs.
//!
//! `selection_custody_model()` is model-checked in the abstract at Tier-0
//! (`aterm-spec/tests/derived_ring_ty.rs`), which proves the DESIGN sound: a bare
//! modifier destroys nothing, ordinary output destroys nothing, damage that missed
//! the selected rows spares them, damage that hit them clears them, and losing the
//! oldest line of a selection clamps the head instead of throwing the whole span
//! away. None of that ties the model to the shipping engine. This closes that gap.
//!
//! METHOD — strict per-transition validation, the same shape
//! `window_routing_conformance` uses: each step drives a REAL shipping seam, projects
//! the real `Terminal` onto the spec variables, and asks
//! [`aterm_spec::verify::validate_transition_tiered`] whether the observed step is one
//! the derived `Next` admits (in-process interpreter always; `ty trace validate` on top
//! wherever the Trust toolchain is installed, with the two verdicts asserted to agree).
//! `Init` is pinned to `prev`, so a corrupted `next` is reliably REJECTED — which the
//! negative controls at the end assert, so a pass is never vacuous.
//!
//! THE SEAMS DRIVEN (all eleven actions are anchored; see each `#[refines]`):
//!
//! * `App::begin_selection` → `App::drag_selection` → `App::finish_selection` — the
//!   genuine left-drag gesture, for `SelectLow` / `SelectOldest` / `SelectHigh`, and
//!   the press-release-inside-one-cell arm for `UserClear`.
//! * `app_input::apply_press_custody` — the ONE press-custody authority, for
//!   `TypingPress` (`disturbs = true`) and `InertPress` (`disturbs = false`).
//! * `Terminal::process` → `Terminal::post_process` — real VT batches, for
//!   `UniformScroll`, `RegionDamageLow` and `RegionDamageHigh`.
//! * `Terminal::set_scrollback_line_limit` — the eviction-with-no-delta entry point,
//!   for `Evict`.
//!
//! WHAT AN ANCHOR PROVES, AND WHAT IT DOES NOT. The `#[refines]` attribute emits an
//! inventory record naming a (machine, action) pair. The gate checks that every action
//! of an active machine appears in that set — it does NOT check that the attribute sits
//! on a function which performs the action. `xref.rs` says so itself: obligation 2, that
//! `project` resolves to a live symbol, "is NOT enforced here". Moving an anchor to an
//! unrelated stub keeps the gate green, which was verified rather than assumed.
//!
//! So the anchors here are documentation with a coverage gate attached, and THIS module
//! is the part that can fail. It drives real gestures and real `Terminal::process`
//! batches, projects the engine before and after, and asserts the model admits the
//! transition — plus negative controls asserting the model REFUSES the regression
//! members. Deleting `force_selection_invalidation()` from ED 3 in the shipping grid
//! turns it red; that is the standard each case is held to.
//!
//! * `Terminal::post_process`'s `SelectionDamage::All` arm — for
//!   `WholesaleInvalidate`, driven with real ED 3 (`\x1b[3J`) and RIS (`\x1bc`) bytes.
//!
//! THE ABSTRACTION FUNCTION. The model works in a four-row window; the engine works in
//! absolute grid rows. Model row `r` IS absolute row `base + r`, where `base` is read
//! from the real grid at fixture time (`Grid::oldest_absolute_row()`), and every
//! projected quantity is an absolute-space read of the live `TextSelection`:
//!
//! * `alive` ← `TextSelection::has_selection()`
//! * `sel_lo`/`sel_hi` ← the anchor rows lifted into absolute space the same way
//!   `TextSelection::intersects_absolute_band` lifts them (`live_top_abs + row`),
//!   minus `base`
//! * `floor` ← `Grid::oldest_absolute_row() - base`
//! * `truncated` ← `TextSelection::truncated()`
//!
//! Lifting into ABSOLUTE space is what makes `UniformScrollPreservesTheSelection`
//! stateable at all: ordinary output moves `live_top_abs` up and the relative anchors
//! down by the same amount, so the absolute interval is unchanged — which is precisely
//! the law "the anchors ride the content".
//!
//! THE TWO HARNESS-CARRIED VARIABLES, stated plainly rather than hidden:
//!
//! * `band_lo`/`band_hi` are an INPUT to the step (the model's damage actions ASSIGN
//!   them), so the harness names the visible rows it damages and reads their absolute
//!   numbers back out of the real grid (`Grid::visible_to_absolute`). Nothing about the
//!   OUTCOME is assumed: whether the selection survives is read off the real
//!   `TextSelection` afterwards, and the disjoint/overlap pair of cases below is exactly
//!   what would fail if the engine's recorded band were wider or narrower than named.
//! * `last_event` and the `prev_*` shadows are trace bookkeeping — the same class as
//!   `exited` in `window_routing_conformance`. The shadows are the harness's OWN
//!   projection of the pre-state, so they are real observations; `last_event` is the
//!   action tag.
//!
//! WHICH VARIABLES ARE REALLY OBSERVED, stated exactly, because "11 variables" reads
//! stronger than what is actually checked.
//!
//! * `alive`, `floor`, `truncated` come from the real `Terminal` on EVERY transition —
//!   `project_selection_custody` reads `text_selection()`, `scrollback_lines()` and the
//!   clamp record. These carry the properties the design is about: whether a selection
//!   survived, and whether a partial loss was recorded rather than silently swallowed.
//! * `sel_lo`/`sel_hi` are real WHILE THE SELECTION LIVES. On a clearing transition a
//!   cleared `TextSelection` has no interval left to read, so the harness carries the
//!   last live one. The model's `sel_lo' = sel_lo` on those actions is therefore
//!   satisfied BY CONSTRUCTION, not checked — and that is true of every clearing action
//!   (`TypingPress`, `UserClear`, `WholesaleInvalidate`, and both damage-clears), not
//!   just `Evict`. An earlier draft of this note said "one gap" and named only `Evict`,
//!   which understated it.
//! * `band_lo`/`band_hi` are the harness's own aim, re-derived through
//!   `Grid::visible_to_absolute` — NOT read back from the engine's recorded
//!   `SelectionDamage`. They check coordinate arithmetic, not the lattice. What
//!   constrains the real band is indirect but genuine: the four disjoint/overlap cases
//!   assert the resulting `alive`, and those come from the engine.
//!
//! `Evict` is the one action that re-floors `sel_lo` even on the arm where the selection
//! dies, so its both-endpoints-evicted arm is driven for real and asserted directly
//! rather than ty-validated. See `evict_destroys_a_selection_whose_whole_interval_fell_off`.

#![cfg(test)]

use std::collections::BTreeMap;

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_spec::derive::selection_custody_model;

use crate::app_input::apply_press_custody;
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

/// The OBSERVED half of the projection: `[alive, sel_lo, sel_hi, floor, truncated]`
/// read out of a real `Terminal` against the fixture's absolute-row `base`.
///
/// Named by every `#[refines(project = …)]` on the `SelectionCustody` seams. The
/// anchor rows are lifted into absolute space exactly as
/// `TextSelection::intersects_absolute_band` lifts them — `live_top_abs + row`, with
/// `live_top_abs = absolute_row_counter - visible_rows` — so the projection and the
/// engine's own damage test speak the same coordinates.
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

/// One fixture's running abstract state: the observed projection plus the three
/// harness-carried variables documented in the module header.
struct Custody {
    /// Absolute row that model row 0 names.
    base: u64,
    /// The last damage band the harness aimed, in model rows.
    band: (i64, i64),
    /// The last action tag.
    last_event: i64,
    /// `(prev_alive, prev_sel_lo, prev_sel_hi)` — the model's pre-state shadows.
    shadow: (i64, i64, i64),
    /// The last LIVE interval, which is what the model carries once `alive = 0`.
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

/// Validate ONE real transition against the derived `SelectionCustody` spec.
/// `Buggy` stays 0 — the committed, correct custody discipline the engine implements.
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
/// Returns `(terminal, base)`; `rows` is the visible height, `history` the number of
/// retained scrollback lines the fixture should end up holding.
fn engine_fixture(rows: u16, history: usize) -> (Terminal, u64) {
    let mut term = Terminal::new(rows, 20);
    if history == 0 {
        term.set_scrollback_line_limit(Some(0));
        term.process(b"aaa\r\nbbb\r\nccc\r\nddd");
    } else {
        // Generous limit first so the ring really fills, then pin the retained count
        // exactly, then lift the limit again so an ordinary scroll does NOT evict
        // (that is `UniformScroll`'s premise: motion without eviction).
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

/// Arm a completed selection over MODEL rows `lo..=hi` of an engine fixture.
///
/// FIXTURE CONSTRUCTION, not a claimed transition — the same status
/// `App::headless_for_test()` has in `window_routing_conformance`. The GUI gesture
/// seam that really makes selections is driven for real in `gui_gesture_chain`.
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

/// Model rows of the damage band the harness is about to aim at VISIBLE rows
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
    action: &str,
    sel: (i64, i64),
    rows: (u16, u16),
    expect_alive: bool,
    validated: &mut usize,
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
    // EL (`\e[K`) records exactly the cursor's row on the selection-damage lattice,
    // and adjacent bands merge — so this is a real, precisely-aimed band, not a hull.
    let mut batch = Vec::new();
    for row in rows.0..=rows.1 {
        batch.extend_from_slice(format!("\x1b[{};1H\x1b[K", row + 1).as_bytes());
    }
    term.process(&batch);

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
fn gui_gesture_chain(validated: &mut usize) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    // Keep the REAL system clipboard untouched: `finish_selection`'s copy-on-select and
    // X11-PRIMARY channels are exfil side effects, not custody. The scoped-edge fence
    // (`suppress_copy_on_select = true`) skips BOTH while still COMPLETING the
    // selection, which is the only half this conformance is about.
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

    // PROJECTION-DRIFT GUARD: the untouched fixture must project to the model's own
    // `Init`. If the absolute-row lift or the field reads drift, this fails before any
    // transition is validated.
    let init = selection_custody_model().init_state();
    let model_init: [i64; 11] = VARS.map(|v| init[v]);
    assert_eq!(
        c.state(&term_lock(&terminal)),
        model_init,
        "the untouched headless terminal must project to SelectionCustody's Init"
    );
    assert_eq!(model_init, [0; 11], "sanity: Init is the all-zero state");

    // The genuine left-drag: press in a cell, move to another, release.
    let drag = |app: &mut App, from: (u16, u16), to: (u16, u16)| {
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.last_mouse_cell = from;
        }
        app.begin_selection(wid, SelectionType::Simple);
        app.drag_selection(wid, to.0, to.1);
        let _ = app.finish_selection(wid, true);
    };

    // --- SelectLow: a two-row selection on the model's low pair.
    let prev = c.state(&term_lock(&terminal));
    drag(&mut app, (0, 0), (1, 5));
    c.fired(prev, EV_GESTURE, None);
    let next = c.state(&term_lock(&terminal));
    let (ok, out) = validate_transition("SelectLow", prev, next);
    assert!(
        ok,
        "real drag-select {prev:?} -> {next:?} must conform to SelectLow\n--- ty ---\n{out}"
    );
    *validated += 1;

    // --- InertPress: a bare modifier may DESTROY NOTHING.
    let prev = next;
    {
        let mut t = term_lock(&terminal);
        assert_eq!(
            apply_press_custody(&mut t, false),
            (false, false),
            "an inert press must neither snap the viewport nor clear the selection"
        );
    }
    c.fired(prev, EV_INERT, None);
    let next = c.state(&term_lock(&terminal));
    let (ok, out) = validate_transition("InertPress", prev, next);
    assert!(
        ok,
        "real inert press {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );
    *validated += 1;

    // --- TypingPress: the ONE handover.
    let prev = next;
    {
        let mut t = term_lock(&terminal);
        assert!(
            apply_press_custody(&mut t, true).1,
            "a disturbing press must clear a live selection"
        );
    }
    c.fired(prev, EV_TYPING, None);
    let next = c.state(&term_lock(&terminal));
    let (ok, out) = validate_transition("TypingPress", prev, next);
    assert!(
        ok,
        "real typing press {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );
    *validated += 1;

    // --- SelectHigh: the live-screen pair.
    let prev = next;
    drag(&mut app, (2, 0), (3, 5));
    c.fired(prev, EV_GESTURE, None);
    let next = c.state(&term_lock(&terminal));
    let (ok, out) = validate_transition("SelectHigh", prev, next);
    assert!(
        ok,
        "real drag-select {prev:?} -> {next:?} must conform to SelectHigh\n--- ty ---\n{out}"
    );
    *validated += 1;

    // --- UserClear: press and release inside ONE cell is a deliberate deselect.
    let prev = next;
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.last_mouse_cell = (2, 0);
    }
    app.begin_selection(wid, SelectionType::Simple);
    let _ = app.finish_selection(wid, true);
    c.fired(prev, EV_GESTURE, None);
    let next = c.state(&term_lock(&terminal));
    assert_eq!(next[0], 0, "a click without a drag deselects");
    let (ok, out) = validate_transition("UserClear", prev, next);
    assert!(
        ok,
        "real deselecting click {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );
    *validated += 1;

    // --- SelectOldest: a ONE-ROW selection on the oldest retained row. Without this
    // shape the both-endpoints-evicted arm of `Evict` is unreachable at all.
    let prev = next;
    drag(&mut app, (0, 0), (0, 5));
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

/// The both-endpoints-gone arm of `Evict`, asserted DIRECTLY rather than
/// ty-validated — and the module header says why: `Evict` re-floors `sel_lo` even on
/// the arm that destroys the selection, so the model's post-state names a row the
/// dead concrete selection cannot be asked about. What IS checkable is the behaviour
/// the invariant exists for, and that is checked here.
fn evict_destroys_a_selection_whose_whole_interval_fell_off() {
    let (mut term, base) = engine_fixture(4, 2);
    arm(&mut term, base, 0, 0);
    assert_eq!(
        project_selection_custody(&term, base),
        [1, 0, 0, 0, 0],
        "a one-row selection on the oldest retained row, floor 0"
    );
    let retained = term.grid().scrollback_lines();
    term.set_scrollback_line_limit(Some(retained - 1));
    let after = project_selection_custody(&term, base);
    assert_eq!(after[3], 1, "the floor rose by one");
    assert_eq!(
        after[0], 0,
        "a selection BOTH of whose endpoints were evicted is gone, not clamped: {after:?}"
    );
    assert_eq!(after[4], 0, "…and a destroyed selection reports no truncation");
}

#[test]
fn real_selection_custody_conforms() {
    run_conformance();
}

/// The `SelectionCustody` Tier-1 body, factored out so the `spec_xref_gate` can RUN
/// it: the gate's "SelectionCustody is actively bound" claim then means the real
/// engine and GUI seams were driven and checked, not merely that anchors exist.
pub(crate) fn run_conformance() {
    let mut validated = 0usize;

    // ---- The GUI half: six transitions through the real gesture/press seams. ----
    gui_gesture_chain(&mut validated);

    // ---- The DAMAGE half: both sides of the lattice, both outcomes. ----
    // Model rows are visible rows here (a zero-history fixture), and the band is read
    // back out of `Grid::visible_to_absolute`.
    //
    // `DisjointDamagePreserves`: rows 0-1 rewritten, the selection sits on 2-3.
    damage_case("RegionDamageLow", (2, 3), (0, 1), true, &mut validated);
    // `OverlapDamageClears`: the same band, the selection now sits ON it.
    damage_case("RegionDamageLow", (0, 1), (0, 1), false, &mut validated);
    // The INVERSE half — the hole where a highlight survived over replaced text.
    damage_case("RegionDamageHigh", (2, 3), (3, 3), false, &mut validated);
    damage_case("RegionDamageHigh", (0, 1), (3, 3), true, &mut validated);

    // ---- WholesaleInvalidate: the coordinate space itself is gone. ----
    //
    // Driven with REAL BYTES, through the same `Terminal::process` batch path a program
    // uses, because that is the only way this action reaches a user. The first version
    // of this case called `Terminal::clear_scrollback()` directly — a host-facing API
    // that NOTHING inside the engine calls — so it validated a seam no VT sequence can
    // drive, and the anchor sat on that seam too. ED 3 is one of the three producers
    // the model names; it arrives as `\x1b[3J`.
    {
        let (mut term, base) = engine_fixture(4, 0);
        arm(&mut term, base, 0, 1);
        let mut c = Custody::new(base);
        let prev = c.state(&term);
        term.process(b"\x1b[3J");
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

    // …and RIS, the second producer, through the same batch path. Two producers rather
    // than one because they take DIFFERENT routes to the same destruction: ED 3 records
    // `SelectionDamage::All` and lets the drain clear, while RIS clears directly in
    // `Terminal::reset` and the drain finds nothing left to do. A binding that only ever
    // saw one of those would miss a regression in the other.
    {
        let (mut term, base) = engine_fixture(4, 0);
        arm(&mut term, base, 0, 1);
        let mut c = Custody::new(base);
        let prev = c.state(&term);
        term.process(b"\x1bc");
        c.fired(prev, EV_WHOLESALE, None);
        let next = c.state(&term);
        assert_eq!(next[0], 0, "RIS destroys the coordinate space the anchors name");
        let (ok, out) = validate_transition("WholesaleInvalidate", prev, next);
        assert!(
            ok,
            "real RIS batch {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
        );
        validated += 1;
    }

    // ---- UniformScroll then Evict, as ONE chain on ONE terminal. ----
    {
        let (mut term, base) = engine_fixture(4, 2);
        arm(&mut term, base, 0, 1);
        let mut c = Custody::new(base);

        // Ordinary output while the user reads history: the anchors ride the content,
        // so the ABSOLUTE interval does not move and the floor does not rise.
        let prev = c.state(&term);
        term.process(b"more\r\n");
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

        // Retention pressure now drops the oldest line under the selection's head:
        // a PARTIAL loss, so the head clamps to the new floor and records it.
        let prev = next;
        let retained = term.grid().scrollback_lines();
        term.set_scrollback_line_limit(Some(retained - 1));
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

    // ---- Evict, the arm that must change nothing: no endpoint is below the floor. ----
    {
        let (mut term, base) = engine_fixture(4, 2);
        arm(&mut term, base, 2, 3);
        let mut c = Custody::new(base);
        let prev = c.state(&term);
        let retained = term.grid().scrollback_lines();
        term.set_scrollback_line_limit(Some(retained - 1));
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

    // ---- The third `Evict` arm, asserted directly (see the fn doc). ----
    evict_destroys_a_selection_whose_whole_interval_fell_off();

    // ---- NEGATIVE CONTROLS (non-vacuity). ----
    // Each is a `Buggy = 1` regression member falsifying a NAMED invariant. If ANY of
    // them were admitted, a real custody regression would sail through the checks above
    // and the whole conformance would prove nothing.
    let live_low = [1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    let live_high = [1, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0];

    let rejected: &[(&str, [i64; 11], [i64; 11], &str)] = &[
        // (a) A bare modifier destroys the selection — complaint (1),
        // `InertPressPreservesTheSelection`.
        (
            "InertPress",
            live_low,
            [0, 0, 1, 0, 0, 0, 0, 1, 0, 1, EV_INERT],
            "a bare modifier expresses no intent and may destroy nothing",
        ),
        // (b) Ordinary output takes a highlight — complaint (2),
        // `UniformScrollPreservesTheSelection`.
        (
            "UniformScroll",
            live_low,
            [0, 0, 1, 0, 0, 0, 0, 1, 0, 1, EV_SCROLL],
            "ordinary output cannot take a selection",
        ),
        // (c) Damage that MISSED clears anyway — the shipping sentinel's whole failure
        // mode, `DisjointDamagePreserves`.
        (
            "RegionDamageLow",
            live_high,
            [0, 2, 3, 0, 1, 0, 0, 1, 2, 3, EV_DAMAGE],
            "damage confined to rows 0-1 may not clear a selection on rows 2-3",
        ),
        // (d) Damage that HIT leaves the highlight — the INVERSE hole,
        // `OverlapDamageClears`: a copy then returns text the user never selected.
        (
            "RegionDamageHigh",
            live_high,
            [1, 2, 3, 3, 3, 0, 0, 1, 2, 3, EV_DAMAGE],
            "a highlight left over replaced text is worse than a lost one",
        ),
        // (e) Eviction reports the loss without acting on it: the floor rises and
        // `truncated` is set, but the head still names the evicted row —
        // `NoDanglingAnchors` / `TruncationImpliesAClampedHead`.
        (
            "Evict",
            live_low,
            [1, 0, 1, 0, 0, 1, 1, 1, 0, 1, EV_EVICT],
            "a truncation is only ever recorded against a CLAMPED head",
        ),
        // (f) Eviction destroys a selection the user can still see half of —
        // `PartialEvictionTruncates`.
        (
            "Evict",
            live_low,
            [0, 1, 1, 0, 0, 1, 0, 1, 0, 1, EV_EVICT],
            "losing the oldest line of a selection is not losing the selection",
        ),
        // (g) A GUARD, not an invariant: `SelectLow` is disabled once the floor has
        // risen, because a row that has been evicted is not on screen to select. This
        // control proves the guards bite, not merely the update expressions.
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

    eprintln!(
        "SelectionCustody Tier-1 conformance: {validated} real transitions strictly validated \
         against the derived spec (6 through the real App gesture/press seams, 4 damage cases \
         through real VT batches, 1 wholesale, 1 uniform scroll, 2 evictions), the \
         both-endpoints-evicted arm asserted directly, and {} negative controls \
         (inert-press destroys, scroll destroys, disjoint damage clears, overlap damage \
         spares, truncation without a clamp, partial eviction destroys, select below the \
         floor) all rejected.",
        rejected.len()
    );
}
