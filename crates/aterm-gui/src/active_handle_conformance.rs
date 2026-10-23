// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance tests binding `ActiveHandle` to `aterm_models_gui::active_handle_model`.

use std::collections::BTreeMap;

use aterm_models_gui::active_handle_model;
use aterm_spec::{interp, verify};

use crate::{App, WindowId, stub_session};

type State = BTreeMap<&'static str, i64>;

const OVERRIDES: &[(&str, i64)] = &[("MaxId", 1_000_000)];

fn truth(app: &App) -> Option<u64> {
    let front = app.frontmost_window?;
    let tab = app.windows.get(&front)?.tab_set.active()?;
    app.view_store
        .get(tab.focus)
        .copied()
        .and_then(crate::tab_model::View::terminal_session)
}

fn handle(app: &App) -> Option<u64> {
    app.active_handle
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|active| active.id)
}

struct Projection {
    real_truth: Option<u64>,
    real_handle: Option<u64>,
    state: State,
}

impl Projection {
    fn start(app: &App) -> Self {
        let model = active_handle_model();
        assert_eq!(
            handle(app),
            truth(app),
            "a fresh App's handle names the front session"
        );
        Self {
            real_truth: truth(app),
            real_handle: handle(app),
            state: model.init_state(),
        }
    }

    fn after(&mut self, app: &App) -> Option<State> {
        let (t, h) = (truth(app), handle(app));
        if t == self.real_truth {
            assert_eq!(h, t, "a step that kept the front session moved the handle");
            self.real_handle = h;
            return None;
        }
        let fresh = self.state["next"];
        let handle_id = if h == t {
            fresh
        } else if h == self.real_handle {
            self.state["handle"]
        } else {
            -1 // moved, but not to the truth: no action produces this
        };
        let next: State = [("truth", fresh), ("handle", handle_id), ("next", fresh + 1)]
            .into_iter()
            .collect();
        self.real_truth = t;
        self.real_handle = h;
        Some(next)
    }
}

fn step(
    app: &mut App,
    p: &mut Projection,
    action: &str,
    what: &str,
    drive: impl FnOnce(&mut App),
) -> bool {
    drive(app);
    let Some(next) = p.after(app) else {
        return false;
    };
    let (ok, why) = verify::validate_transition_tiered(
        &active_handle_model(),
        OVERRIDES,
        &p.state,
        &next,
        Some(action),
        "ActiveHandle conformance",
    );
    assert!(
        ok,
        "{what}: {:?} -> {next:?} is not {action} (real truth {:?}, handle {:?})\n{why}",
        p.state, p.real_truth, p.real_handle
    );
    assert!(
        active_handle_model().check_invariant("HandleMirrorsFront", &next),
        "{what}: the global handle no longer names the front session"
    );
    p.state = next;
    true
}

type Step = (&'static str, &'static str, fn(&mut App));

const STEPS: &[Step] = &[
    ("SwitchActive", "append tab", |a| {
        let sid = a.next_session_id;
        a.install_new_tab(WindowId(0), stub_session(sid));
    }),
    ("SwitchActive", "append tab", |a| {
        let sid = a.next_session_id;
        a.install_new_tab(WindowId(0), stub_session(sid));
    }),
    ("SwitchActive", "switch to tab 0", |a| {
        a.switch_tab_in(WindowId(0), 0)
    }),
    ("SwitchActive", "cycle forward", |a| a.cycle_tab(true)),
    ("SwitchActive", "move a tab", |a| {
        a.move_tab(WindowId(0), 0, 2)
    }),
    ("CloseOrNewFront", "split", |a| {
        let sid = a.next_session_id;
        a.install_split_pane(
            WindowId(0),
            crate::pane::SplitDir::Vertical,
            stub_session(sid),
        )
        .expect("the split installs");
    }),
    ("CloseOrNewFront", "focus left", |a| {
        assert!(a.focus_pane_in(WindowId(0), crate::pane::FocusDir::Left));
    }),
    ("CloseOrNewFront", "focus right", |a| {
        assert!(a.focus_pane_in(WindowId(0), crate::pane::FocusDir::Right));
    }),
    ("CloseOrNewFront", "close focused pane", |a| {
        a.close_active_tab();
    }),
    ("CloseOrNewFront", "close tab", |a| {
        a.close_active_tab();
    }),
    ("CloseOrNewFront", "active pane EOF", |a| {
        let sid = truth(a).expect("front terminal");
        assert!(!a.close_session(WindowId(0), sid), "not the last pane");
    }),
    ("CloseOrNewFront", "new window", |a| {
        let sid = a.next_session_id;
        a.insert_logical_window(stub_session(sid), 24, 80);
        assert_ne!(a.frontmost_window, Some(WindowId(0)));
    }),
    ("SwitchActive", "background tab append", |a| {
        let sid = a.next_session_id;
        a.install_new_tab(WindowId(0), stub_session(sid));
    }),
    ("CloseOrNewFront", "close front window", |a| {
        let front = a.frontmost_window.expect("front window");
        a.close_window_logical(front);
        assert_eq!(a.frontmost_window, Some(WindowId(0)));
    }),
];

#[test]
fn real_front_session_moves_keep_the_handle_conforming() {
    let mut app = App::headless_for_test();
    let mut p = Projection::start(&app);
    let mut moved = 0usize;
    for &(action, what, drive) in STEPS {
        moved += usize::from(step(&mut app, &mut p, action, what, drive));
    }
    assert!(moved >= 10, "the run moved the front session {moved} times");
    assert!(
        STEPS.len() - moved >= 2,
        "steps that keep the front session were exercised"
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn a_front_move_that_leaves_the_handle_behind_is_the_buggy_step() {
    let mut app = App::headless_for_test();
    let mut p = Projection::start(&app);
    let w0 = WindowId(0);
    let before = p.state.clone();
    let sid = app.next_session_id;
    app.install_new_tab(w0, stub_session(sid));
    let real = p.after(&app).expect("the append moved the front session");
    let mut swallowed = real.clone();
    swallowed.insert("handle", before["handle"]);

    let model = active_handle_model();
    let (ok, _) = verify::validate_transition_tiered(
        &model,
        OVERRIDES,
        &before,
        &swallowed,
        Some("CloseOrNewFront"),
        "ActiveHandle negative control",
    );
    assert!(!ok, "the committed model must reject a stale handle");
    assert!(
        interp::with_consts(&interp::with_buggy(&model, 1), OVERRIDES)
            .successors("CloseOrNewFront", &before)
            .contains(&swallowed),
        "Buggy = 1 admits exactly this swallow"
    );
    assert!(!model.check_invariant("HandleMirrorsFront", &swallowed));
}
