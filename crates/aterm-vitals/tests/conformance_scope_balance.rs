// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `ScopeBalance` (`aterm_models_misc::scope_balance_model`): each scene of the split's
//! tests (`aterm_vitals::explain::fixtures`) is read through the real `Vitals::absorb`, and its
//! split, as the caller's tab sees it (`CoalSplit::parts`), is projected onto the model.

use std::collections::BTreeMap;

use aterm_models_misc::scope_balance_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_vitals::Vitals;
use aterm_vitals::explain::fixtures::{
    Scene, build_start_1032, delegated_1032, delegated_in_aterm_1032, reaped_1032, spotlight_1032,
    tab_ahead_1032, two_coalitions_1421,
};
use aterm_vitals::explain::{CoalSplit, Fault};

type S = BTreeMap<&'static str, i64>;

/// A scene's readings in the model's terms, millicores over its window.
#[derive(Clone, Copy, Debug)]
struct Truth {
    /// The cores' busy figure.
    g: i64,
    /// Aterm's coalitions.
    a: i64,
    /// The caller tab's live members in aterm's coalitions over the window
    /// (a delegated member there included).
    t: i64,
    /// The same over the last sweep.
    tf: i64,
    /// The tab's delegated members under a launcher outside aterm, live.
    d: i64,
    /// Their launcher's coalition.
    l: i64,
    /// The user's other coalitions.
    o: i64,
    /// The services' coalitions.
    s: i64,
}

/// The 10:32 moment's readings.
const T1032: Truth = Truth {
    g: 3300,
    a: 400,
    t: 200,
    tf: 200,
    d: 0,
    l: 0,
    o: 730,
    s: 2170,
};

/// The scenes, each with the caller's tab and its readings.
fn cases() -> Vec<(&'static str, Scene, u64, Truth)> {
    let at_1421 = Truth {
        g: 10_000,
        a: 9760,
        t: 3350,
        tf: 3350,
        o: 30,
        s: 180,
        ..T1032
    };
    let mut behind = spotlight_1032();
    behind.busy_mc -= 50;
    vec![
        ("10:32", spotlight_1032(), 1, T1032),
        ("14:21, tab 1", two_coalitions_1421(), 1, at_1421),
        (
            "14:21, tab 2",
            two_coalitions_1421(),
            2,
            Truth {
                t: 80,
                tf: 80,
                ..at_1421
            },
        ),
        (
            "made and reaped inside the window",
            reaped_1032(),
            1,
            Truth {
                g: 3850,
                o: 1030,
                ..T1032
            },
        ),
        (
            "the cores behind the counters",
            behind,
            1,
            Truth { g: 3250, ..T1032 },
        ),
        (
            "the tab read past aterm's coalitions",
            tab_ahead_1032(),
            1,
            Truth {
                t: 450,
                tf: 450,
                ..T1032
            },
        ),
        (
            "a delegated member",
            delegated_1032(500, 100),
            1,
            Truth {
                g: 3900,
                d: 500,
                l: 600,
                ..T1032
            },
        ),
        (
            "a delegated member ahead of its launcher",
            delegated_1032(700, 0),
            1,
            Truth {
                g: 3900,
                d: 700,
                l: 600,
                ..T1032
            },
        ),
        (
            "a delegated member in aterm's coalition",
            delegated_in_aterm_1032(500),
            1,
            Truth {
                g: 3800,
                a: 900,
                t: 700,
                tf: 700,
                ..T1032
            },
        ),
        (
            "a build starting",
            build_start_1032(),
            1,
            Truth {
                g: 4100,
                a: 1200,
                t: 1000,
                tf: 3000,
                ..T1032
            },
        ),
    ]
}

/// The split of `scene`'s window, with `fault` injected.
fn split(scene: &Scene, fault: Option<Fault>) -> CoalSplit {
    let mut v = match fault {
        Some(f) => Vitals::new().with_fault(f),
        None => Vitals::new(),
    };
    let [base, sweep, now] = scene.samples();
    v.absorb(&base);
    v.absorb(&sweep);
    v.absorb(&now).why.expect("a window's facts").split.clone()
}

