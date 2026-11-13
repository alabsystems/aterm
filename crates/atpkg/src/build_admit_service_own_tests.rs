// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn publication(revision: u64, first: u64, count: usize) -> Observer {
    Observer(
        Arc::new(Mutex::new(Publication {
            snapshot: Arc::new(Snapshot {
                generation: Generation([8; 16]),
                revision,
                closed: false,
                jobs: Vec::new(),
                epochs: Vec::new(),
            }),
            at: Instant::now(),
            events: (0..count)
                .map(|index| Record {
                    revision: first + index as u64,
                    attempt: None,
                    event: Event::StateChanged,
                })
                .collect(),
            limit: 1024,
            exhausted: false,
        })),
        Arc::new(Condvar::new()),
    )
}

#[test]
fn bounded_pages_report_exact_retention_gap_without_skipping_to_snapshot() {
    let observer = publication(200, 101, 100);
    let first = observer
        .read(Some(Cursor {
            generation: Generation([8; 16]),
            revision: 3,
        }))
        .unwrap();
    assert_eq!(
        first.gap,
        Some(Gap {
            first: 4,
            last: 100
        })
    );
    assert_eq!(first.events.len(), READ_PAGE);
    assert_eq!(first.events[0].revision, 101);
    assert_eq!(first.next.revision, 164);
    assert_eq!(first.snapshot.revision, 200);
    let second = observer.read(Some(first.next)).unwrap();
    assert_eq!(second.events.len(), 36);
    assert_eq!(second.events[0].revision, 165);
    assert_eq!(second.next.revision, 200);
    assert!(second.gap.is_none());
    assert_eq!(second.observed_at, first.observed_at);
    assert!(observer.read(Some(second.next)).unwrap().events.is_empty());
    // Subscribing later does not pretend the old snapshot was just observed.
    assert_eq!(
        observer.clone().read(None).unwrap().observed_at,
        first.observed_at
    );
}

#[test]
fn empty_initial_and_max_cursors_do_not_overflow_or_invent_events() {
    let observer = publication(0, 0, 0);
    let read = observer.read(None).unwrap();
    assert_eq!(read.next.revision, 0);
    assert!(read.events.is_empty());
    let observer = publication(u64::MAX, u64::MAX, 1);
    let read = observer.read(None).unwrap();
    assert_eq!(read.next.revision, u64::MAX);
    assert!(observer.read(Some(read.next)).unwrap().events.is_empty());
    observer.0.lock().unwrap().exhausted = true;
    assert_eq!(observer.read(None).unwrap_err(), ReadError::Exhausted);
}

#[test]
fn actual_owner_eviction_closure_and_exhaustion_keep_original_observation() {
    let owner = Owner::new(1, 2).unwrap();
    let observer = owner.observer();
    owner.publish(vec![(None, Event::StateChanged); 5]).unwrap();
    let read = observer
        .read(Some(Cursor {
            generation: owner.generation,
            revision: 0,
        }))
        .unwrap();
    assert_eq!(read.gap, Some(Gap { first: 1, last: 3 }));
    assert_eq!(read.events.len(), 2);
    let at = read.observed_at;
    drop(owner);
    let read = observer.read(Some(read.next)).unwrap();
    assert!(read.snapshot.closed);
    assert_eq!(read.observed_at, at);
    assert_eq!(read.events.len(), 1);
    assert!(matches!(read.events[0].event, Event::OwnerClosed));

    let owner = Owner::new(1, 2).unwrap();
    let observer = owner.observer();
    observer.0.lock().unwrap().snapshot = Arc::new(Snapshot {
        generation: owner.generation,
        revision: u64::MAX,
        closed: false,
        jobs: Vec::new(),
        epochs: Vec::new(),
    });
    owner.publish(vec![(None, Event::StateChanged)]).unwrap();
    assert_eq!(observer.read(None).unwrap_err(), ReadError::Exhausted);
    assert!(owner.is_empty());
}
