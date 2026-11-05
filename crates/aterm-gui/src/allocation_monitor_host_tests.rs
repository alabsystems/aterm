// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual private fenced sources and deterministic attachment replacement.

use super::*;
use aterm_capacity::{cohort_namespace::Namespace, leadership::Fence};
use atpkg::build_admit::service::{
    Owner,
    monitor::{Exchange, Progress},
};
use std::{
    os::unix::fs::DirBuilderExt as _,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Source {
    owner: Option<Owner>,
    source: atpkg::build_admit::service::monitor::Source,
    generation: Generation,
    _world: aterm_tempfile::TempDir,
}

impl Source {
    fn new() -> Self {
        Self::with_history(4)
    }
    fn with_history(history: usize) -> Self {
        let world = aterm_tempfile::tempdir().unwrap();
        let home = world.path().join("home");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        let namespace = Namespace::create_private(world.path(), &home).unwrap();
        let fence = Fence::acquire(&namespace, Instant::now()).unwrap();
        let generation = fence.generation();
        let owner = Owner::fenced(fence, 1, history).unwrap();
        let source = owner.monitor_source().unwrap();
        Self {
            _world: world,
            owner: Some(owner),
            source,
            generation,
        }
    }
    fn pair(&self) -> (Client, Exchange) {
        self.source
            .pair(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
            .unwrap()
    }
    fn publish(&mut self) {
        let owner = self.owner.as_mut().unwrap();
        owner
            .controller()
            .cancel(
                aterm_capacity::host_protocol::Attempt {
                    generation: self.generation,
                    sequence: 1,
                },
                atpkg::build_admit::CancelRequest::Terminate,
            )
            .unwrap();
        assert!(
            owner
                .tick(
                    &mut |_, _| Ok(atpkg::build_admit::JobControl::Wait),
                    &mut |_| {},
                )
                .unwrap()
                .is_empty()
        );
    }
    fn close(&mut self) {
        drop(self.owner.take());
    }
}

fn serve(mut exchange: Exchange) -> io::Result<()> {
    while exchange.step()? == Progress::Pending {
        exchange.wait(Instant::now() + aterm_spec::wait::MUST_HAPPEN)?;
    }
    Ok(())
}
fn until(host: &Host, found: impl Fn(&Presentation) -> bool) -> Arc<Presentation> {
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "monitor host publication",
        || {
            let view = host.view();
            found(&view).then_some(view)
        },
    )
}
fn notify() -> Notify {
    Notify::Callback(Arc::new(|| {}))
}

#[test]
fn actual_fenced_read_is_immutable_and_failed_replacement_retains_original() {
    let source = Source::new();
    let host = Host::default();
    let wakes = Arc::new(AtomicUsize::new(0));
    let wake_count = Arc::clone(&wakes);
    let callback = Notify::Callback(Arc::new(move || {
        wake_count.fetch_add(1, Ordering::AcqRel);
    }));
    let (client, server) = source.pair();
    let first = host.attach(client, source.generation, callback).unwrap();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server));
        let view = until(&host, |view| view.phase == Phase::Observed);
        assert_eq!(view.requested, first);
        assert_eq!(view.last.as_ref().unwrap().attachment, first);
        assert_eq!(
            view.last.as_ref().unwrap().observation.snapshot.generation,
            source.generation
        );
        assert_eq!(
            wakes.load(Ordering::Acquire),
            1,
            "phase wakes coalesce before UI consumption"
        );
        let original = Arc::clone(&host.take_view().last.as_ref().unwrap().observation);
        let original_age = original.age_bounds().unwrap();
        let (client, refused) = source.pair();
        drop(refused);
        let replacement = host.attach(client, source.generation, notify()).unwrap();
        let failed = until(&host, |view| matches!(view.phase, Phase::Failed(_)));
        assert_eq!(failed.requested, replacement);
        let retained = failed.last.as_ref().unwrap();
        assert_eq!(retained.attachment, first);
        assert!(Arc::ptr_eq(&retained.observation, &original));
        assert!(retained.observation.age_bounds().unwrap().lower >= original_age.lower);
        // Replacement shuts down the original socket at an arbitrary I/O
        // boundary. EOF or a local framing/write error both end that reader;
        // neither is a native execution failure.
        let _ = server.join().unwrap();
    });
}

