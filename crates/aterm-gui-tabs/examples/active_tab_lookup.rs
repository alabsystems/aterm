// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Wall-time samples of active-tab lookup through the public tab API. This is a development
//! microbenchmark, not a measurement of input latency or a pass/fail performance check.

use std::hint::black_box;
use std::process::ExitCode;
use std::time::Instant;

use aterm_capacity::{Admit, Claim, Need, admit};
use aterm_gui_tabs::tab_model::{IdAllocator, Tab, TabId, TabPresentation, TabSet, ViewStore};

const HELP: &str = "usage: active_tab_lookup [--iterations N] [--batches N] [--warmup N]

Measure active() with the first and last tab selected in sets of 1, 30 and 200 tabs.
Defaults: 1000000 lookups per sample, 9 batches, 10000 warmup lookups before each sample.
The six cases are interleaved in every batch, rotating which case runs first.
Setup, warmup and CSV output are outside each measured monotonic wall-time interval.
Measurements require strict timing admission and a fresh entry CPU sample. CSV is emitted
only while the completed run remains qualified. This samples readiness, not continuous quiet.
Exit 3 means timing was refused and no CSV was accepted; usage errors exit 2.
No timing threshold is judged, and these samples do not measure terminal responsiveness.
";

enum Failure {
    Usage(String),
    Timing(String),
}

impl From<String> for Failure {
    fn from(error: String) -> Self {
        Self::Usage(error)
    }
}

struct Settings {
    iterations: u64,
    batches: usize,
    warmup: u64,
}

struct Case {
    tabs: TabSet,
    views: ViewStore,
    count: usize,
    position: &'static str,
}

impl Case {
    fn new(count: usize, last: bool) -> Self {
        let mut tabs = TabSet::default();
        let mut views = ViewStore::default();
        let mut ids = IdAllocator::<TabId>::default();
        for session in 0..count {
            let view = views
                .insert_terminal(session as u64)
                .expect("the fixed corpus has at most 200 views");
            let id = ids
                .allocate()
                .expect("the fixed corpus has at most 200 tabs");
            tabs.push(Tab::new(id, view, TabPresentation::terminal("terminal")))
                .expect("new tab identity");
        }
        let index = if last { count - 1 } else { 0 };
        let expected = tabs.tab_at(index).expect("selected slot exists").id;
        assert!(tabs.switch_to_index(index));
        assert_eq!(tabs.active().map(|tab| tab.id), Some(expected));
        Self {
            tabs,
            views,
            count,
            position: if last { "last" } else { "first" },
        }
    }
}

fn lookups(tabs: &TabSet, iterations: u64) {
    for _ in 0..iterations {
        black_box(black_box(tabs).active());
    }
}

fn note_timing(phase: &str, claim: &Claim) {
    eprintln!(
        "timing phase={phase} provider=sysprobe_core_ticks scope=sampled_global_cpu_window;not_continuous_quiet evidence={:?}",
        claim.record().timing,
    );
}

fn run() -> Result<(), Failure> {
    let mut settings = Settings {
        iterations: 1_000_000,
        batches: 9,
        warmup: 10_000,
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--help" || flag == "-h" {
            print!("{HELP}");
            return Ok(());
        }
        if !matches!(flag.as_str(), "--iterations" | "--batches" | "--warmup") {
            return Err(format!("unknown argument {flag:?}; use --help").into());
        }
        let raw = args
            .next()
            .ok_or_else(|| format!("{flag} needs an integer"))?;
        match flag.as_str() {
            "--iterations" => {
                settings.iterations = raw
                    .parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| "--iterations needs a positive integer".to_string())?;
            }
            "--batches" => {
                settings.batches = raw
                    .parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| "--batches needs a positive integer".to_string())?;
            }
            "--warmup" => {
                settings.warmup = raw
                    .parse()
                    .map_err(|_| "--warmup needs a non-negative integer".to_string())?;
            }
            _ => unreachable!("argument checked before reading its value"),
        }
    }

    let cases: Vec<_> = [1, 30, 200]
        .into_iter()
        .flat_map(|count| [Case::new(count, false), Case::new(count, true)])
        .collect();
    for case in &cases {
        assert!(case.tabs.invariant_holds(&case.views));
    }

    let mut claim = admit(Need::timing("active_tab_lookup").cores(1), Admit::Wait)
        .map_err(|error| Failure::Timing(format!("admission refused: {error}")))?;
    claim
        .measurement_entry()
        .map_err(|error| Failure::Timing(format!("measurement entry refused: {error}")))?;
    note_timing("measurement_entry", &claim);

    let mut rows = Vec::new();
    for batch in 0..settings.batches {
        for offset in 0..cases.len() {
            let case = &cases[(batch % cases.len() + offset) % cases.len()];
            lookups(&case.tabs, settings.warmup);
            let start = Instant::now();
            lookups(&case.tabs, settings.iterations);
            let elapsed_ns = start.elapsed().as_nanos();
            let ns_per_lookup = elapsed_ns as f64 / settings.iterations as f64;
            rows.push(format!(
                "{},{},{},{},{},{},{},{:.6}",
                case.count,
                case.position,
                batch + 1,
                settings.batches,
                settings.iterations,
                settings.warmup,
                elapsed_ns,
                ns_per_lookup,
            ));
        }
    }
    claim
        .timing_admission()
        .map_err(|error| Failure::Timing(format!("completed measurement refused: {error}")))?;
    note_timing("measurement_complete", &claim);
    println!("tabs,position,batch,batches,iterations,warmup,elapsed_ns,ns_per_lookup");
    for row in rows {
        println!("{row}");
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(error)) => {
            eprintln!("active_tab_lookup: {error}");
            ExitCode::from(2)
        }
        Err(Failure::Timing(error)) => {
            eprintln!("active_tab_lookup: {error}; no CSV accepted");
            ExitCode::from(3)
        }
    }
}
