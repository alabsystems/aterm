// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 trace conformance for `PressCustody` — WHO owns the reading position and
//! the highlight, bound to the code that actually runs.
//!
//! `press_custody_model()` is model-checked in the abstract at Tier-0
//! (`aterm-spec/tests/derived_ring_ty.rs`), which proves the DESIGN sound: a release,
//! an auto-repeat tick and a bare modifier each destroy nothing, typing takes the
//! viewport back to live, output re-pins instead of snapping, output that replaced
//! the selected rows may take the highlight but never the reading position, and an
//! invalidated coordinate space may not leave a selection dangling. None of that
//! ties the model to the shipping engine. This closes that gap.
//!
//! WHY THIS COULD NOT BE WRITTEN BEFORE. Three of the eleven actions — `RepeatPress`,
//! `InertPress`, `ReleaseEvent` — are IDENTICAL in every observable variable. All
//! three leave offset, ownership and the selection exactly as they found them; the
//! model gives all three the same body; and all three used to reach
//! `apply_press_custody` as the single value `disturbs == false`. A conformance that
//! had to INFER the action from the state change could not tell them apart, so the
//! three invariants that name them could only ever be "asserted" by not looking —
//! which is the ghost the model's own header warns about. `OutputAtLive` was worse:
//! at live the offset is already 0, so its `offset = 0` is a self-assignment.
//!
//! The fix is the SEAM, not the harness. `Terminal::note_custody` records which
//! transition fired, at the site that decided it, while the discriminating facts are
//! still in hand (`aterm_core::terminal::custody`). This module reads that record
//! back and validates the observed step against THE ACTION THE ENGINE NAMED — so the
//! action tag is an observation, not a harness stamp, and the three inert classes are
//! separable for the first time.
//!
//! METHOD — strict per-transition validation, the same shape
//! `selection_custody_conformance` uses: each step drives a REAL shipping seam,
//! projects the real `Terminal` onto the spec variables, and asks
//! [`aterm_spec::verify::validate_transition_tiered`] whether the observed step is one
//! the derived `Next` admits (in-process interpreter always; `ty trace validate` on
//! top wherever the Trust toolchain is installed). `Init` is pinned to `prev`, so a
//! corrupted `next` is reliably REJECTED — which the negative controls at the end
//! assert, so a pass is never vacuous.
//!
//! WHICH TIER CHECKS WHAT, because this module used to credit the wrong one.
//! `validate_transition_tiered` is a REFINEMENT check and only that: the guard admits
//! `prev`, and `next` is the action's exact update image. It evaluates no invariant at
//! either tier — its `ty` config emits `SPECIFICATION Spec` with no `INVARIANT` lines
//! and its interpreter verdict is `successors(action, prev).contains(next)`.
//! Invariants over all reachable states are TIER-0's job (`derived_ring_ty.rs`). But
//! Tier-0 quantifies over the MODEL's reachable states and this module supplies
//! OBSERVED ones, so [`validate_transition`] additionally evaluates every invariant
//! over `prev` and `next` here — eleven expression evaluations per step, and the only
//! thing that can reject a projection which drifted out of `0..=MaxOffset`. Negative
//! control (l) is a step the refinement check admits and only an invariant rejects.
//!
//! EVERY STEP IS A THREE-PART CLAIM, and all three parts can fail:
//!
//! 1. the record is CLEARED before the gesture and must come back `Some(_)`, so a
//!    seam that stopped recording — or that DECLINED to record because what it
//!    observed was not the transition it was about to name — fails here rather than
//!    passing on a stale tag;
//! 2. the recorded variant must be the one this step is about, so a seam that
//!    MIS-CLASSIFIES fails here even though the state change may be identical. All
//!    four press classes are delivered as real `App::input` key events — a bare
//!    `NamedKey::ShiftLeft` at `KeyEventType::Press`, a `Character` press, a Repeat
//!    tick and a Release — so `PressKind::of` and `is_modifier_or_lock_key` are what
//!    answer, not a `PressKind` the harness chose. Two of the four are additionally
//!    AMBIGUOUS in the raw bits (`is_release && inert_modifier`,
//!    `is_repeat && inert_modifier`), and a mis-ordered priority files them as
//!    `InertPress` while changing nothing observable;
//! 3. the observed `prev -> next` pair must be admitted by the action the ENGINE
//!    named, so a seam that classified correctly and then moved something it may not
//!    fails here.
//!
//! THE SEAMS DRIVEN (all eleven actions are anchored; see each `#[refines]`):
//!
//! * `app_input::note_press_custody`, through the real `App::input` key seam and the
//!   `apply_press_custody` authority under it — `TypingPress`, `RepeatPress`,
//!   `InertPress`, `ReleaseEvent`.
//! * `app_mouse::note_selection_custody`, through the real
//!   `begin_selection` → `drag_selection` → `finish_selection` gesture —
//!   `UserSelect` and `UserClear`.
//! * `Terminal::note_scroll_custody` — in aterm-core, inside the three primitives
//!   that can RAISE `display_offset` — through the real `App::input` →
//!   `InputEvent::ScrollView` seam AND through a real `InputEvent::Wheel` notch on
//!   both of `input_wheel`'s motion routes — `UserScroll`.
//! * `Terminal::note_output_custody`, through real `Terminal::process` batches —
//!   `OutputAtLive`, `OutputWhileReading`, `OutputDamagesTheSelectedRows` (from BOTH
//!   ownerships), `OutputInvalidatesTheCoordinateSpace`.
//!
//! WHAT AN ANCHOR PROVES, AND WHAT IT DOES NOT. The `#[refines]` attribute emits an
//! inventory record naming a (machine, action) pair. The gate checks that every action
//! of an active machine appears in that set — it does NOT check that the attribute sits
//! on a function which performs the action. `xref.rs` says so itself: obligation 2, that
//! `project` resolves to a live symbol, "is NOT enforced here". So the anchors here are
//! documentation with a coverage gate attached, and THIS module is the part that can
//! fail.
//!
//! THE ABSTRACTION FUNCTION — three real reads, one derivation, one recorded tag:
//!
//! * `offset` ← `Grid::display_offset()`, as the IDENTITY on `0..=MaxOffset`. Every
//!   step below is driven one row at a time (`ScrollIntent::By(1)`, one-line output
//!   batches) so the real offset never leaves `0..=2` and nothing is saturated. That
//!   is a deliberate limit, not a claim: a real wheel notch moves
//!   `wheel_viewport_lines` rows and a real PgUp moves a page, and a projection that
//!   bucketed those would be monotone but would break the model's exact
//!   `offset = offset + 1`. A step that left the range is REJECTED by `StateBounds`,
//!   never silently clamped — which is a statement about the invariant evaluation
//!   [`validate_transition`] performs, not about `validate_transition_tiered`, whose
//!   refinement check admits an out-of-range `InertPress` happily (control (l)).
//! * `selection` ← `TextSelection::has_selection()`. The model's prose says "a
//!   COMPLETED text selection exists", which would be `state() == Complete`; the
//!   engine's own press-path clear gate reads `has_selection()`, and a projection that
//!   disagreed with the gate it is checking would report 0 before and after while the
//!   engine really did destroy something. Under this choice `begin_selection` is where
//!   a selection comes into existence and `finish_selection`'s complete arm is an
//!   idempotent re-`UserSelect`; both are driven.
//! * `owner` ← `usize::from(offset > 0)`, DERIVED, never carried. `TailOwnerAtBottom`
//!   (`if owner == 0 { offset == 0 } else { offset > 0 }`) with `owner <= 1` is a
//!   biconditional, so ownership is not independent state — it is a reading of the
//!   offset. A harness-carried ownership flag would satisfy that invariant by
//!   construction and turn the model's one state-consistency guard into a ghost,
//!   which is the same failure mode the model's header warns about for self-reported
//!   "this press disturbed something" flags. The honest consequence, stated plainly:
//!   every `owner == prev_owner` conjunct is entailed by an offset clause except one —
//!   that output may not carry a LIVE view into scrollback.
//! * `last_event` ← `CustodyTransition::last_event()` of the record the ENGINE wrote.
//! * the `prev_*` shadows are the harness's own projection of the pre-state, so they
//!   are real observations, re-read from the terminal at every step.
//!
//! KNOWN GAPS, stated rather than papered over:
//!
//! * A batch that destroyed the selection for a reason that is NOT damage overlap —
//!   `post_process`'s fail-closed splice arm and its four siblings — is unmodelled.
//!   The engine records it as `OutputTookTheSelectionUnattributed`, which is not a
//!   model action and carries `last_event() == -1`, outside the model's tag space, so
//!   it can never be handed to `validate_transition` as if it were one
//!   (`Model::successors` panics on an unknown action name, so an attempt would be
//!   loud rather than quiet). It is a real answer for the `custody` verb and a
//!   deliberate non-step for the trace.
//! * A DAMAGING batch whose SCR-1 re-pin saturated at the history floor — the user
//!   parked at the top of a full scrollback, the arriving line evicting the oldest —
//!   is unmodelled: its offset does not rise, and `OutputDamagesTheSelectedRows`
//!   mandates the rise at `owner == 1`. It is recorded anyway, because
//!   `OutputDamagesTheSelectedRows` is a TRUE statement about what happened and only
//!   the offset arithmetic is out of range; the undamaged twin of the same shape
//!   records NOTHING, because its only alternative name (`OutputAtLive`) would be a
//!   false statement about where the view is. `Terminal::note_output_custody` carries
//!   the argument.
//! * Offset-to-zero WITHOUT a selection clear — the paste / IME `snap_to_bottom`, and
//!   `ScrollIntent::Bottom`/`Down` — is likewise unmodelled: `TypingPress` is the only
//!   action that lands at live and it clears on the way. Those sites record nothing,
//!   so they are absent from the trace instead of mislabelled.
//! * `drag_selection` now records the 0 -> 1 EDGE at its own seam. Its gesture arms
//!   re-issue `start_selection` on every pointer move with no state test, so a drag
//!   DOES turn `has_selection()` on from off when something cleared it mid-gesture —
//!   `Some(g)` guards a live gesture, not a live selection. This bullet twice
//!   asserted the opposite (first that the site could not turn the selection on, then
//!   that it "requires a selection already in progress"); both were false, and the
//!   seam is recorded rather than argued about. `extend_selection_to` genuinely does
//!   bail unless the selection is `Complete` (app_mouse.rs:2585), so it produces
//!   nothing to record. The three that DID turn it on
//!   from off without a record — Select-All, a word/line click's PRESS, and a search
//!   jump — now record at their own seams; the earlier text claimed they could not,
//!   which was false as written.
//! * `MaxOffset = 2` means the abstract offset saturates two rows into history. The
//!   steps here are driven from offset 0 and 1 so every modelled rise is observable;
//!   a trace that started at 2 would satisfy `prev_offset <= offset` with a no-op.

