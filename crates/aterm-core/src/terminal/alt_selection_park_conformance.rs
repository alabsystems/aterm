// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for `AltSelectionPark`
//! (`aterm_spec::derive::alt_selection_park_model`): the alt-screen selection
//! park, driven on the genuine [`Terminal`].
//!
//! Every step is the shipping path. A selection gesture goes through
//! [`Terminal::text_selection_mut`], exactly as a host's drag does; the screen
//! switch is real `?1049h` / `?1049l` bytes, parked and restored by the two
//! `mem::take`s at the top of `post_process`; and each destroyer is its own
//! entry point, driven from BOTH screens — [`Terminal::clear_scrollback`] and a
//! width [`Terminal::resize`] (the model's `Wholesale`), a direct
//! [`Terminal::reset`], byte-stream RIS alone, and RIS then `?1049h` in ONE batch
//! (the model's `Reset`, the last as `Reset` then `Enter`), and
//! [`Terminal::restore_checkpoint`] of a main-screen and of an alt-screen
//! checkpoint (`RestoreMain` / `RestoreAlt`).
//!
//! The projection reads the terminal, never the model: `on_alt` is
//! [`Terminal::is_alternate_screen`], `live_sel` the live selection, and
//! `parked_sel` the private parked slot itself — which is why this bind lives
//! inside the crate. `last_event` is the one harness-supplied variable: it
//! names which KIND of step was just driven, a history the terminal does not
//! (and should not) keep.
//!
//! The walk covers the model's WHOLE reachable space: at every model state it
//! replays a real trace that reached it, drives every real step the model
//! enables there, and requires the projection to be exactly the model's one
//! successor for that step. A pass is never vacuous — the negative control
//! injects each of the model's own `Buggy = 1` members (the symmetric SWAP, and
//! the destroyer that forgets the parked slot, at each of the three destroyer
//! shapes separately) into the same real trace and requires the healthy model to
//! reject it at that member's own action.
//!
//! Why RIS is driven twice. On its own, RIS on the alt screen returns to main
//! through the ordinary restore, which takes the parked slot anyway, so the RIS
//! clear is not what empties it there. RIS then a re-entry in the same batch is
//! the shape where neither the park nor the restore runs — the case that clear
//! exists for (`processing.rs`) — and it is the step whose projection moves if
//! the clear goes.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_spec::derive::{Model, alt_selection_park_model};
use aterm_spec::interp::{State, admits, bmc, with_buggy};

use super::{Terminal, TerminalCheckpoint};
use crate::selection::{SelectionSide, SelectionType, TextSelection};

const ROWS: u16 = 6;
const COLS: u16 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Select,
    Deselect,
    Enter,
    Leave,
    ClearScrollback,
    WidthResize,
    Reset,
    Ris,
    RisReenter,
    RestoreMain,
    RestoreAlt,
}

/// Which destroyer shape a step is — what the forgetful member corrupts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    InPlace,
    Reset,
    Restore,
}

impl Step {
    const ALL: [Self; 11] = [
        Self::Select,
        Self::Deselect,
        Self::Enter,
        Self::Leave,
        Self::ClearScrollback,
        Self::WidthResize,
        Self::Reset,
        Self::Ris,
        Self::RisReenter,
        Self::RestoreMain,
        Self::RestoreAlt,
    ];

    /// The model actions this real step IS, in order. Every step is one action
    /// except RIS then a re-entry in the SAME batch: that is `Reset` then
    /// `Enter` with no `post_process` between them.
    const fn actions(self) -> &'static [&'static str] {
        match self {
            Self::Select => &["Select"],
            Self::Deselect => &["Deselect"],
            Self::Enter => &["Enter"],
            Self::Leave => &["Leave"],
            Self::ClearScrollback | Self::WidthResize => &["Wholesale"],
            Self::Reset | Self::Ris => &["Reset"],
            Self::RisReenter => &["Reset", "Enter"],
            Self::RestoreMain => &["RestoreMain"],
            Self::RestoreAlt => &["RestoreAlt"],
        }
    }

    /// The model's `last_event` code for the last action of this step.
    const fn last_event(self) -> i64 {
        match self {
            Self::Select | Self::Deselect => 0,
            Self::Enter | Self::RisReenter => 1,
            Self::ClearScrollback | Self::WidthResize => 2,
            Self::Leave => 3,
            Self::Reset | Self::Ris => 4,
            Self::RestoreMain | Self::RestoreAlt => 5,
        }
    }

    const fn shape(self) -> Option<Shape> {
        match self {
            Self::ClearScrollback | Self::WidthResize => Some(Shape::InPlace),
            Self::Reset | Self::Ris | Self::RisReenter => Some(Shape::Reset),
            Self::RestoreMain | Self::RestoreAlt => Some(Shape::Restore),
            Self::Select | Self::Deselect | Self::Enter | Self::Leave => None,
        }
    }
}

/// A main screen with history under it, so the width resize has something to
/// rewrap and `clear_scrollback` has something to erase.
fn fixture() -> Terminal {
    let mut term = Terminal::new(ROWS, COLS);
    for i in 0..40 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    term
}