#[test]
fn replacement_cancels_old_socket_and_late_result_cannot_paint_new_attachment() {
    let a = Source::new();
    let mut b = Source::new();
    b.close();
    let host = Host::default();
    let (client_a, mut server_a) = a.pair();
    let first = host.attach(client_a, a.generation, notify()).unwrap();
    until(&host, |view| view.phase == Phase::Reading);
    let (client_b, server_b) = b.pair();
    let replacement = host.attach(client_b, b.generation, notify()).unwrap();
    assert!(replacement > first);
    std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server_b));
        let view = until(&host, |view| {
            assert_eq!(view.requested, replacement);
            assert!(
                !matches!(view.phase, Phase::Failed(_)),
                "A failure must not paint B"
            );
            view.phase == Phase::Observed
        });
        assert_eq!(view.desired_generation, Some(b.generation));
        assert_eq!(view.last.as_ref().unwrap().attachment, replacement);
        assert_eq!(
            view.last.as_ref().unwrap().observation.snapshot.generation,
            b.generation
        );
        server.join().unwrap().unwrap();
    });
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "replaced monitor peer",
        || match server_a.step() {
            Ok(Progress::Pending) => None,
            _ => Some(()),
        },
    );

    // Deliver an actual old-source observation after replacement B completed.
    let (client, server) = a.pair();
    let old = std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server));
        let observation = client
            .read_until(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        server.join().unwrap().unwrap();
        observation
    });
    let stale = complete(&host.0.shared, first, Ok(old));
    assert!(stale.notify.is_none());
    assert!(!stale.keep_reading);
    let view = host.view();
    assert_eq!(view.requested, replacement);
    assert_eq!(
        view.last.as_ref().unwrap().observation.snapshot.generation,
        b.generation
    );
}

#[test]
fn callbacks_run_outside_state_lock_and_last_host_drop_cancels_without_join() {
    let source = Source::new();
    let host = Host::default();
    let weak = Arc::downgrade(&host.0);
    let (client, mut server) = source.pair();
    let called = Arc::new(AtomicUsize::new(0));
    let callbacks = Arc::clone(&called);
    let callback = Notify::Callback(Arc::new(move || {
        let control = weak.upgrade().unwrap();
        // The worker may hold this lock on another thread. Acquiring it proves
        // reentrancy; try_lock would also reject ordinary concurrent publication.
        let _state = control.shared.state.lock().unwrap();
        callbacks.fetch_add(1, Ordering::AcqRel);
    }));
    // If attach itself held the state lock during the callback, this completion
    // would never arrive. Bound that deadlock without a timing assertion about
    // how quickly another thread releases the lock.
    let (completed, completion) = std::sync::mpsc::sync_channel(1);
    let attaching = host.clone();
    let generation = source.generation;
    let attacher = std::thread::spawn(move || {
        completed
            .send(attaching.attach(client, generation, callback))
            .unwrap();
    });
    completion
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("monitor callback must return without holding the state lock")
        .unwrap();
    attacher.join().unwrap();
    until(&host, |view| view.phase == Phase::Reading);
    assert!(called.load(Ordering::Acquire) > 0);
    let shared = Arc::clone(&host.0.shared);
    drop(host);
    assert!(shared.state.lock().unwrap().stopped);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "cancelled monitor peer",
        || match server.step() {
            Ok(Progress::Pending) => None,
            _ => Some(()),
        },
    );
    let (_client, _server) = source.pair();
}

#[test]
fn retiring_last_host_capturing_callback_releases_state_before_control_drop() {
    let mut source = Source::new();
    source.close();
    let host = Host::default();
    let captured = host.clone();
    let callback = Notify::Callback(Arc::new(move || {
        std::hint::black_box(&captured);
    }));
    let shared = Arc::clone(&host.0.shared);
    let weak = Arc::downgrade(&host.0);
    let (client, server) = source.pair();
    host.attach(client, source.generation, callback).unwrap();
    assert!(
        Arc::strong_count(&host.0) > 1,
        "the notifier really retains a Host"
    );
    drop(host);
    std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server));
        server.join().unwrap().unwrap();
    });
    // No UI consumption acknowledges the initial wake. Completion therefore
    // retires the last callback without cloning it as another notification.
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "captured Host clean shutdown",
        || {
            shared
                .state
                .try_lock()
                .ok()
                .and_then(|state| state.stopped.then_some(()))
        },
    );
    assert!(weak.upgrade().is_none());
}

