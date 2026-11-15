// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::BTreeSet;
use std::time::Duration;

/// The value `name` has in `line` (`name=value`), 0 where the line leaves it out.
fn said(line: &str, name: &str) -> u64 {
    line.split(' ')
        .find_map(|field| field.strip_prefix(name)?.strip_prefix('='))
        .map_or(0, |value| value.parse().expect("a number"))
}

#[test]
fn every_total_has_its_own_name() {
    let names = PassCounts::names();
    let distinct: BTreeSet<&String> = names.iter().collect();
    assert_eq!(distinct.len(), names.len(), "{names:?}");
    assert_eq!(names.len(), PassCounts::default().totals().len());
    for (at, what) in Count::ALL.iter().enumerate() {
        assert_eq!(*what as usize, at, "{what:?} is not at its own index");
    }
    for (at, phase) in Phase::ALL.iter().enumerate() {
        assert_eq!(*phase as usize, at, "{phase:?} is not at its own index");
    }
}

/// Negative control: with [`metered`] installing nothing, the pass's `parses` reads 0, wants 2.
#[test]
fn a_count_goes_to_the_pass_its_thread_works_for_and_to_none_otherwise() {
    let counts = Arc::new(PassCounts::default());
    add(Count::Parses);
    assert_eq!(counts.get(Count::Parses), 0);
    {
        let _metered = metered(&counts);
        add(Count::Parses);
        add_n(Count::ProbeBytes, 40);
        // A second pass on the thread of the first changes nothing: the first keeps the thread.
        let other = Arc::new(PassCounts::default());
        let _inner = metered(&other);
        add(Count::Parses);
        assert_eq!(other.get(Count::Parses), 0);
    }
    add(Count::Parses);
    assert_eq!(counts.get(Count::Parses), 2);
    assert_eq!(counts.get(Count::ProbeBytes), 40);
}

/// A mark into a phase is one entry of it on the thread that keeps time; the phase a `within`
/// returns to as it ends is not entered again, the phase a pass's thread begins in was entered
/// by no mark, and a thread a pass started counts no entry.
///
/// Negative controls: with `Within`'s drop entering nothing, the thread stays in `Peers` and the
/// assertion after the inner guard reads `Some(Peers)`, not `Some(Records)`; with `Here::enter`
/// counting no entry, the entries read `[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]`, not
/// `[0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0]`.
#[test]
fn a_phase_ends_in_the_phase_it_began_in() {
    assert_eq!(phase(), None);
    let _outside = within(Phase::Probe);
    assert_eq!(phase(), None, "no pass, so no phase");
    let counts = Arc::new(PassCounts::default());
    let _metered = metered(&counts);
    assert_eq!(phase(), Some(Phase::Rest));
    {
        let _records = within(Phase::Records);
        {
            let _peers = within(Phase::Peers);
            assert_eq!(phase(), Some(Phase::Peers));
        }
        assert_eq!(phase(), Some(Phase::Records));
    }
    assert_eq!(phase(), Some(Phase::Rest));
    enter(Phase::After);
    assert_eq!(phase(), Some(Phase::After));
    // A thread the pass started passes a mark: it is in the phase, and no entry is counted.
    let started = starting();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            started.run(|| {
                let _select = within(Phase::Select);
                assert_eq!(phase(), Some(Phase::Select));
            });
        });
    });
    let entered: Vec<u64> = Phase::ALL
        .iter()
        .map(|phase| counts.entered(*phase))
        .collect();
    assert_eq!(entered, [0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0]);
}