/// The two sessions a restore can adopt: one on the main screen, one on the
/// alternate screen.
struct Checkpoints {
    main: TerminalCheckpoint,
    alt: TerminalCheckpoint,
}

impl Checkpoints {
    fn new() -> Self {
        let mut alt = fixture();
        alt.process(b"\x1b[?1049hfull-screen app");
        Self {
            main: fixture().checkpoint(),
            alt: alt.checkpoint(),
        }
    }
}

/// Drive one real step. On main a selection is made in HISTORY when there is
/// any (the reported shape: read back, highlight, run a pager); the alt buffer
/// has none, so there it is a live row.
fn drive(term: &mut Terminal, step: Step, checkpoints: &Checkpoints) {
    match step {
        Step::Select => {
            let row = if !term.is_alternate_screen() && term.grid().scrollback_lines() >= 3 {
                -3
            } else {
                1
            };
            let sel = term.text_selection_mut();
            sel.start_selection(row, 0, SelectionSide::Left, SelectionType::Simple);
            sel.update_selection(row, 4, SelectionSide::Right);
            sel.complete_selection();
        }
        Step::Deselect => term.text_selection_mut().clear(),
        Step::Enter => term.process(b"\x1b[?1049h"),
        Step::Leave => term.process(b"\x1b[?1049l"),
        Step::ClearScrollback => term.clear_scrollback(),
        Step::WidthResize => {
            let cols = if term.cols() == COLS { COLS - 1 } else { COLS };
            term.resize(ROWS, cols);
        }
        Step::Reset => term.reset(),
        Step::Ris => term.process(b"\x1bc"),
        Step::RisReenter => term.process(b"\x1bc\x1b[?1049h"),
        Step::RestoreMain => term.restore_checkpoint(&checkpoints.main),
        Step::RestoreAlt => term.restore_checkpoint(&checkpoints.alt),
    }
}

/// Both slots as they stood before a step — what a mutant needs to undo.
struct Before {
    live: TextSelection,
    parked: TextSelection,
}

/// A fault injected AFTER the real step, or none.
type Patch = fn(&mut Terminal, Step, Before);

/// The healthy code, untouched.
fn no_patch(_: &mut Terminal, _: Step, _: Before) {}

/// The model's first `Buggy = 1` member, as the net effect it would have on the
/// real terminal: the park and the restore become a symmetric SWAP — whatever
/// was parked is handed to the live slot on the way in, and the alt screen's
/// own selection lands in the parked slot on the way out.
fn swap_patch(term: &mut Terminal, step: Step, before: Before) {
    match step {
        Step::Enter => term.text_selection = before.parked,
        Step::Leave => term.parked_text_selection = before.live,
        _ => {}
    }
}

/// The second member: a destroyer of shape `shape` that forgets the parked
/// slot. One patch per shape, so each is rejected at its OWN action rather than
/// at whichever destroyer the walk happens to reach first.
fn forget_parked(term: &mut Terminal, step: Step, before: Before, shape: Shape) {
    if step.shape() == Some(shape) {
        term.parked_text_selection = before.parked;
    }
}

fn forgetful_in_place(term: &mut Terminal, step: Step, before: Before) {
    forget_parked(term, step, before, Shape::InPlace);
}

fn forgetful_reset(term: &mut Terminal, step: Step, before: Before) {
    forget_parked(term, step, before, Shape::Reset);
}

fn forgetful_restore(term: &mut Terminal, step: Step, before: Before) {
    forget_parked(term, step, before, Shape::Restore);
}

fn apply(term: &mut Terminal, step: Step, patch: Patch, checkpoints: &Checkpoints) {
    let before = Before {
        live: term.text_selection.clone(),
        parked: term.parked_text_selection.clone(),
    };
    drive(term, step, checkpoints);
    patch(term, step, before);
}

fn replay(path: &[Step], patch: Patch, checkpoints: &Checkpoints) -> Terminal {
    let mut term = fixture();
    for &step in path {
        apply(&mut term, step, patch, checkpoints);
    }
    term
}

fn project(term: &Terminal, last_event: i64) -> State {
    BTreeMap::from([
        ("on_alt", i64::from(term.is_alternate_screen())),
        ("live_sel", i64::from(term.text_selection().has_selection())),
        (
            "parked_sel",
            i64::from(term.parked_text_selection.has_selection()),
        ),
        ("last_event", last_event),
    ])
}

/// Every state `model` can reach from `state` by firing `actions` in order.
fn successors(model: &Model, state: &State, actions: &[&str]) -> Vec<State> {
    let mut states = vec![state.clone()];
    for action in actions {
        states = states
            .iter()
            .flat_map(|s| model.successors(action, s))
            .collect();
    }
    states
}

/// What the walk covered, so a pass can be checked for non-vacuity.
struct Coverage {
    states: usize,
    edges: usize,
    actions: BTreeSet<&'static str>,
    /// Destroyers driven from a state with a selection PARKED — the only place
    /// the forgetful member can show.
    destroyers_over_a_park: BTreeSet<&'static str>,
}