#![cfg(test)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{CustodyTransition, Terminal};
use aterm_spec::derive::press_custody_model;

use crate::input::{InputEvent, ScrollIntent, Source};
use crate::{App, WindowId, term_lock};

/// The spec variables, in the model's declared order.
const VARS: [&str; 7] = [
    "owner",
    "offset",
    "selection",
    "prev_owner",
    "prev_offset",
    "prev_selection",
    "last_event",
];

/// The OBSERVED projection: `[owner, offset, selection]` read out of a real
/// `Terminal`.
///
/// Named by every `#[refines(project = …)]` on the `PressCustody` seams. `owner` is
/// DERIVED from the offset rather than carried — see the module header on why a
/// carried ownership flag would make `TailOwnerAtBottom` unfalsifiable.
pub(crate) fn project_press_custody(term: &Terminal) -> [i64; 3] {
    let offset = i64::try_from(term.grid().display_offset()).unwrap_or(i64::MAX);
    [
        i64::from(offset > 0),
        offset,
        i64::from(term.text_selection().has_selection()),
    ]
}

/// One fixture's running abstract state: the observed projection plus the trace
/// bookkeeping the model's shadow variables need.
struct Press {
    /// `(prev_owner, prev_offset, prev_selection)` — the harness's own projection of
    /// the pre-state, re-read from the real terminal at every step.
    shadow: (i64, i64, i64),
    /// The tag of the transition the ENGINE last recorded.
    last_event: i64,
}