/// Negative control: with `settle_at` leaving the clock at its old readings, the second settle
/// adds the first's time again: `probe` reads (70, 60), not (40, 10).
#[test]
fn each_nanosecond_goes_to_the_one_phase_the_thread_was_in() {
    let counts = Arc::new(PassCounts::default());
    let began = Instant::now();
    let mut here = Here {
        counts: Arc::clone(&counts),
        phase: Phase::Rest,
        clock: Some(Clock {
            cpu_ns: 100,
            wall: began,
        }),
    };
    let at = |cpu_ns, wall_ns| Clock {
        cpu_ns,
        wall: began + Duration::from_nanos(wall_ns),
    };
    here.settle_at(at(130, 50));
    here.phase = Phase::Probe;
    here.settle_at(at(170, 60));
    let spent = |phase: Phase| {
        (
            counts.cpu_ns[phase as usize].load(Ordering::Relaxed),
            counts.wall_ns[phase as usize].load(Ordering::Relaxed),
        )
    };
    assert_eq!(spent(Phase::Rest), (30, 50));
    assert_eq!(spent(Phase::Probe), (40, 10));
    let all: (u64, u64) = Phase::ALL
        .iter()
        .map(|phase| spent(*phase))
        .fold((0, 0), |sum, (cpu, wall)| (sum.0 + cpu, sum.1 + wall));
    assert_eq!(all, (170 - 100, 60));
    // A thread a pass started has no clock: its time is counted once, as it ends.
    let mut started = Here {
        counts: Arc::clone(&counts),
        phase: Phase::Probe,
        clock: None,
    };
    started.settle_at(at(900, 900));
    assert_eq!(spent(Phase::Probe), (40, 10));
}

/// Negative control: with [`Started::run`] putting no pass on its thread, the started thread's
/// count goes nowhere: `probe_bodies` reads 0, not 1.
#[test]
fn a_started_thread_counts_for_the_pass_and_is_counted_in_the_phase_it_was_started_in() {
    let counts = Arc::new(PassCounts::default());
    // Started by a thread that works for no pass: the work alone.
    let alone = starting();
    std::thread::scope(|scope| {
        scope.spawn(|| alone.run(|| add(Count::ProbeBodies)));
    });
    assert_eq!(counts.get(Count::ProbeBodies), 0);
    let _metered = metered(&counts);
    let rest = starting();
    let probing = {
        let _probe = within(Phase::Probe);
        starting()
    };
    std::thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| probing.run(|| add(Count::ProbeBodies)));
        }
        scope.spawn(|| {
            rest.run(|| {
                add(Count::Parses);
                // A thread it starts in turn is counted in the same phase.
                let again = starting();
                std::thread::scope(|inner| {
                    inner.spawn(|| again.run(|| add(Count::Parses)));
                });
            });
        });
    });
    assert_eq!(counts.get(Count::ProbeBodies), 3);
    assert_eq!(counts.get(Count::Parses), 2);
    let ended = |phase: Phase| counts.threads(phase).ended.load(Ordering::Relaxed);
    assert_eq!(ended(Phase::Probe), 3);
    assert_eq!(ended(Phase::Rest), 2);
    assert_eq!(Phase::ALL.iter().map(|phase| ended(*phase)).sum::<u64>(), 5);
}

/// Negative control: with the window never dropping its oldest pass, the second line at the 16th
/// pass covers all 16: it says `last 16 passes` and `probe_bodies=136`, not 8 and 100.
#[test]
fn the_lines_are_said_as_due_passes_end_and_the_second_covers_the_last_eight() {
    let counts = Arc::new(PassCounts::default());
    let _metered = metered(&counts);
    let mut lines = Vec::new();
    for pass in 1..=20_usize {
        add(Count::Passes);
        add_n(Count::ProbeBodies, pass);
        if let Some(said) = counts.pass_ended() {
            lines.push((pass, said));
        }
    }
    let due: Vec<usize> = lines.iter().map(|(pass, _)| *pass).collect();
    assert_eq!(due, [1, 2, 4, 8, 16]);
    for (pass, [totals, window]) in &lines {
        let pass = *pass as u64;
        assert!(totals.starts_with("pass counts: "), "{totals}");
        assert_eq!(said(totals, "passes"), pass);
        assert_eq!(said(totals, "probe_bodies"), pass * (pass + 1) / 2);
        // A total that is 0 is left out.
        assert!(!totals.contains("probe_bytes"), "{totals}");
        let over = pass.min(WINDOW as u64);
        assert!(
            window.starts_with(&format!("pass counts, last {over} passes: ")),
            "{window}"
        );
        assert_eq!(said(window, "passes"), over);
        let first = pass - over + 1;
        assert_eq!(
            said(window, "probe_bodies"),
            (first..=pass).sum::<u64>(),
            "{window}"
        );
    }
    let (_, [_, sixteenth]) = &lines[4];
    assert_eq!(said(sixteenth, "probe_bodies"), 100);
}
