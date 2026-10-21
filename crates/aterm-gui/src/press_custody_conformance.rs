// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 trace conformance for `PressCustody` (who owns the reading position and the
//! highlight), bound to the shipping code.
//!
//! `press_custody_model()` is model-checked in the abstract at Tier-0
//! (`aterm-models/tests/derived_ring_ty.rs`), which proves the DESIGN sound: a release,
//! an auto-repeat tick and a bare modifier each destroy nothing, typing takes the
//! viewport back to live, output re-pins instead of snapping, output that replaced
//! the selected rows may take the highlight but never the reading position, and an
//! invalidated coordinate space may not leave a selection dangling. None of that
//! ties the model to the shipping engine. This closes that gap.
//!
//! The action is not inferred from the state change: `RepeatPress`, `InertPress` and
//! `ReleaseEvent` are identical in every observable, and at live `OutputAtLive` is a
//! self-assignment. `Terminal::note_custody` records which transition fired at the site
//! that decided it (`aterm_core::terminal::custody`), and each step is a three-part
//! claim:
//!
//! 1. the record, cleared before the gesture, comes back `Some(_)`, so a seam that
//!    stopped recording, or declined because what it observed was not the transition it
//!    would name, fails rather than passing on a stale tag;
//! 2. the recorded variant is the one the step is about, so a seam that misclassifies
//!    fails even when the state change is identical. The four press classes are real
//!    `App::input` key events, so `PressKind::of` and `is_modifier_or_lock_key` answer;
//! 3. the observed `prev -> next` is admitted by the action the engine named.
//!
//! Seams driven (all sixteen actions are anchored; see each `#[refines]`):
//!
//! * `app_input::note_press_custody`, via `App::input` and `apply_press_custody`:
//!   `TypingPress`, `RepeatPress`, `InertPress`, `ReleaseEvent`.
//! * `app_mouse::note_selection_custody`, via `begin_selection` → `drag_selection` →
//!   `finish_selection`: `UserSelect`, `UserClear`.
//! * `Terminal::note_scroll_custody`, inside the aterm-core primitives that move
//!   `display_offset` for a user (`scroll_display`, `scroll_to_top`,
//!   `scroll_to_absolute_row`, `return_to_live`), via `InputEvent::ScrollView`, a real
//!   `InputEvent::Wheel` notch on both of `input_wheel`'s motion routes, and the ⌘-V /
//!   IME snap (`App::snap_to_bottom`): `UserScroll`, `UserScrollTowardLive`,
//!   `SnapToLive`.
//! * `Terminal::note_output_custody`, via real `Terminal::process` batches: the seven
//!   `Output*` actions.
//!
//! [`step`] runs each gesture inside a one-step-wide `aterm_spec::xref::StepEvidence`
//! window, and `spec_xref_gate::spec_xref_closure` requires each action's annotated
//! function to be entered by every window that drives it and by no window driving an
//! action it is not anchored for. That evidence is function entry, not branch entry:
//! each anchored function carries several actions, and `note_output_custody` runs on
//! every non-screen-switching batch, so claim 2 is the discriminator the anchors
//! cannot be.
//!
//! The abstraction function:
//!
//! * `offset` ← `Grid::display_offset()`, the identity on `0..=MaxOffset`. Steps move
//!   one row at a time (`ScrollIntent::By(1)`, one-line batches), so the offset stays
//!   in `0..=2`; bucketing a real wheel notch or PgUp would break the model's exact
//!   `offset = offset + 1`. A step outside the range is rejected by `StateBounds` in
//!   the invariant evaluation, not by the refinement check (control (l)).
//! * `selection` ← `TextSelection::has_selection()`, the same read as the engine's
//!   press-path clear gate, so `begin_selection` creates a selection and
//!   `finish_selection`'s complete arm is an idempotent `UserSelect`.
//! * `owner` ← `usize::from(offset > 0)`, derived: `TailOwnerAtBottom` with
//!   `owner <= 1` is a biconditional, and a carried flag would satisfy it by
//!   construction. Every `owner == prev_owner` conjunct is then entailed by an offset
//!   clause except one: output may not carry a live view into scrollback.
//! * `last_event` ← `CustodyTransition::last_event()` of the engine's record.
//! * `prev_*` ← the harness's projection of the pre-state, re-read at every step.
//!
//! * `app_input::note_press_custody`, through the real `App::input` key seam and the
//!   `apply_press_custody` authority under it — `TypingPress`, `RepeatPress`,
//!   `InertPress`, `ReleaseEvent`.
//! * `app_mouse::note_selection_custody`, through the real
//!   `begin_selection` → `drag_selection` → `finish_selection` gesture —
//!   `UserSelect` and `UserClear`.
//! * `Terminal::note_scroll_custody` — in aterm-core, inside the primitives that
//!   move `display_offset` for a user (`scroll_display`, `scroll_to_top`,
//!   `scroll_to_absolute_row`, `return_to_live`) — through the real `App::input` →
//!   `InputEvent::ScrollView` seam, a real `InputEvent::Wheel` notch on both of
//!   `input_wheel`'s motion routes, and the ⌘-V / IME snap (`App::snap_to_bottom`) —
//!   `UserScroll`, `UserScrollTowardLive`, `SnapToLive`.
//! * `Terminal::note_output_custody`, through real `Terminal::process` batches —
//!   `OutputAtLive`, `OutputWhileReading`, `OutputDamagesTheSelectedRows` (from BOTH
//!   ownerships) and its in-place twin, `OutputTookTheSelectionUnattributed` and its
//!   in-place twin, `OutputInvalidatesTheCoordinateSpace`.
//!
//! WHAT AN ANCHOR PROVES, AND WHAT IT DOES NOT — the CURRENT state, which is not what
//! this paragraph used to say. It used to say the gate "does NOT check that the
//! attribute sits on a function which performs the action", so "the anchors here are
//! documentation with a coverage gate attached". That was true and is no longer.
//!
//! [`step`] now runs each gesture inside a ONE-STEP-WIDE execution-evidence window
//! (`aterm_spec::xref::StepEvidence`) — the thread-local anchor-entry record is
//! cleared, the shipping seam is called, and what was entered is snapshotted before
//! `recorded()` or the post-state projection touch anything. `spec_xref_gate`'s
//! `press_custody_steps_enter_their_own_seams` then requires each action's
//! annotated function to have been ENTERED BY EVERY WINDOW THAT DRIVES IT, and — the
//! contrapositive, which is what discriminates inside a `Terminal::process` batch — to
//! be entered by NO window driving an action it is not anchored for. An anchor parked
//! on a stub fails the first; an anchor moved onto another function the same batch
//! enters fails the second, because that function also runs when the batch's siblings
//! fire.
//!
//! THE SURVIVING LIMIT, in numbers rather than adjectives. The evidence is
//! FUNCTION-ENTRY, not branch-entry. Every one of the sixteen actions sits on a
//! function carrying more than one of them — `Terminal::note_output_custody` carries
//! seven, `app_input::note_press_custody` four, `Terminal::note_scroll_custody` three,
//! `app_mouse::note_selection_custody` two — so
//! one entry marks every anchor on that function and NOTHING here separates those
//! siblings: permuting their labels changes no observable. `note_output_custody` is
//! called on EVERY non-screen-switching `Terminal::process` batch, so its entry is not
//! conditional on which output action the batch turned out to be — and that fact is now
//! REPORTED, as the set of actions whose windows entered it, rather than through a
//! "entered in every window" intersection that no ledger mixing gesture, batch and
//! eviction steps could ever satisfy. The gate derives those counts from the anchors and
//! the ledger and prints them instead of claiming more; obligation 2 proper (`project` and
//! `rust_method` resolving to live symbols) is still unresolved strings, still Phase
//! 3's `trust-ir` job. THIS module remains the part that carries the weight — and it is
//! the discriminator the anchors cannot be: the ENGINE's own custody record names each
//! step's action, so a seam that mis-classifies fails at claim 2 above.
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
//! COVERAGE NOTES — no open gap remains. The three shapes this header once listed
//! as gaps are modelled (2026-09-25) and each has a driven step below: output that
//! took the highlight for a reason that is not damage overlap is the model action
//! `OutputTookTheSelectionUnattributed` (+ its in-place twin); a damaging batch that
//! advanced no rows under a reader is `OutputDamagesTheSelectedRowsInPlace`; and a
//! return toward live that is not typing — End, a downward scroll, the ⌘-V / IME
//! snaps — is `SnapToLive` / `UserScrollTowardLive`. What remains is how two seams
//! are observed and how far the abstraction reaches:
//!
//! * `drag_selection` now records the 0 -> 1 EDGE at its own seam. Its gesture arms
//!   re-issue `start_selection` on every pointer move with no state test, so a drag
//!   DOES turn `has_selection()` on from off when something cleared it mid-gesture —
//!   `Some(g)` guards a live gesture, not a live selection. This bullet twice
//!   asserted the opposite (first that the site could not turn the selection on, then
//!   that it "requires a selection already in progress"); both were false, and the
//!   seam is recorded rather than argued about. `extend_selection_to` genuinely does
//!   bail unless the selection is `Complete`, so it produces
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
use aterm_models_misc::press_custody_model;
use aterm_spec::xref::StepEvidence;

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