#[test]
fn exhausted_revision_refuses_without_replacing_prior_state() {
    let source = Source::new();
    let host = Host::default();
    {
        let mut state = host.0.shared.state.lock().unwrap();
        let view = Presentation {
            requested: u64::MAX,
            ..Presentation::default()
        };
        state.view = Arc::new(view);
    }
    let (client, _server) = source.pair();
    assert!(host.attach(client, source.generation, notify()).is_err());
    assert_eq!(host.view().requested, u64::MAX);
    assert_eq!(host.view().phase, Phase::Unattached);
}

#[test]
fn persistent_updates_keep_attachment_until_source_closes() {
    let mut source = Source::with_history(128);
    let host = Host::default();
    let (client, server) = source.pair();
    let attachment = host.attach(client, source.generation, notify()).unwrap();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server));
        until(&host, |view| view.phase == Phase::Observed);
        source.publish();
        let updated = until(&host, |view| {
            view.last
                .as_ref()
                .is_some_and(|last| last.observation.snapshot.revision > 0)
        });
        assert_eq!(updated.requested, attachment);
        assert_eq!(updated.last.as_ref().unwrap().attachment, attachment);
        assert_eq!(
            updated
                .last
                .as_ref()
                .unwrap()
                .observation
                .snapshot
                .generation,
            source.generation
        );
        source.close();
        let closed = until(&host, |view| {
            view.last.as_ref().is_some_and(|last| {
                last.observation.snapshot.closed
                    && last.observation.next.revision == last.observation.snapshot.revision
            })
        });
        let reading = closed.last.as_ref().unwrap();
        assert_eq!(closed.requested, attachment);
        assert_eq!(reading.attachment, attachment);
        assert_eq!(closed.phase, Phase::Observed);
        assert!(reading.observation.request_sequence > 1);
        let state = host.0.shared.state.lock().unwrap();
        assert!(state.active.is_none());
        assert!(state.cancel.is_none());
        assert!(state.notify.is_none());
        drop(state);
        server.join().unwrap().unwrap();
    });
}

#[test]
fn closed_source_drains_every_retained_history_page_before_stopping() {
    let mut source = Source::with_history(128);
    for _ in 0..130 {
        source.publish();
    }
    source.close();
    // The first read has a closed snapshot but only64 of128 retained records.
    // A worker that stops at snapshot.closed loses the remaining page.
    let host = Host::default();
    let (client, server) = source.pair();
    let attachment = host.attach(client, source.generation, notify()).unwrap();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server));
        let closed = until(&host, |view| {
            view.last.as_ref().is_some_and(|last| {
                last.observation.snapshot.closed
                    && last.observation.next.revision == last.observation.snapshot.revision
            })
        });
        let reading = closed.last.as_ref().unwrap();
        assert_eq!(reading.attachment, attachment);
        assert_eq!(reading.observation.snapshot.revision, 131);
        assert_eq!(reading.observation.request_sequence, 2);
        assert_eq!(closed.phase, Phase::Observed);
        assert!(host.0.shared.state.lock().unwrap().active.is_none());
        server.join().unwrap().unwrap();
    });
}

#[test]
fn unchanged_heartbeat_keeps_original_arc_age_and_does_not_wake() {
    let source = Source::new();
    let host = Host::default();
    {
        let mut state = host.0.shared.state.lock().unwrap();
        state.view = Arc::new(Presentation {
            requested: 1,
            desired_generation: Some(source.generation),
            phase: Phase::Reading,
            last: None,
        });
        state.active = Some(1);
        state.notify = Some(notify());
    }
    let (mut client, server) = source.pair();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server));
        let first = client
            .read_next(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        let sequence = first.request_sequence;
        let first_delivery = complete(&host.0.shared, 1, Ok(first));
        assert!(first_delivery.keep_reading);
        assert!(first_delivery.notify.is_some());
        let original = host.take_view();
        let original_age = original
            .last
            .as_ref()
            .unwrap()
            .observation
            .age_bounds()
            .unwrap();
        let heartbeat = client
            .read_next(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        assert!(heartbeat.request_sequence > sequence);
        let delivered = complete(&host.0.shared, 1, Ok(heartbeat));
        assert!(delivered.keep_reading);
        assert!(delivered.notify.is_none());
        assert!(Arc::ptr_eq(&original, &host.view()));
        assert!(
            host.view()
                .last
                .as_ref()
                .unwrap()
                .observation
                .age_bounds()
                .unwrap()
                .lower
                >= original_age.lower
        );
        drop(client);
        server.join().unwrap().unwrap();
    });
}