impl Press {
    fn new() -> Self {
        Self {
            shadow: (0, 0, 0),
            last_event: 0,
        }
    }

    /// The full 7-variable state.
    fn state(&self, term: &Terminal) -> [i64; 7] {
        let [owner, offset, selection] = project_press_custody(term);
        [
            owner,
            offset,
            selection,
            self.shadow.0,
            self.shadow.1,
            self.shadow.2,
            self.last_event,
        ]
    }

    /// Record what the step just fired: the model writes the pre-state into the
    /// `prev_*` shadows and stamps `last_event` from the recorded transition.
    fn fired(&mut self, prev: [i64; 7], recorded: CustodyTransition) {
        self.shadow = (prev[0], prev[1], prev[2]);
        self.last_event = recorded.last_event();
    }
}

fn as_state(s: [i64; 7]) -> BTreeMap<&'static str, i64> {
    VARS.iter().copied().zip(s).collect()
}

/// Validate ONE real transition against the derived `PressCustody` spec.
/// `Buggy` stays 0 — the committed, correct custody discipline the engine implements.
///
/// TWO CHECKS, and it is worth naming which does which, because this module used to
/// credit the wrong one. [`aterm_spec::verify::validate_transition_tiered`] is a
/// REFINEMENT check and nothing else: its interpreter verdict is
/// `m.successors(action, prev).contains(next)`, and its `ty` twin writes a
/// `Model::transition_cfg` that emits `SPECIFICATION Spec` with NO `INVARIANT` lines.
/// Both tiers therefore answer exactly one question — does this action's GUARD admit
/// `prev`, and is `next` its exact update image? — and no invariant is consulted at
/// either. That is the HOUSE DIVISION OF LABOUR and it is the right one: Tier-0
/// (`aterm_spec::verify::prove_and_catch`, run by `derived_ring_ty.rs`) evaluates every
/// invariant over every state reachable from the model's own `Init`, which is strictly
/// stronger than anything a two-state trace could say.
///
/// It is stronger over the MODEL's reachable states. This module feeds in states
/// OBSERVED from a real terminal, and nothing makes those two sets the same. A
/// projection that saturated, drifted or read the wrong field can hand Tier-1 a state
/// the model never reaches, and the refinement check alone admits it happily: a real
/// wheel notch moves `wheel_viewport_lines` rows, so an `InertPress` observed at
/// `display_offset == 7` is the exact update image of `InertPress` at that state and
/// used to validate cleanly even though `StateBounds` says `offset <= MaxOffset` (= 2).
/// So the invariants are evaluated HERE as well, over `prev` AND `next`, at the cost of
/// eleven expression evaluations per step. Negative control (l) is a step no refinement
/// check can reject and only an invariant can, so this tier is not decorative either.
fn validate_transition(action: &str, prev: [i64; 7], next: [i64; 7]) -> (bool, String) {
    let model = press_custody_model();
    let (refines, mut why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &as_state(prev),
        &as_state(next),
        Some(action),
        "PressCustody Tier-1 conformance",
    );
    let mut holds = true;
    for (label, observed) in [("prev", prev), ("next", next)] {
        let state = as_state(observed);
        let env = model.eval_env(&state);
        for inv in &model.invariants {
            if !model.check_invariant_in(inv, &env) {
                holds = false;
                why.push_str(&format!(
                    "\ninvariant {} VIOLATED by the {label} state {observed:?}",
                    inv.name
                ));
            }
        }
    }
    (refines && holds, why)
}

/// Clear the custody record, so the step that follows must WRITE one.
///
/// This is what makes claim 1 of the three-part claim checkable: without it a seam
/// that stopped recording altogether would leave the previous step's tag in place and
/// a harness reading it back could not tell.
fn arm(term: &Arc<Mutex<Terminal>>) {
    let _ = term_lock(term).take_custody_transition();
}

/// Read back what the step recorded, requiring that it recorded SOMETHING and that it
/// recorded the RIGHT thing.
fn recorded(term: &Arc<Mutex<Terminal>>, expect: CustodyTransition, what: &str) -> CustodyTransition {
    let got = term_lock(term).take_custody_transition();
    assert_eq!(
        got,
        Some(expect),
        "{what}: the shipping seam must RECORD {expect:?}. The record was cleared \
         immediately before this step, so `None` means either that the seam records \
         nothing at all, or that it DECLINED because the state change it observed is \
         not the one this action describes — an output re-pin that did not move the \
         offset declines rather than claim `OutputWhileReading`. A different variant \
         means it classified the event wrongly, which for the output kinds includes \
         having destroyed a highlight it was not entitled to destroy."
    );
    expect
}

/// Drive ONE real step, then validate it against the action the ENGINE named.
///
/// `gesture` runs the shipping seam. Nothing here holds the terminal lock while it
/// runs — the pre-state read, the gesture and the post-state read are three separate
/// acquisitions on purpose, because the SEAMS are what must read their own before and
/// after inside one guard, and they do.
fn step(
    term: &Arc<Mutex<Terminal>>,
    c: &mut Press,
    expect: CustodyTransition,
    what: &str,
    gesture: impl FnOnce(),
    validated: &mut usize,
) -> [i64; 7] {
    let prev = c.state(&term_lock(term));
    arm(term);
    gesture();
    let got = recorded(term, expect, what);
    c.fired(prev, got);
    let next = c.state(&term_lock(term));
    let (ok, out) = validate_transition(got.action(), prev, next);
    assert!(
        ok,
        "{what}: real {} {prev:?} -> {next:?} must conform\n--- ty ---\n{out}",
        got.action()
    );
    *validated += 1;
    next
}

