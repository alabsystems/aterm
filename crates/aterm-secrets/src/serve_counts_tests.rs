// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::BTreeSet;

#[test]
fn every_total_has_its_own_name_and_index() {
    let counts = ServeCounts::default();
    let names: Vec<String> = counts.totals().into_iter().map(|(name, _)| name).collect();
    let distinct: BTreeSet<&String> = names.iter().collect();
    assert_eq!(distinct.len(), names.len(), "{names:?}");
    for (at, what) in Count::ALL.iter().enumerate() {
        assert_eq!(*what as usize, at, "{what:?} is not at its own index");
    }
    for (at, phase) in Phase::ALL.iter().enumerate() {
        assert_eq!(*phase as usize, at, "{phase:?} is not at its own index");
    }
}

/// Each way a nap ends, and no nap, is a count of its own.
///
/// Negative control: count a round after a `Wake::Put` as `NapTimer` in `Count::after` (8
/// distinct counts, wants 9).
#[test]
fn a_round_is_counted_by_what_ended_the_nap_before_it() {
    let whys = [
        None,
        Some(Wake::Timer),
        Some(Wake::Route),
        Some(Wake::Browse),
        Some(Wake::Broker),
        Some(Wake::Listener),
        Some(Wake::Put),
        Some(Wake::Resumed),
        Some(Wake::Stop),
    ];
    let named: BTreeSet<&str> = whys.iter().map(|why| Count::after(*why).name()).collect();
    assert_eq!(named.len(), whys.len(), "{named:?}");
    assert_eq!(Count::after(None), Count::RoundFirst);
    assert_eq!(Count::after(Some(Wake::Timer)), Count::NapTimer);
    assert_eq!(Count::after(Some(Wake::Put)), Count::NapPut);
}

/// The line is said as the 1st, 2nd, 4th and 8th round end, holds every total that is not 0
/// under its name, and leaves out one that is 0.
///
/// Negative control: say the line at every round (`round_ended` returning `Some(self.line())`
/// always: due reads `[1, 2, 3, 4, 5, 6, 7, 8, 9]`, wants `[1, 2, 4, 8]`).
#[test]
fn the_line_is_said_as_due_rounds_end() {
    let counts = ServeCounts::default();
    let mut due = Vec::new();
    for round in 1..=9_u64 {
        assert_eq!(counts.add(Count::Rounds), round);
        counts.add_n(Count::Pulls, 3);
        if let Some(line) = counts.round_ended() {
            // The process's CPU time ends the line: a clock's reading, whatever it is.
            let (said, cpu) = line
                .rsplit_once(" process_cpu_ns=")
                .expect("the process's CPU");
            assert_eq!(
                said,
                format!("round counts: rounds={round} pulls={}", 3 * round)
            );
            let _: u64 = cpu.parse().expect("a number");
            due.push(round);
        }
    }
    assert_eq!(due, [1, 2, 4, 8]);
    counts.tend_cpu(Phase::Pull).fetch_add(7, Ordering::Relaxed);
    counts.conn_threads.ended.fetch_add(2, Ordering::Relaxed);
    counts.resolve_threads.ended.fetch_add(5, Ordering::Relaxed);
    assert_eq!(
        counts.line_at(11),
        "round counts: rounds=9 pulls=27 tend_pull_cpu_ns=7 resolve_threads=5 conn_threads=2 \
         process_cpu_ns=11"
    );
}

/// A thread started to resolve a name is counted for the server whose round its starter runs,
/// as it ends, and for none where its starter runs no round; a round on a thread that already
/// runs one changes nothing, and the round's end takes the server off the thread.
///
/// Negative controls: with [`tending`] putting nothing on the thread (`Tended(false)` at once),
/// the three threads started in the round are counted nowhere (`left: 0, right: 3`); with a
/// guard whose drop leaves the server on the thread, the two started after the round are
/// counted too (`left: 6, right: 4`).
#[test]
fn a_resolve_thread_is_counted_for_the_round_that_started_it() {
    let counts = Arc::new(ServeCounts::default());
    let ended = |counts: &ServeCounts| counts.resolve_threads.ended.load(Ordering::Relaxed);
    let start = |threads: usize| {
        let resolving = resolving();
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| resolving.run(|| ()));
            }
        });
    };
    start(2);
    assert_eq!(ended(&counts), 0);
    {
        let _round = tending(&counts);
        start(3);
        assert_eq!(ended(&counts), 3);
        let other = Arc::new(ServeCounts::default());
        let _inner = tending(&other);
        start(1);
        assert_eq!((ended(&counts), ended(&other)), (4, 0));
    }
    start(2);
    assert_eq!(ended(&counts), 4);
}
