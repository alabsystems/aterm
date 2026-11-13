// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn simultaneous_resource_shortfalls_remain_distinct() {
    use aterm_capacity::admit::{Short, Shortfall};
    let reason = Reason::ledger(&Shortfall {
        shorts: vec![Short::Pressure, Short::Cores, Short::Turn { holder: None }],
        holders: Vec::new(),
    });
    assert_eq!(reason.kinds, ["core", "ledger", "memory"]);
    let b = Planned {
        index: 0,
        width: 2,
        charge: 2,
        peak: 4,
        predicted: None,
    };
    let reason = Reason::local(
        &b,
        DispatchResources {
            held: 3,
            share: 4,
            held_bytes: 3,
            memory: Some(5),
            live: 3.0,
        },
    );
    assert_eq!(reason.kinds, ["core", "memory"]);
}

#[test]
fn wait_transitions_cover_queue_time_without_counting_execution_or_splitting_each_poll() {
    let mut book = Book(vec![Some(Waiting {
        since: Duration::ZERO,
        first_detail: "ordered".into(),
        reason: Reason::ordered(),
    })]);
    let first = book
        .change(0, Some(Reason::fifo()), Duration::from_secs(2))
        .unwrap();
    assert_eq!((first.from.as_secs(), first.to.as_secs()), (0, 2));
    assert!(
        book.change(0, Some(Reason::fifo()), Duration::from_secs(3))
            .is_none()
    );
    let second = book
        .change(
            0,
            Some(Reason::unreachable("memory need cannot fit".into())),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(second.reason.kinds, ["core"]);
    assert_eq!((second.from.as_secs(), second.to.as_secs()), (2, 5));
    let third = book.change(0, None, Duration::from_secs(9)).unwrap();
    assert_eq!((third.from.as_secs(), third.to.as_secs()), (5, 9));
    assert!(
        book.change(0, None, Duration::from_secs(60)).is_none(),
        "execution and a repeated final flush cannot become queue time"
    );
}