/// A terminal with `history` retained scrollback lines and the viewport at live.
///
/// Built by feeding REAL bytes through `Terminal::process`, so the scrollback is the
/// engine's own and the offsets the steps below take are offsets into real history.
fn engine_fixture(rows: u16, history: usize) -> Terminal {
    let mut term = Terminal::new(rows, 20);
    term.set_scrollback_line_limit(Some(history + 8));
    for i in 0..(usize::from(rows) + history) {
        term.process(format!("line{i}\r\n").as_bytes());
    }
    assert!(
        term.grid().scrollback_lines() >= history,
        "fixture must retain at least {history} scrollback line(s)"
    );
    assert_eq!(
        term.grid().display_offset(),
        0,
        "a freshly-fed fixture sits at live"
    );
    term
}

/// Arm a completed selection over LIVE-SCREEN selection rows `first..=last` — the
/// same coordinates `App::begin_selection` computes (viewport row minus
/// `display_offset`), so row 0 is the top of the live screen however far back the
/// view is scrolled.
///
/// FIXTURE CONSTRUCTION, not a claimed transition — the same status
/// `App::headless_for_test()` has. The GUI gesture seam that really makes selections
/// is driven for real in [`gui_gesture_chain`].
fn arm_selection(term: &mut Terminal, first: i32, last: i32) {
    let sel = term.text_selection_mut();
    sel.start_selection(first, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(last, 5, SelectionSide::Right);
    sel.complete_selection();
    assert!(sel.has_selection(), "the fixture selection must be alive");
}

/// `OutputDamagesTheSelectedRows`: one batch that BOTH replaces the rows the
/// selection is sitting on AND scrolls a line in — driven from BOTH ownerships.
///
/// Both halves of the batch are load-bearing for the reading case. The action assigns
/// `offset = offset + 1` when the user owns the viewport, so a batch that damaged
/// without advancing any row would leave the offset where it was and be REJECTED —
/// the action says the re-pin rides the new lines exactly as for undamaged output, and
/// that is the half of it a damage-only batch could not check.
///
/// BOTH OWNERSHIPS, because the engine records both. The action used to be guarded
/// `owner == 1`, which disabled it outright at live — and at LIVE is where its
/// commonest real instance happens: a status line or progress bar repainting the rows
/// a highlight sits on, which is the shipping defect this design came from. The
/// recorder never consulted the offset for this arm, so that instance was a step the
/// spec could not admit while the gate called the machine 11/11 bound with zero
/// waivers. The model now carries the split in its offset clause instead of its guard
/// (`offset = if owner == 1 { offset + 1 } else { 0 }`), and both shapes are DRIVEN
/// here rather than one of them being stated as a gap.
///
/// The band is aimed with EL (`\e[K`), which records exactly the cursor's row on the
/// selection-damage lattice, at the LIVE screen's row 0 — the prologue has forced the
/// offset to 0 for the duration of the batch, so VT row 1 is the live top, which is
/// where the fixture's selection starts.
fn damaged_rows_case(validated: &mut usize) {
    // EL over the live top (the selection's own rows), then a line feed from the
    // bottom row so a row really enters scrollback.
    const DAMAGE: &[u8] = b"\x1b[1;1H\x1b[Knew text\x1b[6;1H\r\n";

    // (a) The user is reading history: the re-pin rides the arriving line.
    let term = Arc::new(Mutex::new(engine_fixture(6, 4)));
    {
        let mut t = term_lock(&term);
        t.scroll_display(1);
        arm_selection(&mut t, 0, 1);
        assert_eq!(t.grid().display_offset(), 1, "the user is reading history");
    }
    let mut c = Press::new();
    let next = step(
        &term,
        &mut c,
        CustodyTransition::OutputDamagesTheSelectedRows,
        "output that REPLACED the selected rows while the user was reading",
        || {
            term_lock(&term).process(DAMAGE);
        },
        validated,
    );
    assert_eq!(
        [next[0], next[1], next[2]],
        [1, 2, 0],
        "the highlight over replaced text goes, the reading position does NOT"
    );

    // (b) The view is at LIVE — the dominant instance, and the one the model used to
    // shut out. Same bytes, same action, ownership unchanged at the tail.
    let term = Arc::new(Mutex::new(engine_fixture(6, 4)));
    {
        let mut t = term_lock(&term);
        arm_selection(&mut t, 0, 1);
        assert_eq!(t.grid().display_offset(), 0, "the tail-follower owns the view");
    }
    let mut c = Press::new();
    let next = step(
        &term,
        &mut c,
        CustodyTransition::OutputDamagesTheSelectedRows,
        "a status line REPLACING the rows a live highlight sits on",
        || {
            term_lock(&term).process(DAMAGE);
        },
        validated,
    );
    assert_eq!(
        [next[0], next[1], next[2]],
        [0, 0, 0],
        "the highlight over replaced text goes; at live there is no reading position \
         to ride and the view stays where it was"
    );
}

/// The GUI half: one real `App`, one real terminal, eleven validated transitions
/// driven through the genuine scroll, gesture and press seams.
fn gui_gesture_chain(validated: &mut usize) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    // Keep the REAL system clipboard untouched: `finish_selection`'s copy-on-select
    // and X11-PRIMARY channels are exfil side effects, not custody.
    app.copy_on_select = false;
    let term = app
        .front_terminal(wid)
        .expect("headless_for_test seeds one window with one terminal")
        .term
        .clone();

    // Give the fixture real history to scroll into, through the real output path.
    {
        let mut t = term_lock(&term);
        let rows = t.grid().rows();
        for i in 0..(u32::from(rows) + 4) {
            t.process(format!("row{i}\r\n").as_bytes());
        }
        assert!(
            t.grid().scrollback_lines() >= 4,
            "the fixture needs history to scroll into"
        );
        assert_eq!(t.grid().display_offset(), 0, "…and starts at live");
    }
    let mut c = Press::new();

    // PROJECTION-DRIFT GUARD: the fixture at live with no selection must project to
    // the model's own `Init`. If the field reads or the ownership derivation drift,
    // this fails before any transition is validated.
    let init = press_custody_model().init_state();
    let model_init: [i64; 7] = VARS.map(|v| init[v]);
    assert_eq!(
        c.state(&term_lock(&term)),
        model_init,
        "a live, unselected terminal must project to PressCustody's Init"
    );
    assert_eq!(model_init, [0; 7], "sanity: Init is the all-zero state");

    // --- UserScroll: the user takes the viewport, ONE row, through the real seam.
    step(
        &term,
        &mut c,
        CustodyTransition::UserScroll,
        "one-row scroll back into history",
        || {
            app.input(
                wid,
                InputEvent::ScrollView(ScrollIntent::By(1)),
                Source::Human,
            );
        },
        validated,
    );

    // --- UserSelect: the press that starts a drag brings a selection into existence.
    let drag_from = |app: &mut App, from: (u16, u16)| {
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.last_mouse_cell = from;
        }
        app.begin_selection(wid, SelectionType::Simple);
    };
    step(
        &term,
        &mut c,
        CustodyTransition::UserSelect,
        "left press starting a drag",
        || drag_from(&mut app, (1, 0)),
        validated,
    );

    // --- UserSelect again: the RELEASE that completes the drag. Idempotent in the
    // model (`selection = 1` from `selection == 1`) and an honest report of a second
    // user gesture that leaves a selection behind.
    step(
        &term,
        &mut c,
        CustodyTransition::UserSelect,
        "left release completing the drag",
        || {
            app.drag_selection(wid, 2, 5);
            let _ = app.finish_selection(wid, true);
        },
        validated,
    );

    // --- ALL FOUR PRESS CLASSES, each as a REAL `App::input` delivery.
    //
    // These used to be `apply_press_custody(&mut t, kind)` with `kind` chosen by the
    // harness, and `note_press_custody` is a four-arm identity match on that same
    // value — so "a seam that MIS-CLASSIFIES fails here" was a claim about a
    // hardcoded table, not about any classification of a real key event. Two of
    // `PressKind::of`'s four arms were never reached, and the mutant that deletes the
    // `inert_modifier` arm — a bare ⌘ keydown snapping the viewport and clearing the
    // highlight, which is LITERALLY the defect this whole design exists to prevent —
    // survived every check in this module. It does not survive now: the delivery
    // below is a real `NamedKey::ShiftLeft` at `KeyEventType::Press`, so
    // `is_modifier_or_lock_key` is the thing that decides, and the record comes back
    // named by the seam's own resolution.
    //
    // THE ORDER IS ALSO THE PRIORITY TEST. The three press bits OVERLAP:
    // `is_modifier_or_lock_key` matches on the KEY alone with no event-type term, so
    // a Shift key-UP is `is_release && inert_modifier` at once and a held Shift's
    // auto-repeat is `is_repeat && inert_modifier` at once. The old `!a && !b && !c`
    // predicate was order-free; a four-way tag is not. A mis-ordered `PressKind::of`
    // files the first two under `InertPress`, changes nothing observable, and is
    // invisible to every other check here.
    //
    // Every one of the four runs with the user owning the viewport AND a live
    // selection, so a press class that started disturbing has BOTH things to take and
    // the step is rejected rather than passing on an empty state.
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    let shift_left = || Key::Named(NamedKey::ShiftLeft);
    for (key, mods, event_type, expect, what) in [
        (
            shift_left(),
            Modifiers::SHIFT,
            KeyEventType::Release,
            CustodyTransition::ReleaseEvent,
            "a Shift key-UP (release AND inert modifier at once)",
        ),
        (
            shift_left(),
            Modifiers::SHIFT,
            KeyEventType::Repeat,
            CustodyTransition::RepeatPress,
            "a held Shift auto-repeating (repeat AND inert modifier at once)",
        ),
        (
            shift_left(),
            Modifiers::SHIFT,
            KeyEventType::Press,
            CustodyTransition::InertPress,
            "a bare Shift keydown — the first half of Cmd-C, classified by \
             `is_modifier_or_lock_key` and by nothing the harness said",
        ),
        (
            Key::Character('a'),
            Modifiers::empty(),
            KeyEventType::Press,
            CustodyTransition::TypingPress,
            "a byte-producing keydown — the ONE handover",
        ),
    ] {
        let before = c.state(&term_lock(&term));
        let after = step(
            &term,
            &mut c,
            expect,
            what,
            || {
                app.input(
                    wid,
                    InputEvent::Key {
                        key,
                        mods,
                        base_layout: None,
                        event_type,
                    },
                    Source::Human,
                );
            },
            validated,
        );
        if expect == CustodyTransition::TypingPress {
            assert_eq!(
                [after[0], after[1], after[2]],
                [0, 0, 0],
                "typing lands at live, tail-owned, unselected"
            );
        } else {
            assert_eq!(
                [after[0], after[1], after[2]],
                [before[0], before[1], before[2]],
                "{what}: an inert press may move neither the viewport nor the highlight"
            );
            assert_eq!(
                [before[0], before[1], before[2]],
                [1, 1, 1],
                "{what}: it must be driven with something TO take — a reading \
                 position and a live highlight — or `inert` is free"
            );
        }
    }

    // --- UserSelect then UserClear: a press and release inside ONE cell is a
    // deliberate deselect, and a deliberate deselect is always allowed.
    step(
        &term,
        &mut c,
        CustodyTransition::UserSelect,
        "left press with no drag",
        || drag_from(&mut app, (2, 0)),
        validated,
    );
    let after_clear = step(
        &term,
        &mut c,
        CustodyTransition::UserClear,
        "left release inside the press cell",
        || {
            let _ = app.finish_selection(wid, true);
        },
        validated,
    );
    assert_eq!(after_clear[2], 0, "a click without a drag deselects");
}

