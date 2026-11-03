// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shared writer's batching: how it takes a streaming reader's slices ([`super::serve`]).

use super::urgency::{Demands, Reason};
use super::{Inbox, serve};
use std::collections::VecDeque;

/// An inbox whose messages arrive on a script: each wait on the channel, and each nap, lets the
/// next arrival reach the queue, the way a reader streaming output keeps queueing slices while
/// the writer is busy or napping.
struct Script {
    arrivals: VecDeque<Vec<u32>>,
    queued: VecDeque<u32>,
    waits: usize,
    naps: usize,
}

impl Script {
    fn new(arrivals: impl IntoIterator<Item = Vec<u32>>) -> Self {
        Self {
            arrivals: arrivals.into_iter().collect(),
            queued: VecDeque::new(),
            waits: 0,
            naps: 0,
        }
    }

    fn arrive(&mut self) {
        if let Some(batch) = self.arrivals.pop_front() {
            self.queued.extend(batch);
        }
    }
}

impl Inbox<u32> for Script {
    fn wait(&mut self) -> Option<u32> {
        self.waits += 1;
        if self.queued.is_empty() {
            self.arrive();
        }
        self.queued.pop_front()
    }

    fn take(&mut self) -> Option<u32> {
        self.queued.pop_front()
    }

    fn nap(&mut self) {
        self.naps += 1;
        self.arrive();
    }
}

#[test]
fn a_streaming_reader_queues_batches_and_the_writer_waits_on_the_channel_only_when_it_stops() {
    // Two queued slices start a batch; six more arrive during naps, then the stream stops.
    let mut inbox = Script::new(
        [vec![1, 2]]
            .into_iter()
            .chain((3..=8).map(|n| vec![n]))
            .chain([vec![]]),
    );
    let mut written = Vec::new();
    serve(&mut inbox, || false, |n| written.push(n));
    assert_eq!(
        written,
        (1..=8).collect::<Vec<_>>(),
        "every slice, in order"
    );
    assert_eq!(
        inbox.waits, 2,
        "the writer waited on the channel, where each send wakes it, only to start and once the \
         stream stopped, not once per slice"
    );
    assert_eq!(
        inbox.naps, 7,
        "it napped after emptying the backlog each time, the last nap bringing nothing"
    );
}

#[test]
fn a_batch_queued_during_a_nap_is_written_whole_before_the_next_nap() {
    let mut inbox = Script::new([vec![1, 2], vec![3, 4], vec![5, 6], vec![]]);
    let mut written = Vec::new();
    serve(&mut inbox, || false, |n| written.push(n));
    assert_eq!(written, vec![1, 2, 3, 4, 5, 6]);
    assert_eq!((inbox.waits, inbox.naps), (2, 3));
}

#[test]
fn isolated_arrivals_never_schedule_a_batch_nap() {
    let mut inbox = Script::new((1..=4).map(|n| vec![n]));
    let mut written = Vec::new();
    serve(&mut inbox, || false, |n| written.push(n));
    assert_eq!(written, vec![1, 2, 3, 4]);
    assert_eq!((inbox.waits, inbox.naps), (5, 0));
}

#[test]
fn an_empty_nap_ends_batching_before_the_next_isolated_arrival() {
    let mut inbox = Script::new([vec![1, 2], vec![3], vec![], vec![4]]);
    let mut written = Vec::new();
    serve(&mut inbox, || false, |n| written.push(n));
    assert_eq!(written, vec![1, 2, 3, 4]);
    assert_eq!((inbox.waits, inbox.naps), (3, 2));
}

#[test]
fn an_urgent_writer_never_naps_and_waits_on_the_channel_for_each_arrival() {
    let mut inbox = Script::new([vec![1, 2], vec![3, 4]]);
    let mut written = Vec::new();
    serve(&mut inbox, || true, |n| written.push(n));
    assert_eq!(written, vec![1, 2, 3, 4]);
    assert_eq!(
        (inbox.waits, inbox.naps),
        (3, 0),
        "a drain, a view or queue pressure is served as it arrives, never after a nap"
    );
}

#[test]
fn each_demand_requests_a_wake_after_publishing_urgency() {
    let demands = Demands::new();
    let writer = std::thread::current();
    let mut woken = Vec::new();
    let early = demands.acquire_with(Reason::Drain, |thread| woken.push(thread.id()));
    assert!(woken.is_empty(), "no writer has adopted the demands yet");
    drop(early);

    demands.adopt_writer(writer.clone());
    let mut held = Vec::new();
    for (index, reason) in [Reason::Drain, Reason::View, Reason::Pressure]
        .into_iter()
        .enumerate()
    {
        held.push(demands.acquire_with(reason, |thread| {
            assert!(demands.is_urgent(), "publish urgency before waking");
            assert_eq!(demands.snapshot().counts.iter().sum::<usize>(), index + 1);
            woken.push(thread.id());
        }));
    }
    assert_eq!(woken, vec![writer.id(); 3], "every demand wakes its writer");
    drop(held);
    assert!(!demands.is_urgent());
}