#[test]
fn reported_gap_survives_later_pages_without_a_new_gap() {
    let mut source = Source::with_history(2);
    let host = Host::default();
    {
        let mut state = host.0.shared.state.lock().unwrap();
        state.view = Arc::new(Presentation {
            requested: 1,
            desired_generation: Some(source.generation),
            phase: Phase::Reading,
            last: None,
        });
        state.active = Some(1);
    }
    let (mut client, server) = source.pair();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server));
        let first = client
            .read_next(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        complete(&host.0.shared, 1, Ok(first));
        for _ in 0..3 {
            source.publish();
        }
        let overflow = client
            .read_next(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        assert_eq!(overflow.gap, Some(Gap { first: 1, last: 1 }));
        complete(&host.0.shared, 1, Ok(overflow));
        source.publish();
        let later = client
            .read_next(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        assert!(later.gap.is_none());
        complete(&host.0.shared, 1, Ok(later));
        let retained = host.view();
        assert_eq!(
            retained.last.as_ref().unwrap().last_gap,
            Some(Gap { first: 1, last: 1 })
        );
        assert_eq!(retained.last.as_ref().unwrap().observation.next.revision, 4);
        drop(client);
        server.join().unwrap().unwrap();
    });
}

fn receiving(generation: Generation, attachment: u64) -> Host {
    let host = Host::default();
    let mut state = host.0.shared.state.lock().unwrap();
    state.view = Arc::new(Presentation {
        requested: attachment,
        desired_generation: Some(generation),
        phase: Phase::Reading,
        last: None,
    });
    state.active = Some(attachment);
    drop(state);
    host
}

// These GUI projection controls use real fenced monitor pages and their actual
// cursors, generations, ages and retention gaps. Substitute terminal kinds only
// after decoding, at the GUI delivery boundary; real native outcomes are covered
// by the maintained allocation-monitor window qualification.
fn terminal_page(mut observation: Observation) -> Observation {
    for record in &mut observation.history {
        if record.kind == EventKind::Rejected {
            record.kind = match record.revision % 4 {
                0 => EventKind::Finished {
                    succeeded: true,
                    cancelled: false,
                },
                1 => EventKind::Finished {
                    succeeded: false,
                    cancelled: false,
                },
                2 => EventKind::Finished {
                    succeeded: false,
                    cancelled: true,
                },
                _ => EventKind::Failed,
            };
            record.attempt = if record.kind == EventKind::Failed {
                None
            } else {
                Some(record.revision % 3 + 1)
            };
        }
    }
    observation
}

fn first_page(source: &Source) -> Observation {
    let (client, server) = source.pair();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server));
        let observation = client
            .read_until(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        server.join().unwrap().unwrap();
        observation
    })
}

#[test]
fn terminal_history_collects_closed_pages_before_the_snapshot_cursor() {
    let mut source = Source::with_history(256);
    for _ in 0..130 {
        source.publish();
    }
    source.close();
    let host = receiving(source.generation, 1);
    let (mut client, server) = source.pair();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server));
        let mut expected = Vec::new();
        for (page, next) in [(1, 64), (2, 128), (3, 131)] {
            let observation = terminal_page(
                client
                    .read_next(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
                    .unwrap(),
            );
            assert!(observation.snapshot.closed);
            assert_eq!(observation.snapshot.revision, 131);
            assert_eq!(observation.next.revision, next);
            expected.extend(
                observation
                    .history
                    .iter()
                    .filter(|row| {
                        matches!(row.kind, EventKind::Finished { .. } | EventKind::Failed)
                    })
                    .cloned(),
            );
            let delivery = complete(&host.0.shared, 1, Ok(observation));
            assert_eq!(delivery.keep_reading, page != 3);
            let view = host.view();
            let last = view.last.as_ref().unwrap();
            let retained = expected.len().min(64);
            assert_eq!(last.recent.as_ref(), &expected[expected.len() - retained..]);
            assert_eq!(last.omitted_terminals, (expected.len() - retained) as u64);
            assert!(last.last_gap.is_none());
        }
        let view = host.view();
        let last = view.last.as_ref().unwrap();
        assert_eq!(last.recent.len(), 64);
        assert_eq!(last.recent.first().unwrap().revision, 67);
        assert_eq!(last.recent.last().unwrap().revision, 130);
        assert_eq!(last.omitted_terminals, 66);
        assert!(last.recent.iter().any(|row| row.attempt.is_none()));
        assert!(host.0.shared.state.lock().unwrap().active.is_none());
        drop(client);
        server.join().unwrap().unwrap();
    });
}

