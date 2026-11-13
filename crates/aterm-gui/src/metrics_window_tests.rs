// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::instrument::{Counter, Dist, Probe, Windowed, window_ledger};
use super::{Metrics, emit_quantiles, inert_text, now_ns, wire};
use std::sync::atomic::Ordering;

/// Replay known elapsed times through both live ledgers and the public
/// percentile emitter. This controls recording/rounding error, independently
/// of the scheduler noise in a windowed workload.
#[test]
fn latency_wire_resolves_identical_replay_and_a_two_percent_delay() {
    let keys = [
        "input_p50_ms",
        "input_p95_ms",
        "input_p99_ms",
        "present_p50_ms",
        "present_p95_ms",
        "present_p99_ms",
    ];
    let replay = |pair: u64, percent: u64| {
        let m = Metrics::new();
        for sample in 1..=1000 {
            let elapsed = (100_000 + sample * 1000 + pair * 37) * percent / 100;
            m.window.input.input.record(elapsed);
            m.window.present.latency.record(elapsed);
        }
        let ctx = wire::ReadCtx::inert(&m, 0);
        let text = wire::render_text(|s| super::emit_percentiles(&ctx, s));
        keys.map(|key| {
            text.split_whitespace()
                .find_map(|field| {
                    let (name, value) = field.split_once('=')?;
                    (name == key).then(|| value.parse::<f64>().unwrap())
                })
                .unwrap()
        })
    };
    let mut control: [Vec<f64>; 6] = std::array::from_fn(|_| Vec::new());
    let mut planted: [Vec<f64>; 6] = std::array::from_fn(|_| Vec::new());
    for pair in 0..32 {
        let (a, aa, delayed) = (replay(pair, 100), replay(pair, 100), replay(pair, 102));
        for i in 0..6 {
            control[i].push((aa[i] / a[i]).ln());
            planted[i].push((delayed[i] / a[i]).ln());
        }
    }
    for (i, key) in keys.into_iter().enumerate() {
        for (name, values) in [("aa", &control[i]), ("delay2", &planted[i])] {
            let mean = values.iter().sum::<f64>() / 32.0;
            let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / 31.0;
            let half = 2.039_513 * (variance / 32.0).sqrt();
            let (lo, hi) = (
                100.0 * (mean - half).exp_m1(),
                100.0 * (mean + half).exp_m1(),
            );
            println!(
                "latency-replay control={name} field={key} pairs=32 lo_pct={lo:.6} hi_pct={hi:.6}"
            );
            if name == "aa" {
                assert_eq!((lo, hi), (0.0, 0.0));
            } else {
                assert!(lo > 1.8 && hi < 2.2, "{key}: [{lo}, {hi}]");
            }
        }
    }
}

fn silent<R>(
    fresh: impl Fn() -> R,
    visit: impl Fn(&R, &mut dyn FnMut(&str, &dyn Probe)),
    render: impl Fn(&R) -> String,
) -> Vec<String> {
    let mut names = Vec::new();
    visit(&fresh(), &mut |name, probe| {
        if probe.observes() {
            names.push(name.to_string());
        }
    });
    assert!(!names.is_empty(), "the root holds no observation");
    names
        .into_iter()
        .filter(|name| {
            let root = fresh();
            let before = render(&root);
            visit(&root, &mut |path, probe| {
                if path == name.as_str() {
                    probe.poke();
                }
            });
            render(&root) == before
        })
        .collect()
}

#[test]
fn every_observation_instrument_reaches_a_surface() {
    assert_eq!(
        silent(
            Metrics::new,
            |m, f| m.window.visit("window", f),
            |m| inert_text(m, 0)
        ),
        Vec::<String>::new(),
        "these instruments are written and published nowhere"
    );

    window_ledger! {
        struct Muted {
            published: Counter,
            unpublished: Counter,
        }
    }
    assert_eq!(
        silent(
            Muted::new,
            |l, f| l.visit("muted", f),
            |l| wire::render_text(|s| s.count("published", l.published.get()))
        ),
        ["muted.unpublished"]
    );

    window_ledger! {
        struct Spread {
            wait: Dist,
        }
    }
    assert_eq!(
        silent(
            Spread::new,
            |l, f| l.visit("spread", f),
            |l| wire::render_text(|s| emit_quantiles(
                s,
                ["n_wait", "wait_p50_ms", "wait_p95_ms", "wait_p99_ms"],
                &l.wait.copy_out()
            ))
        ),
        ["spread.wait#last", "spread.wait#max"]
    );
}

#[test]
fn reset_clears_every_window_instrument_and_no_live_state() {
    let m = Metrics::new();
    let mut cells = 0;
    m.window.visit("window", &mut |name, probe| {
        probe.poke();
        assert!(!probe.is_empty(), "{name}: a poke left it empty");
        cells += 1;
    });
    assert_ne!(cells, 0);
    let live = &m.live;
    live.backend_gpu.store(true, Ordering::Relaxed);
    live.perf_reduced.store(true, Ordering::Relaxed);
    live.pty_sessions.store(3, Ordering::Relaxed);
    live.spare_ptys.store(2, Ordering::Relaxed);
    let counts = [
        &live.sync_holding_windows,
        &live.deck_links_silent,
        &live.capture_depth,
        &live.taint_until,
        &live.fx_glow_quads_last,
        &live.fx_under_quads_last,
        &live.fx_trail_cells_last,
        &live.last_wake_kind,
        &live.last_wake_owner,
        &live.last_wake_late,
        &live.last_deadline_owner,
        &live.last_deadline_due,
        &live.last_deadline_late,
        &live.handoff_input_dropped,
        &live.handoff_input_incoherent,
        &live.needle_episode_owed,
        &live.needle_episode_fired,
    ];
    for (n, cell) in (1..).zip(counts) {
        cell.store(n, Ordering::Relaxed);
    }

    let before = now_ns();
    m.reset();
    m.window.visit("window", &mut |name, probe| {
        assert!(probe.is_empty(), "{name} survived the reset");
    });
    assert!(m.epoch.load(Ordering::Relaxed) >= before);

    assert!(live.backend_gpu.load(Ordering::Relaxed));
    assert!(live.perf_reduced.load(Ordering::Relaxed));
    assert_eq!(live.pty_sessions.load(Ordering::Relaxed), 3);
    assert_eq!(live.spare_ptys.load(Ordering::Relaxed), 2);
    for (n, cell) in (1..).zip(counts) {
        assert_eq!(cell.load(Ordering::Relaxed), n, "live state #{n} was reset");
    }
}