/// THE THREE SEAMS THAT USED TO BRING A SELECTION INTO EXISTENCE SILENTLY.
///
/// `UserSelect` was recorded only from `begin_selection` / `finish_selection`, and the
/// module's own header justified the omission of the rest with a sentence that was
/// false as written: that they "cannot turn `has_selection()` on from off without a
/// `start_selection` that IS recorded". `App::select_all` reaches the `TextSelection`
/// primitive directly from a menu command with no mouse-down before it and no
/// `finish_selection` after it; a double- or triple-click's PRESS builds a completed
/// selection through `control::select_word` / `select_line` and only ARMS the drag, so
/// the record stayed stale for the whole press-to-release window; and a search jump
/// clears and re-anchors the highlight with no gesture around it at all. All three
/// moved the projected `selection` 0 -> 1 with nothing recorded.
///
/// Two of the three are driven here as real validated steps. The search jump records
/// at its own seam (`app_search`'s navigation apply, and its refusal path records the
/// matching `UserClear`) but needs a populated find-bar match vector to reach, so it
/// is covered by its seam's assertion rather than by a step — stated, not implied.
fn silent_select_seams(validated: &mut usize) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.copy_on_select = false;
    let term = app
        .front_terminal(wid)
        .expect("headless_for_test seeds one window with one terminal")
        .term
        .clone();
    {
        let mut t = term_lock(&term);
        let rows = t.grid().rows();
        for i in 0..(u32::from(rows) + 4) {
            t.process(format!("row{i}\r\n").as_bytes());
        }
        assert!(
            !t.text_selection().has_selection(),
            "the fixture starts with nothing selected"
        );
    }
    let mut c = Press::new();

    // ⌘-A: a window-level menu command, no gesture around it.
    let after_all = step(
        &term,
        &mut c,
        CustodyTransition::UserSelect,
        "Select All from the menu, with no mouse-down before it",
        || app.select_all(),
        validated,
    );
    assert_eq!(
        after_all[2], 1,
        "Select All really did bring a selection into existence"
    );

    // A double-click PRESS: `control::select_word` completes a selection and
    // `arm_gesture_drag` only ARMS the release. The release's own `UserSelect` is
    // driven in `gui_gesture_chain`; this is the press that made the highlight.
    let after_word = step(
        &term,
        &mut c,
        CustodyTransition::UserSelect,
        "the double-click PRESS that word-selects, before any release",
        || app.select_word_click(wid, 0, 1),
        validated,
    );
    assert_eq!(
        after_word[2], 1,
        "the word-click press leaves a completed selection behind"
    );
}

