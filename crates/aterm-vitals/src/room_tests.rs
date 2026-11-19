// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::model::{Resource, Sample, State, Vitals};
use crate::words::{InkKind, Mark, well};

fn pairs(values: [(Option<u64>, Option<u64>); 4]) -> TaskRoom {
    TaskRoom {
        procs: values[0].0,
        procs_max: values[0].1,
        user_procs: values[1].0,
        user_procs_max: values[1].1,
        threads: values[2].0,
        threads_max: values[2].1,
        files: values[3].0,
        files_max: values[3].1,
    }
}

fn processes(count: u64, cap: u64) -> TaskRoom {
    TaskRoom {
        procs: Some(count),
        procs_max: Some(cap),
        ..TaskRoom::default()
    }
}

#[test]
fn processes_room_is_the_largest_of_four_shares_and_names_its_limit() {
    for (index, limit) in [
        Limit::Processes,
        Limit::UserProcesses,
        Limit::Threads,
        Limit::Files,
    ]
    .into_iter()
    .enumerate()
    {
        let mut values = [
            (Some(500), Some(1000)),
            (Some(1), Some(20)),
            (Some(30), Some(100)),
            (Some(4), Some(10)),
        ];
        values[index] = (Some(91), Some(100));
        let reading = Tracker::default().absorb(pairs(values)).unwrap();
        assert_eq!(
            (reading.limit(), reading.count(), reading.cap()),
            (limit, 91, 100)
        );
        assert!(reading.near());
        assert_eq!(reading.share(), 0.91);
    }

    let mut tracker = Tracker::default();
    let zero = tracker
        .absorb(pairs([
            (Some(u64::MAX), None), // Unknown or unlimited: not a finite pair.
            (None, Some(1)),
            (Some(100), Some(0)),
            (Some(0), Some(100)),
        ]))
        .unwrap();
    assert_eq!(
        (zero.limit(), zero.count(), zero.share(), zero.near()),
        (Limit::Files, 0, 0.0, false)
    );
    assert_eq!(tracker.absorb(TaskRoom::default()), None);
    assert_eq!(tracker.absorb(processes(100, 0)), None);

    let tied = Tracker::default()
        .absorb(pairs([
            (Some(1), Some(2)),
            (Some(2), Some(4)),
            (Some(3), Some(6)),
            (Some(4), Some(8)),
        ]))
        .unwrap();
    assert_eq!(
        tied.limit(),
        Limit::Processes,
        "equal shares keep the fixed limit order"
    );
    let exceeded = Tracker::default().absorb(processes(120, 100)).unwrap();
    assert_eq!(
        (exceeded.count(), exceeded.cap(), exceeded.share()),
        (120, 100, 1.2)
    );

    let precise = Tracker::default()
        .absorb(pairs([
            (Some(u64::MAX - 1), Some(u64::MAX)),
            (None, None),
            (None, None),
            (Some(u64::MAX), Some(u64::MAX)),
        ]))
        .unwrap();
    assert_eq!(
        precise.limit(),
        Limit::Files,
        "a rounded f64 tie would keep Processes despite its smaller exact share"
    );

    let cap = u64::MAX - u64::MAX % 100;
    let raised = cap / 100 * 90;
    let cleared = cap / 100 * 85;
    let mut tracker = Tracker::default();
    assert!(!tracker.absorb(processes(raised - 1, cap)).unwrap().near());
    assert!(tracker.absorb(processes(raised, cap)).unwrap().near());
    assert!(tracker.absorb(processes(cleared, cap)).unwrap().near());
    assert!(
        !tracker.absorb(processes(cleared - 1, cap)).unwrap().near(),
        "threshold products must not overflow or round at large finite limits"
    );
}

#[test]
fn processes_room_raises_the_dot_and_clears_with_hysteresis() {
    let mut model = Vitals::new();
    let cases = [
        (processes(899, 1000), Some(false)),
        (processes(900, 1000), Some(true)),
        (processes(899, 1000), Some(true)),
        (processes(850, 1000), Some(true)),
        (TaskRoom::default(), None),
        (
            TaskRoom {
                threads: Some(875),
                threads_max: Some(1000),
                ..TaskRoom::default()
            },
            Some(true),
        ),
        (
            TaskRoom {
                files: Some(849),
                files_max: Some(1000),
                ..TaskRoom::default()
            },
            Some(false),
        ),
        (
            TaskRoom {
                user_procs: Some(875),
                user_procs_max: Some(1000),
                ..TaskRoom::default()
            },
            Some(false),
        ),
        (processes(900, 1000), Some(true)),
        (processes(0, 1000), Some(false)),
    ];
    let mut held = None;
    for (index, (room, near)) in cases.into_iter().enumerate() {
        let at = u64::try_from(index).unwrap() * 1000;
        let mut snapshot = model.absorb(&Sample {
            at_ns: at * 1_000_000,
            at_unix_ms: i64::try_from(at).unwrap(),
            mono_ms: at,
            room,
            ..Sample::default()
        });
        assert_eq!(snapshot.facts.room, room, "the raw reading stays unchanged");
        assert_eq!(
            snapshot.facts.process_room.map(Reading::near),
            near,
            "reading {index}"
        );
        let state = near.filter(|near| *near).map(|_| State::Warn);
        let mark = state.map(|_| (Mark::Dot, InkKind::Warn));
        assert_eq!(
            snapshot.resources[Resource::Processes.index()].state,
            state,
            "Room supplies the state before any Jobs publication at reading {index}"
        );
        for jobs in [
            Some(std::sync::Arc::new(crate::jobs::JobsView::default())),
            None,
        ] {
            snapshot.set_jobs(jobs);
            let row = &snapshot.resources[Resource::Processes.index()];
            assert_eq!(
                row.state, state,
                "Jobs cannot erase Room at reading {index}"
            );
            assert_eq!(well(Resource::Processes, row), mark, "reading {index}");
        }
        if index == 1 {
            held = snapshot.facts.process_room;
        }
    }
    let original = held.unwrap();
    assert_eq!(
        (
            original.limit(),
            original.count(),
            original.cap(),
            original.near()
        ),
        (Limit::Processes, 900, 1000, true),
        "a later reading cannot restamp an already published binding pair"
    );
    let fresh = Vitals::new().absorb(&Sample {
        room: processes(875, 1000),
        ..Sample::default()
    });
    assert!(
        !fresh.facts.process_room.unwrap().near(),
        "a new model has no inherited latch"
    );
}