#[test]
fn terminal_revision_dedup_survives_local_eviction_and_keeps_heartbeat_age() {
    let mut source = Source::with_history(256);
    for _ in 0..70 {
        source.publish();
    }
    let host = receiving(source.generation, 1);
    let (mut client, server) = source.pair();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server));
        let first = terminal_page(
            client
                .read_next(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
                .unwrap(),
        );
        let old = first.history.clone();
        complete(&host.0.shared, 1, Ok(first));
        let mut overlapping = terminal_page(
            client
                .read_next(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
                .unwrap(),
        );
        assert_eq!(overlapping.next.revision, 70);
        overlapping.history.insert(0, old[63].clone());
        overlapping.history.insert(0, old[0].clone());
        overlapping
            .history
            .insert(3, overlapping.history[2].clone());
        complete(&host.0.shared, 1, Ok(overlapping));
        let retained = host.view();
        let last = retained.last.as_ref().unwrap();
        assert_eq!(last.recent.len(), 64);
        assert_eq!(last.omitted_terminals, 6);
        assert_eq!(
            last.recent
                .iter()
                .map(|row| row.revision)
                .collect::<Vec<_>>(),
            (7..=70).collect::<Vec<_>>()
        );
        let recent = Arc::clone(&last.recent);

        source.publish();
        let mut nonterminal = client
            .read_next(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        assert_eq!(nonterminal.next.revision, 71);
        // An already-evicted revision and a still-retained revision must both
        // stay excluded by the delivered cursor, without another local eviction.
        nonterminal.history.insert(0, old[9].clone());
        nonterminal.history.insert(0, old[0].clone());
        complete(&host.0.shared, 1, Ok(nonterminal));
        let before_heartbeat = host.take_view();
        let last = before_heartbeat.last.as_ref().unwrap();
        assert!(Arc::ptr_eq(&recent, &last.recent));
        assert_eq!(last.omitted_terminals, 6);
        let original_age = last.observation.age_bounds().unwrap();
        let original_observation = Arc::clone(&last.observation);
        let heartbeat = client
            .read_next(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        assert!(heartbeat.request_sequence > original_observation.request_sequence);
        assert!(heartbeat.history.is_empty());
        let delivered = complete(&host.0.shared, 1, Ok(heartbeat));
        assert!(delivered.keep_reading);
        assert!(delivered.notify.is_none());
        let after_heartbeat = host.view();
        assert!(Arc::ptr_eq(&before_heartbeat, &after_heartbeat));
        let last = after_heartbeat.last.as_ref().unwrap();
        assert!(Arc::ptr_eq(&original_observation, &last.observation));
        assert!(Arc::ptr_eq(&recent, &last.recent));
        assert!(last.observation.age_bounds().unwrap().lower >= original_age.lower);
        drop(client);
        server.join().unwrap().unwrap();
    });
}

#[test]
fn terminal_local_truncation_stays_separate_from_the_latest_source_gap() {
    let mut source = Source::with_history(128);
    let host = receiving(source.generation, 1);
    let (mut client, server) = source.pair();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server));
        complete(
            &host.0.shared,
            1,
            Ok(client
                .read_next(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
                .unwrap()),
        );
        for _ in 0..200 {
            source.publish();
        }
        let overflow = terminal_page(
            client
                .read_next(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
                .unwrap(),
        );
        let producer_gap = Some(Gap { first: 1, last: 72 });
        assert_eq!(overflow.gap, producer_gap);
        complete(&host.0.shared, 1, Ok(overflow));
        let first = host.view();
        let last = first.last.as_ref().unwrap();
        assert_eq!(last.recent.len(), 64);
        assert_eq!(last.recent.first().unwrap().revision, 73);
        assert_eq!(last.omitted_terminals, 0);
        let later = terminal_page(
            client
                .read_next(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
                .unwrap(),
        );
        assert!(later.gap.is_none());
        complete(&host.0.shared, 1, Ok(later));
        let last = host.view().last.clone().unwrap();
        assert_eq!(last.last_gap, producer_gap);
        assert_eq!(last.omitted_terminals, 64);
        assert_eq!(last.recent.first().unwrap().revision, 137);
        assert_eq!(last.recent.last().unwrap().revision, 200);
        drop(client);
        server.join().unwrap().unwrap();
    });
}