/// THE WHEEL — the dominant scroll gesture, driven through both of `input_wheel`'s
/// real motion routes.
///
/// This is the check the module was missing entirely. `UserScroll` used to be
/// anchored on a GUI helper wired into two of the roughly eight seams that raise
/// `display_offset`, and the wheel was in neither: `input_wheel`'s instant arm, its
/// glide tick, `settle_scroll_motion_at_target`, `control::apply_scroll_intent`, the
/// cross-session `mouse` wheel fallback and the search jump all reached
/// `Terminal::scroll_display` / `scroll_to_absolute_row` directly and recorded
/// nothing, so after a wheel notch the `custody` verb named an innocent press beside a
/// reading position that press had not moved. The Tier-1 body could not see it,
/// because it drove `UserScroll` only through `InputEvent::ScrollView`, the one
/// recorded route.
///
/// The recorder now lives in the three `Terminal` primitives that can raise the
/// offset, so every one of those routes records by construction. Two of them are
/// driven here to prove it rather than assert it — and they are the two the motion
/// policy switches between, so neither can regress without the other noticing:
///
/// * INSTANT (`motion = reduced`, OS Reduce Motion, or an unfocused window — the M1
///   accessibility clause): `scroll_wheel_animated`'s early arm calls
///   `scroll_display` under its own lock.
/// * GLIDE + SETTLE: the ~180 ms ease banks the target and
///   `settle_scroll_motion_at_target` lands it in whole rows, on the terminal the
///   glide pinned rather than whatever pane is frontmost now.
///
/// One fixture each, because both start from live and the abstract offset is bounded
/// at `MaxOffset = 2` — a chain of notches would saturate rather than step.
fn wheel_seams(validated: &mut usize) {
    for (focused, what) in [
        (
            false,
            "a wheel notch on an unfocused window (the INSTANT motion arm)",
        ),
        (
            true,
            "a wheel notch settled out of its GLIDE (the eased motion arm)",
        ),
    ] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.copy_on_select = false;
        let term = app
            .front_terminal(wid)
            .expect("headless_for_test seeds one window with one terminal")
            .term
            .clone();
        {
            let mut t = term_lock(&term);
            let rows = t.grid().rows();
            for i in 0..(u32::from(rows) + 4) {
                t.process(format!("row{i}\r\n").as_bytes());
            }
            assert!(
                t.grid().scrollback_lines() >= 4,
                "the fixture needs history to scroll into"
            );
            assert_eq!(t.grid().display_offset(), 0, "…and starts at live");
        }
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.focused = focused;
        }
        let mut c = Press::new();
        let next = step(
            &term,
            &mut c,
            CustodyTransition::UserScroll,
            what,
            || {
                app.input(
                    wid,
                    InputEvent::Wheel {
                        // ONE line, so the abstract step is the model's exact
                        // `offset + 1`. Off Windows `wheel_platform_lines` is the
                        // identity, so this really is one row; a real notch moves
                        // `wheel_viewport_lines` rows, which is a granularity gap the
                        // record deliberately does not carry (it says the user took
                        // the viewport, not how far).
                        dir: aterm_types::mouse::WheelDir::Up,
                        lines: 1,
                        row: 0,
                        col: 0,
                        mods: 0,
                        px_off: crate::input::PixelOffset::CELL_ORIGIN,
                    },
                    Source::Human,
                );
                // The eased arm banks a whole-row target and lands it here. Inside
                // the SAME step as the notch, because the gesture is the notch —
                // `settle_scroll_motion_at_target` is how it finishes, not a second
                // user action.
                app.settle_scroll_motion_at_target(wid, std::time::Instant::now());
            },
            validated,
        );
        assert_eq!(
            [next[0], next[1]],
            [1, 1],
            "{what}: the wheel really moved the engine's viewport, not just the record"
        );
    }
}

