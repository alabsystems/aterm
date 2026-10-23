// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance bind for `NovaPhase` transitions against `nova_phase_model`.

use std::collections::BTreeMap;

use aterm_effects::genome::{nova_duration_ms, nova_features};
use aterm_effects::nova::{
    self, EMBER_TAIL_MS, MAX_NOVA_QUADS_PER, NOVA_PALETTES, NovaEnv, NovaPhase,
};
use aterm_models_effects::nova_phase_model;
use aterm_spec::verify;

type State = BTreeMap<&'static str, i64>;

fn phase_index(p: NovaPhase) -> i64 {
    match p {
        NovaPhase::Armed => 0,
        NovaPhase::Dip => 1,
        NovaPhase::Flash => 2,
        NovaPhase::Ring => 3,
        NovaPhase::Debris => 4,
        NovaPhase::Ember => 5,
        NovaPhase::Settled => 6,
    }
}

fn state(phase: i64, steps: i64, flashes: i64, arms: i64) -> State {
    [
        ("phase", phase),
        ("steps", steps),
        ("flashes", flashes),
        ("arms", arms),
    ]
    .into_iter()
    .collect()
}

fn genome_per_duration() -> Vec<u64> {
    let mut found: Vec<(u32, u64)> = Vec::new();
    for g in 0u64..(1 << 16) {
        let d = nova_duration_ms(g);
        if !found.iter().any(|(seen, _)| *seen == d) {
            found.push((d, g));
        }
    }
    found.sort_unstable();
    assert_eq!(
        found.iter().map(|(d, _)| *d).collect::<Vec<_>>(),
        vec![1000, 1133, 1266, 1400],
        "the genome decodes exactly four nova durations"
    );
    found.into_iter().map(|(_, g)| g).collect()
}

fn env_for(g: u64) -> NovaEnv {
    let feats = nova_features(g);
    let (cell_w, cell_h) = (10, 20);
    NovaEnv {
        grid_w: 80 * cell_w,
        grid_h: 24 * cell_h,
        cell_w,
        cell_h,
        cx: 40 * cell_w,
        cy: 12 * cell_h,
        r_max: feats.radius * cell_h as f32,
        feats,
        magic: None,
        core: NOVA_PALETTES[usize::from(feats.palette & 7)].0,
        fringe: NOVA_PALETTES[usize::from(feats.palette & 7)].1,
        intensity: 1.0,
        seed: g,
    }
}

fn crown_lit(t: i64, env: &NovaEnv) -> bool {
    let Ok(t) = u64::try_from(t) else {
        return false; // Armed: a deferred ignition emits nothing.
    };
    let mut out = Vec::new();
    nova::emit_nova(t, env, MAX_NOVA_QUADS_PER, &mut out).crown > 0
}

struct Window {
    phase: i64,
    blooms: i64,
    entered_at: i64,
}

fn real_windows(env: &NovaEnv) -> Vec<Window> {
    let d = env.feats.duration_ms;
    let end = i64::from(d) + i64::try_from(EMBER_TAIL_MS).expect("small") + 5;
    let mut windows: Vec<Window> = Vec::new();
    let mut lit_before = false;
    for t in -5..=end {
        let phase = phase_index(nova::phase(t, d));
        let lit = crown_lit(t, env);
        let bloom = i64::from(lit && !lit_before);
        lit_before = lit;
        match windows.last_mut() {
            Some(w) if w.phase == phase => w.blooms += bloom,
            _ => windows.push(Window {
                phase,
                blooms: bloom,
                entered_at: t,
            }),
        }
    }
    windows
}

fn walk_arm(model: &aterm_spec::derive::Model, start: State, env: &NovaEnv) -> State {
    let d = env.feats.duration_ms;
    let windows = real_windows(env);
    let first = windows.first().expect("the walk has a window");
    assert_eq!(
        (first.phase, first.blooms),
        (start["phase"], 0),
        "D={d}: the real walk must open in the model's Init, with the crown dark"
    );
    let mut cur = start;
    for w in &windows[1..] {
        let next = state(
            w.phase,
            cur["steps"] + 1,
            cur["flashes"] + w.blooms,
            cur["arms"],
        );
        let (ok, why) = verify::validate_transition_tiered(
            model,
            &[],
            &cur,
            &next,
            Some("Step"),
            "nova phase conformance",
        );
        assert!(
            ok,
            "D={d} t={}: real step {cur:?} -> {next:?}\n{why}",
            w.entered_at
        );
        for inv in ["Monotone", "OneFlashPerArm", "CanSettle"] {
            assert!(
                model.check_invariant(inv, &next),
                "D={d} t={}: {inv} broken by {next:?}",
                w.entered_at
            );
        }
        cur = next;
    }
    assert_eq!(cur["phase"], 6, "D={d}: every arm settles");
    assert_eq!(cur["flashes"], 1, "D={d}: exactly one crown per arm");
    cur
}

#[test]
fn real_nova_phase_walk_conforms_to_nova_phase_model() {
    let model = nova_phase_model();
    for g in genome_per_duration() {
        let settled = walk_arm(&model, model.init_state(), &env_for(g));
        assert_eq!(settled["steps"], 6, "one arm is the six-step walk");
    }
}
