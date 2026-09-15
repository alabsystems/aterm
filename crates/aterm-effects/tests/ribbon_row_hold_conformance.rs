// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real ribbon clocks bind the derived renewal protocol to shipping code.

use std::time::{Duration, Instant};

use aterm_effects::cursor_glow::Geom;
use aterm_effects::rainbow_kitty::ribbon::{Ribbon, SWOOSH_TOTAL_S};
use aterm_effects::rainbow_kitty::{Config, Ctx, Event, TypedClass};
use aterm_spec::derive::ribbon_row_hold_model;

fn config() -> Config {
    Config {
        dark_theme: true,
        intensity: 1.0,
        duration: Duration::from_millis(400),
        ribbon_tall: true,
        ribbon_flat: false,
        theme_fg: 0x00e6_e6e6,
        theme_bg: 0x001c_1c1c,
        reduced_motion: false,
    }
}

fn context(now: Instant, cfg: &Config, caret: (u16, u16)) -> Ctx<'_> {
    Ctx {
        now,
        geom: Geom {
            cw: 9,
            ch: 18,
            rows: 40,
            cols: 120,
            origin_x: 0,
            origin_y: 0,
            win_w: 1080,
            win_h: 720,
            head: 0,
        },
        cfg,
        disp: 0.9,
        birth_disp: 0.9,
        phase: 0.0,
        caret,
        caret_t: 0.0,
        caret_walk: None,
        mend: None,
        surge: 0.0,
        flow: Default::default(),
    }
}

fn typed(cells: u16) -> Event {
    Event::Typed {
        cells,
        shifted: false,
        class: TypedClass::Glyph,
    }
}

fn seed(now: Instant, cfg: &Config) -> Ribbon {
    let mut ribbon = Ribbon::new();
    // The two cohorts already exist. No navigation/abandon, so an accidental
    // global renewal cannot hide behind the abandoned-cohort exclusion.
    for row in [2, 3] {
        ribbon.on_event(&typed(4), now, &context(now, cfg, (row, 6)));
    }
    assert_eq!(ribbon.cohorts().len(), 2);
    ribbon
}

fn clock(ribbon: &Ribbon, row: u16) -> Instant {
    let mut cohorts = ribbon.cohorts().iter().filter(|cohort| cohort.row == row);
    let cohort = cohorts.next().expect("the fixture's resident cohort");
    assert!(cohorts.next().is_none(), "one ordinary cohort per row");
    cohort.alive_at
}

#[test]
fn typing_and_erase_renew_only_their_own_row_and_conform() {
    let cfg = config();
    let base = Instant::now();
    let model = ribbon_row_hold_model();
    // Either row can own the hand. The final gesture returns to the first
    // row, a positive control against a policy that simply freezes old rows.
    for first_owner in [0u16, 1] {
        let mut ribbon = seed(base, &cfg);
        let mut state = model.init_state();
        for (index, owner) in [first_owner, 1 - first_owner, first_owner]
            .into_iter()
            .enumerate()
        {
            let stamp = i64::try_from(index + 1).expect("three events");
            let now = base + Duration::from_millis((index as u64 + 1) * 100);
            let before = state.clone();
            let row = owner + 2;
            let (event, col) = if index == 1 {
                (Event::Erase, 5)
            } else {
                (typed(1), 7)
            };
            ribbon.on_event(&event, now, &context(now, &cfg, (row, col)));

            // Clock fields come from the real cohorts, not model execution.
            state.insert("prior0", before["clock0"]);
            state.insert("prior1", before["clock1"]);
            state.insert("owner", i64::from(owner));
            state.insert("stamp", stamp);
            for (key, row) in [("clock0", 2), ("clock1", 3)] {
                let millis = clock(&ribbon, row).duration_since(base).as_millis();
                assert_eq!(millis % 100, 0, "an actual gesture stamp");
                state.insert(key, i64::try_from(millis / 100).expect("three events"));
            }
            let action = if owner == 0 { "TouchRow0" } else { "TouchRow1" };
            assert!(
                model.successors(action, &before).contains(&state),
                "real row renewal escaped {action}: {before:?} -> {state:?}"
            );
            assert!(model.check_invariant("OnlyOwnerRenews", &state));
            assert!(model.check_invariant("OwnerKeepsItsLease", &state));

            // Historical defect: the same key renews the other row too.
            // It must fail both the real trace relation and the invariant.
            let mut global_hold = state.clone();
            global_hold.insert(if owner == 0 { "clock1" } else { "clock0" }, stamp);
            assert!(!model.successors(action, &before).contains(&global_hold));
            assert!(!model.check_invariant("OnlyOwnerRenews", &global_hold));
        }
    }
}

#[test]
fn continuous_typing_below_does_not_postpone_the_earlier_rows_exit() {
    let cfg = config();
    let base = Instant::now();
    let mut ribbon = seed(base, &cfg);
    let steps = (SWOOSH_TOTAL_S * 10.0).ceil() as u16 + 2;
    for step in 1..=steps {
        let now = base + Duration::from_millis(u64::from(step) * 100);
        let ctx = context(now, &cfg, (3, 6 + step));
        ribbon.on_event(&typed(1), now, &ctx);
        if ribbon.cohorts().iter().any(|cohort| cohort.row == 2) {
            assert_eq!(clock(&ribbon, 2), base, "the upper row's lease moved");
        }
        ribbon.plan(&ctx);
    }
    assert!(ribbon.cells().iter().all(|cell| cell.row != 2));
    assert!(ribbon.cells().iter().any(|cell| cell.row == 3));
}
