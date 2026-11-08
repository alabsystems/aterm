// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The remaining chrome row is the tab strip. Diagnostics reserve no rows.

use std::time::{Duration, Instant};

use aterm_messages::chrome::CHROME_REGROW_SETTLE;
use aterm_models_gui::chrome_regrow_floor_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};

use crate::{App, WindowId};

/// The observed strip transitions, independent of the model.
#[derive(Default)]
pub(crate) struct Seen {
    rise: Option<Instant>,
    quick: bool,
    flips: i64,
}

/// Project the live strip into the chrome-floor model.
pub(crate) fn project(app: &App, wid: WindowId, now: Instant, seen: &Seen) -> State {
    let ws = &app.windows[&wid];
    let strip = app.tab_strip_rows > 0;
    let mut state = State::new();
    for (name, value) in [
        (
            "sw",
            i64::from(app.strip_fall.map_or(strip, |rows| rows > 0)),
        ),
        ("s", i64::from(strip)),
        (
            "age",
            i64::from(!seen.rise.is_some_and(|at| now < at + CHROME_REGROW_SETTLE)),
        ),
        (
            "since",
            i64::from(!ws.regrow_floor.floor().is_some_and(|at| now < at)),
        ),
        ("quick", i64::from(seen.quick)),
        ("flips", seen.flips),
    ] {
        state.insert(name, value);
    }
    state
}

struct Rig {
    app: App,
    now: Instant,
    seen: Seen,
    model: Model,
}

impl Rig {
    fn new(floored: bool) -> Self {
        let mut app = App::headless_for_test();
        let sid = app.next_session_id;
        app.push_stub_tab(WindowId(0), crate::stub_session(sid));
        app.windows
            .get_mut(&WindowId(0))
            .unwrap()
            .chrome_regrids_for_test = floored;
        let rig = Self {
            app,
            now: Instant::now(),
            seen: Seen::default(),
            model: chrome_regrow_floor_model(),
        };
        assert_eq!(rig.state(), rig.model.init_state());
        rig
    }

    fn state(&self) -> State {
        project(&self.app, WindowId(0), self.now, &self.seen)
    }

    fn strip(&mut self, rows: u16) -> (State, State) {
        let before = self.state();
        let old = self.app.chrome_rows(WindowId(0));
        self.app.set_tab_strip_rows(rows, self.now);
        let new = self.app.chrome_rows(WindowId(0));
        if new > old {
            self.seen.rise = Some(self.now);
        }
        if new < old
            && self
                .seen
                .rise
                .is_some_and(|at| self.now < at + CHROME_REGROW_SETTLE)
        {
            self.seen.quick = true;
        }
        self.seen.flips += 1;
        (before, self.state())
    }

    fn admits(&self, action: &str, before: &State, after: &State) -> bool {
        self.model
            .successors(action, before)
            .iter()
            .any(|state| state == after)
    }
}

#[test]
fn a_settings_edit_down_outside_the_floor_takes_the_strip_at_once() {
    crate::fabric::with_link_reset(|| {
        let mut rig = Rig::new(true);
        let (before, after) = rig.strip(1);
        assert!(rig.admits("StripRise", &before, &after));
        let before = rig.state();
        rig.now += Duration::from_millis(500);
        assert!(rig.admits("Tick", &before, &rig.state()));
        let (before, after) = rig.strip(0);
        assert!(
            rig.model
                .successors("StripUnwant", &before)
                .iter()
                .any(|mid| rig.admits("StripFall", mid, &after))
        );
        assert_eq!(rig.app.tab_strip_rows, 0);
        assert_eq!(rig.app.strip_fall_due(), None);
    });
}

#[test]
fn the_strip_floor_defers_a_quick_fall_and_the_control_exposes_it() {
    crate::fabric::with_link_reset(|| {
        for floored in [true, false] {
            let mut rig = Rig::new(floored);
            rig.strip(1);
            rig.now += Duration::from_millis(20);
            let (before, after) = rig.strip(0);
            if floored {
                assert!(rig.admits("StripUnwant", &before, &after));
                assert_eq!(rig.app.tab_strip_rows, 1);
                assert!(rig.app.strip_fall_due().is_some());
                assert!(!rig.seen.quick);
            } else {
                assert!(interp::admits(&rig.model, &before, &after).is_none());
                let buggy = interp::with_buggy(&rig.model, 1);
                assert!(buggy.successors("StripUnwant", &before).iter().any(|mid| {
                    buggy
                        .successors("StripFallAtOnce", mid)
                        .iter()
                        .any(|state| state == &after)
                }));
                assert!(rig.seen.quick, "negative control regrids inside the floor");
            }
        }
    });
}
