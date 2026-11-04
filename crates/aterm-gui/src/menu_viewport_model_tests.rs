// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance of the pure viewport helper, not App selection or input authority.

use crate::menu_viewport::MenuViewport;
use aterm_models_gui::menu_viewport_model;
use aterm_spec::{derive::Model, interp::State};
use std::collections::{BTreeSet, VecDeque};

#[derive(Clone)]
struct Concrete {
    viewport: MenuViewport,
    total: usize,
    shown: usize,
    target: Option<usize>,
    revealed: bool,
    cell: usize,
    mapped: Option<usize>,
    hit: bool,
}

impl Concrete {
    fn new(model: &Model) -> Self {
        let initial = model.init_state();
        Self {
            viewport: MenuViewport::default(),
            total: usize::try_from(initial["total"]).unwrap(),
            shown: usize::try_from(initial["shown"]).unwrap(),
            target: None,
            revealed: false,
            cell: 0,
            mapped: None,
            hit: false,
        }
    }

    fn withdraw_intent(&mut self) {
        self.target = None;
        self.revealed = false;
        self.hit = false;
    }

    fn apply(&mut self, action: &str, rows: usize) {
        match action {
            "RevealFirst" | "RevealLast" => {
                let target = if action == "RevealFirst" {
                    0
                } else {
                    self.total - 1
                };
                self.viewport.reveal(self.total, self.shown, Some(target));
                self.target = Some(target);
                self.revealed = self.shown > 0;
                self.hit = false;
            }
            "ScrollDown" | "ScrollUp" => {
                let delta = if action == "ScrollDown" { 1 } else { -1 };
                self.viewport.scroll(self.total, self.shown, delta);
                self.withdraw_intent();
            }
            "ResizeShort" | "ResizeTall" | "ResizeZero" => {
                self.shown = match action {
                    "ResizeShort" => 1,
                    "ResizeTall" => 3,
                    _ => 0,
                };
                self.viewport.reveal(self.total, self.shown, None);
                self.withdraw_intent();
            }
            "Empty" | "Refill" => {
                self.total = if action == "Empty" { 0 } else { rows };
                self.viewport.reveal(self.total, self.shown, None);
                self.withdraw_intent();
            }
            "HitFirst" | "HitSecond" | "HitOutside" => {
                self.cell = match action {
                    "HitFirst" => 0,
                    "HitSecond" => 1,
                    _ => self.shown,
                };
                self.mapped = self.viewport.absolute(self.total, self.shown, self.cell);
                self.hit = true;
            }
            _ => unreachable!("unknown viewport action {action}"),
        }
    }

    fn project(&self, model: &Model) -> State {
        let absent = model.init_state()["mapped"];
        let mut state = model.init_state();
        for (name, value) in [
            ("total", self.total),
            ("shown", self.shown),
            ("first", self.viewport.first()),
            ("cell", self.cell),
        ] {
            state.insert(name, i64::try_from(value).unwrap());
        }
        state.insert(
            "target",
            self.target.map_or(absent, |n| i64::try_from(n).unwrap()),
        );
        state.insert(
            "mapped",
            self.mapped.map_or(absent, |n| i64::try_from(n).unwrap()),
        );
        state.insert("revealed", i64::from(self.revealed));
        state.insert("hit", i64::from(self.hit));
        state
    }

    fn check_range(&self, state: &State) {
        let range = self.viewport.range(self.total, self.shown);
        let first = usize::try_from(state["first"]).unwrap();
        assert_eq!(range.start, first);
        assert_eq!(range.end, self.total.min(first + self.shown));
        for (visible, logical) in range.enumerate() {
            assert_eq!(
                self.viewport.absolute(self.total, self.shown, visible),
                Some(logical)
            );
        }
        assert_eq!(
            self.viewport.absolute(self.total, self.shown, self.shown),
            None
        );
    }
}

#[test]
fn the_actual_viewport_refines_every_bounded_reveal_scroll_resize_and_hit() {
    let model = menu_viewport_model();
    let initial = model.init_state();
    let rows = usize::try_from(initial["total"]).unwrap();
    let concrete = Concrete::new(&model);
    assert_eq!(concrete.project(&model), initial);
    let mut seen = BTreeSet::from([initial.clone()]);
    let mut queue = VecDeque::from([(initial, concrete)]);
    let mut exercised = BTreeSet::new();
    while let Some((before, concrete)) = queue.pop_front() {
        for action in &model.actions {
            let mut expected = before.clone();
            if !model.fire(action.name, &mut expected) {
                continue;
            }
            let mut actual = concrete.clone();
            actual.apply(action.name, rows);
            let projected = actual.project(&model);
            assert_eq!(projected, expected, "{} from {before:?}", action.name);
            actual.check_range(&projected);
            for law in &model.invariants {
                assert!(
                    model.check_invariant(law.name, &projected),
                    "{}: {projected:?}",
                    law.name
                );
            }
            exercised.insert(action.name);
            if seen.insert(projected.clone()) {
                queue.push_back((projected, actual));
            }
        }
    }
    assert_eq!(exercised.len(), model.actions.len());
}

#[test]
fn the_former_prefix_and_an_offset_free_hit_are_rejected_by_the_same_laws() {
    let model = menu_viewport_model();
    let mut actual = Concrete::new(&model);
    actual.apply("RevealLast", actual.total);
    let current = actual.project(&model);
    assert!(model.check_invariant("RequestedRowIsRevealed", &current));

    // The former popup painted 0..shown regardless of its requested logical selection.
    let former_prefix = 0..actual.total.min(actual.shown);
    let mut hidden = current;
    hidden.insert("first", i64::try_from(former_prefix.start).unwrap());
    assert!(!model.check_invariant("RequestedRowIsRevealed", &hidden));

    actual.apply("HitFirst", actual.total);
    let current = actual.project(&model);
    assert!(actual.viewport.first() > 0);
    assert!(model.check_invariant("HitNamesVisibleLogicalRow", &current));
    // The former row_at used the card-local cell as the logical source index.
    let mut wrong_account = current;
    wrong_account.insert("mapped", i64::try_from(actual.cell).unwrap());
    assert!(!model.check_invariant("HitNamesVisibleLogicalRow", &wrong_account));
}