/// The observed projection `[owner, offset, selection]` of a real `Terminal`, named by
/// every `#[refines(project = …)]` on the `PressCustody` seams. `owner` is derived from
/// the offset (see the module doc).
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
    /// The tag of the transition the engine last recorded.
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

/// Validate one real transition against the derived `PressCustody` spec (`Buggy` = 0).
///
/// [`aterm_spec::verify::validate_transition_tiered`] is a refinement check only: the
/// action's guard admits `prev` and `next` is its exact update image; neither tier
/// evaluates an invariant. Tier-0 checks invariants over the model's reachable states,
/// but this module feeds in observed states, which a drifted projection can push outside
/// them (an `InertPress` at `display_offset == 7` refines cleanly). So every invariant
/// is also evaluated here over `prev` and `next`; negative control (l) is the step only
/// this rejects.
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

/// Clear the custody record so the step that follows must write one (claim 1): a seam
/// that stopped recording cannot pass on the previous step's tag.
fn arm(term: &Arc<Mutex<Terminal>>) {
    let _ = term_lock(term).take_custody_transition();
}

/// Read back what the step recorded; it must be `Some(expect)`.
fn recorded(
    term: &Arc<Mutex<Terminal>>,
    expect: CustodyTransition,
    what: &str,
) -> CustodyTransition {
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

/// Drive one real step and validate it against the action the engine recorded.
///
/// The pre-state read, the gesture and the post-state read take the terminal lock
/// separately; the seams read their own before/after inside one guard. Only `gesture`
/// runs inside the `StepEvidence` window: the harness reads (`Terminal::grid`,
/// `text_selection`, `take_custody_transition`) stay outside it, so an anchor parked on
/// a harness read path cannot discharge a shipping obligation.
fn step(
    term: &Arc<Mutex<Terminal>>,
    c: &mut Press,
    expect: CustodyTransition,
    what: &str,
    gesture: impl FnOnce(),
    validated: &mut usize,
    ev: &mut StepEvidence,
) -> [i64; 7] {
    let prev = c.state(&term_lock(term));
    arm(term);
    ev.step(expect.action(), gesture);
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

/// A terminal with `history` scrollback lines fed through `Terminal::process`, and the
/// viewport at live.
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

/// Arm a completed selection over live-screen rows `first..=last`, in the coordinates
/// `App::begin_selection` computes (viewport row minus `display_offset`). Fixture
/// construction, not a claimed transition; the real gesture seam is driven in
/// [`gui_gesture_chain`].
fn arm_selection(term: &mut Terminal, first: i32, last: i32) {
    let sel = term.text_selection_mut();
    sel.start_selection(first, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(last, 5, SelectionSide::Right);
    sel.complete_selection();
    assert!(sel.has_selection(), "the fixture selection must be alive");
}

/// `OutputDamagesTheSelectedRows`: one batch that both replaces the rows the selection
/// sits on and scrolls a line in, driven from both ownerships.
///
/// The model's offset clause is `if owner == 1 { offset + 1 } else { 0 }`, so for the
/// reader the batch must advance a row. At live, the commonest real instance is a
/// status line or progress bar repainting the highlighted rows.
///
/// EL (`\e[K`) damages exactly the cursor's row: the live screen's row 0, since the
/// prologue forces the offset to 0 for the batch, so VT row 1 is the live top where the
/// fixture's selection starts.
fn damaged_rows_case(validated: &mut usize, ev: &mut StepEvidence) {
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
        ev,
    );
    assert_eq!(
        [next[0], next[1], next[2]],
        [1, 2, 0],
        "the highlight over replaced text goes, the reading position does NOT"
    );

    // (b) The view is at live, the commonest real instance. Same bytes, same action,
    // ownership unchanged at the tail.
    let term = Arc::new(Mutex::new(engine_fixture(6, 4)));
    {
        let mut t = term_lock(&term);
        arm_selection(&mut t, 0, 1);
        assert_eq!(
            t.grid().display_offset(),
            0,
            "the tail-follower owns the view"
        );
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
        ev,
    );
    assert_eq!(
        [next[0], next[1], next[2]],
        [0, 0, 0],
        "the highlight over replaced text goes; at live there is no reading position \
         to ride and the view stays where it was"
    );
}

/// The in-place and unattributed takes:
///
/// (a) EL over the selected rows under a reader, with no line feed, so the view stays
/// put: `OutputDamagesTheSelectedRowsInPlace`, `[1, 1, 0]`.
///
/// (b) The selected oldest row evicted at the history floor by output at live; no
/// damage band names it, so `post_process` fails closed:
/// `OutputTookTheSelectionUnattributed`, `[0, 0, 0]`.
///
/// (c) The same eviction under a reader parked at the top of a full two-line history;
/// the re-pin saturates: `OutputTookTheSelectionUnattributedInPlace`, `[1, 2, 0]`.
fn in_place_and_unattributed_takes(validated: &mut usize, ev: &mut StepEvidence) {
    // (a)
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
        CustodyTransition::OutputDamagesTheSelectedRowsInPlace,
        "an in-place rewrite (EL, no line feed) over the selected rows while reading",
        || {
            term_lock(&term).process(b"\x1b[1;1H\x1b[Knew text");
        },
        validated,
        ev,
    );
    assert_eq!(
        [next[0], next[1], next[2]],
        [1, 1, 0],
        "the highlight over rewritten text goes; the reader does not move"
    );

    // (b) and (c) share the two-line history floor.
    let floor = |read_top: bool| {
        let mut t = Terminal::new(4, 20);
        t.set_scrollback_line_limit(Some(2));
        for i in 0..8 {
            t.process(format!("line{i}\r\n").as_bytes());
        }
        assert_eq!(t.grid().scrollback_lines(), 2, "a two-line history floor");
        if read_top {
            t.scroll_display(2);
            assert_eq!(t.grid().display_offset(), 2, "parked at the top of history");
        }
        let sel = t.text_selection_mut();
        sel.start_selection(-2, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(-2, 5, SelectionSide::Right);
        sel.complete_selection();
        assert!(sel.has_selection(), "the oldest retained row is selected");
        Arc::new(Mutex::new(t))
    };
    for (read_top, expect, bytes, projected, what) in [
        (
            false,
            CustodyTransition::OutputTookTheSelectionUnattributed,
            &b"a\r\nb\r\nc\r\n"[..],
            [0, 0, 0],
            "output at live evicting the selected row off the history floor",
        ),
        (
            true,
            CustodyTransition::OutputTookTheSelectionUnattributedInPlace,
            &b"a\r\n"[..],
            [1, 2, 0],
            "output evicting the selected row under a reader parked at the floor",
        ),
    ] {
        let term = floor(read_top);
        let mut c = Press::new();
        let next = step(
            &term,
            &mut c,
            expect,
            what,
            || {
                term_lock(&term).process(bytes);
            },
            validated,
            ev,
        );
        assert_eq!([next[0], next[1], next[2]], projected, "{what}");
    }
}

/// Back toward live without typing. A one-row scroll down that stops short of the tail
/// is `UserScrollTowardLive` (the user still owns the view); End
/// (`ScrollIntent::Bottom`), a page down that lands at the tail, and the ⌘-V / IME snap
/// (`App::snap_to_bottom`) are `SnapToLive`. Each runs with a live highlight, and keeps
/// it.
fn toward_live_seams(validated: &mut usize, ev: &mut StepEvidence) {
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
    }
    let mut c = Press::new();
    let scroll = |app: &mut App, intent: ScrollIntent| {
        app.input(wid, InputEvent::ScrollView(intent), Source::Human);
    };
    for _ in 0..2 {
        step(
            &term,
            &mut c,
            CustodyTransition::UserScroll,
            "one-row scroll back into history",
            || scroll(&mut app, ScrollIntent::By(1)),
            validated,
            ev,
        );
    }
    step(
        &term,
        &mut c,
        CustodyTransition::UserSelect,
        "Select All, so the trip down has a highlight to keep",
        || app.select_all(),
        validated,
        ev,
    );
    let next = step(
        &term,
        &mut c,
        CustodyTransition::UserScrollTowardLive,
        "one row back down, stopping short of the tail",
        || scroll(&mut app, ScrollIntent::By(-1)),
        validated,
        ev,
    );
    assert_eq!(
        [next[0], next[1], next[2]],
        [1, 1, 1],
        "still the user's view"
    );
    for (what, gesture) in [
        (
            "End (ScrollIntent::Bottom)",
            Box::new(|app: &mut App| scroll(app, ScrollIntent::Bottom)) as Box<dyn Fn(&mut App)>,
        ),
        (
            "a page down that lands at the tail",
            Box::new(|app: &mut App| scroll(app, ScrollIntent::Down)),
        ),
        (
            "the ⌘-V / IME snap",
            Box::new(|app: &mut App| app.snap_to_bottom(wid)),
        ),
    ] {
        if c.state(&term_lock(&term))[1] == 0 {
            step(
                &term,
                &mut c,
                CustodyTransition::UserScroll,
                "one-row scroll back into history",
                || scroll(&mut app, ScrollIntent::By(1)),
                validated,
                ev,
            );
        }
        let next = step(
            &term,
            &mut c,
            CustodyTransition::SnapToLive,
            what,
            || gesture(&mut app),
            validated,
            ev,
        );
        assert_eq!(
            [next[0], next[1], next[2]],
            [0, 0, 1],
            "{what}: at live, tail-owned, and the highlight kept"
        );
    }
}

/// The GUI half: one real `App` and terminal, nine validated transitions through the
/// real scroll, gesture and press seams (the downward gestures are in
/// [`toward_live_seams`]).
fn gui_gesture_chain(validated: &mut usize, ev: &mut StepEvidence) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    // Keep the real system clipboard untouched: `finish_selection`'s copy-on-select
    // and X11 PRIMARY writes are side effects, not custody.
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

    // Projection-drift guard: a live, unselected fixture must project to the model's
    // `Init` before any transition is validated.
    let init = press_custody_model().init_state();
    let model_init: [i64; 7] = VARS.map(|v| init[v]);
    assert_eq!(
        c.state(&term_lock(&term)),
        model_init,
        "a live, unselected terminal must project to PressCustody's Init"
    );
    assert_eq!(model_init, [0; 7], "sanity: Init is the all-zero state");

    // --- UserScroll: the user takes the viewport, one row, through the real seam.
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
        ev,
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
        ev,
    );

    // --- UserSelect again: the release that completes the drag (idempotent in the
    // model: `selection = 1` from `selection == 1`).
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
        ev,
    );

    // --- All four press classes, each a real `App::input` key event, so
    // `PressKind::of` and `is_modifier_or_lock_key` classify them, not the harness.
    //
    // The order is also the priority test: `is_modifier_or_lock_key` matches the key
    // alone, so a Shift key-up is `is_release && inert_modifier` and a held Shift's
    // auto-repeat is `is_repeat && inert_modifier`. A mis-ordered `PressKind::of` files
    // both under `InertPress` and changes nothing observable.
    //
    // Each runs with the user owning the viewport and a live selection, so a press
    // class that started disturbing has both things to take.
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
                        layout_text: None,
                        event_type,
                    },
                    Source::Human,
                );
            },
            validated,
            ev,
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

    // --- UserSelect then UserClear: a press and release inside one cell is a
    // deliberate deselect, and a deliberate deselect is always allowed.
    step(
        &term,
        &mut c,
        CustodyTransition::UserSelect,
        "left press with no drag",
        || drag_from(&mut app, (2, 0)),
        validated,
        ev,
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
        ev,
    );
    assert_eq!(after_clear[2], 0, "a click without a drag deselects");
}

