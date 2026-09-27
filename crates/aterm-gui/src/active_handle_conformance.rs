// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind for `ActiveHandle` (`aterm_spec::derive::active_handle_model`):
//! the process-wide control `ActiveHandle` must always name the session the
//! user is looking at — the FRONT window's active tab's focused pane.
//!
//! This drives a headless `App` through the shipping seams that move that
//! session — tab append/switch/cycle/move, a split and pane-focus moves, a
//! pane collapse, a tab close, a background pane EOF, a new front window, a
//! background window's tab churn, and a front-window close — and after each
//! one projects the real state onto the model's `<<truth, handle, next>>`.
//!
//! The three seams that CREATE a session (`open_tab_in`,
//! `split_focused_pane_in_window`, `create_window_logical`) spawn a PTY through
//! the event loop, which a unit test does not have. Each is split at that spawn,
//! and the step drives the shipping half after it — `install_new_tab`,
//! `install_split_pane`, `install_window_state_with_parent` — with a stub
//! session. That half is where the front moves and where the handle is
//! re-pointed, so it is the half the model judges; the spawn moves nothing.
//!
//! The projection:
//!
//! * `truth` is read from the CANONICAL tab model (`tab_set` → focused view →
//!   terminal session), not from any mirror the sync paths maintain;
//! * `handle` is the real `active_handle`'s session;
//! * the model gives every change of the front session a FRESH id, so the
//!   projection relabels: a real change of `truth` takes `next`, and `handle`
//!   projects to that same id when the real handle followed, to its previous
//!   id when it stayed put (the stale-handle shape `Buggy = 1` allows), and to
//!   an id no action produces when it moved anywhere else.
//!
//! A step that does not move the front session is not a model step; it must
//! leave the handle on the truth too.
//!
//! NEGATIVE CONTROL: the swallow, projected from a real state (the front moves
//! and the handle stays), is rejected by the committed model and admitted by
//! `Buggy = 1`'s `CloseOrNewFront`.

use std::collections::BTreeMap;

use aterm_spec::derive::active_handle_model;
use aterm_spec::{interp, verify};

use crate::{App, WindowId, stub_session};

type State = BTreeMap<&'static str, i64>;

/// `MaxId` lifted past every real run.
const OVERRIDES: &[(&str, i64)] = &[("MaxId", 1_000_000)];

/// The session the user is looking at, from the canonical tab model.
fn truth(app: &App) -> Option<u64> {
    let front = app.frontmost_window?;
    let tab = app.windows.get(&front)?.tab_set.active()?;
    app.view_store
        .get(tab.focus)
        .copied()
        .and_then(crate::tab_model::View::terminal_session)
}

/// The session the control socket's global handle names.
fn handle(app: &App) -> Option<u64> {
    app.active_handle
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|active| active.id)
}

/// The relabelling projection (see the module docs), carrying the real
/// sessions the current model ids stand for.
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

    /// Project the App after a step. `None` when the front session did not move
    /// (no model step), else the model state the real step reached.
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

/// Drive one real step and, when it moved the front session, validate it as
/// `action`. Returns whether it was a model step.
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

/// One real step: the model action it is (when it moves the front session),
/// what it is, and the shipping seam that performs it.
type Step = (&'static str, &'static str, fn(&mut App));

/// Every real step the run drives.
const STEPS: &[Step] = &[
    // Tab churn in the front window: the always-lockstep path.
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
    // Pane paths — the 2026-09-14 audit found these re-mirroring only the
    // per-window state: the swallow-prone family.
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
    // A new window becomes the front; a background window's churn moves
    // nothing; closing the front window hands the front back.
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

/// The swallow from a REAL state: the front session moves, the handle stays.
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