#[test]
fn terminal_history_retains_failed_replacements_and_resets_exact_source_identity() {
    let mut a = Source::with_history(256);
    for _ in 0..70 {
        a.publish();
    }
    let mut b = Source::new();
    b.publish();
    let host = receiving(a.generation, 1);
    let (mut client, server) = a.pair();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server));
        for next in [64, 70] {
            let observation = terminal_page(
                client
                    .read_next(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
                    .unwrap(),
            );
            assert_eq!(observation.next.revision, next);
            complete(&host.0.shared, 1, Ok(observation));
        }
        drop(client);
        server.join().unwrap().unwrap();
    });
    let original = host.view().last.clone().unwrap();
    assert_eq!(original.recent.len(), 64);
    assert_eq!(original.omitted_terminals, 6);
    {
        let mut state = host.0.shared.state.lock().unwrap();
        state.view = Arc::new(Presentation {
            requested: 2,
            desired_generation: Some(b.generation),
            phase: Phase::Queued,
            last: Some(original.clone()),
        });
        state.active = Some(2);
    }
    let queued = host.view().last.clone().unwrap();
    assert!(Arc::ptr_eq(&original.observation, &queued.observation));
    assert!(Arc::ptr_eq(&original.recent, &queued.recent));
    complete(
        &host.0.shared,
        2,
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "replacement refused",
        )),
    );
    let failed = host.view();
    assert!(matches!(failed.phase, Phase::Failed(_)));
    let last = failed.last.as_ref().unwrap();
    assert_eq!(last.attachment, 1);
    assert!(Arc::ptr_eq(&original.observation, &last.observation));
    assert!(Arc::ptr_eq(&original.recent, &last.recent));
    assert_eq!(last.omitted_terminals, original.omitted_terminals);
    {
        let mut state = host.0.shared.state.lock().unwrap();
        state.view = Arc::new(Presentation {
            requested: 3,
            desired_generation: Some(a.generation),
            phase: Phase::Reading,
            last: Some(original.clone()),
        });
        state.active = Some(3);
    }
    // A new attachment starts its own delivered history even when the source
    // generation is unchanged and its first page repeats old source revisions.
    complete(&host.0.shared, 3, Ok(terminal_page(first_page(&a))));
    let replaced = host.view();
    let last = replaced.last.as_ref().unwrap();
    assert_eq!(last.attachment, 3);
    assert_eq!(last.observation.snapshot.generation, a.generation);
    assert_eq!(last.recent.len(), 64);
    assert_eq!(last.recent[0].revision, 1);
    assert_eq!(last.omitted_terminals, 0);
    let stale = complete(&host.0.shared, 1, Ok(terminal_page(first_page(&a))));
    assert!(!stale.keep_reading);
    assert!(Arc::ptr_eq(&replaced, &host.view()));

    // Generation is independently part of the carry boundary, even if an
    // internal delivery accidentally supplies the same attachment number.
    let same_attachment = receiving(b.generation, 1);
    same_attachment.0.shared.state.lock().unwrap().view = Arc::new(Presentation {
        requested: 1,
        desired_generation: Some(b.generation),
        phase: Phase::Reading,
        last: Some(original),
    });
    complete(
        &same_attachment.0.shared,
        1,
        Ok(terminal_page(first_page(&b))),
    );
    let view = same_attachment.view();
    let last = view.last.as_ref().unwrap();
    assert_eq!(last.observation.snapshot.generation, b.generation);
    assert_eq!(last.recent.len(), 1);
    assert_eq!(last.omitted_terminals, 0);
}
