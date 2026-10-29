// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of `LightLatch` (`aterm_models_effects::light_latch_model`): the real latch
//! walks every transition of the model's reachable graph beside it, and on every `Frame` its
//! verdict is the model's; each `Buggy` control leaves it on some frame.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use aterm_effects::host::Instant;
use aterm_effects::host::light_latch::{
    LIGHT_ENTRY_HOLD, LatchKey, LatchOut, LightLatch, raw_light,
};
use aterm_models_effects::light_latch_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;

type State = BTreeMap<&'static str, i64>;

/// The model's tick.
const TICK: Duration = Duration::from_millis(100);

/// The model's key `key + 2 * scheme` as the host would hand it over: four distinct keys, the
/// dark ones Default's and Dracula's pairs.
fn latch_key(state: &State) -> LatchKey {
    let default_light = (0x002b_2d33, 0x00f6_f7f9);
    let (session, (fg, bg)) = match (state["key"], state["scheme"]) {
        (1, 0) => (1, (0x00d0_d0d0, 0x0011_1318)),
        (2, 0) => (1, (0x00f8_f8f2, 0x0028_2a36)),
        (1, 1) => (1, default_light),
        // Another session in the same light pair: the key holds the front session.
        (2, 1) => (2, default_light),
        other => panic!("no key {other:?}"),
    };
    LatchKey { session, fg, bg }
}

/// The live page the model's `raw` stands for, read as the host reads it.
fn page_raw(state: &State) -> bool {
    let (fg, bg) = if state["raw"] == 1 {
        (0x002b_2d33, 0x00ff_ffff)
    } else {
        (0x00d0_d0d0, 0x0011_1318)
    };
    raw_light(fg, bg, aterm_render::theme_is_dark(bg))
}

/// What the walk saw.
#[derive(Default)]
struct Walked {
    frames: usize,
    on: usize,
    fell: usize,
    changed: usize,
}

/// Every transition of `model`'s reachable graph from its initial state, the real latch carried
/// along the first path to each state: `Err` names the first frame whose verdict is not the
/// model's.
fn walk(model: &Model) -> Result<Walked, String> {
    let init = model.init_state();
    let start = init["now"];
    let t0 = Instant::now();
    let mut walked = Walked::default();
    let mut seen = BTreeSet::from([init.clone()]);
    let mut queue = VecDeque::from([(init, LightLatch::default())]);
    while let Some((state, latch)) = queue.pop_front() {
        for action in &model.actions {
            for next in model.successors(action.name, &state) {
                let mut latch = latch;
                if action.name == "Frame" {
                    let ticks = u32::try_from(state["now"] - start).expect("time runs forward");
                    let out = latch.admits(t0 + TICK * ticks, latch_key(&state), page_raw(&state));
                    let model_out = LatchOut {
                        on: next["on"] == 1,
                        scheme_changed: next["changed"] == 1,
                        fell: next["fell"] == 1,
                    };
                    if out != model_out {
                        return Err(format!(
                            "{state:?}: the latch says {out:?}, the model {model_out:?}"
                        ));
                    }
                    walked.frames += 1;
                    walked.on += usize::from(out.on);
                    walked.fell += usize::from(out.fell);
                    walked.changed += usize::from(out.scheme_changed);
                }
                if seen.insert(next.clone()) {
                    queue.push_back((next, latch));
                }
            }
        }
    }
    Ok(walked)
}

#[test]
fn the_latch_refines_its_model_on_every_reachable_frame() {
    let model = light_latch_model();
    let hold = model
        .consts
        .iter()
        .find_map(|&(name, value)| (name == "Hold").then_some(value))
        .expect("the model's hold");
    assert_eq!(
        TICK * u32::try_from(hold).expect("a short hold"),
        LIGHT_ENTRY_HOLD,
        "the model's hold is the latch's"
    );
    let walked = walk(&model).unwrap_or_else(|why| panic!("{why}"));
    assert!(walked.frames > 0 && walked.on > 0 && walked.fell > 0 && walked.changed > 0);
    assert!(walked.on < walked.frames, "and frames stayed off");
}

#[test]
fn negative_control_every_buggy_latch_is_left_on_some_frame() {
    let model = light_latch_model();
    for dial in 1..=5 {
        assert!(
            walk(&interp::with_buggy(&model, dial)).is_err(),
            "Buggy = {dial} walked with the real latch"
        );
    }
}