/// Seams that bring a selection into existence with no drag around them: Select All (a
/// menu command) and a double-click's press. The third, a search jump, needs find-bar
/// match state this module cannot build;
/// `app_search::tests::a_search_jump_records_who_took_the_selection` drives it.
fn silent_select_seams(validated: &mut usize, ev: &mut StepEvidence) {
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
        ev,
    );
    assert_eq!(
        after_all[2], 1,
        "Select All really did bring a selection into existence"
    );

    // A double-click press: `control::select_word` completes a selection and
    // `arm_gesture_drag` only arms the release.
    let after_word = step(
        &term,
        &mut c,
        CustodyTransition::UserSelect,
        "the double-click PRESS that word-selects, before any release",
        || app.select_word_click(wid, 0, 1),
        validated,
        ev,
    );
    assert_eq!(
        after_word[2], 1,
        "the word-click press leaves a completed selection behind"
    );
}

/// The wheel, driven through both of `input_wheel`'s motion routes. The recorder lives
/// in the `Terminal` primitives that raise the offset, so every route records; these two
/// are the ones the motion policy switches between:
///
/// * instant (`motion = reduced`, OS Reduce Motion, or an unfocused window):
///   `scroll_wheel_animated`'s early arm calls `scroll_display` under its own lock.
/// * glide + settle: the ~180 ms ease banks the target and
///   `settle_scroll_motion_at_target` lands it in whole rows, on the terminal the glide
///   pinned rather than whatever pane is frontmost now.
///
/// One fixture each: both start from live, and a chain of notches would saturate at
/// `MaxOffset = 2`.
fn wheel_seams(validated: &mut usize, ev: &mut StepEvidence) {
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
                        // ONE row, so the abstract step is the model's exact
                        // `offset + 1`: a precise device's row (`detent: false`) is
                        // one row on every platform. A real notch moves
                        // `wheel_viewport_lines` rows, which is a granularity gap the
                        // record deliberately does not carry (it says the user took
                        // the viewport, not how far).
                        dir: aterm_types::mouse::WheelDir::Up,
                        lines: 1,
                        detent: false,
                        row: 0,
                        col: 0,
                        mods: 0,
                        px_off: crate::input::PixelOffset::CELL_ORIGIN,
                    },
                    Source::Human,
                );
                // The eased arm banks a whole-row target; settling it finishes the
                // same gesture, not a second user action.
                app.settle_scroll_motion_at_target(wid, std::time::Instant::now());
            },
            validated,
            ev,
        );
        assert_eq!(
            [next[0], next[1]],
            [1, 1],
            "{what}: the wheel really moved the engine's viewport, not just the record"
        );
    }
}

