// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-only prerequisite probe, not a shipped-image qualification. One warm-up followed by
//! 32 adjacent A/A pairs in ABAB order, with an identical workload and executable in each
//! position. Raw rows precede paired log-ratio Student intervals (95%, df=31). No samples
//! are discarded and no sequential stopping is used. Process counters cover this one thread.
//! Run with `targo --unverified run --release -p aterm-bench --example counter_resolution`.

use aterm_bench::measurement_clock as clock;
use aterm_core::terminal::Terminal;
use std::hint::black_box;
use std::time::Instant;

const PAIRS: usize = 32;

fn measure(lane: &str, payload: &[u8]) -> Option<[f64; 3]> {
    let mut term = Terminal::new(32, 110);
    term.process(b"initial frame\r\n");
    let before = clock::counters()?;
    let start = Instant::now();
    match lane {
        "parse" => {
            for chunk in payload.chunks(16 * 1024) {
                term.process(black_box(chunk));
            }
        }
        "frame" => {
            for _ in 0..4096 {
                term.process(black_box(b"\rX"));
                black_box(term.cell_frame(32, 110));
            }
        }
        _ => unreachable!(),
    }
    let elapsed = start.elapsed().as_secs_f64();
    let delta = clock::counters()?.since(before);
    Some([delta.instructions as f64, delta.cycles as f64, elapsed])
}

/// Paired geometric mean and two-sided 95% Student interval, reported as percent change.
fn interval(ratios: &[f64]) -> (f64, f64, f64) {
    assert_eq!(ratios.len(), PAIRS, "the critical value is for df=31");
    assert!(ratios.iter().all(|x| x.is_finite() && *x > 0.0));
    let n = ratios.len() as f64;
    let logs: Vec<_> = ratios.iter().map(|x| x.ln()).collect();
    let mean = logs.iter().sum::<f64>() / n;
    let variance = logs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let half = 2.039_513 * (variance / n).sqrt();
    let pct = |x: f64| 100.0 * x.exp_m1();
    (pct(mean), pct(mean - half), pct(mean + half))
}

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 2 && args[0] == "--sample" {
        let lane = args[1].to_str().expect("UTF-8 lane");
        assert!(["parse", "frame"].contains(&lane));
        clock::declare_interactive();
        let payload = aterm_bench::throughput_corpus(8 << 20);
        measure(lane, &payload).expect("host counters unavailable during warmup");
        let row = measure(lane, &payload).expect("host counters unavailable");
        println!("{:.0} {:.0} {:.9}", row[0], row[1], row[2]);
        return;
    }
    if args.len() == 3 && args[0] == "--compare" {
        compare(
            std::path::Path::new(&args[1]),
            std::path::Path::new(&args[2]),
        );
        return;
    }
    assert!(
        args.is_empty(),
        "expected --sample LANE or --compare BASE CANDIDATE"
    );
    println!(
        "protocol=aa-abab pairs={PAIRS} qos={}",
        clock::declare_interactive()
    );
    let payload = aterm_bench::throughput_corpus(8 << 20);
    let mut resolved = true;
    for lane in ["parse", "frame"] {
        if measure(lane, &payload).is_none() {
            eprintln!("unavailable: macOS retired-work counters; no resolution claim");
            std::process::exit(3);
        }
        let mut ratios = [Vec::new(), Vec::new(), Vec::new()];
        for pair in 0..PAIRS {
            let mut rows = Vec::new();
            for position in ["A", "A-prime"] {
                let Some(row) = measure(lane, &payload) else {
                    eprintln!("unavailable: counters at {lane}/{pair}/{position}");
                    std::process::exit(3);
                };
                println!(
                    "row lane={lane} pair={pair} position={position} instructions={:.0} cycles={:.0} wall_s={:.9}",
                    row[0], row[1], row[2]
                );
                rows.push(row);
            }
            for (index, values) in ratios.iter_mut().enumerate() {
                values.push(rows[1][index] / rows[0][index]);
            }
        }
        for (metric, values) in ["instructions", "cycles", "wall"].into_iter().zip(ratios) {
            let (mean, lo, hi) = interval(&values);
            let pass = lo > -1.0 && hi < 1.0;
            resolved &= pass;
            println!(
                "interval lane={lane} metric={metric} n={PAIRS} change_pct={mean:.6} lo_pct={lo:.6} hi_pct={hi:.6} within_one_pct={pass}"
            );
        }
    }
    println!("prerequisite_resolved={resolved} qualification=false missing=pty,present,input");
    if !resolved {
        std::process::exit(2);
    }
}

/// Adjacent, separate-process pairs; reverse each pair to balance order effects.
/// Only the child's measurement region is timed. Process startup and its one
/// warmup are outside that region. This is an isolated engine comparison, not
/// a claim about the app's WindowServer or PTY path.
fn compare(base: &std::path::Path, candidate: &std::path::Path) {
    println!(
        "protocol=process-paired-balanced pairs={PAIRS} base={} candidate={}",
        base.display(),
        candidate.display()
    );
    for lane in ["parse", "frame"] {
        let mut ratios = [Vec::new(), Vec::new(), Vec::new()];
        for pair in 0..PAIRS {
            let order = if pair % 2 == 0 { [0, 1] } else { [1, 0] };
            let mut rows = [[0.0; 3]; 2];
            for position in order {
                let path = [base, candidate][position];
                let mut command = aterm_uds::spawnfd::command(path);
                command.args(["--sample", lane]);
                let output = aterm_uds::deadline::output_within(
                    &mut command,
                    std::time::Duration::from_secs(60),
                )
                .expect("sample process must finish");
                assert!(
                    output.status.success(),
                    "sample failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let values: Vec<f64> = std::str::from_utf8(&output.stdout)
                    .expect("sample UTF-8")
                    .split_whitespace()
                    .map(|v| v.parse().expect("numeric sample"))
                    .collect();
                rows[position] = values.try_into().expect("three sample counters");
                println!(
                    "row lane={lane} pair={pair} position={position} instructions={:.0} cycles={:.0} wall_s={:.9}",
                    rows[position][0], rows[position][1], rows[position][2]
                );
            }
            for (i, values) in ratios.iter_mut().enumerate() {
                values.push(rows[1][i] / rows[0][i]);
            }
        }
        for (metric, values) in ["instructions", "cycles", "wall"].into_iter().zip(ratios) {
            let (mean, lo, hi) = interval(&values);
            println!(
                "interval lane={lane} metric={metric} n={PAIRS} change_pct={mean:.6} lo_pct={lo:.6} hi_pct={hi:.6} within_one_pct={}",
                lo > -1.0 && hi < 1.0
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_detects_a_one_percent_change_and_keeps_uncertainty() {
        let (mean, lo, hi) = interval(&[1.01; PAIRS]);
        assert!((mean - 1.0).abs() < 1e-10);
        assert!((lo - 1.0).abs() < 1e-10 && (hi - 1.0).abs() < 1e-10);
        let varied: Vec<_> = (0..PAIRS)
            .map(|i| if i % 2 == 0 { 0.9 } else { 1.0 / 0.9 })
            .collect();
        let (mean, lo, hi) = interval(&varied);
        assert!(mean.abs() < 1e-10);
        assert!(lo < -1.0 && hi > 1.0);
    }
}