/// The OUTPUT half: real `Terminal::process` batches, each on its own fixture so the
/// bounded offset never has to saturate.
fn output_batches(validated: &mut usize) {
    // ---- OutputAtLive, WITH a live selection the batch does not touch. ----
    //
    // The action is an identity transition on the observable variables — at live the
    // offset is already 0, so its `offset = 0` is a self-assignment — and Tier-0 can
    // therefore falsify nothing through it.
    //
    // WHAT ACTUALLY GUARDS THE SHIPPING DEFECT HERE, stated precisely rather than
    // credited to the invariant. An engine that destroyed a live highlight when a
    // status bar repainted rows it never selected — the defect this whole design came
    // from — is caught by the named assertion below, and by `recorded()`: a batch
    // that destroys a selection it did not damage is classified
    // `OutputTookTheSelectionUnattributed` and one that damaged it is classified
    // `OutputDamagesTheSelectedRows`, so either way the record does not come back
    // `OutputAtLive` and the step fails at claim 2. `OutputSparesAnUndamagedSelection`
    // IS evaluated on this step (see `validate_transition`), and it would reject the
    // same shape — but it has no reachable counterexample under this recorder, so
    // saying it is the thing doing the rejecting would be borrowed credit.
    {
        let term = Arc::new(Mutex::new(engine_fixture(6, 4)));
        {
            let mut t = term_lock(&term);
            arm_selection(&mut t, 0, 1);
        }
        let mut c = Press::new();
        let next = step(
            &term,
            &mut c,
            CustodyTransition::OutputAtLive,
            "one line of output while the tail-follower owns the viewport",
            || {
                term_lock(&term).process(b"tail\r\n");
            },
            validated,
        );
        assert!(
            term_lock(&term).text_selection().has_selection(),
            "a status bar repainting rows it never selected may not destroy the \
             highlight — the shipping defect, failing under its own name"
        );
        assert_eq!(
            [next[0], next[1], next[2]],
            [0, 0, 1],
            "output at live leaves the view at live AND spares an undamaged selection"
        );
    }

    // ---- OutputWhileReading: the re-pin rides the arriving line. ----
    //
    // ARMED WITH A SELECTION, like its at-live sibling. Without one this step's
    // `selection` frame is the trivial 0 -> 0 and the event-5 claim "output that did
    // not touch the selected rows leaves the highlight alone" says nothing here at
    // all — the same emptiness the at-live case was accused of. The highlight is on
    // the LIVE screen while the view is a row back in history, which is precisely the
    // shape a `tail -f` under a scrolled-back reader produces.
    {
        let term = Arc::new(Mutex::new(engine_fixture(6, 4)));
        {
            let mut t = term_lock(&term);
            t.scroll_display(1);
            arm_selection(&mut t, 0, 1);
        }
        let mut c = Press::new();
        let next = step(
            &term,
            &mut c,
            CustodyTransition::OutputWhileReading,
            "one line of output while the USER owns the viewport",
            || {
                term_lock(&term).process(b"more\r\n");
            },
            validated,
        );
        assert_eq!(
            [next[0], next[1], next[2]],
            [1, 2, 1],
            "the offset RISES with the arriving line, ownership stays with the user, \
             AND a highlight the batch never touched survives"
        );
    }

    // ---- OutputInvalidatesTheCoordinateSpace: real ED 3 bytes. ----
    {
        let term = Arc::new(Mutex::new(engine_fixture(6, 4)));
        {
            let mut t = term_lock(&term);
            t.scroll_display(1);
            arm_selection(&mut t, 0, 1);
        }
        let mut c = Press::new();
        let next = step(
            &term,
            &mut c,
            CustodyTransition::OutputInvalidatesTheCoordinateSpace,
            "ED 3 erasing the scrollback the offset and the anchors named",
            || {
                term_lock(&term).process(b"\x1b[3J");
            },
            validated,
        );
        assert_eq!(
            [next[0], next[1], next[2]],
            [0, 0, 0],
            "the space the offset named is gone, so the viewport goes back and the \
             selection cannot outlive it"
        );
    }

    // ---- …and RIS, the second producer of the SAME action through a different
    // route: ED 3 records `SelectionDamage::All` and lets the drain clear, while RIS
    // clears directly in `Terminal::reset`. A binding that only ever saw one of those
    // would miss a regression in the other.
    {
        let term = Arc::new(Mutex::new(engine_fixture(6, 4)));
        {
            let mut t = term_lock(&term);
            t.scroll_display(1);
            arm_selection(&mut t, 0, 1);
        }
        let mut c = Press::new();
        let next = step(
            &term,
            &mut c,
            CustodyTransition::OutputInvalidatesTheCoordinateSpace,
            "RIS destroying the coordinate space",
            || {
                term_lock(&term).process(b"\x1bc");
            },
            validated,
        );
        assert_eq!([next[0], next[1], next[2]], [0, 0, 0], "same law, other route");
    }
}

#[test]
fn real_press_custody_conforms() {
    run_conformance();
}