/// The output half: real `Terminal::process` batches, each on its own fixture so the
/// bounded offset never has to saturate.
fn output_batches(validated: &mut usize, ev: &mut StepEvidence) {
    // ---- OutputAtLive, with a live selection the batch does not touch. ----
    //
    // An identity transition on the observables (at live `offset = 0` is a
    // self-assignment). A highlight destroyed by a batch that did not damage it is
    // caught by the named assertion below and by `recorded()`: such a batch records
    // `OutputTookTheSelectionUnattributed` or `OutputDamagesTheSelectedRows`, never
    // `OutputAtLive`. `OutputSparesAnUndamagedSelection` is evaluated too, but has no
    // reachable counterexample under this recorder.
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
            ev,
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
    // Armed with a live-screen selection while the view is a row back in history (a
    // `tail -f` under a scrolled-back reader), so the `selection` frame is not a
    // trivial 0 -> 0.
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
            ev,
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
            ev,
        );
        assert_eq!(
            [next[0], next[1], next[2]],
            [0, 0, 0],
            "the space the offset named is gone, so the viewport goes back and the \
             selection cannot outlive it"
        );
    }

    // ---- RIS: the same action by another route. ED 3 records `SelectionDamage::All`
    // and lets the drain clear; RIS clears directly in `Terminal::reset`. ----
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
            ev,
        );
        assert_eq!(
            [next[0], next[1], next[2]],
            [0, 0, 0],
            "same law, other route"
        );
    }
}