/// Walk `model`'s whole reachable space on the real terminal (with `patch`
/// injected after every step). `Err` names the first real transition the model
/// does not admit — the real trace, the step, its actions, and both
/// projections.
fn walk(model: &Model, patch: Patch) -> Result<Coverage, String> {
    let checkpoints = Checkpoints::new();
    let init = model.init_state();
    let start = project(&fixture(), 0);
    if start != init {
        return Err(format!(
            "fixture projects to {start:?}, model starts at {init:?}"
        ));
    }
    let mut seen: BTreeSet<State> = BTreeSet::from([init.clone()]);
    let mut queue: VecDeque<(State, Vec<Step>)> = VecDeque::from([(init, Vec::new())]);
    let mut coverage = Coverage {
        states: 0,
        edges: 0,
        actions: BTreeSet::new(),
        destroyers_over_a_park: BTreeSet::new(),
    };
    while let Some((state, path)) = queue.pop_front() {
        coverage.states += 1;
        for step in Step::ALL {
            let actions = step.actions();
            let expected = successors(model, &state, actions);
            if expected.is_empty() {
                continue;
            }
            let mut term = replay(&path, patch, &checkpoints);
            apply(&mut term, step, patch, &checkpoints);
            let after = project(&term, step.last_event());
            // A one-action step must also be admitted AS that action — not merely
            // land on a state some other action would have produced.
            let admitted = match actions {
                [action] => admits(model, &state, &after) == Some(*action),
                _ => true,
            };
            if expected.as_slice() != std::slice::from_ref(&after) || !admitted {
                return Err(format!(
                    "{}: real {step:?} after {path:?} took {state:?} to {after:?}, \
                     the model's {} to {expected:?}",
                    model.name,
                    actions.join(" then ")
                ));
            }
            // No separate invariant check: every real state is now a state the
            // model reaches, and `bmc` proves the invariants over all of those.
            coverage.edges += 1;
            coverage.actions.extend(actions.iter().copied());
            if step.shape().is_some() && state["parked_sel"] == 1 {
                coverage.destroyers_over_a_park.insert(actions[0]);
            }
            if seen.insert(after.clone()) {
                let mut next = path.clone();
                next.push(step);
                queue.push_back((after, next));
            }
        }
    }
    Ok(coverage)
}

#[test]
fn the_real_park_conforms_to_the_model_over_its_whole_reachable_space() {
    let model = alt_selection_park_model();
    let coverage = walk(&model, no_patch).unwrap_or_else(|why| panic!("{why}"));
    let reachable = bmc(&model).expect("the healthy model holds");
    assert_eq!(
        coverage.states, reachable,
        "the real trace must reach every state the model can"
    );
    assert_eq!(
        coverage.actions,
        model
            .actions
            .iter()
            .map(|a| a.name)
            .collect::<BTreeSet<_>>(),
        "every model action must be driven on the real terminal"
    );
    assert_eq!(
        coverage.destroyers_over_a_park,
        BTreeSet::from(["Wholesale", "Reset", "RestoreMain", "RestoreAlt"]),
        "every destroyer must be driven over a parked selection — the one state \
         where forgetting the slot is visible"
    );
    assert!(
        coverage.edges > coverage.states,
        "every state is left more than one way"
    );
}

/// NEGATIVE CONTROL. The same walk over the same real terminal, with each of
/// the model's `Buggy = 1` members injected after the real step it corrupts:
/// the healthy model must reject every one, AT that member's own action. And
/// the shipped park is in turn rejected by the buggy model, so the bind tells
/// the two apart in both directions.
///
/// The forgetful destroyer is only visible because the projection reads the
/// parked SLOT. Observed from outside at the next restore it would pass: the
/// erase that `clear_scrollback` runs also records `SelectionDamage::All` on the
/// parked grid, and the exit batch drains it right after the restore, retiring
/// the stale selection on arrival. That second enforcer is real and welcome,
/// but it is not the clear this bind is about — so the slot is what is read.
#[test]
fn the_swap_and_each_forgetful_destroyer_are_rejected() {
    let healthy = alt_selection_park_model();
    for (member, patch, at) in [
        ("swap", swap_patch as Patch, ["Enter", "Leave"].as_slice()),
        (
            "forgetful in-place destroyer",
            forgetful_in_place,
            ["Wholesale"].as_slice(),
        ),
        ("forgetful reset", forgetful_reset, ["Reset "].as_slice()),
        (
            "forgetful checkpoint restore",
            forgetful_restore,
            ["RestoreMain", "RestoreAlt"].as_slice(),
        ),
    ] {
        let rejected = walk(&healthy, patch)
            .err()
            .unwrap_or_else(|| panic!("the healthy model must reject the {member}"));
        assert!(
            at.iter()
                .any(|action| rejected.contains(&format!("the model's {action}"))),
            "the {member} must be rejected at its own action ({at:?}): {rejected}"
        );
    }

    let shipped = walk(&with_buggy(&healthy, 1), no_patch)
        .err()
        .expect("the shipped park must not conform to the Buggy = 1 model");
    assert!(shipped.contains("the model's"), "{shipped}");
}