/// The states before `Move`, after it, and after `Close`.
fn project(m: &Model, truth: Truth, split: &CoalSplit, tab: u64) -> [S; 3] {
    let p = split.parts(Some(tab));
    let mut pre = m.init_state();
    pre.extend([
        ("g", truth.g),
        ("a", truth.a),
        ("t", truth.t),
        ("tf", truth.tf),
        ("d", truth.d),
        ("l", truth.l),
        ("o", truth.o),
        ("s", truth.s),
        ("fresh", 0),
    ]);
    let mut mid = pre.clone();
    mid.extend([
        ("moved", split.delegated_mc),
        ("others", p.others),
        ("tab", p.tab),
        ("fresh", 1),
    ]);
    let mut post = mid.clone();
    post.extend([
        ("rest", p.aterm_rest),
        ("services", p.services),
        ("residual", p.unaccounted),
        ("fresh", 2),
    ]);
    [pre, mid, post]
}

/// Whether `m` takes the projection through `Move` and `Close`; the laws its
/// last state breaks.
fn replay(m: &Model, [pre, mid, post]: &[S; 3]) -> Result<Vec<&'static str>, String> {
    for (from, to, step) in [(pre, mid, "Move"), (mid, post, "Close")] {
        if interp::admits(m, from, to) != Some(step) {
            return Err(format!("`{step}` does not take {from:?} to {to:?}"));
        }
    }
    Ok(m.invariants
        .iter()
        .filter(|i| !m.check_invariant(i.name, post))
        .map(|i| i.name)
        .collect())
}

#[test]
fn every_scene_is_a_run_of_the_model() {
    let m = interp::with_buggy(&scope_balance_model(), 0);
    let mut bad = Vec::new();
    for (name, scene, tab, truth) in cases() {
        let states = project(&m, truth, &split(&scene, None), tab);
        match replay(&m, &states) {
            Ok(broken) if broken.is_empty() => {}
            Ok(broken) => bad.push(format!("{name}: breaks {broken:?}")),
            Err(e) => bad.push(format!("{name}: {e}")),
        }
    }
    assert!(
        bad.is_empty(),
        "{} scene(s) the model refuses:\n  {}",
        bad.len(),
        bad.join("\n  ")
    );
}

/// A defect's split: refused at Buggy 0; admitted at its own dial, where its
/// own law breaks.
fn caught(fault: Fault, case: &str, dial: i64, law: &str) {
    let (_, scene, tab, truth) = cases()
        .into_iter()
        .find(|c| c.0 == case)
        .unwrap_or_else(|| panic!("no scene {case}"));
    let model = scope_balance_model();
    let healthy = interp::with_buggy(&model, 0);
    let states = project(&healthy, truth, &split(&scene, Some(fault)), tab);
    assert!(
        replay(&healthy, &states).is_err(),
        "{fault:?} on {case}: the healthy model admits the defect"
    );
    let mutant = interp::with_buggy(&model, dial);
    let broken = replay(&mutant, &states).unwrap_or_else(|e| {
        panic!("{fault:?} on {case}: Buggy {dial} does not admit the split: {e}")
    });
    assert_eq!(broken, [law], "{fault:?} on {case}");
}

/// Services by subtraction with the lane on: the reaped coalition's 0.25
/// folded into services, the residual silent.
#[test]
fn services_by_subtraction_with_the_lane_on_is_caught_by_residual_is_shown() {
    caught(
        Fault::SplitBySubtraction,
        "made and reaped inside the window",
        4,
        "ResidualIsShown",
    );
}

/// The tab's figure over the last 1 s sweep against a 5 s split.
#[test]
fn the_tab_over_the_sweeps_window_is_caught_by_matched_windows_only() {
    caught(
        Fault::TabOverProcsWindow,
        "a build starting",
        5,
        "MatchedWindowsOnly",
    );
}