/// The `PressCustody` Tier-1 body, run by `spec_xref_gate` with its execution evidence:
/// the real engine and GUI seams are driven and checked, and each step must enter its own
/// action's anchored function.
pub(crate) fn run_conformance() -> StepEvidence {
    let mut validated = 0usize;
    // Each step runs its seam in a one-step-wide evidence window, so the gate can
    // require an action's anchored function to be entered by that action's own step.
    let mut ev = StepEvidence::new("PressCustody");

    gui_gesture_chain(&mut validated, &mut ev);
    wheel_seams(&mut validated, &mut ev);
    silent_select_seams(&mut validated, &mut ev);
    toward_live_seams(&mut validated, &mut ev);
    // Steps so far came through the App's input seams, the rest through the engine's
    // output path; counted, not hardcoded, so the summary line stays true.
    let gui = validated;
    output_batches(&mut validated, &mut ev);
    damaged_rows_case(&mut validated, &mut ev);
    in_place_and_unattributed_takes(&mut validated, &mut ev);

    // ---- Negative controls: regressions the model must refuse, each evaluated
    // against the named action's own guard and updates, so the passes above are not
    // vacuous. ----
    let read_low = [1, 1, 1, 0, 0, 0, 0];
    let read_high = [1, 1, 1, 1, 1, 1, 0];

    let rejected: &[(&str, [i64; 7], [i64; 7], &str)] = &[
        // (a) A bare modifier snaps the viewport and takes the highlight, so Cmd-C
        // would copy nothing.
        (
            "InertPress",
            read_low,
            [0, 0, 0, 1, 1, 1, 3],
            "a bare modifier expresses no intent and may disturb nothing",
        ),
        // (b) An auto-repeat tick (~30 Hz) re-runs the snap, destroying any scroll or
        // selection made mid-hold.
        (
            "RepeatPress",
            read_low,
            [0, 0, 0, 1, 1, 1, 2],
            "the press already took custody; a tick of the same hold cannot take it twice",
        ),
        // (c) A key release disturbs. Observably identical to (a) and (b); only the
        // record tells them apart.
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
        // (e) Output that missed the selected rows takes the highlight anyway (the
        // failure mode of the `content_scroll_delta == i32::MAX` sentinel).
        (
            "OutputAtLive",
            [0, 0, 1, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 1, 5],
            "a status bar repainting rows it never selected may not destroy the highlight",
        ),
        // (f) Output that replaced the selected rows takes the reading position too.
        // Damaging the selection is not a licence to snap.
        (
            "OutputDamagesTheSelectedRows",
            read_low,
            [0, 0, 0, 1, 1, 1, 6],
            "damaging the selected rows is not a licence to move the view",
        ),
        // (g) An invalidation leaves the highlight naming rows that no longer exist,
        // so a copy would return text from a destroyed space.
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
        // (f2) The in-place twins and the unattributed take snap the reader.
        (
            "OutputDamagesTheSelectedRowsInPlace",
            read_low,
            [0, 0, 0, 1, 1, 1, 6],
            "an in-place rewrite is not a licence to move the view",
        ),
        (
            "OutputTookTheSelectionUnattributed",
            read_low,
            [0, 0, 0, 1, 1, 1, 6],
            "a fail-closed take is not a licence to move the view",
        ),
        (
            "OutputTookTheSelectionUnattributedInPlace",
            read_low,
            [0, 0, 0, 1, 1, 1, 6],
            "a fail-closed take in place is not a licence to move the view",
        ),
        // (f3) A return toward live that deselects — typing's job, not End's.
        (
            "SnapToLive",
            read_low,
            [0, 0, 0, 1, 1, 1, 8],
            "End or a ⌘-V snap brings the view home and leaves the highlight alone",
        ),
        (
            "UserScrollTowardLive",
            [1, 2, 1, 0, 0, 0, 0],
            [1, 1, 0, 1, 2, 1, 8],
            "a scroll down is not a deselect",
        ),
        // (i) A guard, not an update: `UserScroll` is disabled at the abstract ceiling.
        (
            "UserScroll",
            [1, 2, 0, 0, 0, 0, 0],
            [1, 2, 0, 1, 2, 0, 0],
            "UserScroll is guarded `offset <= MaxOffset - 1` and cannot fire at the cap",
        ),
        // (j) A user scroll that raises the offset but leaves the tail-follower owning
        // the view (`TailOwnerAtBottom`; `UserScroll`'s `owner = 1` enforces it).
        (
            "UserScroll",
            [0, 0, 0, 0, 0, 0, 0],
            [0, 1, 0, 0, 0, 0, 0],
            "an offset above the tail means the USER owns the viewport",
        ),
        // (k) A deselecting click that also moves the view: `UserClear` assigns only
        // the selection.
        (
            "UserClear",
            read_high,
            [0, 0, 0, 1, 1, 1, 0],
            "a deselecting click is not a scroll",
        ),
        // (l) The exact update image of `InertPress` at its own `prev`, so the
        // refinement check admits it at both tiers; only `StateBounds` (`offset <=
        // MaxOffset` = 2) rejects the offset of 7 a drifted projection would produce.
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
         output snaps — in place and unattributed too — a return to live that deselects, \
         invalidation leaves a dangling selection, typing without a snap, \
         scroll past the cap, a scroll that does not take ownership, a deselect that \
         scrolls, a projection that left `0..=MaxOffset`) all rejected — the last of those \
         rejectable only by the invariant evaluation this module performs on top of the \
         refinement check.",
        validated - gui,
        rejected.len()
    );

    ev
}