/// The `PressCustody` Tier-1 body, factored out so the `spec_xref_gate` can RUN it:
/// the gate's "PressCustody is actively bound" claim then means the real engine and
/// GUI seams were driven and checked, not merely that anchors exist.
pub(crate) fn run_conformance() {
    let mut validated = 0usize;

    gui_gesture_chain(&mut validated);
    wheel_seams(&mut validated);
    silent_select_seams(&mut validated);
    // Everything counted so far came through the App's real input seams; the rest is
    // the engine's own output path. Derived rather than hardcoded, so a step added to
    // either half cannot make the summary line lie.
    let gui = validated;
    output_batches(&mut validated);
    damaged_rows_case(&mut validated);

    // ---- NEGATIVE CONTROLS (non-vacuity). ----
    // Each is a regression the model must REFUSE. If ANY were admitted, a real
    // custody regression would sail through the checks above and the whole
    // conformance would prove nothing. Every one of them is a state pair the
    // interpreter evaluates against the named action's own guard and updates, so a
    // rejection here is the model doing work, not the harness.
    let read_low = [1, 1, 1, 0, 0, 0, 0];
    let read_high = [1, 1, 1, 1, 1, 1, 0];

    let rejected: &[(&str, [i64; 7], [i64; 7], &str)] = &[
        // (a) A bare modifier snaps the viewport and takes the highlight — the
        // literal shipping defect, and why Cmd-C copied nothing.
        (
            "InertPress",
            read_low,
            [0, 0, 0, 1, 1, 1, 3],
            "a bare modifier expresses no intent and may disturb nothing",
        ),
        // (b) An auto-repeat tick re-runs the snap — the ~30 Hz destroyer of any
        // scroll or selection made mid-hold.
        (
            "RepeatPress",
            read_low,
            [0, 0, 0, 1, 1, 1, 2],
            "the press already took custody; a tick of the same hold cannot take it twice",
        ),
        // (c) A key RELEASE disturbs. Indistinguishable from (a) and (b) in every
        // observable variable — it is a DIFFERENT control only because the record
        // carries the discriminator.
        (
            "ReleaseEvent",
            read_low,
            [0, 0, 0, 1, 1, 1, 4],
            "a key-up is not typing",
        ),
        // (d) Output snaps the reader back to live instead of re-pinning.
        (
            "OutputWhileReading",
            read_low,
            [0, 0, 1, 1, 1, 1, 5],
            "output may never take the reading position",
        ),
        // (e) Output that MISSED the selected rows takes the highlight anyway — the
        // `content_scroll_delta = i32::MAX` sentinel's whole failure mode, and the one
        // regression `OutputAtLive` can catch now that the record makes it nameable.
        (
            "OutputAtLive",
            [0, 0, 1, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 1, 5],
            "a status bar repainting rows it never selected may not destroy the highlight",
        ),
        // (f) Output that REPLACED the selected rows takes the reading position too.
        // Damaging the selection is not a licence to snap.
        (
            "OutputDamagesTheSelectedRows",
            read_low,
            [0, 0, 0, 1, 1, 1, 6],
            "damaging the selected rows is not a licence to move the view",
        ),
        // (g) An invalidation leaves the highlight naming rows that no longer exist —
        // the fail-OPEN direction, where a copy returns text from a destroyed space.
        (
            "OutputInvalidatesTheCoordinateSpace",
            read_low,
            [0, 0, 1, 1, 1, 1, 7],
            "a selection may not outlive the space that gives its rows meaning",
        ),
        // (h) Typing deselects without snapping — the one handover, half-done.
        (
            "TypingPress",
            read_low,
            [1, 1, 0, 1, 1, 1, 1],
            "typing means take me to the prompt, so it lands at live or it is not typing",
        ),
        // (i) A GUARD, not an update: `UserScroll` is disabled at the abstract
        // ceiling, so this control proves the guards bite and not merely the update
        // expressions.
        (
            "UserScroll",
            [1, 2, 0, 0, 0, 0, 0],
            [1, 2, 0, 1, 2, 0, 0],
            "UserScroll is guarded `offset <= MaxOffset - 1` and cannot fire at the cap",
        ),
        // (j) STATE CONSISTENCY: a user scroll that raises the offset but leaves the
        // tail-follower owning the viewport. `TailOwnerAtBottom` is the invariant, and
        // `UserScroll`'s own `owner = 1` is what enforces it here.
        (
            "UserScroll",
            [0, 0, 0, 0, 0, 0, 0],
            [0, 1, 0, 0, 0, 0, 0],
            "an offset above the tail means the USER owns the viewport",
        ),
        // (k) A deliberate deselect that also moves the view. `UserClear` assigns the
        // selection and nothing else — the model's claim that a deselecting click
        // leaves the reading position alone.
        (
            "UserClear",
            read_high,
            [0, 0, 0, 1, 1, 1, 0],
            "a deselecting click is not a scroll",
        ),
        // (l) THE INVARIANT TIER, and the proof that it is not decorative. This pair
        // is the EXACT update image of `InertPress` at its own `prev` — offset,
        // ownership and selection all carried through, the shadows written from the
        // pre-state, `last_event = 3` — so the refinement check that
        // `validate_transition_tiered` performs ADMITS it at both tiers, and every
        // other control above would too. The only thing wrong with it is that the
        // offset is 7, which `StateBounds` bounds at `MaxOffset` (= 2). That is not a
        // hypothetical shape: a real wheel notch moves `wheel_viewport_lines` rows,
        // so a projection that stopped being the identity on `0..=MaxOffset` lands
        // exactly here. Before the invariants were evaluated at this tier, the
        // module's claim that such a step "would be REJECTED by `StateBounds`, never
        // silently clamped" was false.
        (
            "InertPress",
            [1, 7, 0, 0, 0, 0, 0],
            [1, 7, 0, 1, 7, 0, 3],
            "a projection that left `0..=MaxOffset` must be REJECTED, not clamped",
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
        "PressCustody Tier-1 conformance: {validated} real transitions strictly validated \
         against the derived spec ({gui} through the real App scroll/wheel/gesture/press \
         seams — every press class delivered as a real key event, and the wheel driven on \
         both of `input_wheel`'s motion routes — and {} through real `Terminal::process` \
         batches), every one of them named by the ENGINE's own custody record rather than \
         by the harness, and {} negative controls (inert press disturbs, repeat disturbs, \
         release disturbs, output snaps, output takes an undamaged highlight, damaging \
         output snaps, invalidation leaves a dangling selection, typing without a snap, \
         scroll past the cap, a scroll that does not take ownership, a deselect that \
         scrolls, a projection that left `0..=MaxOffset`) all rejected — the last of those \
         rejectable only by the invariant evaluation this module performs on top of the \
         refinement check.",
        validated - gui,
        rejected.len()
    );
}

